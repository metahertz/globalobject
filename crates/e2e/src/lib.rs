//! Test harness: a real control plane on MongoDB, and real nodes and clients talking over
//! local HTTP. Every test gets its own database.

use anyhow::Result;
use globalobject_client::Client;
use globalobject_control_plane::{connect, db::Db, serve, Config, Handle};
use globalobject_net::NetMode;
use globalobject_node::{start, AccountAuth, Node, NodeConfig};
use globalobject_policy::{Policy, QuotaParams};
use globalobject_proto::*;
use std::path::PathBuf;
use std::{
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};
use tempfile::TempDir;

pub const MB: u64 = 1 << 20;
pub const GB: u64 = 1 << 30;
pub const OPERATOR: &str = "test-operator-token";
pub const SEED: [u8; 32] = [42; 32];
/// A local single-node replica set, as `scripts/test.sh` expects.
pub const DEFAULT_MONGODB_URI: &str = "mongodb://127.0.0.1:27017/?replicaSet=rs0&directConnection=true";

pub fn fast_config() -> Config {
    Config {
        operator_token: OPERATOR.into(),
        signing_seed: SEED,
        grace_secs: 4.0,
        over_quota_grace_secs: 2.0,
        read_only_secs: 3.0,
        grant_ttl_secs: 120,
        reconcile_every: Duration::from_millis(250),
        audit_every: Duration::from_millis(400),
        audits_per_round: 16,
        disqualify_after: 3,
        probe_every: Duration::from_secs(3600),
        probe_new_nodes: true,
        probe_samples: 5,
        unmeasured_bps: 200.0 * MB as f64,
        burst_secs: 1.0,
        refusals_to_disqualify: 3,
        group_segments: 16,
        repairs_per_round: 64,
        repair_timeout_secs: 30.0,
        pending_timeout_secs: 600.0,
        net: NetMode::Local,
        public_url: None,
        trusted_proxies: vec![],
        metrics_every: Duration::from_millis(500),
    }
}

pub fn generous_quota() -> QuotaParams {
    QuotaParams { reserve: 0.2, headroom: 1.5, probation_allowance: 4 * GB }
}

pub struct Member {
    pub client: Client,
    pub nodes: Vec<Arc<Node>>,
    pub phrase: String,
    _state: TempDir,
}

pub struct Grid {
    pub cp: Handle,
    pub url: String,
    pub tmp: TempDir,
    counter: std::sync::atomic::AtomicU32,
}

pub struct Spec {
    pub pledged: u64,
    pub real_capacity: Option<u64>,
    pub link_up_bps: u64,
    pub site: Option<String>,
    pub disks: usize,
}

impl Default for Spec {
    fn default() -> Self {
        Spec { pledged: 8 * GB, real_capacity: None, link_up_bps: 0, site: None, disks: 1 }
    }
}

impl Grid {
    pub async fn start(cfg: Config) -> Result<Grid> {
        let uri = std::env::var("MONGODB_URI").unwrap_or_else(|_| DEFAULT_MONGODB_URI.into());
        let db = connect(&uri, Some(&format!("t_{}", globalobject_crypto::random_hex(8)))).await?;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let cp = serve(cfg, db, listener).await?;
        let url = format!("http://{}", cp.addr);
        Ok(Grid { cp, url, tmp: tempfile::tempdir()?, counter: 0.into() })
    }

    pub fn db(&self) -> &Db {
        &self.cp.shared.db
    }

    pub async fn pool(&self, private: bool, policy: Policy, quota: QuotaParams, distinct_sites: bool) -> Result<PoolCreated> {
        let r = reqwest::Client::builder()
            .no_proxy()
            .build()?
            .post(format!("{}/v1/pools", self.url))
            .header("x-globalobject-operator", OPERATOR)
            .json(&CreatePool { name: "test".into(), private, policy, quota, distinct_sites })
            .send()
            .await?;
        anyhow::ensure!(r.status().is_success(), "pool: {} {}", r.status(), r.text().await?);
        Ok(r.json().await?)
    }

    fn next(&self) -> u32 {
        self.counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    }

    pub async fn member(&self, invite: &str, spec: Spec) -> Result<Member> {
        let i = self.next();
        let phrase = format!("test phrase {i} {}", globalobject_crypto::random_hex(8));
        let state = tempfile::tempdir()?;
        let client = Client::register(&self.url, invite, &phrase, "", state.path(), NetMode::Local).await?;
        let mut nodes = vec![];
        for d in 0..spec.disks {
            nodes.push(self.node(&client, &spec, d, i).await?);
        }
        Ok(Member { client, nodes, phrase, _state: state })
    }

    async fn node(&self, client: &Client, spec: &Spec, disk: usize, owner: u32) -> Result<Arc<Node>> {
        let store = self.tmp.path().join(format!("node-{owner}-{disk}"));
        start(NodeConfig {
            control_plane: self.url.clone(),
            owner: AccountAuth { id: client.id(), seed: client.signing().seed() },
            store,
            pledged: spec.pledged,
            real_capacity: spec.real_capacity.unwrap_or(spec.pledged),
            site: spec.site.clone().unwrap_or_else(|| format!("site-{owner}")),
            asn: format!("asn-{owner}"),
            link_up_bps: spec.link_up_bps,
            heartbeat: Duration::from_millis(150),
            net: NetMode::Local,
            probe_bytes: MB,
            gc_every: Duration::from_secs(1),
        })
        .await
    }

