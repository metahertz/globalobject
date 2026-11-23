use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use globalobject_client::{
    agent,
    config::{format_size, parse_size, Paths},
    service, setup, Client, Retention,
};
use globalobject_net::NetMode;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "globalobject", version, about = "GlobalObject: encrypted backup, stored on other members' machines.\n\nStart with `globalobject setup`.")]
struct Args {
    /// Normally read from the config that `setup` writes.
    #[arg(long, env = "GLOBALOBJECT_CONTROL_PLANE", global = true)]
    control_plane: Option<String>,
    /// Normally read from the credentials file that `setup` writes.
    #[arg(long, env = "GLOBALOBJECT_PHRASE", hide_env_values = true, global = true)]
    phrase: Option<String>,
    #[arg(long, env = "GLOBALOBJECT_PASSPHRASE", hide_env_values = true, global = true)]
    passphrase: Option<String>,
    /// Account state directory. Normally inside the data directory `setup` uses.
    #[arg(long, env = "GLOBALOBJECT_STATE", global = true)]
    state: Option<PathBuf>,
    /// Loopback only, with no relays. For local grids.
    #[arg(long, env = "GLOBALOBJECT_LOCAL", global = true)]
    local: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Join a pool, or reconnect to your account on a new machine, in a few questions.
    Setup(setup::SetupArgs),
    /// Is everything healthy? Backups, space, the background service.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Back up now: the configured folders, or the path given.
    Backup { path: Option<PathBuf> },
    /// List snapshots.
    Snapshots,
    /// Restore a snapshot into a folder.
    Restore { snapshot: String, dest: PathBuf },
    /// Open the dashboard in your browser.
    Dashboard,
    /// Change how much disk you share, like `globalobject slice 500GB`. Shrinking drains first.
    Slice { size: String },
    /// Show, add or remove folders to back up.
    Paths {
        #[command(subcommand)]
        cmd: Option<PathsCmd>,
    },
    /// Install, control or inspect the background service.
    Service {
        #[command(subcommand)]
        cmd: ServiceCmd,
    },
    /// Make a one-use invite link for this account's pool.
    Invite,
    /// Run the background service in the foreground: storage slice, scheduled backups and
    /// dashboard. The installed service runs this.
    Agent,
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
    /// Create an account from a bare invite code (scripts; `setup` does this for people).
    #[command(hide = true)]
    Register { invite: String },
    /// Print the account ID and node-owner seed, for a standalone `globalobject-node`.
    Account,
    /// Set a node's slice size directly, like `pledge 7 500GB`.
    #[command(hide = true)]
    Pledge { node: i64, size: String },
}

#[derive(Subcommand)]
enum PathsCmd {
    Add { path: PathBuf },
    Remove { path: PathBuf },
}

#[derive(Subcommand)]
enum ServiceCmd {
    Install,
    Uninstall,
    Start,
    Stop,
    Restart,
    Status,
    Logs {
        #[arg(long, short)]
        follow: bool,
    },
}

/// Where to connect and as whom: flags and environment first, then what `setup` saved.
struct Ctx {
    cp: String,
    phrase: String,
    passphrase: String,
    state: PathBuf,
    net: NetMode,
}

fn context(a: &Args, paths: &Paths) -> Result<Ctx> {
    let saved = if paths.is_set_up() { Some(paths.load()?) } else { None };
    let cp = a.control_plane.clone().or_else(|| saved.as_ref().map(|(c, _)| c.control_plane.clone())).unwrap_or_else(|| "http://127.0.0.1:7400".into());
    let phrase = match (&a.phrase, &saved) {
        (Some(p), _) => p.clone(),
        (None, Some((_, k))) => k.phrase.clone(),
        (None, None) => bail!("this machine isn't set up yet: run `globalobject setup`"),
    };
    let passphrase = a.passphrase.clone().or_else(|| saved.as_ref().map(|(_, k)| k.passphrase.clone())).unwrap_or_default();
    let state = a.state.clone().unwrap_or_else(|| if saved.is_some() { paths.state_dir() } else { ".globalobject-state".into() });
    let local = a.local || saved.as_ref().is_some_and(|(c, _)| c.local);
    Ok(Ctx { cp, phrase, passphrase, state, net: if local { NetMode::Local } else { NetMode::Internet } })
}

fn print_prune(p: &globalobject_client::PruneSummary) {
    println!("pruned: {} packs ({}) deleted, {} repacked", p.packs_removed, format_size(p.bytes_removed), p.packs_repacked);
}

fn ago(t: f64) -> String {
    let d = (agent::now() - t).max(0.0);
    match d {
        d if d < 90.0 => "just now".into(),
        d if d < 5400.0 => format!("{:.0} minutes ago", d / 60.0),
        d if d < 172800.0 => format!("{:.0} hours ago", d / 3600.0),
        d => format!("{:.0} days ago", d / 86400.0),
    }
}

