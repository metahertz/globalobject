//! Control plane: identity, pools, placement, grants, ledger, audit and repair.
//! It holds no decryption keys and never sees file names or contents.

mod api;
pub mod db;
mod reconcile;

use anyhow::Result;
use db::{bytes_by, int, Db, DbResult};
use globalobject_crypto::Identity;
use globalobject_net::NetMode;
use globalobject_policy::{Policy, QuotaParams};
use globalobject_proto::*;
use mongodb::{bson::doc, ClientSession};
use std::{sync::Arc, time::Duration};
use tokio::{net::TcpListener, task::JoinHandle};

#[derive(Clone)]
pub struct Config {
    pub operator_token: String,
    pub signing_seed: [u8; 32],
    /// A host unseen for this long stops counting as healthy.
    pub grace_secs: f64,
    /// Over quota this long moves an account to read-only.
    pub over_quota_grace_secs: f64,
    /// Read-only this long leads to deletion.
    pub read_only_secs: f64,
    pub grant_ttl_secs: u64,
    pub reconcile_every: Duration,
    pub audit_every: Duration,
    pub audits_per_round: usize,
    /// Failed audits, in total, leading to disqualification.
    pub disqualify_after: i32,
    pub probe_every: Duration,
    /// Measure new nodes straight away, rather than leaving them on `unmeasured_bps`
    /// until the next scheduled round.
    pub probe_new_nodes: bool,
    /// Samples kept per target. The median becomes its measured upload.
    pub probe_samples: i64,
    /// Rate for accounts with no measurement yet, in bytes per second.
    pub unmeasured_bps: f64,
    pub burst_secs: f64,
    /// Refusal reports from this many different accounts make a host disqualified.
    pub refusals_to_disqualify: i64,
    /// Max segments per placement group.
    pub group_segments: i32,
    pub repairs_per_round: i64,
    pub repair_timeout_secs: f64,
    pub pending_timeout_secs: f64,
    pub net: NetMode,
}

impl Config {
    pub fn production_defaults() -> Self {
        Config {
            operator_token: String::new(),
            signing_seed: [0; 32],
            grace_secs: 72.0 * 3600.0,
            over_quota_grace_secs: 14.0 * 86400.0,
            read_only_secs: 30.0 * 86400.0,
            grant_ttl_secs: 600,
            reconcile_every: Duration::from_secs(30),
            audit_every: Duration::from_secs(60),
            audits_per_round: 32,
            disqualify_after: 8,
            probe_every: Duration::from_secs(3600),
            probe_new_nodes: true,
            probe_samples: 5,
            unmeasured_bps: 125_000.0,
            burst_secs: 10.0,
            refusals_to_disqualify: 3,
            group_segments: 16,
            repairs_per_round: 64,
            repair_timeout_secs: 3600.0,
            pending_timeout_secs: 3600.0,
            net: NetMode::Internet,
        }
    }
}

pub struct Shared {
    pub cfg: Config,
    pub db: Db,
    pub identity: Identity,
    /// For auditing hosts. Holds no data, and accepts no connections.
    pub endpoint: iroh::Endpoint,
}

pub fn fnow() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("clock").as_secs_f64()
}

/// Connect to MongoDB, using `database` instead of the URI's default database if given.
/// Transactions need a replica set: a single-node one is enough.
pub async fn connect(uri: &str, database: Option<&str>) -> Result<Db> {
    Ok(Db::connect(uri, database).await?)
}

pub struct Handle {
    pub addr: std::net::SocketAddr,
    pub shared: Arc<Shared>,
    tasks: Vec<JoinHandle<()>>,
}

impl Handle {
    pub async fn wait(mut self) {
        if let Some(t) = self.tasks.pop() {
            let _ = t.await;
        }
    }
    pub fn stop(&self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

pub async fn serve(cfg: Config, db: Db, listener: TcpListener) -> Result<Handle> {
    let addr = listener.local_addr()?;
    let shared = Arc::new(Shared { identity: Identity::from_seed(&cfg.signing_seed), endpoint: globalobject_net::bind(cfg.net, vec![]).await?, cfg, db });
    let mut tasks = reconcile::spawn(shared.clone());
    let app = api::router(shared.clone());
    tasks.push(tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).await {
            tracing::error!("server stopped: {e}");
        }
    }));
    Ok(Handle { addr, shared, tasks })
}

// ------------------------------------------------- helpers used by both halves

