//! Client. Encryption, chunking, dedupe, compression and snapshots all come from `rustic_core`,
//! using restic's repository format, which was designed for untrusted storage. This crate
//! supplies a backend that stores those repository files on the grid.

use anyhow::{anyhow, bail, Context, Result};
use bytes::Bytes;
use futures::{stream::FuturesUnordered, StreamExt};
use globalobject_crypto::{random_hex, seed_from_phrase, Identity};
use globalobject_net::{addr_string, bind, fetch_range, fetch_verified, hash_of, peer_call, NetMode, BLOBS_ALPN};
use globalobject_proto::*;
use globalobject_segment::{encode, join, reconstruct_data, Layout, MAX_SEGMENT};
use iroh::{protocol::Router, Endpoint};
use iroh_blobs::{
    store::{
        mem::{MemStore, Options as MemOptions},
        GcConfig,
    },
    BlobsProtocol,
};
use rustic_core::{
    BackupOptions, BytesList, ConfigOptions, Credentials, ErrorKind, FileType, Id, KeepOptions, KeyOptions, LocalDestination, LsOptions, PathList,
    PruneOptions, ReadBackend, Repository, RepositoryBackends, RepositoryOptions, RestoreOptions, RusticError, RusticResult, SnapshotOptions, WriteBackend,
};
use serde::{de::DeserializeOwned, Serialize};
use std::{
    collections::{HashSet, VecDeque},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

const UPLOAD_ATTEMPTS: usize = 4;
const MAX_READ_FAILURES: usize = 64;
/// Repository packs are kept well under a segment.
const PACK_LIMIT: u64 = 48 * 1024 * 1024;
/// Whole objects cached in memory, because restores read many ranges of the same pack.
const CACHE_OBJECTS: usize = 3;
/// rustic works from many threads. These bound how many objects are in memory at once.
const PARALLEL_READS: usize = 2;
const PARALLEL_WRITES: usize = 2;

#[derive(Debug, thiserror::Error, Clone)]
pub enum ClientError {
    #[error("quota exceeded: {0}")]
    Quota(String),
    #[error("account is read-only or removed: {0}")]
    Forbidden(String),
    #[error("not enough failure domains in this pool: {0}")]
    TooFewDomains(String),
    #[error("data is unavailable right now: {0}")]
    Unavailable(String),
    #[error("the control plane no longer lists snapshot {0}, seen earlier by this client. Possible rollback")]
    Rollback(String),
}

struct Inner {
    cp: String,
    id: i64,
    pool: i64,
    identity: Identity,
    password: String,
    http: reqwest::Client,
    state_dir: PathBuf,
    endpoint: Endpoint,
    mem: MemStore,
    _router: Router,
    /// The most recent failure with a meaning of its own, since rustic's errors flatten it.
    last_error: Mutex<Option<ClientError>>,
    cache: Mutex<VecDeque<(String, Bytes)>>,
    reads: tokio::sync::Semaphore,
    writes: tokio::sync::Semaphore,
}

#[derive(Clone)]
pub struct Client(Arc<Inner>);

/// Which snapshots `forget` keeps, as in restic and rustic. At least one must be set.
#[derive(Debug, Clone, Default)]
pub struct Retention {
    pub last: Option<i32>,
    pub daily: Option<i32>,
    pub weekly: Option<i32>,
    pub monthly: Option<i32>,
}

#[derive(Debug, Clone)]
pub struct PruneSummary {
    pub packs_removed: u64,
    pub bytes_removed: u64,
    pub packs_repacked: u64,
}

#[derive(Debug, Clone)]
pub struct BackupStats {
    pub snapshot: String,
    pub files: u64,
    pub bytes_in: u64,
    pub bytes_added: u64,
}

impl Client {
    /// Create an account from an invite.
    pub async fn register(cp: &str, invite: &str, phrase: &str, passphrase: &str, state_dir: &Path, net: NetMode) -> Result<Client> {
        let identity = Identity::from_seed(&seed_from_phrase(phrase, passphrase));
        let http = http_client()?;
        let resp = http.post(format!("{cp}/v1/accounts")).json(&RegisterAccount { invite: invite.into(), public_key: identity.public_hex() }).send().await?;
        if !resp.status().is_success() {
            bail!("registration failed: {} {}", resp.status(), resp.text().await.unwrap_or_default());
        }
        let c: AccountCreated = resp.json().await?;
        std::fs::create_dir_all(state_dir)?;
        std::fs::write(state_dir.join("account"), format!("{} {}", c.account, c.pool))?;
        Self::build(cp, c.account, c.pool, identity, phrase, passphrase, state_dir, net).await
    }

    /// Open an existing account from local state.
    pub async fn open(cp: &str, phrase: &str, passphrase: &str, state_dir: &Path, net: NetMode) -> Result<Client> {
        let text = std::fs::read_to_string(state_dir.join("account")).context("no account here: run `globalobject register` first")?;
        let mut it = text.split_whitespace();
        let id = it.next().and_then(|x| x.parse().ok()).ok_or_else(|| anyhow!("bad account file"))?;
        let pool = it.next().and_then(|x| x.parse().ok()).ok_or_else(|| anyhow!("bad account file"))?;
        let identity = Identity::from_seed(&seed_from_phrase(phrase, passphrase));
        Self::build(cp, id, pool, identity, phrase, passphrase, state_dir, net).await
    }

    #[allow(clippy::too_many_arguments)]
    async fn build(cp: &str, id: i64, pool: i64, identity: Identity, phrase: &str, passphrase: &str, state_dir: &Path, net: NetMode) -> Result<Client> {
        let endpoint = bind(net, vec![BLOBS_ALPN.to_vec()]).await?;
        // Garbage collection matters: without it, this store keeps every piece ever uploaded.
        let mem = MemStore::new_with_opts(MemOptions { gc_config: Some(GcConfig { interval: Duration::from_secs(2), add_protected: None }) });
        // Pieces wait here for hosts to fetch them. They are ciphertext named by hash.
        let router = Router::builder(endpoint.clone()).accept(BLOBS_ALPN, BlobsProtocol::new(&mem, None)).spawn();
        Ok(Client(Arc::new(Inner {
            cp: cp.into(),
            id,
            pool,
            identity,
            password: format!("{phrase}\0{passphrase}"),
            http: http_client()?,
            state_dir: state_dir.into(),
            endpoint,
            mem,
            _router: router,
            last_error: Mutex::new(None),
            cache: Mutex::new(VecDeque::new()),
            reads: tokio::sync::Semaphore::new(PARALLEL_READS),
            writes: tokio::sync::Semaphore::new(PARALLEL_WRITES),
        })))
    }

    /// Close peer connections cleanly. Call before exiting.
    pub async fn shutdown(&self) {
        self.0.endpoint.close().await;
    }

    pub fn id(&self) -> i64 {
        self.0.id
    }
    pub fn pool(&self) -> i64 {
        self.0.pool
    }
    pub fn signing(&self) -> &Identity {
        &self.0.identity
    }
    pub fn control_plane(&self) -> &str {
        &self.0.cp
    }
    pub fn endpoint_id(&self) -> String {
        self.0.endpoint.id().to_string()
    }

    /// Send a signed control-plane request.
    pub async fn call(&self, method: &str, path: &str, body: Option<&[u8]>) -> Result<reqwest::Response> {
        let raw = body.unwrap_or(b"");
        let id = format!("a{}", self.0.id);
        let token = self.0.identity.token(T_REQ, &RequestClaims::new(&id, method, path, raw));
        let m = reqwest::Method::from_bytes(method.as_bytes())?;
        Ok(self
            .0
            .http
            .request(m, format!("{}{path}", self.0.cp))
            .header(H_ID, id)
            .header(H_AUTH, token)
            .header("content-type", "application/json")
            .body(raw.to_vec())
            .send()
            .await?)
    }

    async fn post<T: Serialize, R: DeserializeOwned>(&self, path: &str, body: &T) -> Result<R> {
        let raw = serde_json::to_vec(body)?;
        let resp = self.call("POST", path, Some(&raw)).await?;
        decode_or_error(path, resp).await
    }

    pub async fn status(&self) -> Result<AccountStatus> {
        decode_or_error("/v1/accounts/me", self.call("GET", "/v1/accounts/me", None).await?).await
    }

    pub async fn invite(&self) -> Result<String> {
        let c: InviteCreated = self.post(&format!("/v1/pools/{}/invites", self.0.pool), &()).await?;
        Ok(c.invite)
    }

    /// Set a node's slice size. Shrinking drains pieces elsewhere first.
    pub async fn set_pledge(&self, node: i64, pledged: u64) -> Result<()> {
        let path = format!("/v1/nodes/{node}");
        let raw = serde_json::to_vec(&UpdateNode { pledged })?;
        let _: serde_json::Value = decode_or_error(&path, self.call("PATCH", &path, Some(&raw)).await?).await?;
        Ok(())
    }

    // ------------------------------------------------------------ pieces

    /// Code an object and upload its pieces to whichever hosts the control plane picks. Hosts
    /// that fail are avoided on the next attempt.
    async fn upload_object(&self, object: ObjectRef, data: Bytes) -> Result<()> {
        if data.len() > MAX_SEGMENT {
            bail!("repository file of {} bytes exceeds the segment limit", data.len());
        }
        let _permit = self.0.writes.acquire().await?;
        let mut avoid: Vec<i64> = vec![];
        for attempt in 0..UPLOAD_ATTEMPTS {
            // A new ID each attempt. Aborting queues deletion of the old one's pieces, which must
            // not be able to reach a retry that lands on the same host.
            let segment = random_hex(16);
            let policy = self.status().await?.policy;
            let layout = Layout::new(policy.k as usize, policy.n as usize, policy.local_groups as usize);
            let pieces = encode(&data, layout).map_err(|e| anyhow!("{e}"))?;
            let hashes: Vec<String> = pieces.iter().map(|p| hash_of(p)).collect();
            let req = BeginSegment {
                segment: segment.clone(),
                object: object.clone(),
                len: data.len() as u64,
                k: policy.k,
                n: policy.n,
                local_groups: policy.local_groups,
                hashes,
                endpoint: self.endpoint_id(),
                avoid: avoid.clone(),
            };
            let plan: SegmentPlan = self.post("/v1/segments/begin", &req).await?;

            // Hold the pieces where hosts can fetch them, until the last one is done.
            let mut held = vec![];
            for p in &pieces {
                held.push(self.0.mem.add_bytes(p.clone()).temp_tag().await?);
            }
            let from = addr_string(&self.0.endpoint);
            let mut jobs = FuturesUnordered::new();
            for p in &plan.pieces {
                let from = from.clone();
                jobs.push(async move {
                    let reply = peer_call(&self.0.endpoint, &p.addr, &PeerRequest::Store { grant: p.grant.clone(), from }).await;
                    (p.host, reply)
                });
            }
            let (mut receipts, mut failed) = (vec![], vec![]);
            while let Some((host, reply)) = jobs.next().await {
                match reply {
                    Ok(PeerReply::Stored { receipt }) => receipts.push(receipt),
                    other => {
                        tracing::warn!("upload to host {host} failed: {other:?}");
                        failed.push(host);
                    }
                }
            }
            drop(jobs);
            drop(held);
            if failed.is_empty() {
                let _: serde_json::Value = self.post(&format!("/v1/segments/{segment}/commit"), &CommitSegment { receipts, failed_hosts: vec![] }).await?;
                return Ok(());
            }
            let _: serde_json::Value =
                self.post(&format!("/v1/segments/{segment}/abort"), &CommitSegment { receipts: vec![], failed_hosts: failed.clone() }).await?;
            avoid.extend(failed);
            tracing::info!("retrying {segment}, attempt {}", attempt + 2);
        }
        bail!("could not place an object after {UPLOAD_ATTEMPTS} attempts")
    }

    async fn authorize(&self, p: &ReadPiece) -> Result<()> {
        match peer_call(&self.0.endpoint, &p.addr, &PeerRequest::Authorize { grant: p.grant.clone() }).await? {
            PeerReply::Ok => Ok(()),
            other => bail!("host {} refused: {other:?}", p.host),
        }
    }

    /// Credits the host for what it served. Best effort.
    async fn send_receipt(&self, p: &ReadPiece, bytes: u64) {
        let token = self.0.identity.token(T_READ, &ReadReceipt { nonce: p.nonce.clone(), host: p.host, reader: self.0.id, bytes });
        let _ = peer_call(&self.0.endpoint, &p.addr, &PeerRequest::Receipt { reader: self.0.id, nonce: p.nonce.clone(), token }).await;
    }

    async fn download_piece(&self, p: &ReadPiece, max: u64) -> Result<Vec<u8>> {
        self.authorize(p).await?;
        let out = fetch_verified(&self.0.endpoint, &p.addr, &p.hash, max).await?;
        self.send_receipt(p, out.len() as u64).await;
        Ok(out)
    }

    /// Fetch the granted range of one piece.
    async fn download_range(&self, p: &ReadPiece) -> Result<Vec<u8>> {
        let (start, end) = p.range.ok_or_else(|| anyhow!("no range in a ranged grant"))?;
        self.authorize(p).await?;
        let (out, sent) = fetch_range(&self.0.endpoint, &p.addr, &p.hash, start, end).await?;
        self.send_receipt(p, sent).await;
        Ok(out)
    }

    /// Fetch enough good pieces to decode, asking for others when a host fails.
    async fn read_segment(&self, id: &str) -> Result<Vec<u8>> {
        let mut pieces: Vec<Option<Vec<u8>>> = vec![];
        let mut tried: Vec<u16> = vec![];
        let mut layout: Option<Layout> = None;
        let mut len = 0u64;
        let mut failures = 0;
        let present = |p: &[Option<Vec<u8>>]| p.iter().map(Option::is_some).collect::<Vec<bool>>();
        loop {
            let want = match layout {
                None => self.status().await?.policy.k,
                // Short by this many, and at least one more: with local groups, `k` pieces
                // aren't always enough.
                Some(l) => l.k.saturating_sub(pieces.iter().filter(|p| p.is_some()).count()).max(1) as u16,
            };
            let req = ReadRequest { want, exclude_pieces: tried.clone(), endpoint: self.endpoint_id(), range: None };
            let plan = match self.request_read(id, &req).await {
                Ok(Some(p)) => p,
                Ok(None) => continue,
                Err(e) if is_unavailable(&e) && failures <= MAX_READ_FAILURES => {
                    // Every reachable piece has been tried. A failure may have been transient,
                    // and hosts come back, so start again and count it against the limit.
                    failures += 1;
                    tried.clear();
                    tried.extend(pieces.iter().enumerate().filter(|(_, p)| p.is_some()).map(|(i, _)| i as u16));
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    continue;
                }
                Err(e) => return Err(e),
            };
            let l = *layout.get_or_insert(Layout::new(plan.k as usize, plan.n as usize, plan.local_groups as usize));
            if pieces.is_empty() {
                len = plan.len;
                pieces = vec![None; l.n];
            }
            let mut jobs = FuturesUnordered::new();
            for p in &plan.pieces {
                jobs.push(async move { (p.piece, self.download_piece(p, plan.piece_len).await) });
            }
            while let Some((piece, res)) = jobs.next().await {
                tried.push(piece);
                match res {
                    Ok(bytes) if (piece as usize) < l.n => pieces[piece as usize] = Some(bytes),
                    Ok(_) => failures += 1,
                    Err(e) => {
                        tracing::warn!("piece {piece} of {id} failed: {e:#}");
                        failures += 1;
                    }
                }
            }
            drop(jobs);
            if l.can_decode(&present(&pieces)) {
                let data = reconstruct_data(pieces, l).map_err(|e| anyhow!("{e}"))?;
                return Ok(join(&data, l.k, len as usize));
            }
            if failures > MAX_READ_FAILURES {
                return Err(ClientError::Unavailable(format!("too many failed reads for segment {id}")).into());
            }
        }
    }

    /// Fetch bytes `[start, end)` of a segment from the data pieces holding them. An error
    /// means the caller should read and decode the whole segment instead.
    async fn read_segment_range(&self, id: &str, start: u64, end: u64) -> Result<Vec<u8>> {
        let mut tried: Vec<u16> = vec![];
        let mut failures = 0;
        loop {
            let req = ReadRequest { want: 1, exclude_pieces: tried.clone(), endpoint: self.endpoint_id(), range: Some((start, end)) };
            let Some(plan) = self.request_read(id, &req).await? else { continue };
            let mut out = Vec::with_capacity((end - start) as usize);
            let mut failed = None;
            for p in &plan.pieces {
                match self.download_range(p).await {
                    Ok(bytes) => out.extend_from_slice(&bytes),
                    Err(e) => {
                        tracing::warn!("range of piece {} of {id} failed: {e:#}", p.piece);
                        failed = Some(p.piece);
                        break;
                    }
                }
            }
            match failed {
                None => return Ok(out),
                Some(piece) => {
                    tried.push(piece);
                    failures += 1;
                    if failures >= 3 {
                        bail!("ranged read of {id} failed {failures} times");
                    }
                }
            }
        }
    }

    /// Ask for read grants, waiting while the bandwidth limit applies.
    async fn request_read(&self, id: &str, req: &ReadRequest) -> Result<Option<ReadPlan>> {
        let path = format!("/v1/segments/{id}/read");
        let raw = serde_json::to_vec(req)?;
        let resp = self.call("POST", &path, Some(&raw)).await?;
        if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let r: RetryAfter = resp.json().await?;
            tokio::time::sleep(Duration::from_secs_f64(r.retry_after.clamp(0.01, 60.0))).await;
            return Ok(None);
        }
        decode_or_error(&path, resp).await.map(Some)
    }

    // ----------------------------------------------------------- backups

    fn backend(&self) -> RepositoryBackends {
        RepositoryBackends::new(Arc::new(GridBackend { client: self.clone(), rt: tokio::runtime::Handle::current() }), None)
    }

    fn repo_options() -> RepositoryOptions {
        RepositoryOptions::default().no_cache(true)
    }

    fn take_error(&self, e: anyhow::Error) -> anyhow::Error {
        match self.0.last_error.lock().unwrap().take() {
            Some(c) => c.into(),
            None => e,
        }
    }

    /// Back up a path as a new snapshot, creating the repository if needed.
    pub async fn backup(&self, path: &Path) -> Result<BackupStats> {
        self.check_rollback().await?;
        let (me, backends, path) = (self.clone(), self.backend(), path.to_path_buf());
        let creds = self.credentials();
        let stats = tokio::task::spawn_blocking(move || -> Result<BackupStats> {
            let opts = Self::repo_options();
            let exists = !backends.repository().list(FileType::Config).map_err(|e| anyhow!("{e}"))?.is_empty();
            if !exists {
                let config = ConfigOptions::default()
                    .set_datapack_size_limit(bytesize::ByteSize::b(PACK_LIMIT))
                    .set_treepack_size_limit(bytesize::ByteSize::b(PACK_LIMIT));
                Repository::new(&opts, &backends)?.init(&creds, &KeyOptions::default(), &config)?;
            }
            let repo = Repository::new(&opts, &backends)?.open(&creds)?.to_indexed_ids()?;
            let source = PathList::from_string(path.to_str().ok_or_else(|| anyhow!("path isn't UTF-8"))?)?.sanitize()?;
            let snap = repo.backup(&BackupOptions::default(), &source, SnapshotOptions::default().to_snapshot()?)?;
            let s = snap.summary.clone().unwrap_or_default();
            drop(me);
            Ok(BackupStats {
                snapshot: snap.id.to_hex().as_str().to_string(),
                files: s.files_new + s.files_changed + s.files_unmodified,
                bytes_in: s.total_bytes_processed,
                bytes_added: s.data_added,
            })
        })
        .await?
        .map_err(|e| self.take_error(e))?;
        self.remember_snapshots(std::slice::from_ref(&stats.snapshot))?;
        Ok(stats)
    }

    fn credentials(&self) -> Credentials {
        Credentials::password(&self.0.password)
    }

    /// Snapshot IDs, oldest first.
    pub async fn snapshots(&self) -> Result<Vec<String>> {
        self.check_rollback().await?;
        let ids = self.list_snapshots().await?;
        self.remember_snapshots(&ids)?;
        Ok(ids)
    }

    async fn list_snapshots(&self) -> Result<Vec<String>> {
        let (backends, creds) = (self.backend(), self.credentials());
        tokio::task::spawn_blocking(move || -> Result<Vec<String>> {
            if backends.repository().list(FileType::Config).map_err(|e| anyhow!("{e}"))?.is_empty() {
                return Ok(vec![]);
            }
            let repo = Repository::new(&Self::repo_options(), &backends)?.open(&creds)?;
            let mut snaps = repo.get_all_snapshots()?;
            snaps.sort_by(|a, b| a.time.cmp(&b.time));
            Ok(snaps.iter().map(|s| s.id.to_hex().as_str().to_string()).collect())
        })
        .await?
        .map_err(|e| self.take_error(e))
    }

    fn seen_path(&self) -> PathBuf {
        self.0.state_dir.join("seen-snapshots")
    }

    fn remember_snapshots(&self, ids: &[String]) -> Result<()> {
        let mut seen: HashSet<String> = std::fs::read_to_string(self.seen_path()).unwrap_or_default().lines().map(str::to_string).collect();
        seen.extend(ids.iter().cloned());
        std::fs::write(self.seen_path(), seen.into_iter().collect::<Vec<_>>().join("\n"))?;
        Ok(())
    }

    fn unremember_snapshots(&self, ids: &[String]) -> Result<()> {
        let gone: HashSet<&String> = ids.iter().collect();
        let seen: Vec<String> = std::fs::read_to_string(self.seen_path())
            .unwrap_or_default()
            .lines()
            .filter(|l| !l.is_empty() && !gone.contains(&l.to_string()))
            .map(str::to_string)
            .collect();
        std::fs::write(self.seen_path(), seen.join("\n"))?;
        Ok(())
    }

    /// A control plane that hides snapshots is caught here. Contents can't be forged, since
    /// restic names repository files by hash, but the listing has to be checked separately.
    async fn check_rollback(&self) -> Result<()> {
        let seen = std::fs::read_to_string(self.seen_path()).unwrap_or_default();
        if seen.trim().is_empty() {
            return Ok(());
        }
        let listed: HashSet<String> = self.list_objects("snapshot").await?.into_iter().map(|o| o.id).collect();
        for id in seen.lines().filter(|l| !l.is_empty()) {
            if !listed.contains(id) {
                return Err(ClientError::Rollback(id.to_string()).into());
            }
        }
        Ok(())
    }

    async fn list_objects(&self, tpe: &str) -> Result<Vec<ObjectEntry>> {
        let path = format!("/v1/objects/{tpe}");
        decode_or_error(&path, self.call("GET", &path, None).await?).await
    }

    /// Read one file from a snapshot. `file` is its full path as backed up.
    pub async fn read_file(&self, snapshot: &str, file: &Path) -> Result<Vec<u8>> {
        let (backends, creds) = (self.backend(), self.credentials());
        let spec = format!("{snapshot}:{}", file.display());
        tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
            let repo = Repository::new(&Self::repo_options(), &backends)?.open(&creds)?.to_indexed()?;
            let node = repo.node_from_snapshot_path(&spec, |_| true)?;
            let size = node.meta.size as usize;
            if size == 0 {
                return Ok(vec![]);
            }
            let f = repo.open_file(&node)?;
            Ok(repo.read_file_at(&f, 0, size)?.to_vec())
        })
        .await?
        .map_err(|e| self.take_error(e))
    }

    /// Restore a whole snapshot under `dest`.
    pub async fn restore(&self, snapshot: &str, dest: &Path) -> Result<()> {
        let (backends, creds) = (self.backend(), self.credentials());
        let (spec, dest) = (format!("{snapshot}:/"), dest.to_path_buf());
        tokio::task::spawn_blocking(move || -> Result<()> {
            let repo = Repository::new(&Self::repo_options(), &backends)?.open(&creds)?.to_indexed()?;
            let node = repo.node_from_snapshot_path(&spec, |_| true)?;
            let ls = repo.ls(&node, &LsOptions::default())?;
            let target = LocalDestination::new(dest.to_str().ok_or_else(|| anyhow!("path isn't UTF-8"))?, true, !node.is_dir())?;
            let opts = RestoreOptions::default();
            let plan = repo.prepare_restore(&opts, ls.clone(), &target, false)?;
            repo.restore(plan, &opts, ls, &target)?;
            Ok(())
        })
        .await?
        .map_err(|e| self.take_error(e))
    }

    fn remember_error(&self, e: &anyhow::Error) {
        if let Some(c) = e.downcast_ref::<ClientError>() {
            *self.0.last_error.lock().unwrap() = Some(c.clone());
        }
    }

    async fn read_object(&self, tpe: &str, id: &str) -> Result<Bytes> {
        let key = format!("{tpe}/{id}");
        if let Some((_, b)) = self.0.cache.lock().unwrap().iter().find(|(k, _)| *k == key) {
            return Ok(b.clone());
        }
        let _permit = self.0.reads.acquire().await?;
        // Another thread may have fetched this while waiting for the permit.
        if let Some((_, b)) = self.0.cache.lock().unwrap().iter().find(|(k, _)| *k == key) {
            return Ok(b.clone());
        }
        let path = format!("/v1/objects/{tpe}/{id}");
        let entry: ObjectEntry = decode_or_error(&path, self.call("GET", &path, None).await?).await?;
        let data = Bytes::from(self.read_segment(&entry.segment).await?);
        let mut cache = self.0.cache.lock().unwrap();
        cache.push_back((key, data.clone()));
        if cache.len() > CACHE_OBJECTS {
            cache.pop_front();
        }
        Ok(data)
    }

    /// Bytes `[offset, offset + length)` of an object. Small parts of large objects are fetched
    /// on their own, and charged as such. Large reads fetch and cache the whole object, which
    /// suits restores, since they read packs almost from end to end.
    async fn read_object_range(&self, tpe: &str, id: &str, offset: u64, length: u64) -> Result<Bytes> {
        let key = format!("{tpe}/{id}");
        let slice = |b: &Bytes| -> Result<Bytes> {
            let (start, end) = (offset as usize, (offset + length) as usize);
            anyhow::ensure!(end <= b.len(), "read past the end of an object");
            Ok(b.slice(start..end))
        };
        if let Some((_, b)) = self.0.cache.lock().unwrap().iter().find(|(k, _)| *k == key) {
            return slice(b);
        }
        if length == 0 {
            return Ok(Bytes::new());
        }
        let path = format!("/v1/objects/{tpe}/{id}");
        let entry: ObjectEntry = decode_or_error(&path, self.call("GET", &path, None).await?).await?;
        anyhow::ensure!(offset + length <= entry.len, "read past the end of an object");
        if length * 2 >= entry.len {
            return slice(&self.read_object(tpe, id).await?);
        }
        let ranged = {
            let _permit = self.0.reads.acquire().await?;
            self.read_segment_range(&entry.segment, offset, offset + length).await
        };
        match ranged {
            Ok(b) => Ok(Bytes::from(b)),
            Err(e) => {
                // Some data piece is out of reach: decode the whole segment from what is.
                tracing::info!("partial read of {key} fell back to a full read: {e:#}");
                slice(&self.read_object(tpe, id).await?)
            }
        }
    }

    /// Remove snapshots outside the retention rules. Their data stays until `prune`.
    /// Returns the IDs removed.
    pub async fn forget(&self, keep: &Retention) -> Result<Vec<String>> {
        self.check_rollback().await?;
        let mut opts = KeepOptions::default();
        if let Some(n) = keep.last {
            opts = opts.keep_last(n);
        }
        if let Some(n) = keep.daily {
            opts = opts.keep_daily(n);
        }
        if let Some(n) = keep.weekly {
            opts = opts.keep_weekly(n);
        }
        if let Some(n) = keep.monthly {
            opts = opts.keep_monthly(n);
        }
        let (backends, creds) = (self.backend(), self.credentials());
        let doomed = tokio::task::spawn_blocking(move || -> Result<Vec<rustic_core::repofile::SnapshotId>> {
            let repo = Repository::new(&Self::repo_options(), &backends)?.open(&creds)?;
            let decided = opts.apply(repo.get_all_snapshots()?, &jiff::Zoned::now())?;
            Ok(decided.into_iter().filter(|f| !f.keep).map(|f| f.snapshot.id).collect())
        })
        .await?
        .map_err(|e| self.take_error(e))?;
        let ids: Vec<String> = doomed.iter().map(|i| i.to_hex().as_str().to_string()).collect();
        if ids.is_empty() {
            return Ok(ids);
        }
        // Forget them locally first, or their absence would later look like a rollback.
        self.unremember_snapshots(&ids)?;
        let (backends, creds) = (self.backend(), self.credentials());
        tokio::task::spawn_blocking(move || -> Result<()> {
            let repo = Repository::new(&Self::repo_options(), &backends)?.open(&creds)?;
            repo.delete_snapshots(&doomed)?;
            Ok(())
        })
        .await?
        .map_err(|e| self.take_error(e))?;
        Ok(ids)
    }

    /// Delete data no snapshot uses, repacking partly used packs within rustic's limits.
    /// Packs wait 23 hours before deletion, unless `instant`, in case a backup running
    /// elsewhere still refers to them. Repacking reads and writes through the normal paths,
    /// so it counts against the bandwidth limit and quota like any other traffic.
    pub async fn prune(&self, instant: bool) -> Result<PruneSummary> {
        self.check_rollback().await?;
        let (backends, creds) = (self.backend(), self.credentials());
        tokio::task::spawn_blocking(move || -> Result<PruneSummary> {
            let repo = Repository::new(&Self::repo_options(), &backends)?.open(&creds)?;
            let opts = PruneOptions::default().instant_delete(instant);
            let plan = repo.prune_plan(&opts)?;
            // Unused packs go now (or are marked, without `instant`); packs marked by an earlier
            // prune go once old enough. Repacking removes the unused parts of the rest.
            let size = plan.stats.size_sum();
            let summary = PruneSummary {
                packs_removed: plan.stats.packs.unused + plan.stats.packs_to_delete.remove,
                bytes_removed: size.remove + size.repackrm + plan.stats.size_to_delete.remove,
                packs_repacked: plan.stats.packs.repack,
            };
            repo.prune(&opts, plan)?;
            Ok(summary)
        })
        .await?
        .map_err(|e| self.take_error(e))
    }
}