fn meter(used: u64, total: u64) -> String {
    let frac = if total == 0 { 0.0 } else { (used as f64 / total as f64).min(1.0) };
    let filled = (frac * 20.0).round() as usize;
    format!("[{}{}] {:>3.0}%", "#".repeat(filled), "-".repeat(20 - filled), frac * 100.0)
}

async fn status(ctx: &Ctx, paths: &Paths, json: bool) -> Result<()> {
    let c = Client::open(&ctx.cp, &ctx.phrase, &ctx.passphrase, &ctx.state, ctx.net).await?;
    let account = c.status().await;
    c.shutdown().await;
    let mut ag = agent::read_status(paths);
    // The agent's file lags by up to a tick: a backup run from the command line may be newer.
    if let (Some(s), Some(r)) = (ag.as_mut(), agent::read_backup(paths)) {
        if s.last_backup.as_ref().is_none_or(|old| r.started > old.started) {
            s.last_backup = Some(r);
        }
    }
    let running = service::is_running() || ag.as_ref().is_some_and(|s| agent::now() - s.updated < 120.0);
    if json {
        println!("{}", serde_json::to_string_pretty(&serde_json::json!({"account": account.as_ref().ok(), "agent": ag, "service_running": running}))?);
        return Ok(());
    }
    let ok = |b: bool| if b { "✓" } else { "✗" };
    println!("GlobalObject account {} in pool {} on {}\n", c.id(), c.pool(), ctx.cp);
    match &account {
        Ok(a) => {
            let state = match a.state.as_str() {
                "active" => "active".to_string(),
                "read_only" => "READ-ONLY: you're using more than your share earns. Share more disk, or delete snapshots".into(),
                s => s.to_string(),
            };
            println!("  {} Account {state}", ok(a.state == "active"));
        }
        Err(e) => println!("  ✗ Can't reach the control plane: {e:#}"),
    }
    match &ag {
        Some(s) if running => println!("  ✓ Background service running (node {}, dashboard {})", s.node.unwrap_or_default(), s.dashboard),
        _ if paths.is_set_up() => println!("  ✗ Background service not running: `globalobject service start`, or `globalobject service install`"),
        _ => {}
    }
    if let Some(s) = &ag {
        match (&s.running, &s.last_backup) {
            (Some(j), _) => println!("  … {} running since {}", j.kind, ago(j.started)),
            (None, Some(b)) if b.ok == Some(true) => println!("  ✓ Last backup {}: {}", ago(b.finished.unwrap_or(b.started)), b.message),
            (None, Some(b)) => println!("  ✗ Last backup FAILED {}: {}", ago(b.finished.unwrap_or(b.started)), b.message),
            (None, None) => println!("  … No backup yet"),
        }
    }
    if let Ok(a) = &account {
        println!();
        println!("  Backup space   {} of {} used  {}", format_size(a.used_raw), format_size(a.quota_raw), meter(a.used_raw, a.quota_raw));
        if let Some(s) = &ag {
            println!("  Shared disk    {} of {} used  {}", format_size(s.slice_used), format_size(s.slice_size), meter(s.slice_used, s.slice_size));
        }
        println!("  Download rate  up to {}/s", format_size(a.entitled_bps as u64));
        let p = a.policy;
        println!("  Redundancy     {}", if p.k == 1 { format!("{} copies", p.n) } else { format!("{}-of-{} erasure coding", p.k, p.n) });
    }
    Ok(())
}

fn edit_config(paths: &Paths, f: impl FnOnce(&mut globalobject_client::config::Config) -> Result<()>) -> Result<()> {
    let (mut cfg, _) = paths.load()?;
    f(&mut cfg)?;
    paths.save_config(&cfg)?;
    if service::is_running() {
        println!("The background service picks this up within a minute.");
    }
    Ok(())
}

