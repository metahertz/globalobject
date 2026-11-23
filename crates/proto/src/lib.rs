//! Wire types shared by the client, node and control plane.

use globalobject_policy::{Policy, QuotaParams};
use serde::{Deserialize, Serialize};

pub const T_GRANT: &str = "globalobject grant v1";
pub const T_STORE: &str = "globalobject store receipt v1";
pub const T_READ: &str = "globalobject read receipt v1";
pub const T_REQ: &str = "globalobject request v1";

pub const H_ID: &str = "x-globalobject-id";
pub const H_AUTH: &str = "x-globalobject-auth";

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("clock").as_secs()
}

/// What a signed control-plane request asserts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestClaims {
    pub id: String,
    pub method: String,
    pub path: String,
    pub ts: u64,
    pub body: String,
}

impl RequestClaims {
    pub fn new(id: &str, method: &str, path: &str, body: &[u8]) -> Self {
        RequestClaims { id: id.into(), method: method.into(), path: path.into(), ts: now(), body: body_hash(body) }
    }
}

/// The request token covers this hash of the body, rather than the body itself.
pub fn body_hash(body: &[u8]) -> String {
    hex::encode(globalobject_crypto::hash(body))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    Write,
    Read,
    RepairRead,
    Audit,
    Delete,
    Probe,
}

/// Permission for one operation on one blob at one host, for one peer. Issued as a token.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Grant {
    pub nonce: String,
    pub account: i64,
    pub pool: i64,
    pub segment: String,
    pub piece: u16,
    pub host: i64,
    /// The iroh endpoint allowed to use this grant.
    pub peer: String,
    /// BLAKE3 hash of the piece.
    pub hash: String,
    pub op: Op,
    pub max_bytes: u64,
    pub rate_bps: u64,
    pub expires_at: u64,
    /// Bytes `[start, end)` of the piece. `None` allows the whole piece.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<(u64, u64)>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoreReceipt {
    pub nonce: String,
    pub segment: String,
    pub piece: u16,
    pub host: i64,
    pub bytes: u64,
    pub hash: String,
}

/// "I received this many bytes under this grant."
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadReceipt {
    pub nonce: String,
    pub host: i64,
    pub reader: i64,
    pub bytes: u64,
}

// ------------------------------------------------------------- between peers

/// Sent over the `PEER_ALPN` protocol. Bytes themselves move over `iroh-blobs`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PeerRequest {
    /// Allow the caller to fetch a blob once, using a token for a read-type grant.
    Authorize { grant: String },
    /// Fetch this piece from the caller and keep it. `from` is the caller's address.
    Store { grant: String, from: String },
    /// A signed read receipt, for the host to pass to the control plane.
    Receipt { reader: i64, nonce: String, token: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PeerReply {
    Ok,
    Stored { receipt: String },
    Refused { reason: String },
    Full,
}

// ------------------------------------------------------------ control plane

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreatePool {
    pub name: String,
    pub private: bool,
    pub policy: Policy,
    pub quota: QuotaParams,
    pub distinct_sites: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolCreated {
    pub pool: i64,
    pub admin_invite: String,
    /// A link for the admin invite, for sending to a person. Present when the control plane
    /// knows its public address.
    #[serde(default)]
    pub join_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterAccount {
    pub invite: String,
    pub public_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountCreated {
    pub account: i64,
    pub pool: i64,
    pub grant_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InviteCreated {
    pub invite: String,
    #[serde(default)]
    pub join_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterNode {
    pub public_key: String,
    /// JSON `EndpointAddr`.
    pub addr: String,
    pub endpoint: String,
    pub pledged: u64,
    pub site: String,
    pub asn: String,
    /// A random blob this node keeps, for peers to measure its upload speed with.
    pub probe_hash: String,
    pub probe_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeCreated {
    pub node: i64,
    pub pool: i64,
    pub grant_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Heartbeat {
    pub used_bytes: u64,
    pub addr: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatReply {
    pub tasks: Vec<Task>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepairSource {
    pub addr: String,
    pub piece: u16,
    pub grant: String,
    pub hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Task {
    Repair {
        repair_id: i64,
        segment: String,
        piece: u16,
        k: u16,
        n: u16,
        #[serde(default)]
        local_groups: u16,
        piece_len: u64,
        /// In the order to try. The target's local group comes first, when it can rebuild alone.
        sources: Vec<RepairSource>,
        expected_hash: String,
        write_grant: String,
    },
    Probe {
        target: i64,
        addr: String,
        grant: String,
        hash: String,
        bytes: u64,
    },
    Delete {
        segment: String,
        piece: u16,
        grant: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepairDone {
    pub repair_id: i64,
    pub receipt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeResult {
    pub target: i64,
    pub bps: f64,
}

/// A file in the account's restic-format repository.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObjectRef {
    pub tpe: String,
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeginSegment {
    pub segment: String,
    pub object: ObjectRef,
    pub len: u64,
    pub k: u16,
    pub n: u16,
    #[serde(default)]
    pub local_groups: u16,
    /// BLAKE3 hash of each piece, in order.
    pub hashes: Vec<String>,
    /// The client's iroh endpoint, which hosts will fetch from.
    pub endpoint: String,
    /// Hosts that failed this client recently.
    #[serde(default)]
    pub avoid: Vec<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlacedPiece {
    pub piece: u16,
    pub host: i64,
    pub addr: String,
    pub grant: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SegmentPlan {
    pub pieces: Vec<PlacedPiece>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitSegment {
    pub receipts: Vec<String>,
    /// Hosts that refused or failed during upload.
    #[serde(default)]
    pub failed_hosts: Vec<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadRequest {
    pub want: u16,
    #[serde(default)]
    pub exclude_pieces: Vec<u16>,
    pub endpoint: String,
    /// Bytes `[start, end)` of the object, for a partial read. The plan then names the data
    /// pieces covering it, each with its own range, and `want` is ignored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<(u64, u64)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadPlan {
    pub k: u16,
    pub n: u16,
    #[serde(default)]
    pub local_groups: u16,
    pub len: u64,
    pub piece_len: u64,
    pub pieces: Vec<ReadPiece>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadPiece {
    pub piece: u16,
    pub host: i64,
    pub addr: String,
    pub grant: String,
    pub nonce: String,
    pub hash: String,
    /// The granted bytes of this piece, for a partial read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<(u64, u64)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryAfter {
    pub retry_after: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedReadReceipt {
    pub reader: i64,
    pub nonce: String,
    pub token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadReceipts {
    pub receipts: Vec<SignedReadReceipt>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectEntry {
    pub id: String,
    pub len: u64,
    pub segment: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountStatus {
    pub account: i64,
    pub pool: i64,
    pub state: String,
    pub pledged: u64,
    pub proven_held: u64,
    pub credited: u64,
    pub quota_raw: u64,
    pub used_raw: u64,
    pub entitled_bps: f64,
    pub banked_credit: f64,
    pub policy: Policy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateNode {
    pub pledged: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiError {
    pub error: String,
}