/// Repository files, stored on the grid. Restic's format needs just these few operations.
#[derive(Clone)]
struct GridBackend {
    client: Client,
    rt: tokio::runtime::Handle,
}

impl std::fmt::Debug for GridBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "GridBackend({})", self.client.0.id)
    }
}

fn type_name(t: FileType) -> &'static str {
    match t {
        FileType::Config => "config",
        FileType::Index => "index",
        FileType::Key => "key",
        FileType::Snapshot => "snapshot",
        FileType::Pack => "pack",
    }
}

impl GridBackend {
    fn run<T>(&self, what: &'static str, f: impl std::future::Future<Output = Result<T>>) -> RusticResult<T> {
        self.rt.block_on(f).map_err(|e| {
            self.client.remember_error(&e);
            RusticError::new(ErrorKind::Backend, format!("{what}: {e:#}"))
        })
    }
}

impl ReadBackend for GridBackend {
    fn location(&self) -> String {
        format!("globalobject:{}", self.client.0.id)
    }
    fn list_with_size(&self, t: FileType) -> RusticResult<Vec<(Id, u32)>> {
        let list = self.run("list", self.client.list_objects(type_name(t)))?;
        list.into_iter()
            .map(|o| {
                let id: Id = o.id.parse().map_err(|_| RusticError::new(ErrorKind::Backend, "bad object id"))?;
                Ok((id, o.len as u32))
            })
            .collect()
    }
    fn read_full(&self, t: FileType, id: &Id) -> RusticResult<Bytes> {
        self.run("read", self.client.read_object(type_name(t), id.to_hex().as_str()))
    }
    fn read_partial(&self, t: FileType, id: &Id, _cacheable: bool, offset: u32, length: u32) -> RusticResult<Bytes> {
        self.run("read", self.client.read_object_range(type_name(t), id.to_hex().as_str(), offset as u64, length as u64))
    }
    fn warmup_path(&self, t: FileType, id: &Id) -> String {
        format!("{}/{}", type_name(t), id.to_hex().as_str())
    }
}