// Blocking work goes through `spawn_blocking`, which needs worker threads free.
#[tokio::main]
async fn main() -> Result<()> {
    let a = Args::parse();
    // The agent's log is its activity record; commands stay quiet unless something's wrong.
    let default = if matches!(a.cmd, Cmd::Agent) { "info" } else { "warn" };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| default.into());
    tracing_subscriber::fmt().with_writer(std::io::stderr).with_env_filter(filter).init();
    let paths = Paths::detect()?;
    match a.cmd {
        Cmd::Setup(s) => return setup::run(s).await,
        Cmd::Agent => return agent::run(paths).await,
        Cmd::Service { cmd } => {
            match cmd {
                ServiceCmd::Install => println!("Installed and started. {}", service::install(&paths)?),
                ServiceCmd::Uninstall => {
                    service::uninstall()?;
                    println!("Service removed. Your settings and account are untouched.");
                }
                ServiceCmd::Start => service::control("start")?,
                ServiceCmd::Stop => service::control("stop")?,
                ServiceCmd::Restart => service::control("restart")?,
                ServiceCmd::Status => {
                    if service::is_running() {
                        println!("running");
                    } else {
                        println!("not running");
                        std::process::exit(3);
                    }
                }
                ServiceCmd::Logs { follow } => service::logs(follow)?,
            }
            return Ok(());
        }
        Cmd::Dashboard => {
            let (cfg, creds) = paths.load()?;
            let base = agent::read_status(&paths).map(|s| s.dashboard).unwrap_or_else(|| format!("http://127.0.0.1:{}", cfg.dashboard.port));
            let url = format!("{base}/login?token={}", creds.dashboard_token);
            if !service::is_running() && agent::read_status(&paths).is_none_or(|s| agent::now() - s.updated > 120.0) {
                println!("The background service isn't running, so the dashboard isn't either: `globalobject service start`.");
            }
            if open::that(&url).is_err() {
                println!("Open this in your browser:");
            }
            println!("{url}");
            return Ok(());
        }
        Cmd::Paths { cmd } => {
            match cmd {
                None => {
                    for p in paths.load()?.0.backup.paths {
                        println!("{}", p.display());
                    }
                }
                Some(PathsCmd::Add { path }) => {
                    let path = std::fs::canonicalize(&path).with_context(|| format!("{} doesn't exist", path.display()))?;
                    edit_config(&paths, |c| {
                        if !c.backup.paths.contains(&path) {
                            c.backup.paths.push(path.clone());
                        }
                        Ok(())
                    })?;
                    println!("Backing up {}", path.display());
                }
                Some(PathsCmd::Remove { path }) => {
                    let path = std::fs::canonicalize(&path).unwrap_or(path);
                    edit_config(&paths, |c| {
                        let before = c.backup.paths.len();
                        c.backup.paths.retain(|p| p != &path);
                        anyhow::ensure!(c.backup.paths.len() < before, "{} isn't in the list", path.display());
                        Ok(())
                    })?;
                    println!("No longer backing up {}. Existing snapshots keep it until they expire.", path.display());
                }
            }
            return Ok(());
        }
        Cmd::Slice { size } => {
            let n = parse_size(&size)?;
            anyhow::ensure!(n >= 1_000_000_000, "share at least 1 GB");
            edit_config(&paths, |c| {
                c.slice.size = n;
                Ok(())
            })?;
            println!("Sharing {}. Shrinking moves other members' pieces away first, so space frees up gradually.", format_size(n));
            return Ok(());
        }
        _ => {}
    }

    let ctx = context(&a, &paths)?;
    if let Cmd::Register { invite } = &a.cmd {
        let c = Client::register(&ctx.cp, invite, &ctx.phrase, &ctx.passphrase, &ctx.state, ctx.net).await?;
        println!("account {} in pool {}", c.id(), c.pool());
        c.shutdown().await;
        return Ok(());
    }
    if let Cmd::Status { json } = a.cmd {
        return status(&ctx, &paths, json).await;
    }
    let c = Client::open(&ctx.cp, &ctx.phrase, &ctx.passphrase, &ctx.state, ctx.net).await?;
    match a.cmd {
        Cmd::Account => println!("{} {}", c.id(), hex::encode(c.signing().seed())),
        Cmd::Invite => {
            let (code, link) = c.invite_link().await?;
            println!("{}", link.unwrap_or(code));
        }
        Cmd::Backup { path } => {
            let targets = match path {
                Some(p) => vec![p],
                None => paths.load().map(|(c, _)| c.backup.paths).context("no folders configured: pass a path, or run `globalobject setup`")?,
            };
            let started = agent::now();
            let result = c.backup_paths(&targets).await;
            if paths.is_set_up() {
                let message = match &result {
                    Ok(s) => format!(
                        "snapshot {}: {} files, {} read, {} new (from the command line)",
                        &s.snapshot[..8.min(s.snapshot.len())],
                        s.files,
                        format_size(s.bytes_in),
                        format_size(s.bytes_added)
                    ),
                    Err(e) => format!("{e:#}"),
                };
                agent::record_backup(
                    &paths,
                    &agent::JobRecord { kind: "backup".into(), started, finished: Some(agent::now()), ok: Some(result.is_ok()), message },
                );
            }
            let s = result?;
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
        Cmd::Pledge { node, size } => {
            c.set_pledge(node, parse_size(&size)?).await?;
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
        _ => unreachable!("handled above"),
    }
    c.shutdown().await;
    Ok(())
}