    /// A pool with `owners` members, each with one node. The first is the admin.
    pub async fn pool_with_members(&self, owners: usize, policy: Policy, quota: QuotaParams, private: bool) -> Result<(PoolCreated, Vec<Member>)> {
        let p = self.pool(private, policy, quota, false).await?;
        let first = self.member(&p.admin_invite, Spec::default()).await?;
        let mut members = vec![first];
        for _ in 1..owners {
            let inv = members[0].client.invite().await?;
            members.push(self.member(&inv, Spec::default()).await?);
        }
        Ok((p, members))
    }

    /// Everything a client's account currently holds, as (segment, piece, host, hash).
    pub async fn pieces_of(&self, account: i64) -> Result<Vec<(String, i32, i64, String)>> {
        let mut out: Vec<_> = q::segments(self.db(), account)
            .await
            .into_iter()
            .flat_map(|g| g.pieces.into_iter().map(move |p| (g.id.clone(), p.piece, p.host, p.hash)))
            .collect();
        out.sort();
        Ok(out)
    }
}

/// Read-only looks at the control plane's records, for assertions. Panics on database errors.
pub mod q {
    use futures::TryStreamExt;
    use globalobject_control_plane::db::{AccountDoc, Db, NodeDoc, ObjectDoc, SegmentDoc};
    use mongodb::bson::{doc, Document};

    async fn all<T: serde::de::DeserializeOwned + Send + Sync + Unpin>(c: &mongodb::Collection<T>, filter: Document) -> Vec<T> {
        c.find(filter).await.unwrap().try_collect().await.unwrap()
    }
    pub async fn segments(db: &Db, owner: i64) -> Vec<SegmentDoc> {
        all(&db.segments, doc! {"owner": owner}).await
    }
    pub async fn all_segments(db: &Db) -> Vec<SegmentDoc> {
        all(&db.segments, doc! {}).await
    }
    /// The segment of `owner` that sorts last by `key`, such as the one with the largest pieces.
    pub async fn largest_segment(db: &Db, owner: i64, key: impl Fn(&SegmentDoc) -> i64) -> String {
        segments(db, owner).await.into_iter().max_by_key(|g| key(g)).expect("no segments").id
    }
    pub async fn node(db: &Db, id: i64) -> NodeDoc {
        db.nodes.find_one(doc! {"_id": id}).await.unwrap().expect("no such node")
    }
    pub async fn nodes(db: &Db) -> Vec<NodeDoc> {
        all(&db.nodes, doc! {}).await
    }
    pub async fn account(db: &Db, id: i64) -> AccountDoc {
        db.accounts.find_one(doc! {"_id": id}).await.unwrap().expect("no such account")
    }
    pub async fn accounts(db: &Db) -> Vec<AccountDoc> {
        all(&db.accounts, doc! {}).await
    }
    pub async fn objects(db: &Db, filter: Document) -> Vec<ObjectDoc> {
        all(&db.objects, filter).await
    }
    pub async fn ledger_sum(db: &Db, kind: &str) -> i64 {
        all(&db.ledger, doc! {"kind": kind}).await.iter().map(|l| l.bytes).sum()
    }
    /// How many of `owner`'s pieces `host` holds.
    pub async fn pieces_on(db: &Db, owner: i64, host: i64) -> usize {
        segments(db, owner).await.iter().flat_map(|g| &g.pieces).filter(|p| p.host == host).count()
    }
    pub async fn repairs_in_flight(db: &Db) -> usize {
        all_segments(db).await.iter().flat_map(|g| &g.pieces).filter(|p| p.repair.is_some()).count()
    }
}

impl Member {
    pub fn node(&self) -> &Arc<Node> {
        &self.nodes[0]
    }
}

/// Wait for a condition, returning whether it became true in time.
pub async fn eventually<F, Fut>(limit: Duration, mut f: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let start = Instant::now();
    loop {
        if f().await {
            return true;
        }
        if start.elapsed() > limit {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Bytes that don't compress or dedupe, from a seed.
pub fn noise(len: usize, seed: u8) -> Vec<u8> {
    let mut x = u64::from(seed).wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut out = Vec::with_capacity(len + 8);
    while out.len() < len {
        // xorshift64*: only test data, which needs to be incompressible rather than secret.
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        out.extend_from_slice(&x.wrapping_mul(0x2545_F491_4F6C_DD1D).to_le_bytes());
    }
    out.truncate(len);
    out
}

/// Files to back up, kept away from the grid's own directories so scans of those mean something.
pub struct Source {
    pub dir: TempDir,
}

impl Source {
    pub fn new(files: &[(&str, &[u8])]) -> Source {
        let dir = tempfile::tempdir().unwrap();
        for (name, data) in files {
            std::fs::write(dir.path().join(name), data).unwrap();
        }
        Source { dir }
    }
    pub fn file(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }
}

/// Back up one file, returning the snapshot and the source, which must outlive reads of it.
pub async fn put(c: &Client, name: &str, data: &[u8]) -> Result<(String, Source)> {
    let src = Source::new(&[(name, data)]);
    let stats = c.backup(src.dir.path()).await?;
    Ok((stats.snapshot, src))
}

pub async fn get(c: &Client, snapshot: &str, src: &Source, name: &str) -> Result<Vec<u8>> {
    c.read_file(snapshot, &src.file(name)).await
}

pub fn scan_for(dir: &Path, needle: &[u8]) -> Vec<String> {
    let mut hits = vec![];
    fn walk(dir: &Path, needle: &[u8], hits: &mut Vec<String>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, needle, hits);
            } else if let Ok(b) = std::fs::read(&p) {
                if b.windows(needle.len()).any(|w| w == needle) {
                    hits.push(p.display().to_string());
                }
            }
        }
    }
    walk(dir, needle, &mut hits);
    hits
}

/// Where a stored piece's bytes are on a node's disk.
pub fn piece_path(node: &Node, hash: &str) -> PathBuf {
    globalobject_node::data_path(node, hash)
}
