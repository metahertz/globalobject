//! HTTP API. Every authenticated request is signed by an account or node key.

use crate::db::{self, bytes_by, find_all, is_duplicate_key, to_bson, transaction, AccountDoc, NodeDoc, PieceDoc, PlacementGroup, SegmentDoc};
use crate::*;
use axum::{
    body::Bytes,
    extract::{ConnectInfo, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, patch, post},
    Json, Router,
};
use globalobject_crypto::open_token;
use globalobject_policy::{place, Candidate, PlacementRequest};
use mongodb::options::ReturnDocument;
use rand::{rngs::StdRng, seq::SliceRandom, Rng, SeedableRng};
use std::collections::{HashMap, HashSet};

type S = State<Arc<Shared>>;

/// The file types in a restic-format repository.
const OBJECT_TYPES: [&str; 5] = ["config", "index", "key", "snapshot", "pack"];
const MAX_OBJECT: u64 = 64 << 20;

pub enum ApiErr {
    Status(StatusCode, String),
    Retry(f64),
    Db(db::Error),
}
impl From<db::Error> for ApiErr {
    fn from(e: db::Error) -> Self {
        ApiErr::Db(e)
    }
}
impl From<mongodb::bson::de::Error> for ApiErr {
    fn from(e: mongodb::bson::de::Error) -> Self {
        ApiErr::Db(e.into())
    }
}
impl db::Transient for ApiErr {
    fn transient(&self) -> bool {
        matches!(self, ApiErr::Db(e) if e.transient())
    }
}
impl IntoResponse for ApiErr {
    fn into_response(self) -> Response {
        match self {
            ApiErr::Status(c, m) => (c, Json(ApiError { error: m })).into_response(),
            ApiErr::Retry(r) => (StatusCode::TOO_MANY_REQUESTS, Json(RetryAfter { retry_after: r })).into_response(),
            ApiErr::Db(e) => {
                tracing::error!("database error: {e}");
                (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiError { error: "internal error".into() })).into_response()
            }
        }
    }
}
pub(crate) fn fail<T>(c: StatusCode, m: impl Into<String>) -> Result<T, ApiErr> {
    Err(ApiErr::Status(c, m.into()))
}