pub(crate) struct Issued {
    pub token: String,
    pub nonce: String,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn sign_grant(
    s: &Shared,
    account: i64,
    pool: i64,
    segment: &str,
    piece: i32,
    host: i64,
    peer: &str,
    hash: &str,
    op: Op,
    max_bytes: u64,
    rate_bps: u64,
) -> Issued {
    sign_grant_range(s, account, pool, segment, piece, host, peer, hash, op, max_bytes, rate_bps, None)
}

/// As `sign_grant`, limited to bytes `[start, end)` of the piece.
#[allow(clippy::too_many_arguments)]
pub(crate) fn sign_grant_range(
    s: &Shared,
    account: i64,
    pool: i64,
    segment: &str,
    piece: i32,
    host: i64,
    peer: &str,
    hash: &str,
    op: Op,
    max_bytes: u64,
    rate_bps: u64,
    range: Option<(u64, u64)>,
) -> Issued {
    let grant = Grant {
        nonce: globalobject_crypto::random_hex(16),
        account,
        pool,
        segment: segment.to_string(),
        piece: piece as u16,
        host,
        peer: peer.to_string(),
        hash: hash.to_string(),
        op,
        max_bytes,
        rate_bps,
        expires_at: now() + s.cfg.grant_ttl_secs,
        range,
    };
    Issued { token: s.identity.token(T_GRANT, &grant), nonce: grant.nonce }
}

pub(crate) async fn pool_settings(db: &Db, pool: i64) -> DbResult<(Policy, QuotaParams, bool)> {
    let p = db.pools.find_one(doc! {"_id": pool}).await?.ok_or_else(|| missing("pool"))?;
    Ok((p.policy, p.quota, p.distinct_sites))
}

/// A record that must exist, such as one a foreign key used to guarantee, is gone.
pub(crate) fn missing(what: &str) -> db::Error {
    db::Error::custom(format!("{what} not found"))
}

pub(crate) struct Usage {
    pub pledged: u64,
    pub proven_held: u64,
    pub credited: u64,
    pub quota_raw: u64,
    pub used_raw: u64,
    pub entitled_bps: f64,
}

/// Account figures, all computed from committed records and never from client claims.
pub(crate) async fn usage(s: &Shared, account: i64, q: &QuotaParams) -> DbResult<Usage> {
    usage_in(&s.db, &s.cfg, account, q, None).await
}

/// As `usage`, inside `session`'s transaction when given, so the figures are consistent with
/// what the transaction then writes.
pub(crate) async fn usage_in(db: &Db, cfg: &Config, account: i64, q: &QuotaParams, mut session: Option<&mut ClientSession>) -> DbResult<Usage> {
    let cutoff = fnow() - cfg.grace_secs;
    let nodes = db::aggregate(&db.nodes, vec![doc! {"$match": {"account": account}}], session.as_deref_mut()).await?;
    let nodes: Vec<db::NodeDoc> = nodes.into_iter().map(mongodb::bson::from_document).collect::<Result<_, _>>()?;
    let ids: Vec<i64> = nodes.iter().map(|n| n.id).collect();
    let held = bytes_by(db, "pieces.host", &ids, doc! {"state": "stored"}, session.as_deref_mut()).await?;
    let (mut pledged, mut held_total, mut credited, mut rate, mut measured) = (0u64, 0u64, 0u64, 0.0, false);
    for n in &nodes {
        let p = n.pledged as u64;
        let h = held.get(&n.id).copied().unwrap_or(0) as u64;
        pledged += p;
        held_total += h;
        // Availability is coarse in the prototype: recently seen and in good standing, or not.
        let availability = if n.state == "active" && n.last_seen >= cutoff { 1.0 } else { 0.0 };
        credited += globalobject_policy::credited(p, h, availability, q);
        if availability > 0.0 {
            if let Some(m) = n.measured_bps {
                rate += m;
                measured = true;
            }
        }
    }
    let used = db::aggregate(
        &db.segments,
        vec![
            doc! {"$match": {"owner": account, "state": {"$in": ["pending", "committed"]}}},
            doc! {"$group": {"_id": null, "raw": {"$sum": {"$multiply": ["$piece_len", "$n"]}}}},
        ],
        session,
    )
    .await?;
    Ok(Usage {
        pledged,
        proven_held: held_total,
        credited,
        quota_raw: globalobject_policy::quota_raw(credited, q),
        used_raw: used.first().map_or(0, |d| int(d.get("raw"))) as u64,
        entitled_bps: if measured { rate } else { cfg.unmeasured_bps },
    })
}

pub(crate) async fn ledger(db: &Db, pool: i64, account: i64, kind: &str, bytes: i64, reference: &str) -> DbResult<()> {
    db.ledger.insert_one(db::LedgerDoc { at: fnow(), pool, account, kind: kind.into(), bytes, reference: reference.into() }).await?;
    Ok(())
}
