//! Storage node. Blobs live in an `iroh-blobs` store and are served by its protocol. What this
//! crate adds is the policy around them: a capacity limit, and serving only against grants.
//! The node never holds a decryption key. Repair works on ciphertext alone.

use anyhow::{anyhow, bail, Context, Result};
use globalobject_crypto::{open_token, Identity};
use globalobject_net::{
    addr_string, bind, chunk_ranges, fetch_verified, hash_of, parse_hash, peer_call, single_blob_ranges, ChunkRanges, NetMode, BLOBS_ALPN, PEER_ALPN,
};
use globalobject_proto::*;
use globalobject_segment::{rebuild_piece, Layout};
use iroh::{
    endpoint::Connection,
    protocol::{AcceptError, ProtocolHandler, Router},
    Endpoint, EndpointId,
};
use iroh_blobs::{
    provider::events::{AbortReason, ConnectMode, EventMask, EventSender, ObserveMode, ProviderMessage, RequestMode, ThrottleMode},
    store::{
        fs::{options::Options, FsStore},
        GcConfig,
    },
    BlobsProtocol, Hash,
};
use n0_future::StreamExt;
use std::{
    collections::{HashMap, HashSet},
    io::Write as _,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

#[derive(Clone)]
pub struct NodeConfig {
    pub control_plane: String,
    pub owner: AccountAuth,
    pub store: PathBuf,
    /// Offered capacity, reported to the control plane.
    pub pledged: u64,
    /// Real disk limit. Can be lower than `pledged` to model a false pledge.
    pub real_capacity: u64,
    pub site: String,
    pub asn: String,
    /// Application-level upload shaping, in bytes per second. Zero means unlimited.
    pub link_up_bps: u64,
    pub heartbeat: Duration,
    pub net: NetMode,
    pub probe_bytes: u64,
    pub gc_every: Duration,
}

/// Credentials of the account that owns this node.
#[derive(Clone)]
pub struct AccountAuth {
    pub id: i64,
    pub seed: [u8; 32],
}

/// One-way shaper. Concurrent senders queue, so total output stays within the rate.
pub struct Pacer {
    rate: f64,
    next: Mutex<Instant>,
}

impl Pacer {
    pub fn new(rate_bps: u64) -> Self {
        Pacer { rate: rate_bps as f64, next: Mutex::new(Instant::now()) }
    }
    /// Wait for as long as sending `bytes` takes on this link.
    pub async fn wait(&self, bytes: u64) {
        if self.rate <= 0.0 {
            return;
        }
        let done = {
            let mut n = self.next.lock().unwrap();
            let start = (*n).max(Instant::now());
            *n = start + Duration::from_secs_f64(bytes as f64 / self.rate);
            *n
        };
        tokio::time::sleep_until(done.into()).await;
    }
}

/// A one-time permission to fetch one blob, or part of it.
struct Allowance {
    pacer: Option<Arc<Pacer>>,
    /// Chunks the request may cover. `None` allows the whole blob.
    ranges: Option<ChunkRanges>,
}

#[derive(Default)]
pub struct Stats {
    pub pieces_stored: AtomicU64,
    pub grants_refused: AtomicU64,
    pub bytes_served: AtomicU64,
    pub repairs_done: AtomicU64,
    /// Bytes this node fetched from other hosts to rebuild pieces.
    pub repair_bytes_fetched: AtomicU64,
}

pub struct Node {
    pub id: i64,
    pub pool: i64,
    identity: Identity,
    grant_key: String,
    dir: PathBuf,
    capacity: u64,
    used: AtomicU64,
    killed: AtomicBool,
    link: Arc<Pacer>,
    nonces: Mutex<HashSet<String>>,
    nonce_log: Mutex<std::fs::File>,
    allowed: Mutex<HashMap<(EndpointId, Hash), Allowance>>,
    receipts: Mutex<Vec<SignedReadReceipt>>,
    store: FsStore,
    endpoint: Endpoint,
    http: reqwest::Client,
    cp: String,
    pub stats: Stats,
    router: Mutex<Option<Router>>,
}

impl Node {
    /// Behave like an unreachable host: for tests.
    pub fn kill(&self) {
        self.killed.store(true, Ordering::SeqCst);
    }
    pub fn revive(&self) {
        self.killed.store(false, Ordering::SeqCst);
    }
    pub fn is_killed(&self) -> bool {
        self.killed.load(Ordering::SeqCst)
    }
    pub fn used_bytes(&self) -> u64 {
        self.used.load(Ordering::SeqCst)
    }
    pub fn store_dir(&self) -> &std::path::Path {
        &self.dir
    }
    pub fn endpoint_id(&self) -> String {
        self.endpoint.id().to_string()
    }
    pub fn addr(&self) -> String {
        addr_string(&self.endpoint)
    }

    fn check(&self, token: &str, ops: &[Op], peer: Option<EndpointId>) -> std::result::Result<Grant, String> {
        let refuse = |why: &str| {
            self.stats.grants_refused.fetch_add(1, Ordering::Relaxed);
            why.to_string()
        };
        let g: Grant = open_token(&self.grant_key, T_GRANT, token).map_err(|_| refuse("bad grant"))?;
        if g.host != self.id {
            return Err(refuse("grant is for another host"));
        }
        if g.pool != self.pool {
            return Err(refuse("grant is for another pool"));
        }
        if !ops.contains(&g.op) {
            return Err(refuse("grant is for another operation"));
        }
        if g.expires_at < now() {
            return Err(refuse("grant expired"));
        }
        if let Some(p) = peer {
            if g.peer != p.to_string() {
                return Err(refuse("grant is for another peer"));
            }
        }
        Ok(g)
    }

    /// Record a single-use nonce durably, before acting on it.
    fn consume_nonce(&self, nonce: &str) -> std::result::Result<(), String> {
        let mut set = self.nonces.lock().unwrap();
        if !set.insert(nonce.to_string()) {
            self.stats.grants_refused.fetch_add(1, Ordering::Relaxed);
            return Err("grant already used".into());
        }
        let mut log = self.nonce_log.lock().unwrap();
        writeln!(log, "{nonce}").and_then(|_| log.flush()).map_err(|e| e.to_string())
    }

    fn receipt(&self, g: &Grant, bytes: u64) -> String {
        self.identity
            .token(T_STORE, &StoreReceipt { nonce: g.nonce.clone(), segment: g.segment.clone(), piece: g.piece, host: self.id, bytes, hash: g.hash.clone() })
    }

    fn tag(segment: &str, piece: u16) -> String {
        format!("seg/{segment}/{piece}")
    }

    /// Keep a verified piece under a named tag, so garbage collection leaves it alone.
    async fn keep(&self, g: &Grant, data: Vec<u8>) -> Result<()> {
        let got = self.store.add_bytes(data).with_named_tag(Self::tag(&g.segment, g.piece)).await?;
        if got.hash.to_hex() != g.hash {
            bail!("stored data doesn't match the grant's hash");
        }
        self.stats.pieces_stored.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    async fn forget(&self, segment: &str, piece: u16) -> Result<()> {
        let tag = Self::tag(segment, piece);
        if let Some(info) = self.store.tags().get(&tag).await? {
            let size = blob_size(&self.store, info.hash).await;
            self.store.tags().delete(&tag).await?;
            self.used.fetch_sub(size.min(self.used.load(Ordering::SeqCst)), Ordering::SeqCst);
        }
        Ok(())
    }

    /// Fetch a piece from `from` and keep it, within this node's real capacity.
    async fn store_from(&self, g: &Grant, from: &str) -> std::result::Result<u64, PeerReply> {
        let before = self.used.fetch_add(g.max_bytes, Ordering::SeqCst);
        if before + g.max_bytes > self.capacity {
            self.used.fetch_sub(g.max_bytes, Ordering::SeqCst);
            return Err(PeerReply::Full);
        }
        let release = |n: u64| {
            self.used.fetch_sub(n, Ordering::SeqCst);
        };
        match fetch_verified(&self.endpoint, from, &g.hash, g.max_bytes).await {
            Ok(data) => {
                let len = data.len() as u64;
                release(g.max_bytes - len);
                if let Err(e) = self.keep(g, data).await {
                    release(len);
                    return Err(PeerReply::Refused { reason: e.to_string() });
                }
                Ok(len)
            }
            Err(e) => {
                release(g.max_bytes);
                Err(PeerReply::Refused { reason: format!("{e:#}") })
            }
        }
    }

    async fn handle_peer(&self, remote: EndpointId, req: PeerRequest) -> PeerReply {
        if self.is_killed() {
            return PeerReply::Refused { reason: "unavailable".into() };
        }
        match req {
            PeerRequest::Authorize { grant } => {
                let g = match self.check(&grant, &[Op::Read, Op::RepairRead, Op::Audit, Op::Probe], Some(remote)) {
                    Ok(g) => g,
                    Err(reason) => return PeerReply::Refused { reason },
                };
                let Ok(hash) = parse_hash(&g.hash) else { return PeerReply::Refused { reason: "bad hash".into() } };
                if let Err(reason) = self.consume_nonce(&g.nonce) {
                    return PeerReply::Refused { reason };
                }
                let pacer = (g.rate_bps > 0).then(|| Arc::new(Pacer::new(g.rate_bps)));
                let ranges = g.range.map(|(start, end)| chunk_ranges(start, end));
                self.allowed.lock().unwrap().insert((remote, hash), Allowance { pacer, ranges });
                PeerReply::Ok
            }
            PeerRequest::Store { grant, from } => {
                let g = match self.check(&grant, &[Op::Write], Some(remote)) {
                    Ok(g) => g,
                    Err(reason) => return PeerReply::Refused { reason },
                };
                if let Err(reason) = self.consume_nonce(&g.nonce) {
                    return PeerReply::Refused { reason };
                }
                match self.store_from(&g, &from).await {
                    Ok(len) => PeerReply::Stored { receipt: self.receipt(&g, len) },
                    Err(reply) => reply,
                }
            }
            PeerRequest::Receipt { reader, nonce, token } => {
                // The control plane checks the signature, against the reader's key.
                self.receipts.lock().unwrap().push(SignedReadReceipt { reader, nonce, token });
                PeerReply::Ok
            }
        }
    }

    async fn signed_post<T: serde::Serialize, R: serde::de::DeserializeOwned>(&self, path: &str, body: &T) -> Result<R> {
        let raw = serde_json::to_vec(body)?;
        let id = format!("n{}", self.id);
        let token = self.identity.token(T_REQ, &RequestClaims::new(&id, "POST", path, &raw));
        let resp = self
            .http
            .post(format!("{}{path}", self.cp))
            .header(H_ID, id)
            .header(H_AUTH, token)
            .header("content-type", "application/json")
            .body(raw)
            .send()
            .await?;
        if !resp.status().is_success() {
            bail!("{path}: {} {}", resp.status(), resp.text().await.unwrap_or_default());
        }
        Ok(resp.json().await?)
    }
}

async fn blob_size(store: &FsStore, hash: Hash) -> u64 {
    use iroh_blobs::api::blobs::BlobStatus;
    match store.blobs().status(hash).await {
        Ok(BlobStatus::Complete { size }) => size,
        _ => 0,
    }
}

/// How the protocol handlers find their node. `Weak`, since the node owns the router that owns
/// them: a strong reference here would be a cycle, and the node's store would never be freed.
type NodeCell = tokio::sync::OnceCell<std::sync::Weak<Node>>;

/// The peer protocol: short JSON requests. Bytes move over `iroh-blobs`.
#[derive(Clone)]
struct PeerProtocol(Arc<NodeCell>);

impl std::fmt::Debug for PeerProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PeerProtocol")
    }
}