fn stable_id(s: &str) -> u64 {
    u64::from_le_bytes(globalobject_crypto::hash(s.as_bytes())[..8].try_into().unwrap())
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Who {
    Account(i64),
    Node(i64),
}

/// Verify the request signature, returning who sent it.
async fn auth(s: &Shared, method: &str, path: &str, h: &HeaderMap, body: &[u8]) -> Result<Who, ApiErr> {
    let hdr = |n: &str| h.get(n).and_then(|v| v.to_str().ok()).map(str::to_string);
    let (Some(id), Some(token)) = (hdr(H_ID), hdr(H_AUTH)) else {
        return fail(StatusCode::UNAUTHORIZED, "missing credentials");
    };
    // `k<public key>`: an account that knows its key but not its number, such as the owner
    // restoring on a new machine with only the recovery phrase.
    let id_num = match id.strip_prefix('k') {
        Some(key) => match s.db.accounts.find_one(doc! {"public_key": key}).await? {
            Some(a) => format!("a{}", a.id),
            None => return fail(StatusCode::UNAUTHORIZED, "unknown identity"),
        },
        None => id.clone(),
    };
    let num: i64 = id_num.get(1..).and_then(|x| x.parse().ok()).ok_or(ApiErr::Status(StatusCode::UNAUTHORIZED, "bad id".into()))?;
    let (who, key): (Who, Option<String>) = if id_num.starts_with('a') {
        (Who::Account(num), s.db.accounts.find_one(doc! {"_id": num}).await?.map(|a| a.public_key))
    } else if id_num.starts_with('n') {
        (Who::Node(num), s.db.nodes.find_one(doc! {"_id": num}).await?.map(|n| n.public_key))
    } else {
        return fail(StatusCode::UNAUTHORIZED, "bad id");
    };
    let Some(key) = key else { return fail(StatusCode::UNAUTHORIZED, "unknown identity") };
    let claims: RequestClaims = open_token(&key, T_REQ, &token).map_err(|_| ApiErr::Status(StatusCode::UNAUTHORIZED, "bad token".into()))?;
    if claims.id != id || claims.method != method || claims.path != path || claims.body != body_hash(body) {
        return fail(StatusCode::UNAUTHORIZED, "token doesn't match this request");
    }
    if now().abs_diff(claims.ts) > 300 {
        return fail(StatusCode::UNAUTHORIZED, "stale token");
    }
    Ok(who)
}

async fn auth_account(s: &Shared, method: &str, path: &str, h: &HeaderMap, body: &[u8]) -> Result<(i64, i64, String), ApiErr> {
    let Who::Account(id) = auth(s, method, path, h, body).await? else {
        return fail(StatusCode::FORBIDDEN, "account credentials required");
    };
    let a = s.db.accounts.find_one(doc! {"_id": id}).await?.ok_or_else(|| missing("account"))?;
    Ok((id, a.pool, a.state))
}

fn parse<T: serde::de::DeserializeOwned>(b: &[u8]) -> Result<T, ApiErr> {
    serde_json::from_slice(b).map_err(|e| ApiErr::Status(StatusCode::BAD_REQUEST, format!("bad body: {e}")))
}

// ---------------------------------------------------------------- pools

async fn create_pool(State(s): S, h: HeaderMap, Json(req): Json<CreatePool>) -> Result<Json<PoolCreated>, ApiErr> {
    ops::require_operator(&s, &h)?;
    validate_pool(&req.policy, &req.quota)?;
    let id = s.db.next_id("pools").await?;
    s.db.pools
        .insert_one(db::PoolDoc {
            id,
            name: req.name,
            private: req.private,
            policy: req.policy,
            quota: req.quota,
            distinct_sites: req.distinct_sites,
            admin: None,
        })
        .await?;
    let invite = globalobject_crypto::random_hex(16);
    s.db.invites.insert_one(db::InviteDoc { token: invite.clone(), pool: id, inviter: None, used_by: None }).await?;
    Ok(Json(PoolCreated { pool: id, join_url: ops::join_url(&s, &h, &invite), admin_invite: invite }))
}

fn validate_pool(p: &Policy, q: &QuotaParams) -> Result<(), ApiErr> {
    p.validate().map_err(|e| ApiErr::Status(StatusCode::BAD_REQUEST, e.to_string()))?;
    if !q.can_grow() {
        return fail(StatusCode::BAD_REQUEST, "headroom × (1 − reserve) must exceed 1, or the pool can't fill");
    }
    Ok(())
}

async fn pool_doc(s: &Shared, pool: i64) -> Result<db::PoolDoc, ApiErr> {
    s.db.pools.find_one(doc! {"_id": pool}).await?.ok_or(ApiErr::Status(StatusCode::NOT_FOUND, "no such pool".into()))
}

async fn update_pool(State(s): S, Path(pool): Path<i64>, h: HeaderMap, body: Bytes) -> Result<Json<serde_json::Value>, ApiErr> {
    let (id, my_pool, _) = auth_account(&s, "PATCH", &format!("/v1/pools/{pool}"), &h, &body).await?;
    let p = pool_doc(&s, pool).await?;
    if my_pool != pool || !p.private || p.admin != Some(id) {
        return fail(StatusCode::FORBIDDEN, "only a private pool's admin can change its policy");
    }
    let policy: Policy = parse(&body)?;
    policy.validate().map_err(|e| ApiErr::Status(StatusCode::BAD_REQUEST, e.to_string()))?;
    s.db.pools.update_one(doc! {"_id": pool}, doc! {"$set": {"policy": to_bson(&policy)}}).await?;
    Ok(Json(serde_json::json!({"ok": true})))
}

/// Members invite into their own pool (only the admin, for a private one). The operator can
/// invite into any pool, with no inviter recorded.
async fn create_invite(State(s): S, Path(pool): Path<i64>, h: HeaderMap, body: Bytes) -> Result<Json<InviteCreated>, ApiErr> {
    let inviter = if ops::is_operator(&s, &h) {
        pool_doc(&s, pool).await?;
        None
    } else {
        let (id, my_pool, _) = auth_account(&s, "POST", &format!("/v1/pools/{pool}/invites"), &h, &body).await?;
        let p = pool_doc(&s, pool).await?;
        if my_pool != pool || (p.private && p.admin != Some(id)) {
            return fail(StatusCode::FORBIDDEN, "not allowed to invite to this pool");
        }
        Some(id)
    };
    let invite = globalobject_crypto::random_hex(16);
    s.db.invites.insert_one(db::InviteDoc { token: invite.clone(), pool, inviter, used_by: None }).await?;
    Ok(Json(InviteCreated { join_url: ops::join_url(&s, &h, &invite), invite }))
}

/// A private pool's admin, or the operator, can remove a member.
async fn remove_member(State(s): S, Path((pool, member)): Path<(i64, i64)>, h: HeaderMap, body: Bytes) -> Result<Json<serde_json::Value>, ApiErr> {
    if !ops::is_operator(&s, &h) {
        let (id, my_pool, _) = auth_account(&s, "POST", &format!("/v1/pools/{pool}/members/{member}/remove"), &h, &body).await?;
        let p = pool_doc(&s, pool).await?;
        if my_pool != pool || p.admin != Some(id) {
            return fail(StatusCode::FORBIDDEN, "admin only");
        }
    }
    let res = s.db.accounts.update_one(doc! {"_id": member, "pool": pool}, doc! {"$set": {"state": "read_only", "read_only_since": fnow()}}).await?;
    if res.matched_count == 0 {
        return fail(StatusCode::NOT_FOUND, "no such member");
    }
    // Draining moves everything they host elsewhere before anything is released.
    s.db.nodes.update_many(doc! {"account": member}, doc! {"$set": {"state": "draining", "target_pledged": 0_i64}}).await?;
    Ok(Json(serde_json::json!({"ok": true})))
}

// ----------------------------------------------------- accounts and nodes

async fn register_account(State(s): S, Json(req): Json<RegisterAccount>) -> Result<Json<AccountCreated>, ApiErr> {
    // Taken outside the transaction, so a retry doesn't conflict on the counter.
    let id = s.db.next_id("accounts").await?;
    let pool = transaction!(s.db, sess, {
        let inv = s.db.invites.find_one_and_update(doc! {"_id": &req.invite, "used_by": null}, doc! {"$set": {"used_by": id}}).session(&mut sess).await?;
        let Some(inv) = inv else { return fail(StatusCode::FORBIDDEN, "invalid invite") };
        let account = AccountDoc {
            id,
            pool: inv.pool,
            public_key: req.public_key.clone(),
            inviter: inv.inviter,
            state: "active".into(),
            over_since: None,
            read_only_since: None,
            banked_credit: 0.0,
            bucket_tokens: 0.0,
            bucket_at: fnow(),
            groups: vec![],
            upload_seq: 0,
        };
        s.db.accounts.insert_one(&account).session(&mut sess).await.map_err(|e| {
            if is_duplicate_key(&e) {
                ApiErr::Status(StatusCode::CONFLICT, "key already registered".into())
            } else {
                e.into()
            }
        })?;
        // The first account through a pool's bootstrap invite becomes its admin.
        s.db.pools.update_one(doc! {"_id": inv.pool, "admin": null}, doc! {"$set": {"admin": id}}).session(&mut sess).await?;
        Ok::<_, ApiErr>(inv.pool)
    })?;
    Ok(Json(AccountCreated { account: id, pool, grant_key: s.identity.public_hex() }))
}

async fn register_node(State(s): S, peer: Option<ConnectInfo<std::net::SocketAddr>>, h: HeaderMap, body: Bytes) -> Result<Json<NodeCreated>, ApiErr> {
    let (id, pool, state) = auth_account(&s, "POST", "/v1/nodes", &h, &body).await?;
    if state == "purged" {
        return fail(StatusCode::FORBIDDEN, "account removed");
    }
    let req: RegisterNode = parse(&body)?;
    // A node re-registering (after a restart, say) refreshes its address. A key that another
    // account registered in this pool is refused.
    let refresh = || async {
        s.db.nodes
            .find_one_and_update(
                doc! {"pool": pool, "public_key": &req.public_key, "account": id},
                doc! {"$set": {"addr": &req.addr, "endpoint": &req.endpoint, "probe_hash": &req.probe_hash, "probe_bytes": req.probe_bytes as i64, "last_seen": fnow()}},
            )
            .await
    };
    if let Some(n) = refresh().await? {
        return Ok(Json(NodeCreated { node: n.id, pool, grant_key: s.identity.public_hex() }));
    }
    let node = NodeDoc {
        id: s.db.next_id("nodes").await?,
        pool,
        account: id,
        public_key: req.public_key.clone(),
        addr: req.addr.clone(),
        endpoint: req.endpoint.clone(),
        probe_hash: req.probe_hash.clone(),
        probe_bytes: req.probe_bytes as i64,
        // Left empty, the site comes from the address the node connects from. The control
        // plane deciding it, rather than the user, is what makes failure domains meaningful.
        site: if req.site.is_empty() { ops::derived_site(&s, peer, &h) } else { req.site.clone() },
        asn: if req.asn.is_empty() { "unknown".into() } else { req.asn.clone() },
        pledged: req.pledged as i64,
        target_pledged: None,
        state: "active".into(),
        last_seen: fnow(),
        audit_ok: 0,
        audit_fail: 0,
        measured_bps: None,
        probe_requested_at: None,
        probe_samples: vec![],
        refused_by: vec![],
    };
    match s.db.nodes.insert_one(&node).await {
        Ok(_) => Ok(Json(NodeCreated { node: node.id, pool, grant_key: s.identity.public_hex() })),
        Err(e) if is_duplicate_key(&e) => match refresh().await? {
            // Registered by this account concurrently.
            Some(n) => Ok(Json(NodeCreated { node: n.id, pool, grant_key: s.identity.public_hex() })),
            None => fail(StatusCode::CONFLICT, "node key belongs to another account"),
        },
        Err(e) => Err(e.into()),
    }
}

async fn update_node(State(s): S, Path(node): Path<i64>, h: HeaderMap, body: Bytes) -> Result<Json<serde_json::Value>, ApiErr> {
    let (id, _, _) = auth_account(&s, "PATCH", &format!("/v1/nodes/{node}"), &h, &body).await?;
    let req: UpdateNode = parse(&body)?;
    let owner = s.db.nodes.find_one(doc! {"_id": node}).await?.map(|n| n.account);
    if owner != Some(id) {
        return fail(StatusCode::FORBIDDEN, "not your node");
    }
    let held = bytes_by(&s.db, "pieces.host", &[node], doc! {"state": "stored"}, None).await?.get(&node).copied().unwrap_or(0);
    if req.pledged as i64 >= held {
        s.db.nodes.update_one(doc! {"_id": node}, doc! {"$set": {"pledged": req.pledged as i64, "target_pledged": null}}).await?;
    } else {
        // Keep the old pledge until enough has drained away. See `reconcile::drain`.
        s.db.nodes.update_one(doc! {"_id": node}, doc! {"$set": {"target_pledged": req.pledged as i64}}).await?;
    }
    Ok(Json(serde_json::json!({"ok": true, "held": held})))
}

/// Which account a key belongs to. Signed with the key itself, using a `k<public key>` id.
async fn whoami(State(s): S, h: HeaderMap) -> Result<Json<AccountCreated>, ApiErr> {
    let (id, pool, _) = auth_account(&s, "GET", "/v1/accounts/whoami", &h, b"").await?;
    Ok(Json(AccountCreated { account: id, pool, grant_key: s.identity.public_hex() }))
}

async fn account_status(State(s): S, h: HeaderMap) -> Result<Json<AccountStatus>, ApiErr> {
    let (id, pool, state) = auth_account(&s, "GET", "/v1/accounts/me", &h, b"").await?;
    let (policy, q, _) = pool_settings(&s.db, pool).await?;
    let u = usage(&s, id, &q).await?;
    let banked = s.db.accounts.find_one(doc! {"_id": id}).await?.map_or(0.0, |a| a.banked_credit);
    Ok(Json(AccountStatus {
        account: id,
        pool,
        state,
        pledged: u.pledged,
        proven_held: u.proven_held,
        credited: u.credited,
        quota_raw: u.quota_raw,
        used_raw: u.used_raw,
        entitled_bps: u.entitled_bps,
        banked_credit: banked,
        policy,
    }))
}

/// Candidates in this pool: seen recently, active, and not over their pledge.
pub(crate) async fn candidates(s: &Shared, pool: i64, online_within: f64) -> DbResult<(Vec<Candidate>, HashMap<i64, (String, i64)>)> {
    let nodes = find_all(&s.db.nodes, doc! {"pool": pool, "state": "active", "last_seen": {"$gte": fnow() - online_within}}).await?;
    let ids: Vec<i64> = nodes.iter().map(|n| n.id).collect();
    // Space already used, and space promised to repairs on their way in.
    let held = bytes_by(&s.db, "pieces.host", &ids, doc! {}, None).await?;
    let incoming = bytes_by(&s.db, "pieces.repair.target", &ids, doc! {}, None).await?;
    let mut cands = vec![];
    let mut info = HashMap::new();
    for n in nodes {
        let eff = n.target_pledged.unwrap_or(n.pledged);
        let used = held.get(&n.id).copied().unwrap_or(0) + incoming.get(&n.id).copied().unwrap_or(0);
        cands.push(Candidate {
            host: n.id as u64,
            owner: n.account as u64,
            site: stable_id(&n.site),
            asn: stable_id(&n.asn),
            free_bytes: (eff - used).max(0) as u64,
            reputation: 1.0,
        });
        info.insert(n.id, (n.addr, n.account));
    }
    Ok((cands, info))
}

// ------------------------------------------------------------- segments

async fn begin_segment(State(s): S, h: HeaderMap, body: Bytes) -> Result<Json<SegmentPlan>, ApiErr> {
    let (id, pool, state) = auth_account(&s, "POST", "/v1/segments/begin", &h, &body).await?;
    if state != "active" {
        return fail(StatusCode::FORBIDDEN, format!("account is {state}"));
    }
    let req: BeginSegment = parse(&body)?;
    if req.segment.len() != 32 || !req.segment.chars().all(|c| c.is_ascii_hexdigit()) {
        return fail(StatusCode::BAD_REQUEST, "segment id must be 16 random bytes in hex");
    }
    if !OBJECT_TYPES.contains(&req.object.tpe.as_str()) || req.object.id.is_empty() || !req.object.id.chars().all(|c| c.is_ascii_hexdigit()) {
        return fail(StatusCode::BAD_REQUEST, "bad object reference");
    }
    if req.len == 0 || req.len > MAX_OBJECT {
        return fail(StatusCode::BAD_REQUEST, "bad segment size");
    }
    let (policy, q, distinct_sites) = pool_settings(&s.db, pool).await?;
    if (req.k, req.n, req.local_groups) != (policy.k, policy.n, policy.local_groups) {
        return fail(StatusCode::CONFLICT, "the pool's policy has changed");
    }
    let (k, n) = (policy.k as i32, policy.n as i32);
    if req.hashes.len() != n as usize || !req.hashes.iter().all(|h| h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit())) {
        return fail(StatusCode::BAD_REQUEST, "need one BLAKE3 hash per piece");
    }
    if k == 1 && req.hashes.iter().collect::<HashSet<_>>().len() != 1 {
        return fail(StatusCode::BAD_REQUEST, "replicas must be identical");
    }
    let piece_len = policy.piece_len(req.len);
    let raw = policy.raw_bytes(req.len);

    // Placement reads the whole pool, so it happens before the transaction below.
    let (cands, info) = candidates(&s, pool, s.cfg.grace_secs.min(s.cfg.reconcile_every.as_secs_f64() * 4.0 + 5.0)).await?;
    let avoid: HashSet<i64> = req.avoid.iter().copied().collect();
    let usable: Vec<Candidate> = cands.into_iter().filter(|c| !avoid.contains(&(c.host as i64))).collect();
    let domains = |c: &[Candidate]| -> usize {
        c.iter().filter(|x| x.owner != id as u64 && x.free_bytes >= piece_len).map(|x| x.domain(policy.domain_level)).collect::<HashSet<_>>().len()
    };

    let hosts = transaction!(s.db, sess, {
        // Serialise this account's uploads: a concurrent one also writes this document, so one
        // of the two transactions hits a write conflict and runs again, seeing the other's segment.
        let account =
            s.db.accounts
                .find_one_and_update(doc! {"_id": id}, doc! {"$inc": {"upload_seq": 1_i64}})
                .return_document(ReturnDocument::After)
                .session(&mut sess)
                .await?
                .ok_or_else(|| missing("account"))?;
        let u = usage_in(&s.db, &s.cfg, id, &q, Some(&mut sess)).await?;
        if u.used_raw + raw > u.quota_raw {
            return fail(StatusCode::PAYMENT_REQUIRED, format!("quota exceeded: {} of {} raw bytes used, need {raw} more", u.used_raw, u.quota_raw));
        }
        if domains(&usable) < (policy.min_failure_domains as usize).max(n as usize) {
            return fail(StatusCode::SERVICE_UNAVAILABLE, format!("this pool has {} eligible failure domains, and the policy needs {}", domains(&usable), n));
        }

        // Reuse the current placement group when all of its hosts can still take a piece.
        let mut groups = account.groups.clone();
        let current = groups.iter().position(|g| g.k == k && g.n == n);
        let reusable = current.filter(|&i| {
            let g = &groups[i];
            g.used < s.cfg.group_segments
                && g.hosts.len() == n as usize
                && g.hosts.iter().all(|h| usable.iter().any(|c| c.host == *h as u64 && c.free_bytes >= piece_len))
        });
        let hosts: Vec<i64> = match reusable {
            Some(i) => {
                groups[i].used += 1;
                groups[i].hosts.clone()
            }
            None => {
                let preq = PlacementRequest { count: n as usize, piece_bytes: piece_len, exclude_owner: Some(id as u64), distinct_sites, ..Default::default() };
                let mut rng = StdRng::from_entropy();
                let chosen =
                    place(&usable, policy.domain_level, &preq, &mut rng).map_err(|e| ApiErr::Status(StatusCode::SERVICE_UNAVAILABLE, e.to_string()))?;
                let hosts: Vec<i64> = chosen.iter().map(|&i| usable[i].host as i64).collect();
                let g = PlacementGroup { k, n, hosts: hosts.clone(), used: 1 };
                match current {
                    Some(i) => groups[i] = g,
                    None => groups.push(g),
                }
                hosts
            }
        };
        s.db.accounts.update_one(doc! {"_id": id}, doc! {"$set": {"groups": to_bson(&groups)}}).session(&mut sess).await?;

        let segment = SegmentDoc {
            id: req.segment.clone(),
            pool,
            owner: id,
            k,
            n,
            local_groups: policy.local_groups as i32,
            len: req.len as i64,
            object_tpe: req.object.tpe.clone(),
            object_id: req.object.id.clone(),
            piece_len: piece_len as i64,
            state: "pending".into(),
            created_at: fnow(),
            pieces: hosts
                .iter()
                .enumerate()
                .map(|(i, &host)| PieceDoc { piece: i as i32, host, hash: req.hashes[i].clone(), state: "pending".into(), repair: None })
                .collect(),
        };
        s.db.segments.insert_one(&segment).session(&mut sess).await.map_err(|e| {
            if is_duplicate_key(&e) {
                ApiErr::Status(StatusCode::CONFLICT, "segment already exists".into())
            } else {
                e.into()
            }
        })?;
        Ok::<_, ApiErr>(hosts)
    })?;

    // Grants are signed only once the placement is committed.
    let pieces = hosts
        .iter()
        .enumerate()
        .map(|(i, host)| {
            let grant = sign_grant(&s, id, pool, &req.segment, i as i32, *host, &req.endpoint, &req.hashes[i], Op::Write, piece_len, 0);
            PlacedPiece { piece: i as u16, host: *host, addr: info[host].0.clone(), grant: grant.token }
        })
        .collect();
    Ok(Json(SegmentPlan { pieces }))
}

