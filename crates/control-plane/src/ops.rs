//! Operations: health checks, Prometheus metrics, join pages, the operator API and the
//! operator dashboard. None of this is on the data path.

use crate::api::{fail, ApiErr};
use crate::db::{self, int};
use crate::*;
use axum::{
    extract::{ConnectInfo, MatchedPath, Path, Query, Request, State},
    http::{header, HeaderMap, StatusCode},
    middleware::Next,
    response::{Html, IntoResponse, Response},
    Json,
};
use prometheus_client::{
    encoding::EncodeLabelSet,
    metrics::{
        counter::Counter,
        family::Family,
        gauge::Gauge,
        histogram::{exponential_buckets, Histogram},
    },
    registry::Registry,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    net::{IpAddr, SocketAddr},
    sync::atomic::AtomicU64,
    time::Instant,
};

type S = State<Arc<Shared>>;

/// The user install script, served so a join link works without knowing where releases live.
const INSTALL_SH: &str = include_str!("../../../install.sh");
const ADMIN_HTML: &str = include_str!("admin.html");

// ---------------------------------------------------------------- metrics

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct HttpLabels {
    pub route: String,
    pub method: String,
    pub status: u16,
}
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct RouteLabels {
    pub route: String,
}
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct StateLabels {
    pub state: String,
}
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct ResultLabels {
    pub result: String,
}

type FGauge = Gauge<f64, AtomicU64>;

pub struct Metrics {
    registry: Registry,
    pub http: Family<HttpLabels, Counter>,
    pub http_seconds: Family<RouteLabels, Histogram, fn() -> Histogram>,
    pub audits: Family<ResultLabels, Counter>,
    pub repairs_completed: Counter,
    pub grants_issued: Counter,
    accounts: Family<StateLabels, FGauge>,
    nodes: Family<StateLabels, FGauge>,
    segments: Family<StateLabels, FGauge>,
    nodes_online: FGauge,
    pledged: FGauge,
    stored: FGauge,
    degraded: FGauge,
    below_repair: FGauge,
    unreadable: FGauge,
    repairs_in_flight: FGauge,
    pending_deletes: FGauge,
    up: FGauge,
}

impl Default for Metrics {
    fn default() -> Self {
        let mut r = Registry::with_prefix("globalobject");
        let m = Metrics {
            http: Family::default(),
            http_seconds: Family::new_with_constructor(|| Histogram::new(exponential_buckets(0.002, 2.0, 14))),
            audits: Family::default(),
            repairs_completed: Counter::default(),
            grants_issued: Counter::default(),
            accounts: Family::default(),
            nodes: Family::default(),
            segments: Family::default(),
            nodes_online: FGauge::default(),
            pledged: FGauge::default(),
            stored: FGauge::default(),
            degraded: FGauge::default(),
            below_repair: FGauge::default(),
            unreadable: FGauge::default(),
            repairs_in_flight: FGauge::default(),
            pending_deletes: FGauge::default(),
            up: FGauge::default(),
            registry: Registry::default(),
        };
        r.register("http_requests", "HTTP requests by route, method and status", m.http.clone());
        r.register("http_request_seconds", "HTTP request latency by route", m.http_seconds.clone());
        r.register("audits", "Host audits by result (pass, fail)", m.audits.clone());
        r.register("repairs_completed", "Pieces moved to a new host by repair or drain", m.repairs_completed.clone());
        r.register("grants_issued", "Signed grants issued to clients", m.grants_issued.clone());
        r.register("transaction_retries", "Database transactions retried after a conflict", db::TXN_RETRIES.clone());
        r.register("accounts", "Accounts by state", m.accounts.clone());
        r.register("nodes", "Nodes by state", m.nodes.clone());
        r.register("segments", "Segments by state", m.segments.clone());
        r.register("nodes_online", "Active nodes seen within the grace period", m.nodes_online.clone());
        r.register("pledged_bytes", "Space pledged by active nodes", m.pledged.clone());
        r.register("stored_bytes", "Bytes of stored pieces", m.stored.clone());
        r.register("segments_degraded", "Committed segments with fewer than n healthy pieces", m.degraded.clone());
        r.register("segments_below_repair", "Committed segments below their pool's repair threshold", m.below_repair.clone());
        r.register("segments_unreadable", "Committed segments with fewer than k healthy pieces right now", m.unreadable.clone());
        r.register("repairs_in_flight", "Piece repairs started and not yet finished", m.repairs_in_flight.clone());
        r.register("pending_deletes", "Pieces queued for deletion on hosts", m.pending_deletes.clone());
        r.register("summary_up", "1 if the last summary query succeeded", m.up.clone());
        Metrics { registry: r, ..m }
    }
}