impl WriteBackend for GridBackend {
    fn write_bytes(&self, t: FileType, id: &Id, _cacheable: bool, content: BytesList) -> RusticResult<()> {
        let mut out = Vec::new();
        for b in content.into_vec() {
            out.extend_from_slice(&b);
        }
        let object = ObjectRef { tpe: type_name(t).into(), id: id.to_hex().as_str().to_string() };
        self.client.0.cache.lock().unwrap().retain(|(k, _)| *k != format!("{}/{}", object.tpe, object.id));
        self.run("write", self.client.upload_object(object, Bytes::from(out)))
    }
    fn remove(&self, t: FileType, id: &Id, _cacheable: bool) -> RusticResult<()> {
        let path = format!("/v1/objects/{}/{}/delete", type_name(t), id.to_hex().as_str());
        self.run("remove", async {
            let _: serde_json::Value = self.client.post(&path, &()).await?;
            Ok(())
        })
    }
}

fn http_client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(600)).build()?)
}

fn is_unavailable(e: &anyhow::Error) -> bool {
    matches!(e.downcast_ref::<ClientError>(), Some(ClientError::Unavailable(_)))
}

async fn decode_or_error<R: DeserializeOwned>(path: &str, resp: reqwest::Response) -> Result<R> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp.json().await?);
    }
    let text = resp.text().await.unwrap_or_default();
    let msg = serde_json::from_str::<ApiError>(&text).map(|e| e.error).unwrap_or(text);
    Err(match status.as_u16() {
        402 => ClientError::Quota(msg).into(),
        403 => ClientError::Forbidden(msg).into(),
        // The same status means different things on the read and write paths.
        503 if path.ends_with("/read") => ClientError::Unavailable(msg).into(),
        503 => ClientError::TooFewDomains(msg).into(),
        _ => anyhow!("{path}: {status}: {msg}"),
    })
}