impl ProtocolHandler for PeerProtocol {
    async fn accept(&self, conn: Connection) -> std::result::Result<(), AcceptError> {
        let remote = conn.remote_id();
        let (mut send, mut recv) = conn.accept_bi().await?;
        let raw = recv.read_to_end(1 << 20).await.map_err(AcceptError::from_err)?;
        let reply = match (serde_json::from_slice::<PeerRequest>(&raw), self.0.get().and_then(std::sync::Weak::upgrade)) {
            (Ok(req), Some(node)) => node.handle_peer(remote, req).await,
            _ => PeerReply::Refused { reason: "bad request".into() },
        };
        let out = serde_json::to_vec(&reply).map_err(AcceptError::from_err)?;
        send.write_all(&out).await.map_err(AcceptError::from_err)?;
        send.finish()?;
        conn.closed().await;
        Ok(())
    }
}

/// Decide what `iroh-blobs` may serve: nothing, unless a grant allowed this peer and this blob.
fn gate(node: Arc<NodeCell>) -> EventSender {
    let mask = EventMask {
        connected: ConnectMode::Intercept,
        get: RequestMode::Intercept,
        get_many: RequestMode::Disabled,
        push: RequestMode::Disabled,
        // Observing would reveal which blobs this host holds, so refuse it below.
        observe: ObserveMode::Intercept,
        throttle: ThrottleMode::Intercept,
    };
    let (tx, mut rx) = EventSender::channel(256, mask);
    tokio::spawn(async move {
        let mut peers: HashMap<u64, EndpointId> = HashMap::new();
        let mut pacers: HashMap<(u64, u64), Arc<Pacer>> = HashMap::new();
        while let Some(msg) = rx.recv().await {
            let Some(n) = node.get().and_then(std::sync::Weak::upgrade) else { continue };
            match msg {
                ProviderMessage::ClientConnected(c) => {
                    let res = match c.endpoint_id {
                        Some(id) if !n.is_killed() => {
                            peers.insert(c.connection_id, id);
                            Ok(())
                        }
                        _ => Err(AbortReason::Permission),
                    };
                    c.tx.send(res).await.ok();
                }
                ProviderMessage::GetRequestReceived(r) => {
                    let peer = peers.get(&r.connection_id).copied();
                    let hash = r.request.hash;
                    let allowance = peer.and_then(|p| n.allowed.lock().unwrap().remove(&(p, hash)));
                    // Only the granted blob, and only the granted part of it. A request for a
                    // hash sequence's children would serve other blobs this host holds, which
                    // the grant for this one says nothing about.
                    let within = |a: &Allowance| match (single_blob_ranges(&r.request), &a.ranges) {
                        (Some(_), None) => true,
                        (Some(asked), Some(granted)) => asked.is_subset(granted),
                        (None, _) => false,
                    };
                    let res = match allowance {
                        Some(a) if !n.is_killed() && within(&a) => {
                            if let Some(p) = a.pacer {
                                pacers.insert((r.connection_id, r.request_id), p);
                            }
                            Ok(())
                        }
                        _ => {
                            n.stats.grants_refused.fetch_add(1, Ordering::Relaxed);
                            Err(AbortReason::Permission)
                        }
                    };
                    r.tx.send(res).await.ok();
                }
                ProviderMessage::ObserveRequestReceived(r) => {
                    r.tx.send(Err(AbortReason::Permission)).await.ok();
                }
                ProviderMessage::Throttle(t) => {
                    let grant = pacers.get(&(t.connection_id, t.request_id)).cloned();
                    let link = n.link.clone();
                    n.stats.bytes_served.fetch_add(t.size, Ordering::Relaxed);
                    tokio::spawn(async move {
                        if let Some(g) = grant {
                            g.wait(t.size).await;
                        }
                        link.wait(t.size).await;
                        t.tx.send(Ok(())).await.ok();
                    });
                }
                _ => {}
            }
        }
    });
    tx
}

