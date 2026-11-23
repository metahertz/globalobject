use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use globalobject_control_plane::{admin, connect, ops::Cidr, serve, Config};

#[derive(Parser)]
#[command(about = "GlobalObject control plane", version)]
struct Args {
    #[command(flatten)]
    serve: ServeArgs,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the control plane. This is the default.
    Serve,
    /// Operator commands: status, pools, invites, nodes. Talks to a running control plane.
    Admin {
        /// The control plane to manage.
        #[arg(long, env = "GLOBALOBJECT_SERVER", default_value = "http://127.0.0.1:7400")]
        server: String,
        #[command(subcommand)]
        cmd: admin::AdminCmd,
    },
}

#[derive(clap::Args)]
struct ServeArgs {
    /// MongoDB connection string. Must reach a replica set (one node is enough), since the
    /// control plane uses transactions: for example
    /// `mongodb://127.0.0.1:27017/globalobject?replicaSet=rs0`, or an Atlas `mongodb+srv://` URI.
    #[arg(long, env = "MONGODB_URI", hide_env_values = true)]
    mongodb_uri: Option<String>,
    /// Database to use, instead of the one named in the URI (default `globalobject`).
    #[arg(long, env = "MONGODB_DATABASE")]
    database: Option<String>,
    #[arg(long, env = "GLOBALOBJECT_LISTEN", default_value = "127.0.0.1:7400")]
    listen: String,
    /// Required to create pools and to use `admin`. Set a long random value.
    #[arg(long, env = "GLOBALOBJECT_OPERATOR_TOKEN", hide_env_values = true, default_value = "")]
    operator_token: String,
    /// File holding the grant-signing seed. Created if missing.
    #[arg(long, env = "GLOBALOBJECT_KEY_FILE", default_value = "control-plane.key")]
    key_file: std::path::PathBuf,
    /// The address users reach this service at, such as `https://cp.example.org`. Used in
    /// join links.
    #[arg(long, env = "GLOBALOBJECT_PUBLIC_URL")]
    public_url: Option<String>,
    /// A reverse proxy whose `X-Forwarded-For` header to believe, as an address or CIDR.
    /// Repeat for several.
    #[arg(long, env = "GLOBALOBJECT_TRUSTED_PROXY", value_delimiter = ',')]
    trusted_proxy: Vec<Cidr>,
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
    match a.cmd {
        Some(Cmd::Admin { server, cmd }) => admin::run(&server, &a.serve.operator_token, cmd).await,
        Some(Cmd::Serve) | None => run_server(a.serve).await,
    }
}

async fn run_server(a: ServeArgs) -> Result<()> {
    let uri = a.mongodb_uri.context("set MONGODB_URI (or --mongodb-uri) to a MongoDB replica set")?;
    if a.operator_token.is_empty() {
        tracing::warn!("no operator token set: pools can't be created and `admin` won't work");
    }
    let seed: [u8; 32] = match std::fs::read(&a.key_file).ok().and_then(|b| b.try_into().ok()) {
        Some(s) => s,
        None => {
            let s = globalobject_crypto::random_key();
            std::fs::write(&a.key_file, s).with_context(|| format!("write {}", a.key_file.display()))?;
            s
        }
    };
    let db = connect(&uri, a.database.as_deref()).await?;
    let listener = tokio::net::TcpListener::bind(&a.listen).await?;
    println!("control plane on {}", listener.local_addr()?);
    let cfg = Config {
        operator_token: a.operator_token,
        signing_seed: seed,
        grace_secs: a.grace_secs,
        net: if a.local { globalobject_net::NetMode::Local } else { globalobject_net::NetMode::Internet },
        public_url: a.public_url,
        trusted_proxies: a.trusted_proxy,
        ..Config::production_defaults()
    };
    let handle = serve(cfg, db, listener).await?;
    handle.wait().await;
    Ok(())
}