impl Metrics {
    fn set(&self, s: &Summary) {
        for (fam, map) in [(&self.accounts, &s.accounts), (&self.nodes, &s.nodes), (&self.segments, &s.segments)] {
            fam.clear();
            for (k, v) in map {
                fam.get_or_create(&StateLabels { state: k.clone() }).set(*v as f64);
            }
        }
        self.nodes_online.set(s.nodes_online as f64);
        self.pledged.set(s.pledged_bytes as f64);
        self.stored.set(s.stored_bytes as f64);
        self.degraded.set(s.segments_degraded as f64);
        self.below_repair.set(s.segments_below_repair as f64);
        self.unreadable.set(s.segments_unreadable as f64);
        self.repairs_in_flight.set(s.repairs_in_flight as f64);
        self.pending_deletes.set(s.pending_deletes as f64);
    }
}

/// Counts requests and their latency by matched route, so ids in paths don't explode labels.
pub async fn track(State(s): S, req: Request, next: Next) -> Response {
    let route = req.extensions().get::<MatchedPath>().map_or("unmatched", |p| p.as_str()).to_string();
    let method = req.method().to_string();
    let start = Instant::now();
    let resp = next.run(req).await;
    s.metrics.http_seconds.get_or_create(&RouteLabels { route: route.clone() }).observe(start.elapsed().as_secs_f64());
    s.metrics.http.get_or_create(&HttpLabels { route, method, status: resp.status().as_u16() }).inc();
    resp
}

/// Refreshes the summary gauges. Scrapes then cost nothing, however often they come.
pub fn spawn(s: Arc<Shared>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(s.cfg.metrics_every);
        loop {
            tick.tick().await;
            match summary(&s).await {
                Ok(sum) => {
                    s.metrics.set(&sum);
                    s.metrics.up.set(1.0);
                }
                Err(e) => {
                    tracing::warn!("summary for metrics failed: {e}");
                    s.metrics.up.set(0.0);
                }
            }
        }
    })
}

async fn metrics(State(s): S) -> Response {
    let mut out = String::new();
    if prometheus_client::encoding::text::encode(&mut out, &s.metrics.registry).is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    ([(header::CONTENT_TYPE, "application/openmetrics-text; version=1.0.0; charset=utf-8")], out).into_response()
}

// ---------------------------------------------------------------- health

async fn healthz() -> &'static str {
    "ok\n"
}

/// Ready when MongoDB answers and has a writable primary: without one, every write would fail.
async fn readyz(State(s): S) -> Response {
    match s.db.db.run_command(mongodb::bson::doc! {"hello": 1}).await {
        Ok(d) if d.get_bool("isWritablePrimary").unwrap_or(false) => "ready\n".into_response(),
        Ok(_) => (StatusCode::SERVICE_UNAVAILABLE, "database has no writable primary\n").into_response(),
        Err(e) => (StatusCode::SERVICE_UNAVAILABLE, format!("database unreachable: {e}\n")).into_response(),
    }
}

// ---------------------------------------------------------------- summary

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Summary {
    pub pools: i64,
    pub accounts: BTreeMap<String, i64>,
    pub nodes: BTreeMap<String, i64>,
    pub nodes_online: i64,
    pub pledged_bytes: i64,
    pub stored_bytes: i64,
    pub segments: BTreeMap<String, i64>,
    /// Fewer than `n` healthy pieces: redundancy is reduced.
    pub segments_degraded: i64,
    /// Below the pool's repair threshold: repair is due.
    pub segments_below_repair: i64,
    /// Fewer than `k` healthy pieces: unreadable until hosts return or repair finishes.
    pub segments_unreadable: i64,
    pub repairs_in_flight: i64,
    pub pending_deletes: i64,
}

