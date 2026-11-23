//! The control plane's MongoDB data model.
//!
//! Documents follow the access patterns, not the old relational tables:
//! - A segment embeds its pieces, and each piece any repair in flight for it. They are always
//!   read and changed together, there are at most `n` of them, and so committing a segment or
//!   finishing a repair is one atomic document update.
//! - An account embeds its placement groups (one per `(k, n)`), which only its uploads touch.
//! - A node embeds its most recent probe samples (a capped array) and the set of accounts that
//!   reported it refusing placements.
//! - What grows without bound stays in its own collection: objects, ledger entries, read
//!   grants, and the delete and probe queues.
//!
//! Multi-document transactions need a replica set. A single-node one is enough for development.

use globalobject_policy::{Policy, QuotaParams};
use mongodb::{
    bson::{self, doc, Bson, Document},
    error::{ErrorKind, WriteFailure, TRANSIENT_TRANSACTION_ERROR},
    options::{ClientOptions, IndexOptions, ReturnDocument},
    ClientSession, Collection, IndexModel,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub use mongodb::error::Error;
pub type DbResult<T> = Result<T, Error>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolDoc {
    #[serde(rename = "_id")]
    pub id: i64,
    pub name: String,
    pub private: bool,
    pub policy: Policy,
    pub quota: QuotaParams,
    pub distinct_sites: bool,
    pub admin: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InviteDoc {
    #[serde(rename = "_id")]
    pub token: String,
    pub pool: i64,
    pub inviter: Option<i64>,
    pub used_by: Option<i64>,
}

/// Hosts reused for an account's consecutive segments, so a file's availability doesn't
/// fall with its length.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlacementGroup {
    pub k: i32,
    pub n: i32,
    pub hosts: Vec<i64>,
    pub used: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountDoc {
    #[serde(rename = "_id")]
    pub id: i64,
    pub pool: i64,
    pub public_key: String,
    pub inviter: Option<i64>,
    pub state: String,
    pub over_since: Option<f64>,
    pub read_only_since: Option<f64>,
    pub banked_credit: f64,
    pub bucket_tokens: f64,
    pub bucket_at: f64,
    #[serde(default)]
    pub groups: Vec<PlacementGroup>,
    /// Bumped by each upload's transaction, so concurrent uploads by one account conflict
    /// and run one after another. That stops them racing past the quota together.
    #[serde(default)]
    pub upload_seq: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeDoc {
    #[serde(rename = "_id")]
    pub id: i64,
    pub pool: i64,
    pub account: i64,
    pub public_key: String,
    pub addr: String,
    pub endpoint: String,
    pub probe_hash: String,
    pub probe_bytes: i64,
    pub site: String,
    pub asn: String,
    pub pledged: i64,
    pub target_pledged: Option<i64>,
    pub state: String,
    pub last_seen: f64,
    pub audit_ok: i64,
    pub audit_fail: i64,
    pub measured_bps: Option<f64>,
    pub probe_requested_at: Option<f64>,
    /// The latest `Config::probe_samples` measurements, oldest first.
    #[serde(default)]
    pub probe_samples: Vec<f64>,
    /// Accounts that reported this host refusing a placement.
    #[serde(default)]
    pub refused_by: Vec<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RepairDoc {
    pub id: i64,
    pub target: i64,
    pub started_at: f64,
    pub delivered: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PieceDoc {
    pub piece: i32,
    pub host: i64,
    pub hash: String,
    pub state: String,
    #[serde(default)]
    pub repair: Option<RepairDoc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SegmentDoc {
    #[serde(rename = "_id")]
    pub id: String,
    pub pool: i64,
    pub owner: i64,
    pub k: i32,
    pub n: i32,
    #[serde(default)]
    pub local_groups: i32,
    pub len: i64,
    pub object_tpe: String,
    pub object_id: String,
    pub piece_len: i64,
    pub state: String,
    pub created_at: f64,
    pub pieces: Vec<PieceDoc>,
}

/// The segment currently holding one repository file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectDoc {
    pub account: i64,
    pub tpe: String,
    pub object: String,
    pub len: i64,
    pub segment: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadGrantDoc {
    #[serde(rename = "_id")]
    pub nonce: String,
    pub reader: i64,
    pub host: i64,
    pub max_bytes: i64,
    pub credited: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerDoc {
    pub at: f64,
    pub pool: i64,
    pub account: i64,
    pub kind: String,
    pub bytes: i64,
    #[serde(rename = "ref")]
    pub reference: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingDeleteDoc {
    pub host: i64,
    pub segment: String,
    pub piece: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingProbeDoc {
    pub prober: i64,
    pub target: i64,
}

#[derive(Clone)]
pub struct Db {
    pub client: mongodb::Client,
    pub db: mongodb::Database,
    pub pools: Collection<PoolDoc>,
    pub invites: Collection<InviteDoc>,
    pub accounts: Collection<AccountDoc>,
    pub nodes: Collection<NodeDoc>,
    pub segments: Collection<SegmentDoc>,
    pub objects: Collection<ObjectDoc>,
    pub read_grants: Collection<ReadGrantDoc>,
    pub ledger: Collection<LedgerDoc>,
    pub pending_deletes: Collection<PendingDeleteDoc>,
    pub pending_probes: Collection<PendingProbeDoc>,
    counters: Collection<Document>,
}

/// Database used when neither the caller nor the URI names one.
pub const DEFAULT_DATABASE: &str = "globalobject";

impl Db {
    /// Connect, and create indexes if missing. `database` overrides the URI's default database.
    pub async fn connect(uri: &str, database: Option<&str>) -> DbResult<Db> {
        let mut opts = ClientOptions::parse(uri).await?;
        // Names this process in the server's logs and `currentOp`. Pool size and timeouts stay
        // at the driver's defaults (100 connections, 30 s server selection): one long-running
        // control plane with short operations, and nothing measured yet to tune them against.
        opts.app_name.get_or_insert_with(|| "globalobject-control-plane".into());
        let name = database.map(str::to_string).or_else(|| opts.default_database.clone()).unwrap_or_else(|| DEFAULT_DATABASE.into());
        let client = mongodb::Client::with_options(opts)?;
        let db = client.database(&name);
        let d = Db {
            pools: db.collection("pools"),
            invites: db.collection("invites"),
            accounts: db.collection("accounts"),
            nodes: db.collection("nodes"),
            segments: db.collection("segments"),
            objects: db.collection("objects"),
            read_grants: db.collection("read_grants"),
            ledger: db.collection("ledger"),
            pending_deletes: db.collection("pending_deletes"),
            pending_probes: db.collection("pending_probes"),
            counters: db.collection("counters"),
            client,
            db,
        };
        d.create_indexes().await?;
        Ok(d)
    }

    /// Every index serves a query in `api.rs` or `reconcile.rs`; the comments say which.
    async fn create_indexes(&self) -> DbResult<()> {
        let unique = || IndexOptions::builder().unique(true).build();
        let ix = |keys: Document| IndexModel::builder().keys(keys).build();
        let uix = |keys: Document| IndexModel::builder().keys(keys).options(unique()).build();
        // Registration rejects a key that's already in use.
        self.accounts.create_index(uix(doc! {"public_key": 1})).await?;
        self.nodes
            .create_indexes([
                // A node key is unique within a pool.
                uix(doc! {"pool": 1, "public_key": 1}),
                // Usage and lifecycle look up an account's nodes.
                ix(doc! {"account": 1}),
                // Placement candidates and probe rounds: active nodes seen recently.
                ix(doc! {"pool": 1, "state": 1, "last_seen": 1}),
            ])
            .await?;
        self.segments
            .create_indexes([
                // Quota usage, and purging an account.
                ix(doc! {"owner": 1, "state": 1}),
                // Space held per host, draining, and audits.
                ix(doc! {"pieces.host": 1}),
                // A node's repair work, and space promised to repair targets.
                ix(doc! {"pieces.repair.target": 1}),
                // Finishing a repair by its id.
                ix(doc! {"pieces.repair.id": 1}),
                // Repair rounds scan a pool's committed segments; pending timeouts by age.
                ix(doc! {"pool": 1, "state": 1}),
                ix(doc! {"state": 1, "created_at": 1}),
            ])
            .await?;
        self.objects
            .create_indexes([
                // A repository file name maps to one segment. Prefix serves listing by type.
                uix(doc! {"account": 1, "tpe": 1, "object": 1}),
                // Removing a segment removes the name pointing at it.
                ix(doc! {"segment": 1}),
            ])
            .await?;
        self.pending_deletes.create_index(uix(doc! {"host": 1, "segment": 1, "piece": 1})).await?;
        self.pending_probes.create_indexes([ix(doc! {"prober": 1}), ix(doc! {"target": 1})]).await?;
        self.read_grants.create_index(ix(doc! {"host": 1})).await?;
        self.ledger.create_index(ix(doc! {"account": 1, "kind": 1})).await?;
        Ok(())
    }

    /// The next id in a named sequence. Ids are numeric because the wire protocol uses them.
    /// Gaps are possible (an aborted registration still uses one) and harmless.
    pub async fn next_id(&self, name: &str) -> DbResult<i64> {
        let d = self
            .counters
            .find_one_and_update(doc! {"_id": name}, doc! {"$inc": {"seq": 1_i64}})
            .upsert(true)
            .return_document(ReturnDocument::After)
            .await?
            .expect("upsert returns a document");
        Ok(int(d.get("seq")))
    }
}

/// An integer from an aggregation result, which may come back as any numeric type.
pub fn int(b: Option<&Bson>) -> i64 {
    match b {
        Some(Bson::Int32(x)) => *x as i64,
        Some(Bson::Int64(x)) => *x,
        Some(Bson::Double(x)) => *x as i64,
        _ => 0,
    }
}

pub fn is_duplicate_key(e: &Error) -> bool {
    matches!(&*e.kind, ErrorKind::Write(WriteFailure::WriteError(w)) if w.code == 11000)
        || matches!(&*e.kind, ErrorKind::InsertMany(m) if m.write_errors.as_ref().is_some_and(|w| w.iter().all(|x| x.code == 11000)))
}

/// Errors after which a whole transaction should be run again.
pub trait Transient {
    fn transient(&self) -> bool;
}
impl Transient for Error {
    fn transient(&self) -> bool {
        self.contains_label(TRANSIENT_TRANSACTION_ERROR)
    }
}

/// Run an aggregation, inside `session`'s transaction if given, and collect the results.
pub async fn aggregate<T: Send + Sync>(coll: &Collection<T>, pipeline: Vec<Document>, session: Option<&mut ClientSession>) -> DbResult<Vec<Document>> {
    use futures::TryStreamExt;
    match session {
        Some(s) => {
            let mut cur = coll.aggregate(pipeline).session(&mut *s).await?;
            cur.stream(s).try_collect().await
        }
        None => coll.aggregate(pipeline).await?.try_collect().await,
    }
}

pub async fn find_all<T>(coll: &Collection<T>, filter: Document) -> DbResult<Vec<T>>
where
    T: serde::de::DeserializeOwned + Send + Sync + Unpin,
{
    use futures::TryStreamExt;
    coll.find(filter).await?.try_collect().await
}

/// Total `piece_len` per host over the pieces whose `field` (`pieces.host` or
/// `pieces.repair.target`) is one of `hosts`, with `extra` conditions on the piece.
pub async fn bytes_by(db: &Db, field: &str, hosts: &[i64], extra: Document, session: Option<&mut ClientSession>) -> DbResult<HashMap<i64, i64>> {
    if hosts.is_empty() {
        return Ok(HashMap::new());
    }
    let mut piece_match = doc! {field: {"$in": hosts}};
    for (k, v) in extra {
        piece_match.insert(format!("pieces.{k}"), v);
    }
    let pipeline = vec![
        doc! {"$match": {field: {"$in": hosts}}},
        doc! {"$unwind": "$pieces"},
        doc! {"$match": piece_match},
        doc! {"$group": {"_id": format!("${field}"), "bytes": {"$sum": "$piece_len"}}},
    ];
    let rows = aggregate(&db.segments, pipeline, session).await?;
    Ok(rows.iter().map(|r| (int(r.get("_id")), int(r.get("bytes")))).collect())
}

pub fn to_bson<T: Serialize>(v: &T) -> Bson {
    bson::to_bson(v).expect("plain data serialises to BSON")
}

/// Transactions retried, process-wide. Exported as a metric.
pub static TXN_RETRIES: std::sync::LazyLock<prometheus_client::metrics::counter::Counter> = std::sync::LazyLock::new(Default::default);

/// Attempts before a transient transaction error is returned to the caller.
pub const TXN_ATTEMPTS: u32 = 10;

/// Sleep before retrying a transaction. Uploads by one account conflict on purpose (see
/// `begin_segment`), so immediate retries would collide again; a random, growing delay spreads
/// them out. Attempt 9 waits at most 180 ms.
pub async fn backoff(attempt: u32) {
    use rand::Rng;
    TXN_RETRIES.inc();
    let ms = rand::thread_rng().gen_range(5..20) * u64::from(attempt);
    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
}

/// Run `$body` in a transaction on `$sess`, retrying it whole after a transient error, such
/// as a write conflict with a concurrent transaction. Commit is retried on an unknown result.
/// `$body` evaluates to `Result<T, E>`, where `E: From<mongodb::error::Error> + Transient`.
macro_rules! transaction {
    ($db:expr, $sess:ident, $body:block) => {{
        use mongodb::{
            error::{TRANSIENT_TRANSACTION_ERROR, UNKNOWN_TRANSACTION_COMMIT_RESULT},
            options::{Acknowledgment, ReadConcern, WriteConcern},
        };
        let mut $sess = $db.client.start_session().await?;
        let mut attempt = 0;
        loop {
            attempt += 1;
            $sess.start_transaction().read_concern(ReadConcern::snapshot()).write_concern(WriteConcern::builder().w(Acknowledgment::Majority).build()).await?;
            let result = async { $body }.await;
            match result {
                Ok(v) => {
                    let mut commit = $sess.commit_transaction().await;
                    let mut tries = 0;
                    while tries < 3 && commit.as_ref().is_err_and(|e| e.contains_label(UNKNOWN_TRANSACTION_COMMIT_RESULT)) {
                        tries += 1;
                        commit = $sess.commit_transaction().await;
                    }
                    match commit {
                        Ok(()) => break Ok(v),
                        Err(e) if e.contains_label(TRANSIENT_TRANSACTION_ERROR) && attempt < $crate::db::TXN_ATTEMPTS => {
                            $crate::db::backoff(attempt).await;
                            continue;
                        }
                        Err(e) => break Err(e.into()),
                    }
                }
                Err(e) => {
                    // Fails harmlessly if the server already aborted it.
                    let _ = $sess.abort_transaction().await;
                    if $crate::db::Transient::transient(&e) && attempt < $crate::db::TXN_ATTEMPTS {
                        $crate::db::backoff(attempt).await;
                        continue;
                    }
                    break Err(e);
                }
            }
        }
    }};
}
pub(crate) use transaction;