// -------------------------------------------------------------------- tasks

async fn fetch_with_grant(n: &Node, addr: &str, grant: &str, hash: &str, max: u64) -> Result<Vec<u8>> {
    match peer_call(&n.endpoint, addr, &PeerRequest::Authorize { grant: grant.to_string() }).await? {
        PeerReply::Ok => fetch_verified(&n.endpoint, addr, hash, max).await,
        other => bail!("source refused: {other:?}"),
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_repair(
    n: &Arc<Node>,
    repair_id: i64,
    piece: u16,
    layout: Layout,
    piece_len: u64,
    sources: Vec<RepairSource>,
    expected: String,
    write_grant: String,
) -> Result<()> {
    let g = n.check(&write_grant, &[Op::Write], None).map_err(|e| anyhow!("repair grant refused: {e}"))?;
    anyhow::ensure!(layout.is_valid() && (piece as usize) < layout.n, "bad repair layout");
    let mut pieces: Vec<Option<Vec<u8>>> = vec![None; layout.n];
    let present = |p: &[Option<Vec<u8>>]| p.iter().map(Option::is_some).collect::<Vec<bool>>();
    // Sources come in the order to try: with local groups, the target's group first. Stop as
    // soon as the target can be rebuilt, which for a single loss in a group is `k / l` pieces.
    for src in &sources {
        if layout.can_rebuild(piece as usize, &present(&pieces)) {
            break;
        }
        if src.piece as usize >= layout.n {
            continue;
        }
        // Verified against its hash as it arrives, so a bad source can't poison the rebuild.
        match fetch_with_grant(n, &src.addr, &src.grant, &src.hash, piece_len).await {
            Ok(bytes) => {
                n.stats.repair_bytes_fetched.fetch_add(bytes.len() as u64, Ordering::Relaxed);
                pieces[src.piece as usize] = Some(bytes);
            }
            Err(e) => tracing::debug!("repair source {} failed: {e:#}", src.piece),
        }
    }
    // A move off a draining host fetches the piece itself, with nothing to rebuild.
    let rebuilt = match pieces[piece as usize].take() {
        Some(p) => p,
        None => rebuild_piece(pieces, piece as usize, layout).map_err(|e| anyhow!("{e}"))?,
    };
    if hash_of(&rebuilt) != expected {
        bail!("rebuilt piece doesn't match its expected hash");
    }
    let len = rebuilt.len() as u64;
    n.consume_nonce(&g.nonce).map_err(|e| anyhow!(e))?;
    let before = n.used.fetch_add(len, Ordering::SeqCst);
    if before + len > n.capacity {
        n.used.fetch_sub(len, Ordering::SeqCst);
        bail!("slice is full");
    }
    n.keep(&g, rebuilt).await?;
    let _: serde_json::Value = n.signed_post("/v1/tasks/repair-done", &RepairDone { repair_id, receipt: n.receipt(&g, len) }).await?;
    n.stats.repairs_done.fetch_add(1, Ordering::Relaxed);
    Ok(())
}

async fn run_probe(n: &Arc<Node>, target: i64, addr: String, grant: String, hash: String, bytes: u64) -> Result<()> {
    let start = Instant::now();
    let got = fetch_with_grant(n, &addr, &grant, &hash, bytes).await?;
    if got.len() as u64 != bytes {
        bail!("probe was short");
    }
    let bps = bytes as f64 / start.elapsed().as_secs_f64();
    let _: serde_json::Value = n.signed_post("/v1/tasks/probe-result", &ProbeResult { target, bps }).await?;
    Ok(())
}

async fn handle_task(n: Arc<Node>, t: Task) {
    let r = match t {
        Task::Repair { repair_id, piece, k, n: total, local_groups, piece_len, sources, expected_hash, write_grant, .. } => {
            let layout = Layout::new(k as usize, total as usize, local_groups as usize);
            run_repair(&n, repair_id, piece, layout, piece_len, sources, expected_hash, write_grant).await
        }
        Task::Probe { target, addr, grant, hash, bytes } => run_probe(&n, target, addr, grant, hash, bytes).await,
        Task::Delete { segment, piece, grant } => match n.check(&grant, &[Op::Delete], None) {
            Ok(g) if g.segment == segment && g.piece == piece => n.forget(&segment, piece).await,
            _ => Err(anyhow!("delete grant refused")),
        },
    };
    if let Err(e) = r {
        tracing::warn!("node {} task failed: {e:#}", n.id);
    }
}

async fn heartbeat_loop(n: Arc<Node>, every: Duration) {
    loop {
        tokio::time::sleep(every).await;
        if n.is_killed() {
            continue;
        }
        let hb = Heartbeat { used_bytes: n.used_bytes(), addr: n.addr() };
        match n.signed_post::<_, HeartbeatReply>(&format!("/v1/nodes/{}/heartbeat", n.id), &hb).await {
            Ok(reply) => {
                for t in reply.tasks {
                    tokio::spawn(handle_task(n.clone(), t));
                }
            }
            Err(e) => tracing::debug!("heartbeat: {e:#}"),
        }
        let batch: Vec<_> = std::mem::take(&mut *n.receipts.lock().unwrap());
        if !batch.is_empty() {
            let _ = n.signed_post::<_, serde_json::Value>("/v1/receipts/read", &ReadReceipts { receipts: batch }).await;
        }
    }
}

/// Register with the control plane and start serving. Returns once ready.
pub async fn start(cfg: NodeConfig) -> Result<Arc<Node>> {
    std::fs::create_dir_all(&cfg.store).context("create store")?;
    let id_path = cfg.store.join("identity");
    let identity = match std::fs::read(&id_path).ok().and_then(|b| <[u8; 32]>::try_from(b).ok()) {
        Some(seed) => Identity::from_seed(&seed),
        None => {
            let id = Identity::generate();
            std::fs::write(&id_path, id.seed())?;
            id
        }
    };
    let blobs_dir = cfg.store.join("blobs");
    std::fs::create_dir_all(&blobs_dir)?;
    let mut options = Options::new(&blobs_dir);
    options.gc = Some(GcConfig { interval: cfg.gc_every, add_protected: None });
    let store = FsStore::load_with_opts(blobs_dir.join("blobs.db"), options).await?;
    let http = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(120)).build()?;

    // A random blob, so peers can measure upload speed. Random so it can't be faked cheaply.
    let probe: Vec<u8> = (0..cfg.probe_bytes.div_ceil(32)).flat_map(|_| globalobject_crypto::random_key()).take(cfg.probe_bytes as usize).collect();
    let probe_hash = store.add_bytes(probe).with_named_tag("probe").await?.hash.to_hex().to_string();

    let endpoint = bind(cfg.net, vec![BLOBS_ALPN.to_vec(), PEER_ALPN.to_vec()]).await?;

    // Registered by the owning account, proving this node is theirs.
    let reg = RegisterNode {
        public_key: identity.public_hex(),
        addr: addr_string(&endpoint),
        endpoint: endpoint.id().to_string(),
        pledged: cfg.pledged,
        site: cfg.site.clone(),
        asn: cfg.asn.clone(),
        probe_hash,
        probe_bytes: cfg.probe_bytes,
    };
    let raw = serde_json::to_vec(&reg)?;
    let owner = Identity::from_seed(&cfg.owner.seed);
    let owner_id = format!("a{}", cfg.owner.id);
    let token = owner.token(T_REQ, &RequestClaims::new(&owner_id, "POST", "/v1/nodes", &raw));
    let resp = http
        .post(format!("{}/v1/nodes", cfg.control_plane))
        .header(H_ID, owner_id)
        .header(H_AUTH, token)
        .header("content-type", "application/json")
        .body(raw)
        .send()
        .await?;
    if !resp.status().is_success() {
        bail!("node registration failed: {} {}", resp.status(), resp.text().await.unwrap_or_default());
    }
    let created: NodeCreated = resp.json().await?;

    // Restore nonce history and used space, so restarts don't reopen replays.
    let log_path = cfg.store.join("nonces.log");
    let nonces: HashSet<String> = std::fs::read_to_string(&log_path).unwrap_or_default().lines().map(str::to_string).collect();
    let nonce_log = std::fs::OpenOptions::new().create(true).append(true).open(&log_path)?;
    let mut used = 0;
    let mut tags = store.tags().list_prefix("seg/").await?;
    while let Some(t) = tags.next().await {
        used += blob_size(&store, t?.hash).await;
    }

    let cell: Arc<NodeCell> = Default::default();
    let blobs = BlobsProtocol::new(&store, Some(gate(cell.clone())));
    let router = Router::builder(endpoint.clone()).accept(BLOBS_ALPN, blobs).accept(PEER_ALPN, PeerProtocol(cell.clone())).spawn();
    let node = Arc::new(Node {
        id: created.node,
        pool: created.pool,
        identity,
        grant_key: created.grant_key,
        dir: cfg.store.clone(),
        capacity: cfg.real_capacity,
        used: AtomicU64::new(used),
        killed: AtomicBool::new(false),
        link: Arc::new(Pacer::new(cfg.link_up_bps)),
        nonces: Mutex::new(nonces),
        nonce_log: Mutex::new(nonce_log),
        allowed: Mutex::new(HashMap::new()),
        receipts: Mutex::new(vec![]),
        store,
        endpoint,
        http,
        cp: cfg.control_plane.clone(),
        stats: Stats::default(),
        router: Mutex::new(Some(router)),
    });
    let _ = cell.set(Arc::downgrade(&node));
    tokio::spawn(heartbeat_loop(node.clone(), cfg.heartbeat));
    Ok(node)
}

/// Where a stored blob's bytes are on disk. For tests that play a dishonest host.
pub fn data_path(node: &Node, hash: &str) -> PathBuf {
    node.store_dir().join("blobs").join("data").join(format!("{hash}.data"))
}

impl Drop for Node {
    fn drop(&mut self) {
        self.router.lock().unwrap().take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_lone_transfer_takes_as_long_as_the_link_needs() {
        let p = Pacer::new(1_000_000);
        let t = Instant::now();
        p.wait(200_000).await;
        let e = t.elapsed().as_secs_f64();
        assert!((0.19..0.5).contains(&e), "{e}");
    }

    #[tokio::test]
    async fn concurrent_transfers_share_the_link() {
        let p = Arc::new(Pacer::new(1_000_000));
        let t = Instant::now();
        let (a, b, c) = (p.clone(), p.clone(), p.clone());
        tokio::join!(a.wait(100_000), b.wait(100_000), c.wait(100_000));
        let e = t.elapsed().as_secs_f64();
        assert!((0.29..0.7).contains(&e), "{e}");
    }

    #[tokio::test]
    async fn zero_rate_means_unlimited() {
        let t = Instant::now();
        Pacer::new(0).wait(1 << 40).await;
        assert!(t.elapsed().as_secs_f64() < 0.1);
    }
}