async fn counts_by<T: Send + Sync>(coll: &mongodb::Collection<T>, field: &str) -> DbResult<BTreeMap<String, i64>> {
    let rows = db::aggregate(coll, vec![doc! {"$group": {"_id": format!("${field}"), "n": {"$sum": 1}}}], None).await?;
    Ok(rows.into_iter().map(|d| (d.get_str("_id").unwrap_or("none").to_string(), int(d.get("n")))).collect())
}

pub async fn summary(s: &Shared) -> DbResult<Summary> {
    let cutoff = fnow() - s.cfg.grace_secs;
    let mut sum = Summary {
        pools: s.db.pools.count_documents(doc! {}).await? as i64,
        accounts: counts_by(&s.db.accounts, "state").await?,
        nodes: counts_by(&s.db.nodes, "state").await?,
        segments: counts_by(&s.db.segments, "state").await?,
        pending_deletes: s.db.pending_deletes.count_documents(doc! {}).await? as i64,
        ..Default::default()
    };
    let online = db::aggregate(
        &s.db.nodes,
        vec![
            doc! {"$match": {"state": "active", "last_seen": {"$gte": cutoff}}},
            doc! {"$group": {"_id": null, "n": {"$sum": 1}, "pledged": {"$sum": "$pledged"}}},
        ],
        None,
    )
    .await?;
    if let Some(d) = online.first() {
        sum.nodes_online = int(d.get("n"));
        sum.pledged_bytes = int(d.get("pledged"));
    }
    let stored = db::aggregate(
        &s.db.segments,
        vec![
            doc! {"$unwind": "$pieces"},
            doc! {"$facet": {
                "stored": [{"$match": {"pieces.state": "stored"}}, {"$group": {"_id": null, "b": {"$sum": "$piece_len"}}}],
                "repairs": [{"$match": {"pieces.repair": {"$ne": null}}}, {"$count": "n"}],
            }},
        ],
        None,
    )
    .await?;
    if let Some(d) = stored.first() {
        let first = |k: &str| d.get_array(k).ok().and_then(|a| a.first()).and_then(|b| b.as_document()).cloned();
        sum.stored_bytes = first("stored").map_or(0, |x| int(x.get("b")));
        sum.repairs_in_flight = first("repairs").map_or(0, |x| int(x.get("n")));
    }
    // Health per segment, against the nodes currently counted healthy. Same rule as repair.
    for pool in db::find_all(&s.db.pools, doc! {}).await? {
        let ok: Vec<i64> =
            s.db.nodes.distinct("_id", doc! {"pool": pool.id, "state": "active", "last_seen": {"$gte": cutoff}}).await?.iter().map(|b| int(Some(b))).collect();
        let healthy = doc! {"$size": {"$filter": {"input": "$pieces", "as": "p", "cond": {"$and": [
        {"$eq": ["$$p.state", "stored"]}, {"$in": ["$$p.host", &ok]}]}}}};
        let rows = db::aggregate(
            &s.db.segments,
            vec![
                doc! {"$match": {"pool": pool.id, "state": "committed"}},
                doc! {"$project": {"h": healthy, "k": 1, "n": 1}},
                doc! {"$group": {
                    "_id": null,
                    "degraded": {"$sum": {"$cond": [{"$lt": ["$h", "$n"]}, 1, 0]}},
                    "below": {"$sum": {"$cond": [{"$lt": ["$h", {"$min": [pool.policy.repair_at as i32, "$n"]}]}, 1, 0]}},
                    "unreadable": {"$sum": {"$cond": [{"$lt": ["$h", "$k"]}, 1, 0]}},
                }},
            ],
            None,
        )
        .await?;
        if let Some(d) = rows.first() {
            sum.segments_degraded += int(d.get("degraded"));
            sum.segments_below_repair += int(d.get("below"));
            sum.segments_unreadable += int(d.get("unreadable"));
        }
    }
    Ok(sum)
}

// ---------------------------------------------------------------- operator API

