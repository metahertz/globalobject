//! `globalobject-control-plane admin`: operator commands over the HTTP API, so they work the
//! same against a local or a remote control plane.

use crate::ops::{NodeInfo, PoolInfo, Summary};
use anyhow::{bail, Context, Result};
use clap::Subcommand;
use globalobject_policy::{Policy, QuotaParams};
use globalobject_proto::{CreatePool, InviteCreated, PoolCreated};

#[derive(Subcommand)]
pub enum AdminCmd {
    /// Health at a glance: nodes online, segments needing repair, space.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// List pools.
    Pools,
    /// Create a pool and print the invite link for its first member, who becomes its admin.
    CreatePool {
        name: String,
        /// Anyone with an invite from a member can join. By default only the admin invites.
        #[arg(long)]
        public: bool,
        /// 10-of-20 erasure coding instead of 3 copies. Needs at least 20 independent hosts.
        #[arg(long)]
        erasure: bool,
    },
    /// Make a one-use invite link for a pool.
    Invite { pool: i64 },
    /// List nodes, optionally in one pool.
    Nodes {
        #[arg(long)]
        pool: Option<i64>,
    },
    /// Remove an account from a pool. Its nodes drain before anything is released.
    RemoveMember { pool: i64, account: i64 },
}

struct Api {
    http: reqwest::Client,
    server: String,
    token: String,
}

impl Api {
    async fn send<T: serde::de::DeserializeOwned>(&self, req: reqwest::RequestBuilder) -> Result<T> {
        let resp =
            req.header("x-globalobject-operator", &self.token).send().await.with_context(|| format!("can't reach the control plane at {}", self.server))?;
        let status = resp.status();
        if status == reqwest::StatusCode::FORBIDDEN {
            bail!("the control plane refused the operator token (check GLOBALOBJECT_OPERATOR_TOKEN)");
        }
        if !status.is_success() {
            bail!("{status}: {}", resp.text().await.unwrap_or_default());
        }
        Ok(resp.json().await?)
    }
    async fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        self.send(self.http.get(format!("{}{path}", self.server))).await
    }
    async fn post<T: serde::de::DeserializeOwned>(&self, path: &str, body: &impl serde::Serialize) -> Result<T> {
        self.send(self.http.post(format!("{}{path}", self.server)).json(body)).await
    }
}

pub fn human_bytes(n: f64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB", "PB"];
    let (mut v, mut i) = (n.max(0.0), 0);
    while v >= 1000.0 && i < units.len() - 1 {
        v /= 1000.0;
        i += 1;
    }
    if i == 0 {
        format!("{v:.0} {}", units[i])
    } else {
        format!("{v:.1} {}", units[i])
    }
}

fn ago(secs: f64) -> String {
    match secs {
        s if s < 90.0 => format!("{s:.0}s"),
        s if s < 5400.0 => format!("{:.0}m", s / 60.0),
        s if s < 172800.0 => format!("{:.0}h", s / 3600.0),
        s => format!("{:.0}d", s / 86400.0),
    }
}

fn print_summary(s: &Summary) {
    let total: i64 = s.nodes.values().sum();
    let mark = |bad: bool| if bad { "!!" } else { "ok" };
    println!("[{}] nodes online       {} of {}", mark(s.nodes_online == 0 && total > 0), s.nodes_online, total);
    println!("[{}] unreadable now     {} segments (fewer than k healthy pieces)", mark(s.segments_unreadable > 0), s.segments_unreadable);
    println!("[{}] due for repair     {} segments ({} repairs in flight)", mark(s.segments_below_repair > 0), s.segments_below_repair, s.repairs_in_flight);
    println!("     degraded           {} segments (fewer than n healthy pieces)", s.segments_degraded);
    println!("     space              {} stored of {} pledged", human_bytes(s.stored_bytes as f64), human_bytes(s.pledged_bytes as f64));
    let list = |m: &std::collections::BTreeMap<String, i64>| m.iter().map(|(k, v)| format!("{v} {k}")).collect::<Vec<_>>().join(", ");
    println!("     pools              {}", s.pools);
    println!("     accounts           {}", list(&s.accounts));
    println!("     nodes by state     {}", list(&s.nodes));
    println!("     segments           {}", list(&s.segments));
    println!("     pending deletes    {}", s.pending_deletes);
}

pub async fn run(server: &str, token: &str, cmd: AdminCmd) -> Result<()> {
    if token.is_empty() {
        bail!("set GLOBALOBJECT_OPERATOR_TOKEN (or pass --operator-token)");
    }
    let api = Api { http: reqwest::Client::builder().no_proxy().build()?, server: server.trim_end_matches('/').to_string(), token: token.into() };
    match cmd {
        AdminCmd::Status { json } => {
            let s: Summary = api.get("/v1/admin/summary").await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&s)?);
            } else {
                print_summary(&s);
            }
        }
        AdminCmd::Pools => {
            let pools: Vec<PoolInfo> = api.get("/v1/pools").await?;
            println!("{:>4}  {:<20} {:<8} {:<8} {:>7} {:>12}", "ID", "NAME", "TYPE", "POLICY", "MEMBERS", "NODES ONLINE");
            for p in pools {
                let kind = if p.private { "private" } else { "public" };
                println!("{:>4}  {:<20} {:<8} {:<8} {:>7} {:>12}", p.id, p.name, kind, format!("{}-of-{}", p.policy.k, p.policy.n), p.members, p.nodes_online);
            }
        }
        AdminCmd::CreatePool { name, public, erasure } => {
            let policy = if erasure { Policy::ec(10, 20, 14) } else { Policy::replicate3() };
            let req = CreatePool { name, private: !public, policy, quota: QuotaParams::default(), distinct_sites: false };
            let r: PoolCreated = api.post("/v1/pools", &req).await?;
            println!("Created pool {}.", r.pool);
            println!("Send this to its first member, who becomes the pool's admin:");
            println!("  {}", r.join_url.unwrap_or(r.admin_invite));
        }
        AdminCmd::Invite { pool } => {
            let r: InviteCreated = api.post(&format!("/v1/pools/{pool}/invites"), &serde_json::json!({})).await?;
            println!("{}", r.join_url.unwrap_or(r.invite));
        }
        AdminCmd::Nodes { pool } => {
            let q = pool.map(|p| format!("?pool={p}")).unwrap_or_default();
            let nodes: Vec<NodeInfo> = api.get(&format!("/v1/admin/nodes{q}")).await?;
            println!(
                "{:>5} {:>4} {:>7}  {:<12} {:<20} {:>9} {:>9} {:>6} {:>10}",
                "ID", "POOL", "ACCOUNT", "STATE", "SITE", "PLEDGED", "HELD", "SEEN", "AUDITS"
            );
            for n in nodes {
                println!(
                    "{:>5} {:>4} {:>7}  {:<12} {:<20} {:>9} {:>9} {:>6} {:>10}",
                    n.id,
                    n.pool,
                    n.account,
                    n.state,
                    n.site,
                    human_bytes(n.pledged as f64),
                    human_bytes(n.held as f64),
                    ago(n.last_seen_secs_ago),
                    format!("{}/{}", n.audit_ok, n.audit_fail)
                );
            }
        }
        AdminCmd::RemoveMember { pool, account } => {
            let _: serde_json::Value = api.post(&format!("/v1/pools/{pool}/members/{account}/remove"), &serde_json::json!({})).await?;
            println!("Account {account} is now read-only in pool {pool}; its nodes are draining.");
        }
    }
    Ok(())
}
