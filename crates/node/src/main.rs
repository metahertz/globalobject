use anyhow::Result;
use clap::Parser;
use globalobject_net::NetMode;
use globalobject_node::{start, AccountAuth, NodeConfig};
use std::{path::PathBuf, time::Duration};

#[derive(Parser)]
#[command(
    about = "GlobalObject storage node, for a machine that shares disk without backing up.\nOn a machine you back up, `globalobject setup` runs one for you.",
    version
)]
struct Args {
    #[arg(long, env = "GLOBALOBJECT_CONTROL_PLANE")]
    control_plane: String,
    /// Owning account ID.
    #[arg(long, env = "GLOBALOBJECT_ACCOUNT")]
    account: i64,
    /// Owning account seed, in hex. `globalobject account` prints it.
    #[arg(long, env = "GLOBALOBJECT_ACCOUNT_SEED", hide_env_values = true)]
    account_seed: String,
    /// Directory for the slice.
    #[arg(long, env = "GLOBALOBJECT_STORE")]
    store: PathBuf,
    /// Slice size, like 500GB or 2TiB.
    #[arg(long, env = "GLOBALOBJECT_PLEDGED", value_parser = parse_size)]
    pledged: u64,
    /// Failure-domain site. Leave unset and the control plane uses this machine's network.
    #[arg(long, default_value = "")]
    site: String,
    #[arg(long, default_value = "")]
    asn: String,
    /// Upload limit in bytes per second, like 2MB. 0 means unlimited.
    #[arg(long, env = "GLOBALOBJECT_UPLOAD_LIMIT", default_value = "0", value_parser = parse_size)]
    link_up_bps: u64,
    #[arg(long, default_value_t = 2)]
    heartbeat_secs: u64,
    /// Loopback only, with no relays. For local grids.
    #[arg(long)]
    local: bool,
}

fn parse_size(s: &str) -> Result<u64, String> {
    s.trim().parse::<bytesize::ByteSize>().map(|b| b.as_u64()).map_err(|e| format!("'{s}' isn't a size like 500GB: {e}"))
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_writer(std::io::stderr).with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).init();
    let a = Args::parse();
    let seed: [u8; 32] = hex::decode(&a.account_seed)?.try_into().map_err(|_| anyhow::anyhow!("seed must be 32 bytes"))?;
    let node = start(NodeConfig {
        control_plane: a.control_plane,
        owner: AccountAuth { id: a.account, seed },
        store: a.store,
        pledged: a.pledged,
        real_capacity: a.pledged,
        site: a.site,
        asn: a.asn,
        link_up_bps: a.link_up_bps,
        heartbeat: Duration::from_secs(a.heartbeat_secs),
        net: if a.local { NetMode::Local } else { NetMode::Internet },
        probe_bytes: 4 << 20,
        gc_every: Duration::from_secs(600),
    })
    .await?;
    println!("node {} as {}", node.id, node.endpoint_id());
    tokio::signal::ctrl_c().await?;
    Ok(())
}