/// Constant-time comparison, so response timing doesn't leak the token.
pub fn is_operator(s: &Shared, h: &HeaderMap) -> bool {
    let given = h.get("x-globalobject-operator").and_then(|v| v.to_str().ok()).unwrap_or("");
    let want = s.cfg.operator_token.as_str();
    !want.is_empty() && globalobject_crypto::hash(given.as_bytes()) == globalobject_crypto::hash(want.as_bytes())
}

pub fn require_operator(s: &Shared, h: &HeaderMap) -> Result<(), ApiErr> {
    if is_operator(s, h) {
        Ok(())
    } else {
        fail(StatusCode::FORBIDDEN, "operator token required")
    }
}

async fn admin_summary(State(s): S, h: HeaderMap) -> Result<Json<Summary>, ApiErr> {
    require_operator(&s, &h)?;
    Ok(Json(summary(&s).await?))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolInfo {
    pub id: i64,
    pub name: String,
    pub private: bool,
    pub policy: globalobject_policy::Policy,
    pub admin: Option<i64>,
    pub members: i64,
    pub nodes_online: i64,
}

pub async fn list_pools(State(s): S, h: HeaderMap) -> Result<Json<Vec<PoolInfo>>, ApiErr> {
    require_operator(&s, &h)?;
    let cutoff = fnow() - s.cfg.grace_secs;
    let mut out = vec![];
    for p in db::find_all(&s.db.pools, doc! {}).await? {
        out.push(PoolInfo {
            members: s.db.accounts.count_documents(doc! {"pool": p.id, "state": {"$ne": "purged"}}).await? as i64,
            nodes_online: s.db.nodes.count_documents(doc! {"pool": p.id, "state": "active", "last_seen": {"$gte": cutoff}}).await? as i64,
            id: p.id,
            name: p.name,
            private: p.private,
            policy: p.policy,
            admin: p.admin,
        });
    }
    Ok(Json(out))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeInfo {
    pub id: i64,
    pub pool: i64,
    pub account: i64,
    pub state: String,
    pub site: String,
    pub pledged: i64,
    pub held: i64,
    pub last_seen_secs_ago: f64,
    pub audit_ok: i64,
    pub audit_fail: i64,
    pub measured_bps: Option<f64>,
}

#[derive(Deserialize)]
struct NodesQuery {
    pool: Option<i64>,
}

async fn list_nodes(State(s): S, h: HeaderMap, Query(q): Query<NodesQuery>) -> Result<Json<Vec<NodeInfo>>, ApiErr> {
    require_operator(&s, &h)?;
    let filter = match q.pool {
        Some(p) => doc! {"pool": p},
        None => doc! {},
    };
    let nodes = db::find_all(&s.db.nodes, filter).await?;
    let ids: Vec<i64> = nodes.iter().map(|n| n.id).collect();
    let held = db::bytes_by(&s.db, "pieces.host", &ids, doc! {"state": "stored"}, None).await?;
    let now = fnow();
    Ok(Json(
        nodes
            .into_iter()
            .map(|n| NodeInfo {
                held: held.get(&n.id).copied().unwrap_or(0),
                id: n.id,
                pool: n.pool,
                account: n.account,
                state: n.state,
                site: n.site,
                pledged: n.pledged,
                last_seen_secs_ago: (now - n.last_seen).max(0.0),
                audit_ok: n.audit_ok,
                audit_fail: n.audit_fail,
                measured_bps: n.measured_bps,
            })
            .collect(),
    ))
}

// ---------------------------------------------------------------- join links

/// The externally visible base URL: `--public-url` if set, otherwise from the request.
pub fn base_url(s: &Shared, h: &HeaderMap) -> Option<String> {
    if let Some(u) = &s.cfg.public_url {
        return Some(u.trim_end_matches('/').to_string());
    }
    let host = h.get(header::HOST)?.to_str().ok()?;
    Some(format!("http://{host}"))
}

pub fn join_url(s: &Shared, h: &HeaderMap, token: &str) -> Option<String> {
    base_url(s, h).map(|b| format!("{b}/join/{token}"))
}

async fn join_page(State(s): S, h: HeaderMap, Path(token): Path<String>) -> Result<Response, ApiErr> {
    // Tokens are hex. Checking that also keeps anything odd out of the page.
    if token.is_empty() || token.len() > 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok((StatusCode::NOT_FOUND, Html(page("Invite not found", "<p>This link isn't a valid invite.</p>"))).into_response());
    }
    let inv = s.db.invites.find_one(doc! {"_id": &token}).await?;
    let Some(inv) = inv else {
        return Ok((StatusCode::NOT_FOUND, Html(page("Invite not found", "<p>This invite doesn't exist. Ask for a new one.</p>"))).into_response());
    };
    if inv.used_by.is_some() {
        return Ok((StatusCode::GONE, Html(page("Invite already used", "<p>Each invite works once. Ask for a new one.</p>"))).into_response());
    }
    let base = base_url(&s, &h).unwrap_or_default();
    let cmd = format!("curl -fsSL {base}/install.sh | sh -s -- --join {base}/join/{token}");
    let body = format!(
        "<p>You've been invited to a GlobalObject pool: encrypted backup, stored on other members' machines in \
         exchange for some of your spare disk.</p>\
         <p>On Linux or macOS, run this in a terminal:</p><pre id=cmd>{}</pre>\
         <button onclick=\"navigator.clipboard.writeText(document.getElementById('cmd').innerText)\">Copy</button>\
         <p>It installs the <code>globalobject</code> program, then asks a few questions: how much disk to share \
         and which folders to back up. It gives you a recovery phrase. <strong>Write it down: without it nobody, \
         including the operator, can recover your data.</strong></p>",
        escape(&cmd)
    );
    Ok(Html(page("Join GlobalObject", &body)).into_response())
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn page(title: &str, body: &str) -> String {
    format!(
        "<!doctype html><html lang=en><meta charset=utf-8><meta name=viewport content='width=device-width,initial-scale=1'>\
         <title>{title}</title><style>body{{font:16px/1.5 system-ui,sans-serif;max-width:42rem;margin:3rem auto;padding:0 1rem;\
         color:#1b1f24;background:#fff}}pre{{background:#f3f4f6;padding:1rem;overflow-x:auto;white-space:pre-wrap;word-break:break-all}}\
         button{{font:inherit;padding:.4rem 1rem}}@media(prefers-color-scheme:dark){{body{{color:#e6e6e6;background:#16181c}}\
         pre{{background:#24272d}}}}</style><h1>{title}</h1>{body}</html>"
    )
}

async fn install_sh() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/x-shellscript; charset=utf-8")], INSTALL_SH)
}

async fn admin_page() -> Html<&'static str> {
    Html(ADMIN_HTML)
}