async fn commit_segment(State(s): S, Path(seg): Path<String>, h: HeaderMap, body: Bytes) -> Result<Json<serde_json::Value>, ApiErr> {
    let (id, pool, _) = auth_account(&s, "POST", &format!("/v1/segments/{seg}/commit"), &h, &body).await?;
    let req: CommitSegment = parse(&body)?;
    record_refusals(&s, id, &req.failed_hosts).await?;
    let Some(g) = s.db.segments.find_one(doc! {"_id": &seg}).await? else { return fail(StatusCode::NOT_FOUND, "no such segment") };
    if g.owner != id {
        return fail(StatusCode::FORBIDDEN, "not your segment");
    }
    if g.state != "pending" {
        return fail(StatusCode::CONFLICT, "segment is not pending");
    }
    if req.receipts.len() != g.n as usize {
        return fail(StatusCode::BAD_REQUEST, format!("need {} receipts, got {}", g.n, req.receipts.len()));
    }
    let hosts: Vec<i64> = g.pieces.iter().map(|p| p.host).collect();
    let keys: HashMap<i64, String> = find_all(&s.db.nodes, doc! {"_id": {"$in": &hosts}}).await?.into_iter().map(|n| (n.id, n.public_key)).collect();
    let mut proven: HashSet<i32> = HashSet::new();
    for token in &req.receipts {
        // A receipt must verify against the key of the host holding the piece it names.
        let found = g.pieces.iter().find_map(|p| {
            let rc: StoreReceipt = open_token(keys.get(&p.host)?, T_STORE, token).ok()?;
            (rc.piece as i32 == p.piece && rc.host == p.host && rc.segment == seg && rc.bytes == g.piece_len as u64 && rc.hash == p.hash).then_some(p.piece)
        });
        let Some(piece) = found else { return fail(StatusCode::BAD_REQUEST, "receipt does not match the placement") };
        proven.insert(piece);
    }
    if proven.len() != g.n as usize {
        return fail(StatusCode::BAD_REQUEST, "duplicate receipts");
    }
    let old = transaction!(s.db, sess, {
        let r =
            s.db.segments
                .update_one(doc! {"_id": &seg, "state": "pending"}, doc! {"$set": {"state": "committed", "pieces.$[].state": "stored"}})
                .session(&mut sess)
                .await?;
        if r.matched_count == 0 {
            return fail(StatusCode::CONFLICT, "segment is not pending");
        }
        // Point the file name at this segment, returning what it pointed at before.
        let old =
            s.db.objects
                .find_one_and_update(doc! {"account": id, "tpe": &g.object_tpe, "object": &g.object_id}, doc! {"$set": {"len": g.len, "segment": &seg}})
                .upsert(true)
                .session(&mut sess)
                .await?;
        Ok::<_, ApiErr>(old.map(|o| o.segment))
    })?;
    // Repository files are immutable, apart from a rewritten config. Reclaim what was replaced.
    if let Some(old) = old.filter(|o| *o != seg) {
        reconcile::remove_segment(&s, &old, Some(id), None).await?;
    }
    ledger(&s.db, pool, id, "store", g.piece_len * g.n as i64, &seg).await?;
    Ok(Json(serde_json::json!({"ok": true})))
}

