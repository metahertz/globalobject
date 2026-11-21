use anyhow::Result;
use clap::Parser;
use globalobject_control_plane::{connect, serve, Config};

#[derive(Parser)]
#[command(about = "GlobalObject control plane")]
struct Args {
    /// MongoDB connection string. Must reach a replica set (one node is enough), since the
    /// control plane uses transactions: for example
    /// `mongodb://127.0.0.1:27017/globalobject?replicaSet=rs0`, or an Atlas `mongodb+srv://` URI.
    #[arg(long, env = "MONGODB_URI", hide_env_values = true)]
    mongodb_uri: String,
    /// Database to use, instead of the one named in the URI (default `globalobject`).
    #[arg(long, env = "MONGODB_DATABASE")]
    database: Option<String>,
    #[arg(long, default_value = "127.0.0.1:7400")]
    listen: String,
    /// Required to create pools. Set a long random value.
    #[arg(long, env = "GLOBALOBJECT_OPERATOR_TOKEN", hide_env_values = true)]
    operator_token: String,
    /// File holding the grant-signing seed. Created if missing.
    #[arg(long, default_value = "control-plane.key")]
    key_file: std::path::PathBuf,
    #[arg(long, default_value_t = 259200.0)]
    grace_secs: f64,
    /// Loopback only, with no relays. For local grids.
    #[arg(long)]
    local: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_writer(std::io::stderr).with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).init();
    let a = Args::parse();
    let seed: [u8; 32] = match std::fs::read(&a.key_file).ok().and_then(|b| b.try_into().ok()) {
        Some(s) => s,
        None => {
            let s = globalobject_crypto::random_key();
            std::fs::write(&a.key_file, s)?;
            s
        }
    };
    let db = connect(&a.mongodb_uri, a.database.as_deref()).await?;
    let listener = tokio::net::TcpListener::bind(&a.listen).await?;
    println!("control plane on {}", listener.local_addr()?);
    let cfg = Config {
        operator_token: a.operator_token,
        signing_seed: seed,
        grace_secs: a.grace_secs,
        net: if a.local { globalobject_net::NetMode::Local } else { globalobject_net::NetMode::Internet },
        ..Config::production_defaults()
    };
    let handle = serve(cfg, db, listener).await?;
    handle.wait().await;
    Ok(())
}