// ---------------------------------------------------------------- client addresses

/// A network in CIDR form, for `--trusted-proxy`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cidr {
    addr: IpAddr,
    bits: u8,
}

impl std::str::FromStr for Cidr {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        let (a, b) = s.split_once('/').unwrap_or((s, ""));
        let addr: IpAddr = a.parse().map_err(|_| format!("bad address in {s}"))?;
        let max = if addr.is_ipv4() { 32 } else { 128 };
        let bits = if b.is_empty() { max } else { b.parse().map_err(|_| format!("bad prefix in {s}"))? };
        if bits > max {
            return Err(format!("prefix too long in {s}"));
        }
        Ok(Cidr { addr, bits })
    }
}

impl Cidr {
    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self.addr, ip) {
            (IpAddr::V4(a), IpAddr::V4(b)) => prefix(&a.octets(), &b.octets(), self.bits),
            (IpAddr::V6(a), IpAddr::V6(b)) => prefix(&a.octets(), &b.octets(), self.bits),
            _ => false,
        }
    }
}

fn prefix(a: &[u8], b: &[u8], bits: u8) -> bool {
    let (full, rest) = ((bits / 8) as usize, bits % 8);
    a[..full] == b[..full] && (rest == 0 || (a[full] ^ b[full]) >> (8 - rest) == 0)
}

/// The client's address. `X-Forwarded-For` counts only when the peer is a trusted proxy, and
/// then its rightmost untrusted entry is used, since a client can prepend anything it likes.
pub fn client_ip(s: &Shared, peer: SocketAddr, h: &HeaderMap) -> IpAddr {
    resolve_client(&s.cfg.trusted_proxies, peer, h)
}