/// A report by one client proves nothing, since clients can lie. Several distinct
/// accounts reporting a host is what lets a false pledge surface. See `reconcile::refusals`.
async fn record_refusals(s: &Shared, reporter: i64, hosts: &[i64]) -> DbResult<()> {
    if !hosts.is_empty() {
        s.db.nodes.update_many(doc! {"_id": {"$in": hosts}}, doc! {"$addToSet": {"refused_by": reporter}}).await?;
    }
    Ok(())
}

async fn abort_segment(State(s): S, Path(seg): Path<String>, h: HeaderMap, body: Bytes) -> Result<Json<serde_json::Value>, ApiErr> {
    let (id, _, _) = auth_account(&s, "POST", &format!("/v1/segments/{seg}/abort"), &h, &body).await?;
    let req: CommitSegment = parse(&body)?;
    record_refusals(&s, id, &req.failed_hosts).await?;
    reconcile::remove_segment(&s, &seg, Some(id), Some("pending")).await?;
    Ok(Json(serde_json::json!({"ok": true})))
}

async fn read_segment(State(s): S, Path(seg): Path<String>, h: HeaderMap, body: Bytes) -> Result<Json<ReadPlan>, ApiErr> {
    let (id, pool, state) = auth_account(&s, "POST", &format!("/v1/segments/{seg}/read"), &h, &body).await?;
    if state == "purged" {
        return fail(StatusCode::FORBIDDEN, "account removed");
    }
    let req: ReadRequest = parse(&body)?;
    let Some(g) = s.db.segments.find_one(doc! {"_id": &seg}).await? else { return fail(StatusCode::NOT_FOUND, "no such segment") };
    let (k, n, len, piece_len, local) = (g.k, g.n, g.len, g.piece_len, g.local_groups);
    if g.owner != id {
        return fail(StatusCode::FORBIDDEN, "not your segment");
    }
    if g.state != "committed" {
        return fail(StatusCode::CONFLICT, "segment is not committed");
    }
    let (_, q, _) = pool_settings(&s.db, pool).await?;
    let online = fnow() - s.cfg.reconcile_every.as_secs_f64().max(1.0) * 3.0 - 3.0;
    // Stored pieces on hosts seen recently, with each host's address.
    let hosts: Vec<i64> = g.pieces.iter().map(|p| p.host).collect();
    let addrs: HashMap<i64, String> =
        find_all(&s.db.nodes, doc! {"_id": {"$in": &hosts}, "last_seen": {"$gte": online}}).await?.into_iter().map(|n| (n.id, n.addr)).collect();
    let mut available: Vec<&PieceDoc> =
        g.pieces.iter().filter(|p| p.state == "stored" && addrs.contains_key(&p.host) && !req.exclude_pieces.contains(&(p.piece as u16))).collect();
    let mut rng = StdRng::from_entropy();
    available.shuffle(&mut rng);

    // Each chosen piece, with the part of it granted. `None` is the whole piece.
    let chosen: Vec<(&PieceDoc, Option<(u64, u64)>)> = match req.range {
        Some((start, end)) => {
            if start >= end || end > len as u64 {
                return fail(StatusCode::BAD_REQUEST, "bad range");
            }
            // Codes are systematic, so a range lives in the data pieces covering it.
            let wanted = globalobject_segment::data_ranges(start, end, piece_len as u64, k as usize);
            if k == 1 {
                let Some(p) = available.first() else { return fail(StatusCode::SERVICE_UNAVAILABLE, "no reachable pieces right now") };
                vec![(*p, Some((start, end)))]
            } else {
                let found: Vec<_> = wanted.iter().filter_map(|w| available.iter().find(|p| p.piece == w.0 as i32).map(|p| (*p, Some((w.1, w.2))))).collect();
                if found.len() != wanted.len() {
                    // The client decodes the whole segment instead: a degraded read.
                    return fail(StatusCode::SERVICE_UNAVAILABLE, "part of this range is unreachable; read the whole object");
                }
                found
            }
        }
        None => {
            // Data and global parity first: local parities only help within their group.
            let want = (req.want as usize).clamp(1, n as usize);
            available.sort_by_key(|p| p.piece >= n - local);
            if available.is_empty() {
                return fail(StatusCode::SERVICE_UNAVAILABLE, "no reachable pieces right now");
            }
            available.iter().take(want).map(|p| (*p, None)).collect()
        }
    };

    // Token bucket. Each grant is charged whether or not the client uses it, so asking for
    // more hosts can't buy more than the entitled rate. A range costs what serving it sends.
    let size = |range: Option<(u64, u64)>| range.map_or(piece_len as u64, |(a, b)| globalobject_net::served_bytes(a, b, piece_len as u64));
    let cost: f64 = chosen.iter().map(|(_, range)| size(*range) as f64).sum();
    let u = usage(&s, id, &q).await?;
    let rate = u.entitled_bps;
    let burst = (rate * s.cfg.burst_secs).max(piece_len as f64 * k as f64);
    // Compare-and-set on the bucket: a concurrent read by the same account makes this retry.
    let mut attempts = 0;
    let spent = loop {
        let a = s.db.accounts.find_one(doc! {"_id": id}).await?.ok_or_else(|| missing("account"))?;
        let mut bucket = globalobject_policy::TokenBucket { tokens: a.bucket_tokens, updated_at: a.bucket_at };
        bucket.refill(fnow(), rate, burst);
        let spent = bucket.take(cost, a.banked_credit, rate).map_err(ApiErr::Retry)?;
        let r =
            s.db.accounts
                .update_one(
                    doc! {"_id": id, "bucket_tokens": a.bucket_tokens, "bucket_at": a.bucket_at},
                    doc! {"$set": {"bucket_tokens": bucket.tokens, "bucket_at": bucket.updated_at}, "$inc": {"banked_credit": -spent}},
                )
                .await?;
        if r.matched_count == 1 {
            break spent;
        }
        attempts += 1;
        if attempts >= 16 {
            return Err(ApiErr::Retry(0.1 + rng.gen::<f64>() * 0.2));
        }
    };

    // Each host also paces its share, so a burst can't saturate a single peer.
    let per_host = if spent > 0.0 { 0 } else { (rate / chosen.len() as f64).max(1.0) as u64 };
    let mut pieces = vec![];
    for (p, range) in chosen {
        let max = size(range);
        let grant = sign_grant_range(&s, id, pool, &seg, p.piece, p.host, &req.endpoint, &p.hash, Op::Read, max, per_host, range);
        s.db.read_grants.insert_one(db::ReadGrantDoc { nonce: grant.nonce.clone(), reader: id, host: p.host, max_bytes: max as i64, credited: 0 }).await?;
        pieces.push(ReadPiece {
            piece: p.piece as u16,
            host: p.host,
            addr: addrs[&p.host].clone(),
            grant: grant.token,
            nonce: grant.nonce,
            hash: p.hash.clone(),
            range,
        });
    }
    ledger(&s.db, pool, id, "read_charge", cost as i64, &seg).await?;
    Ok(Json(ReadPlan { k: k as u16, n: n as u16, local_groups: local as u16, len: len as u64, piece_len: piece_len as u64, pieces }))
}

