use anyhow::Result;
use clap::{Parser, Subcommand};
use globalobject_client::{Client, Retention};
use globalobject_net::NetMode;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "globalobject", about = "GlobalObject client. Encrypts on this machine before anything leaves it.")]
struct Args {
    #[arg(long, env = "GLOBALOBJECT_CONTROL_PLANE", default_value = "http://127.0.0.1:7400")]
    control_plane: String,
    /// Keep this safe. Losing it loses your data, and nobody can recover it.
    #[arg(long, env = "GLOBALOBJECT_PHRASE", hide_env_values = true)]
    phrase: String,
    #[arg(long, env = "GLOBALOBJECT_PASSPHRASE", default_value = "", hide_env_values = true)]
    passphrase: String,
    #[arg(long, env = "GLOBALOBJECT_STATE", default_value = ".globalobject-state")]
    state: PathBuf,
    /// Loopback only, with no relays. For local grids.
    #[arg(long, env = "GLOBALOBJECT_LOCAL")]
    local: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create an account from an invite.
    Register { invite: String },
    /// Print the account ID and node-owner seed, for starting `globalobject-node`.
    Account,
    /// Show quota, usage and bandwidth entitlement.
    Status,
    /// Make an invite for this account's pool.
    Invite,
    /// Back up a file or directory as a new snapshot.
    Backup { path: PathBuf },
    /// List snapshots.
    Snapshots,
    /// Restore a snapshot under a directory.
    Restore { snapshot: String, dest: PathBuf },
    /// Change a node's slice size. Shrinking drains data elsewhere first.
    Pledge { node: i64, bytes: u64 },
    /// Remove snapshots outside the retention rules. Their data stays until `prune`.
    Forget {
        #[arg(long)]
        keep_last: Option<i32>,
        #[arg(long)]
        keep_daily: Option<i32>,
        #[arg(long)]
        keep_weekly: Option<i32>,
        #[arg(long)]
        keep_monthly: Option<i32>,
        /// Prune straight afterwards.
        #[arg(long)]
        prune: bool,
        /// With --prune: delete unused packs now, rather than after 23 hours. Only safe when
        /// no other device is backing up to this account.
        #[arg(long)]
        instant_delete: bool,
    },
    /// Delete data that no snapshot uses, and give the space back.
    Prune {
        /// Delete unused packs now, rather than after 23 hours. Only safe when no other device
        /// is backing up to this account.
        #[arg(long)]
        instant_delete: bool,
    },
}

fn print_prune(p: &globalobject_client::PruneSummary) {
    println!("pruned: {} packs ({} bytes) deleted, {} repacked", p.packs_removed, p.bytes_removed, p.packs_repacked);
}

// Blocking work goes through `spawn_blocking`, which needs worker threads free.
#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_writer(std::io::stderr).with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).init();
    let a = Args::parse();
    let net = if a.local { NetMode::Local } else { NetMode::Internet };
    match a.cmd {
        Cmd::Register { invite } => {
            let c = Client::register(&a.control_plane, &invite, &a.phrase, &a.passphrase, &a.state, net).await?;
            println!("account {} in pool {}", c.id(), c.pool());
            c.shutdown().await;
        }
        cmd => {
            let c = Client::open(&a.control_plane, &a.phrase, &a.passphrase, &a.state, net).await?;
            match cmd {
                Cmd::Register { .. } => unreachable!(),
                Cmd::Account => println!("{} {}", c.id(), hex::encode(c.signing().seed())),
                Cmd::Status => println!("{}", serde_json::to_string_pretty(&c.status().await?)?),
                Cmd::Invite => println!("{}", c.invite().await?),
                Cmd::Backup { path } => {
                    let s = c.backup(&path).await?;
                    println!("snapshot {}: {} files, {} bytes read, {} bytes added", s.snapshot, s.files, s.bytes_in, s.bytes_added);
                }
                Cmd::Snapshots => {
                    for id in c.snapshots().await? {
                        println!("{id}");
                    }
                }
                Cmd::Restore { snapshot, dest } => {
                    c.restore(&snapshot, &dest).await?;
                    println!("restored to {}", dest.display());
                }
                Cmd::Pledge { node, bytes } => {
                    c.set_pledge(node, bytes).await?;
                    println!("pledge change requested");
                }
                Cmd::Forget { keep_last, keep_daily, keep_weekly, keep_monthly, prune, instant_delete } => {
                    let keep = Retention { last: keep_last, daily: keep_daily, weekly: keep_weekly, monthly: keep_monthly };
                    anyhow::ensure!(
                        keep.last.or(keep.daily).or(keep.weekly).or(keep.monthly).is_some(),
                        "say what to keep, for example --keep-last 7. Forgetting everything isn't offered"
                    );
                    let gone = c.forget(&keep).await?;
                    println!("forgot {} snapshots", gone.len());
                    for id in &gone {
                        println!("  {id}");
                    }
                    if prune {
                        print_prune(&c.prune(instant_delete).await?);
                    }
                }
                Cmd::Prune { instant_delete } => print_prune(&c.prune(instant_delete).await?),
            }
            c.shutdown().await;
        }
    }
    Ok(())
}