fn resolve_client(proxies: &[Cidr], peer: SocketAddr, h: &HeaderMap) -> IpAddr {
    let peer = peer.ip().to_canonical();
    let trusted = |ip: IpAddr| proxies.iter().any(|c| c.contains(ip));
    if !trusted(peer) {
        return peer;
    }
    let chain: Vec<IpAddr> = h
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .filter_map(|x| x.trim().parse::<IpAddr>().ok())
        .map(|ip| ip.to_canonical())
        .collect();
    chain.into_iter().rev().find(|ip| !trusted(*ip)).unwrap_or(peer)
}

/// The failure-domain site for an address: its IPv4 /24 or IPv6 /48. Hosts behind one home
/// connection, or one small provider block, share a site.
pub fn site_of(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v) => {
            let o = v.octets();
            format!("{}.{}.{}.0/24", o[0], o[1], o[2])
        }
        IpAddr::V6(v) => {
            let s = v.segments();
            format!("{:x}:{:x}:{:x}::/48", s[0], s[1], s[2])
        }
    }
}

pub fn derived_site(s: &Shared, peer: Option<ConnectInfo<SocketAddr>>, h: &HeaderMap) -> String {
    match peer {
        Some(ConnectInfo(p)) => site_of(client_ip(s, p, h)),
        None => "unknown".into(),
    }
}

pub fn routes() -> axum::Router<Arc<Shared>> {
    use axum::routing::get;
    axum::Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/metrics", get(metrics))
        .route("/install.sh", get(install_sh))
        .route("/join/:token", get(join_page))
        .route("/admin", get(admin_page))
        .route("/v1/admin/summary", get(admin_summary))
        .route("/v1/admin/nodes", get(list_nodes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cidr_matching() {
        let c: Cidr = "10.1.0.0/16".parse().unwrap();
        assert!(c.contains("10.1.200.3".parse().unwrap()));
        assert!(!c.contains("10.2.0.1".parse().unwrap()));
        let c: Cidr = "172.18.0.0/12".parse().unwrap();
        assert!(c.contains("172.31.255.255".parse().unwrap()));
        assert!(!c.contains("172.32.0.0".parse().unwrap()));
        let c: Cidr = "fd00::/8".parse().unwrap();
        assert!(c.contains("fd12::1".parse().unwrap()));
        assert!(!c.contains("10.0.0.1".parse().unwrap()));
        let one: Cidr = "127.0.0.1".parse().unwrap();
        assert!(one.contains("127.0.0.1".parse().unwrap()) && !one.contains("127.0.0.2".parse().unwrap()));
        assert!("10.0.0.0/33".parse::<Cidr>().is_err());
    }

    #[test]
    fn forwarded_for_only_from_trusted_proxies() {
        let proxies: Vec<Cidr> = vec!["172.16.0.0/12".parse().unwrap()];
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", "6.6.6.6, 203.0.113.5".parse().unwrap());
        let proxy: SocketAddr = "172.18.0.4:5000".parse().unwrap();
        let direct: SocketAddr = "198.51.100.7:5000".parse().unwrap();
        // Through the proxy: the rightmost address it didn't add itself. The spoofed
        // leftmost entry is ignored.
        assert_eq!(resolve_client(&proxies, proxy, &h), "203.0.113.5".parse::<IpAddr>().unwrap());
        // Straight from a client, the header is ignored.
        assert_eq!(resolve_client(&proxies, direct, &h), direct.ip());
        // No proxies configured: never trusted.
        assert_eq!(resolve_client(&[], proxy, &h), proxy.ip());
    }

    #[test]
    fn sites() {
        assert_eq!(site_of("203.0.113.77".parse().unwrap()), "203.0.113.0/24");
        assert_eq!(site_of("2001:db8:abcd:12::1".parse().unwrap()), "2001:db8:abcd::/48");
        // An IPv4 client reaching a dual-stack listener shows up mapped into IPv6.
        assert_eq!(site_of("::ffff:198.51.100.9".parse::<IpAddr>().unwrap().to_canonical()), "198.51.100.0/24");
    }
}