async fn read_receipts(State(s): S, h: HeaderMap, body: Bytes) -> Result<Json<serde_json::Value>, ApiErr> {
    let Who::Node(node) = auth(&s, "POST", "/v1/receipts/read", &h, &body).await? else {
        return fail(StatusCode::FORBIDDEN, "node credentials required");
    };
    let req: ReadReceipts = parse(&body)?;
    let me = s.db.nodes.find_one(doc! {"_id": node}).await?.ok_or_else(|| missing("node"))?;
    let (account, pool) = (me.account, me.pool);
    for sr in &req.receipts {
        let Some(g) = s.db.read_grants.find_one(doc! {"_id": &sr.nonce, "host": node}).await? else { continue };
        let Some(reader) = s.db.accounts.find_one(doc! {"_id": g.reader}).await? else { continue };
        let Ok(rc) = open_token::<ReadReceipt>(&reader.public_key, T_READ, &sr.token) else { continue };
        if g.reader != sr.reader || rc.reader != g.reader || rc.nonce != sr.nonce || rc.host != node || g.reader == account {
            continue;
        }
        let claimed = (rc.bytes as i64).min(g.max_bytes);
        // Raise the credited total only upwards, atomically, and credit the difference. Two
        // receipts for one grant arriving together can't both credit the same bytes.
        let before =
            s.db.read_grants
                .find_one_and_update(doc! {"_id": &sr.nonce, "credited": {"$lt": claimed}}, doc! {"$set": {"credited": claimed}})
                .return_document(ReturnDocument::Before)
                .await?;
        let Some(before) = before else { continue };
        let delta = claimed - before.credited;
        s.db.accounts.update_one(doc! {"_id": account}, doc! {"$inc": {"banked_credit": delta as f64}}).await?;
        ledger(&s.db, pool, account, "served", delta, &sr.nonce).await?;
    }
    Ok(Json(serde_json::json!({"ok": true})))
}

// ------------------------------------------------------------------ roots

async fn list_objects(State(s): S, Path(tpe): Path<String>, h: HeaderMap) -> Result<Json<Vec<ObjectEntry>>, ApiErr> {
    let (id, _, _) = auth_account(&s, "GET", &format!("/v1/objects/{tpe}"), &h, b"").await?;
    let rows = find_all(&s.db.objects, doc! {"account": id, "tpe": &tpe}).await?;
    Ok(Json(rows.into_iter().map(|o| ObjectEntry { id: o.object, len: o.len as u64, segment: o.segment }).collect()))
}

async fn get_object(State(s): S, Path((tpe, object)): Path<(String, String)>, h: HeaderMap) -> Result<Json<ObjectEntry>, ApiErr> {
    let (id, _, _) = auth_account(&s, "GET", &format!("/v1/objects/{tpe}/{object}"), &h, b"").await?;
    let Some(o) = s.db.objects.find_one(doc! {"account": id, "tpe": &tpe, "object": &object}).await? else {
        return fail(StatusCode::NOT_FOUND, "no such object");
    };
    Ok(Json(ObjectEntry { id: object, len: o.len as u64, segment: o.segment }))
}

async fn delete_object(State(s): S, Path((tpe, object)): Path<(String, String)>, h: HeaderMap, body: Bytes) -> Result<Json<serde_json::Value>, ApiErr> {
    let (id, _, state) = auth_account(&s, "POST", &format!("/v1/objects/{tpe}/{object}/delete"), &h, &body).await?;
    if state != "active" {
        return fail(StatusCode::FORBIDDEN, format!("account is {state}"));
    }
    if let Some(o) = s.db.objects.find_one(doc! {"account": id, "tpe": &tpe, "object": &object}).await? {
        reconcile::remove_segment(&s, &o.segment, Some(id), None).await?;
    }
    Ok(Json(serde_json::json!({"ok": true})))
}

// ------------------------------------------------------------ node traffic

async fn heartbeat(State(s): S, Path(node): Path<i64>, h: HeaderMap, body: Bytes) -> Result<Json<HeartbeatReply>, ApiErr> {
    let who = auth(&s, "POST", &format!("/v1/nodes/{node}/heartbeat"), &h, &body).await?;
    if who != Who::Node(node) {
        return fail(StatusCode::FORBIDDEN, "wrong node");
    }
    let hb: Heartbeat = parse(&body)?;
    s.db.nodes.update_one(doc! {"_id": node}, doc! {"$set": {"last_seen": fnow(), "addr": &hb.addr}}).await?;
    let tasks = reconcile::tasks_for(&s, node).await?;
    Ok(Json(HeartbeatReply { tasks }))
}

async fn repair_done(State(s): S, h: HeaderMap, body: Bytes) -> Result<Json<serde_json::Value>, ApiErr> {
    let Who::Node(node) = auth(&s, "POST", "/v1/tasks/repair-done", &h, &body).await? else {
        return fail(StatusCode::FORBIDDEN, "node credentials required");
    };
    let req: RepairDone = parse(&body)?;
    reconcile::finish_repair(&s, node, &req).await?;
    Ok(Json(serde_json::json!({"ok": true})))
}

async fn probe_result(State(s): S, h: HeaderMap, body: Bytes) -> Result<Json<serde_json::Value>, ApiErr> {
    let Who::Node(node) = auth(&s, "POST", "/v1/tasks/probe-result", &h, &body).await? else {
        return fail(StatusCode::FORBIDDEN, "node credentials required");
    };
    let req: ProbeResult = parse(&body)?;
    reconcile::record_probe(&s, node, &req).await?;
    Ok(Json(serde_json::json!({"ok": true})))
}

pub fn router(s: Arc<Shared>) -> Router {
    Router::new()
        .route("/v1/pools", post(create_pool).get(ops::list_pools))
        .route("/v1/pools/:id", patch(update_pool))
        .route("/v1/pools/:id/invites", post(create_invite))
        .route("/v1/pools/:id/members/:member/remove", post(remove_member))
        .route("/v1/accounts", post(register_account))
        .route("/v1/accounts/me", get(account_status))
        .route("/v1/accounts/whoami", get(whoami))
        .route("/v1/nodes", post(register_node))
        .route("/v1/nodes/:id", patch(update_node))
        .route("/v1/nodes/:id/heartbeat", post(heartbeat))
        .route("/v1/segments/begin", post(begin_segment))
        .route("/v1/segments/:id/commit", post(commit_segment))
        .route("/v1/segments/:id/abort", post(abort_segment))
        .route("/v1/segments/:id/read", post(read_segment))
        .route("/v1/receipts/read", post(read_receipts))
        .route("/v1/objects/:tpe", get(list_objects))
        .route("/v1/objects/:tpe/:id", get(get_object))
        .route("/v1/objects/:tpe/:id/delete", post(delete_object))
        .route("/v1/tasks/repair-done", post(repair_done))
        .route("/v1/tasks/probe-result", post(probe_result))
        .merge(ops::routes())
        .route_layer(axum::middleware::from_fn_with_state(s.clone(), ops::track))
        .with_state(s)
}
