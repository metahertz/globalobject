//! End-to-end checks, numbered to match the plan's verification list.

use globalobject_client::ClientError;
use globalobject_crypto::Identity;
use globalobject_e2e::*;
use globalobject_net::{fetch_verified, peer_call, NetMode};
use globalobject_policy::{DomainLevel, Policy, QuotaParams};
use globalobject_proto::*;
use std::{
    collections::HashSet,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

fn p3() -> Policy {
    Policy::replicate3()
}
fn ec48() -> Policy {
    Policy { k: 4, n: 8, repair_at: 7, upload_extra: 0, min_failure_domains: 8, domain_level: DomainLevel::Owner, local_groups: 0 }
}
fn plain() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}
fn is<F: Fn(&ClientError) -> bool>(e: &anyhow::Error, f: F) -> bool {
    e.downcast_ref::<ClientError>().is_some_and(f)
}

// 1 ------------------------------------------------------------------ encryption

// One grid per test: a grid's nodes and loops run until its test's runtime ends.
async fn no_plaintext(policy: Policy) {
    {
        let g = Grid::start(fast_config()).await.unwrap();
        let (_, m) = g.pool_with_members(10, policy, generous_quota(), false).await.unwrap();
        let canary = b"CANARY-7f3a9c1e-this-text-must-never-be-stored-in-the-clear";
        let name = "secret-file-name-canary.txt";
        let mut body = noise(300_000, 1);
        for i in 0..20 {
            body[i * 15_000..i * 15_000 + canary.len()].copy_from_slice(canary);
        }
        let (snap, src) = put(&m[0].client, name, &body).await.unwrap();
        assert_eq!(get(&m[0].client, &snap, &src, name).await.unwrap(), body);
        let hits = scan_for(g.tmp.path(), &canary[..24]);
        assert!(hits.is_empty(), "canary on disk: {hits:?}");
        let hits = scan_for(g.tmp.path(), name.as_bytes());
        assert!(hits.is_empty(), "file name on disk: {hits:?}");
        let leak = q::objects(g.db(), mongodb::bson::doc! {}).await.iter().filter(|o| format!("{}{}", o.tpe, o.object).contains("canary")).count();
        assert_eq!(leak, 0, "file name in the control plane");
        // Sanity: the scanner can actually find things.
        std::fs::write(g.tmp.path().join("probe"), canary).unwrap();
        assert_eq!(scan_for(g.tmp.path(), &canary[..24]).len(), 1);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn v01_no_plaintext_with_replicas() {
    no_plaintext(p3()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn v01_no_plaintext_with_erasure_coding() {
    no_plaintext(ec48()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn v01_others_cannot_read_an_account() {
    let g = Grid::start(fast_config()).await.unwrap();
    let (_, m) = g.pool_with_members(4, p3(), generous_quota(), false).await.unwrap();
    put(&m[0].client, "f", &noise(1000, 2)).await.unwrap();
    let seg = g.pieces_of(m[0].client.id()).await.unwrap()[0].0.clone();
    let raw = serde_json::to_vec(&ReadRequest { want: 1, exclude_pieces: vec![], endpoint: m[1].client.endpoint_id(), range: None }).unwrap();
    let r = m[1].client.call("POST", &format!("/v1/segments/{seg}/read"), Some(&raw)).await.unwrap();
    assert_eq!(r.status(), 403);
    assert!(m[1].client.snapshots().await.unwrap().is_empty(), "catalogues are per account");
}

// 2 ------------------------------------------------------------------ round trip

async fn round_trip(policy: Policy) {
    {
        let g = Grid::start(fast_config()).await.unwrap();
        let (_, m) = g.pool_with_members(10, policy, generous_quota(), false).await.unwrap();
        let c = &m[0].client;
        let sizes = [0usize, 1, 1000, MB as usize - 1, MB as usize, 5 * MB as usize, 90 * MB as usize];
        let bodies: Vec<Vec<u8>> = sizes.iter().enumerate().map(|(i, &s)| noise(s, i as u8 + 10)).collect();
        let names: Vec<String> = (0..sizes.len()).map(|i| format!("f{i}")).collect();
        let files: Vec<(&str, &[u8])> = names.iter().map(String::as_str).zip(bodies.iter().map(Vec::as_slice)).collect();
        let src = Source::new(&files);
        let t = Instant::now();
        let stats = c.backup(src.dir.path()).await.unwrap();
        println!("{policy:?}: backup {:?}", t.elapsed());
        assert_eq!(stats.files, sizes.len() as u64);
        for (name, body) in &files {
            assert!(c.read_file(&stats.snapshot, &src.file(name)).await.unwrap() == *body, "{name} under {policy:?}");
        }
        // And a complete restore, through rustic's own restore path.
        let dest = tempfile::tempdir().unwrap();
        let t = Instant::now();
        c.restore(&stats.snapshot, dest.path()).await.unwrap();
        println!("{policy:?}: restore {:?}", t.elapsed());
        let root = dest.path().join(src.dir.path().strip_prefix("/").unwrap());
        for (name, body) in &files {
            assert!(std::fs::read(root.join(name)).unwrap() == *body, "restored {name}");
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn v02_files_round_trip_with_replicas() {
    round_trip(p3()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn v02_files_round_trip_with_erasure_coding() {
    round_trip(ec48()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn v02_repeated_content_dedupes_within_an_account() {
    let g = Grid::start(fast_config()).await.unwrap();
    let (_, m) = g.pool_with_members(4, p3(), generous_quota(), false).await.unwrap();
    let c = &m[0].client;
    let body = noise(20 * MB as usize, 3);
    let src = Source::new(&[("a", &body)]);
    let first = c.backup(src.dir.path()).await.unwrap();
    let used = c.status().await.unwrap().used_raw;
    std::fs::write(src.file("b"), &body).unwrap();
    let second = c.backup(src.dir.path()).await.unwrap();
    assert!(first.bytes_added as usize >= body.len() / 2);
    assert!(second.bytes_added < MB, "identical data added {} bytes", second.bytes_added);
    assert!(c.status().await.unwrap().used_raw < used + 3 * MB);
    assert_eq!(c.snapshots().await.unwrap().len(), 2);
    assert!(c.read_file(&second.snapshot, &src.file("b")).await.unwrap() == body);
}

// 3 ------------------------------------------------------------- failure domains

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn v03_many_disks_one_owner_counts_once() {
    let g = Grid::start(fast_config()).await.unwrap();
    let p = g.pool(false, p3(), generous_quota(), false).await.unwrap();
    let reader = g.member(&p.admin_invite, Spec::default()).await.unwrap();
    let inv = reader.client.invite().await.unwrap();
    let _big = g.member(&inv, Spec { disks: 5, ..Spec::default() }).await.unwrap();
    let inv = reader.client.invite().await.unwrap();
    let _second = g.member(&inv, Spec::default()).await.unwrap();

    // Eligible for `reader`: `_big` (5 disks, 1 owner) and `_second`. That's 2 domains, not 6.
    let e = put(&reader.client, "f", &[1u8; 1000]).await.err().unwrap();
    let msg = format!("{e:#}");
    assert!(is(&e, |x| matches!(x, ClientError::TooFewDomains(_))), "{msg}");
    assert!(msg.contains("2 eligible"), "{msg}");

    // A third owner makes it possible, and no owner holds two pieces.
    let inv = reader.client.invite().await.unwrap();
    let _third = g.member(&inv, Spec::default()).await.unwrap();
    put(&reader.client, "f", &[1u8; 1000]).await.unwrap();
    let account_of: std::collections::HashMap<i64, i64> = q::nodes(g.db()).await.iter().map(|n| (n.id, n.account)).collect();
    let mut per_segment: std::collections::HashMap<String, Vec<i64>> = Default::default();
    for seg in q::segments(g.db(), reader.client.id()).await {
        for p in &seg.pieces {
            per_segment.entry(seg.id.clone()).or_default().push(account_of[&p.host]);
        }
    }
    assert!(!per_segment.is_empty());
    for (seg, owners) in per_segment {
        assert_eq!(owners.iter().collect::<HashSet<_>>().len(), 3, "{seg}: {owners:?}");
        assert!(!owners.contains(&reader.client.id()), "owner holds their own piece");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn v03_distinct_sites_rule_limits_a_single_site_operator() {
    let g = Grid::start(fast_config()).await.unwrap();
    let p = g.pool(false, p3(), generous_quota(), true).await.unwrap();
    let reader = g.member(&p.admin_invite, Spec::default()).await.unwrap();
    let mut keep = vec![];
    for _ in 0..3 {
        let inv = reader.client.invite().await.unwrap();
        keep.push(g.member(&inv, Spec { site: Some("sybil-basement".into()), ..Spec::default() }).await.unwrap());
    }
    let inv = reader.client.invite().await.unwrap();
    keep.push(g.member(&inv, Spec::default()).await.unwrap());
    // Reader excluded, so that's two sites where three are needed.
    assert!(put(&reader.client, "f", &[1u8; 1000]).await.is_err());
    let inv = reader.client.invite().await.unwrap();
    keep.push(g.member(&inv, Spec::default()).await.unwrap());
    put(&reader.client, "f", &[1u8; 1000]).await.unwrap();
    let sybil: HashSet<i64> = q::nodes(g.db()).await.iter().filter(|n| n.site == "sybil-basement").map(|n| n.id).collect();
    let worst = q::segments(g.db(), reader.client.id()).await.iter().map(|s| s.pieces.iter().filter(|p| sybil.contains(&p.host)).count()).max().unwrap_or(0);
    assert_eq!(worst, 1, "one site must hold at most one piece of any segment");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn v03_policy_below_the_floor_is_refused() {
    let g = Grid::start(fast_config()).await.unwrap();
    let bad = Policy { k: 1, n: 2, repair_at: 2, upload_extra: 0, min_failure_domains: 2, domain_level: DomainLevel::Owner, local_groups: 0 };
    assert!(g.pool(false, bad, generous_quota(), false).await.is_err());
    let stall = QuotaParams { reserve: 0.2, headroom: 1.0, probation_allowance: GB };
    let e = g.pool(false, p3(), stall, false).await.unwrap_err();
    assert!(format!("{e}").contains("headroom"), "{e}");
}

// 4 --------------------------------------------------------------- quota bypass

fn begin_body(c: &globalobject_client::Client, len: u64) -> Vec<u8> {
    let hash = "a".repeat(64);
    serde_json::to_vec(&BeginSegment {
        segment: globalobject_crypto::random_hex(16),
        object: ObjectRef { tpe: "pack".into(), id: globalobject_crypto::random_hex(32) },
        len,
        k: 1,
        n: 3,
        local_groups: 0,
        hashes: vec![hash; 3],
        endpoint: c.endpoint_id(),
        avoid: vec![],
    })
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn v04_quota_is_enforced_by_the_control_plane() {
    let g = Grid::start(fast_config()).await.unwrap();
    // 40 MiB credit × 0.8 = 32 MiB raw, or about 10 MiB at 3x.
    let q = QuotaParams { reserve: 0.2, headroom: 1.5, probation_allowance: 40 * MB };
    let (_, m) = g.pool_with_members(4, p3(), q, false).await.unwrap();
    let c = &m[0].client;
    let st = c.status().await.unwrap();
    assert_eq!(st.quota_raw, 32 * MB);
    assert_eq!(st.used_raw, 0);

    let (snap, src) = put(c, "ok", &noise(MB as usize, 4)).await.unwrap();
    let e = put(c, "too-big", &noise(20 * MB as usize, 5)).await.err().unwrap();
    assert!(is(&e, |x| matches!(x, ClientError::Quota(_))), "{e:#}");

    // Skip the client library entirely: ask for far more than is allowed.
    let r = c.call("POST", "/v1/segments/begin", Some(&begin_body(c, 60 * MB))).await.unwrap();
    assert_eq!(r.status(), 402);

    // Usage comes from committed records, so it can't be understated, and it counts redundancy.
    let st = c.status().await.unwrap();
    assert!(st.used_raw >= 3 * MB, "{}", st.used_raw);
    assert!(st.used_raw <= st.quota_raw, "{} {}", st.used_raw, st.quota_raw);
    assert!(get(c, &snap, &src, "ok").await.is_ok(), "earlier data still readable");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn v04_parallel_uploads_cannot_race_past_quota() {
    let g = Grid::start(fast_config()).await.unwrap();
    let q = QuotaParams { reserve: 0.2, headroom: 1.5, probation_allowance: 40 * MB };
    let (_, m) = g.pool_with_members(4, p3(), q, false).await.unwrap();
    let c = &m[0].client;
    let mut jobs = tokio::task::JoinSet::new();
    for _ in 0..20 {
        let (c, raw) = (c.clone(), begin_body(c, 3 * MB));
        jobs.spawn(async move { c.call("POST", "/v1/segments/begin", Some(&raw)).await.unwrap().status().as_u16() });
    }
    let results: Vec<u16> = jobs.join_all().await;
    // 32 MiB raw / 9 MiB per segment = 3 fit.
    assert_eq!(results.iter().filter(|&&s| s == 200).count(), 3, "{results:?}");
    assert!(results.iter().all(|&s| s == 200 || s == 402), "a request failed rather than being refused: {results:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn v04_hosts_refuse_bad_grants() {
    let g = Grid::start(fast_config()).await.unwrap();
    let (p, m) = g.pool_with_members(4, p3(), generous_quota(), false).await.unwrap();
    let node = m[1].node();
    let addr = node.addr();
    let me = globalobject_net::bind(NetMode::Local, vec![]).await.unwrap();
    let real = Identity::from_seed(&SEED);
    let attacker = Identity::generate();
    let hash = "a".repeat(64);
    let base = Grant {
        nonce: globalobject_crypto::random_hex(16),
        account: m[0].client.id(),
        pool: p.pool,
        segment: globalobject_crypto::random_hex(16),
        piece: 0,
        host: node.id,
        peer: me.id().to_string(),
        hash,
        op: Op::Read,
        max_bytes: MB,
        rate_bps: 0,
        expires_at: now() + 60,
        range: None,
    };
    let fresh = || Grant { nonce: globalobject_crypto::random_hex(16), ..base.clone() };
    let refused = node.stats.grants_refused.load(Ordering::Relaxed);
    let try_grant = |token: String| {
        let (me, addr) = (me.clone(), addr.clone());
        async move { peer_call(&me, &addr, &PeerRequest::Authorize { grant: token }).await.unwrap() }
    };
    let is_refused = |r: &PeerReply| matches!(r, PeerReply::Refused { .. });

    assert!(is_refused(&try_grant("not a token".into()).await), "garbage");
    assert!(is_refused(&try_grant(attacker.token(T_GRANT, &fresh())).await), "forged");
    assert!(is_refused(&try_grant(real.token(T_STORE, &fresh())).await), "right key, wrong purpose");
    assert!(is_refused(&try_grant(real.token(T_GRANT, &Grant { expires_at: now() - 5, ..fresh() })).await), "expired");
    assert!(is_refused(&try_grant(real.token(T_GRANT, &Grant { host: node.id + 999, ..fresh() })).await), "other host");
    assert!(is_refused(&try_grant(real.token(T_GRANT, &Grant { pool: p.pool + 999, ..fresh() })).await), "other pool");
    assert!(is_refused(&try_grant(real.token(T_GRANT, &Grant { peer: "someone-else".into(), ..fresh() })).await), "other peer");
    assert!(is_refused(&try_grant(real.token(T_GRANT, &Grant { op: Op::Write, ..fresh() })).await), "wrong operation");
    // A store grant used by someone it wasn't issued to.
    let store = peer_call(
        &me,
        &addr,
        &PeerRequest::Store { grant: real.token(T_GRANT, &Grant { op: Op::Write, peer: "someone-else".into(), ..fresh() }), from: "{}".into() },
    )
    .await
    .unwrap();
    assert!(is_refused(&store), "stolen store grant");
    assert_eq!(node.used_bytes(), 0, "nothing stored by any refused request");
    assert!(node.stats.grants_refused.load(Ordering::Relaxed) >= refused + 9);

    // A real grant works once, and only once.
    let good = real.token(T_GRANT, &fresh());
    assert!(matches!(try_grant(good.clone()).await, PeerReply::Ok), "valid");
    assert!(is_refused(&try_grant(good).await), "replay");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn v04_blobs_are_not_served_without_a_grant() {
    let g = Grid::start(fast_config()).await.unwrap();
    let (_, m) = g.pool_with_members(4, p3(), generous_quota(), false).await.unwrap();
    let c = &m[0].client;
    put(c, "f", &noise(2 * MB as usize, 6)).await.unwrap();
    let nodes: Vec<_> = m.iter().flat_map(|x| x.nodes.iter().cloned()).collect();
    let (seg, piece, host, hash) = g.pieces_of(c.id()).await.unwrap()[0].clone();
    let node = nodes.iter().find(|n| n.id == host).unwrap();
    let stranger = globalobject_net::bind(NetMode::Local, vec![]).await.unwrap();

    // Knowing the address and hash gets a stranger nothing.
    assert!(fetch_verified(&stranger, &node.addr(), &hash, 64 * MB).await.is_err(), "served without a grant");

    // A genuine grant for one peer doesn't help a different peer.
    let raw = serde_json::to_vec(&ReadRequest { want: 1, exclude_pieces: vec![], endpoint: stranger.id().to_string(), range: None }).unwrap();
    let plan: ReadPlan = c.call("POST", &format!("/v1/segments/{seg}/read"), Some(&raw)).await.unwrap().json().await.unwrap();
    let p = &plan.pieces[0];
    let bystander = globalobject_net::bind(NetMode::Local, vec![]).await.unwrap();
    assert!(matches!(peer_call(&bystander, &p.addr, &PeerRequest::Authorize { grant: p.grant.clone() }).await.unwrap(), PeerReply::Refused { .. }));

    // The peer it names can fetch that blob once, verified. A second fetch needs a new grant.
    assert!(matches!(peer_call(&stranger, &p.addr, &PeerRequest::Authorize { grant: p.grant.clone() }).await.unwrap(), PeerReply::Ok));
    assert_eq!(fetch_verified(&stranger, &p.addr, &p.hash, plan.piece_len).await.unwrap().len() as u64, plan.piece_len);
    assert!(fetch_verified(&stranger, &p.addr, &p.hash, plan.piece_len).await.is_err(), "grant was reusable");
    let _ = piece;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn v04_requests_must_be_signed_for_exactly_what_they_do() {
    let g = Grid::start(fast_config()).await.unwrap();
    let (_, m) = g.pool_with_members(4, p3(), generous_quota(), false).await.unwrap();
    let http = plain();
    let url = format!("{}/v1/accounts/me", g.url);
    let id = format!("a{}", m[0].client.id());
    let send = |token: String| {
        let (http, url, id) = (http.clone(), url.clone(), id.clone());
        async move { http.get(url).header(H_ID, id).header(H_AUTH, token).send().await.unwrap().status().as_u16() }
    };
    assert_eq!(http.get(&url).send().await.unwrap().status(), 401, "unsigned");
    let claims = RequestClaims::new(&id, "GET", "/v1/accounts/me", b"");
    let mine = m[0].client.signing();
    assert_eq!(send(mine.token(T_REQ, &claims)).await, 200, "valid");
    assert_eq!(send(Identity::generate().token(T_REQ, &claims)).await, 401, "someone else's key");
    assert_eq!(send(mine.token(T_REQ, &RequestClaims { ts: now() - 3600, ..claims.clone() })).await, 401, "old token replayed");
    assert_eq!(send(mine.token(T_REQ, &RequestClaims { path: "/v1/other".into(), ..claims.clone() })).await, 401, "token for another path");
    assert_eq!(send(mine.token(T_REQ, &RequestClaims { method: "POST".into(), ..claims.clone() })).await, 401, "token for another method");
    assert_eq!(send(mine.token(T_READ, &claims)).await, 401, "token for another purpose");
    let r = http
        .post(format!("{}/v1/pools", g.url))
        .json(&CreatePool { name: "x".into(), private: true, policy: p3(), quota: generous_quota(), distinct_sites: false })
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403, "pools need the operator token");
}

// 5 ------------------------------------------------------------------ bandwidth

/// The reader's own node can upload at 1 MiB/s, as measured by peers. Their aggregate
/// download across all hosts must stay near that, and not fall far below it either.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn v05_download_rate_follows_measured_upload() {
    let mut cfg = fast_config();
    cfg.probe_every = Duration::from_secs(2);
    let g = Grid::start(cfg).await.unwrap();
    let p = g.pool(false, p3(), generous_quota(), false).await.unwrap();
    let rate = MB;
    let reader = g.member(&p.admin_invite, Spec { link_up_bps: rate, ..Spec::default() }).await.unwrap();
    let mut others = vec![];
    for _ in 0..5 {
        let inv = reader.client.invite().await.unwrap();
        others.push(g.member(&inv, Spec::default()).await.unwrap());
    }
    let c = &reader.client;
    let body = noise(10 * MB as usize, 11);
    let (snap, src) = put(c, "f", &body).await.unwrap();

    let measured = eventually(Duration::from_secs(40), || async {
        let st = c.status().await.unwrap();
        st.entitled_bps > 0.7 * rate as f64 && st.entitled_bps < 1.3 * rate as f64
    })
    .await;
    let entitled = c.status().await.unwrap().entitled_bps;
    assert!(measured, "entitlement never settled: {entitled} bps, wanted about {rate}");

    let start = Instant::now();
    assert!(get(c, &snap, &src, "f").await.unwrap() == body);
    let t = start.elapsed().as_secs_f64();
    let total: i64 = q::segments(g.db(), c.id()).await.iter().map(|s| s.piece_len * s.k as i64).sum();
    let got = total as f64 / t;
    println!("read {total} bytes in {t:.1}s = {:.2} MiB/s, entitled {:.2} MiB/s", got / MB as f64, entitled / MB as f64);
    assert!(got < 1.5 * entitled, "aggregate {got} above entitlement {entitled}");
    assert!(got > 0.6 * entitled, "got {got} of {entitled} entitled");
    let peer = others[0].client.status().await.unwrap();
    assert!(peer.entitled_bps > 20.0 * entitled, "peer {} vs {entitled}", peer.entitled_bps);
}

/// Regression: new members were held at the unmeasured rate until the first scheduled probe.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn v05_new_members_are_measured_soon_after_joining() {
    let mut cfg = fast_config();
    cfg.unmeasured_bps = 1000.0;
    let g = Grid::start(cfg).await.unwrap();
    let (_, m) = g.pool_with_members(3, p3(), generous_quota(), false).await.unwrap();
    let c = &m[0].client;
    let ok = eventually(Duration::from_secs(15), || async { c.status().await.unwrap().entitled_bps > 1000.0 }).await;
    assert!(ok, "still at the unmeasured rate: {}", c.status().await.unwrap().entitled_bps);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn v05_asking_for_more_hosts_buys_no_extra_bandwidth() {
    let mut cfg = fast_config();
    cfg.unmeasured_bps = MB as f64;
    cfg.probe_new_nodes = false; // hold everyone at the known rate above
    let g = Grid::start(cfg).await.unwrap();
    let (_, m) = g.pool_with_members(5, p3(), generous_quota(), false).await.unwrap();
    let c = &m[0].client;
    put(c, "f", &noise(MB as usize, 13)).await.unwrap();
    let seg = q::largest_segment(g.db(), c.id(), |s| s.piece_len).await;
    let raw = serde_json::to_vec(&ReadRequest { want: 3, exclude_pieces: vec![], endpoint: c.endpoint_id(), range: None }).unwrap();
    let path = format!("/v1/segments/{seg}/read");
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let mut limited = 0;
    let start = Instant::now();
    let mut granted = 0u64;
    while start.elapsed() < Duration::from_secs(3) {
        let r = c.call("POST", &path, Some(&raw)).await.unwrap();
        if r.status() == 429 {
            limited += 1;
            tokio::time::sleep(Duration::from_millis(30)).await;
        } else if r.status().is_success() {
            let plan: ReadPlan = r.json().await.unwrap();
            granted += plan.piece_len * plan.pieces.len() as u64;
        } else {
            panic!("unexpected {}", r.status());
        }
    }
    let secs = start.elapsed().as_secs_f64();
    assert!(limited > 0, "never limited");
    let allowed = (1.0 + secs) * MB as f64 + 3.5 * MB as f64;
    assert!((granted as f64) <= allowed, "granted {granted} bytes in {secs:.1}s; allowed {allowed}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn v05_serving_banks_credit_and_is_zero_sum() {
    let g = Grid::start(fast_config()).await.unwrap();
    let (_, m) = g.pool_with_members(4, p3(), generous_quota(), false).await.unwrap();
    let c = &m[0].client;
    let (snap, src) = put(c, "f", &noise(5 * MB as usize, 15)).await.unwrap();
    get(c, &snap, &src, "f").await.unwrap();
    let ok = eventually(Duration::from_secs(5), || async {
        q::accounts(g.db()).await.iter().filter(|a| a.id != c.id()).map(|a| a.banked_credit).sum::<f64>() > 0.0
    })
    .await;
    assert!(ok, "serving earned no credit");
    let served = q::ledger_sum(g.db(), "served").await;
    let charged = q::ledger_sum(g.db(), "read_charge").await;
    assert!(served <= charged, "served {served} > charged {charged}");
}

// 6 ----------------------------------------------------------- loss and repair

async fn loss_and_repair(policy: Policy, kill: usize) {
    assert!(kill <= (policy.n - policy.k) as usize);
    let g = Grid::start(fast_config()).await.unwrap();
    let (_, m) = g.pool_with_members(14, policy, generous_quota(), false).await.unwrap();
    let c = &m[0].client;
    let body = noise(6 * MB as usize, 21);
    let (snap, src) = put(c, "f", &body).await.unwrap();
    let pieces = g.pieces_of(c.id()).await.unwrap();
    let first_hosts: Vec<i64> = {
        let seg = &pieces[0].0;
        pieces.iter().filter(|p| &p.0 == seg).map(|p| p.2).collect()
    };
    let nodes: Vec<_> = m.iter().flat_map(|x| x.nodes.iter().cloned()).collect();
    let victims: Vec<_> = nodes.iter().filter(|n| first_hosts.contains(&n.id)).take(kill).cloned().collect();
    for v in &victims {
        v.kill();
    }
    assert!(get(c, &snap, &src, "f").await.unwrap() == body, "not readable after {kill} hosts lost");

    let victim_ids: Vec<i64> = victims.iter().map(|v| v.id).collect();
    let id = c.id();
    let db = g.db().clone();
    // The policy's promise is at least `repair_at` healthy pieces per segment. A piece on a dead
    // host may legitimately remain while the segment still meets that: repair is lazy.
    let repair_at = policy.repair_at as i64;
    let repaired = eventually(Duration::from_secs(40), || {
        let (db, ids) = (db.clone(), victim_ids.clone());
        async move {
            let segs = q::segments(&db, id).await;
            let worst = segs.iter().filter(|s| s.state == "committed").map(|s| s.pieces.iter().filter(|p| !ids.contains(&p.host)).count() as i64).min();
            worst.is_some_and(|w| w >= repair_at)
        }
    })
    .await;
    assert!(repaired, "segments stayed below repair_at");
    let done: u64 = nodes.iter().map(|n| n.stats.repairs_done.load(Ordering::Relaxed)).sum();
    assert!(done >= 1, "repairs done: {done}");

    // Redundancy is really restored: with `repair_at` healthy pieces, every segment can lose
    // `repair_at - k` more. Before repair, this many further losses would have been fatal.
    let kill_again = (policy.repair_at - policy.k) as usize;
    let seg_pieces = g.pieces_of(id).await.unwrap();
    let seg = &seg_pieces[0].0;
    let current: Vec<i64> = seg_pieces.iter().filter(|p| &p.0 == seg).map(|p| p.2).collect();
    let more: Vec<_> = nodes.iter().filter(|n| !n.is_killed() && current.contains(&n.id)).take(kill_again).cloned().collect();
    for v in &more {
        v.kill();
    }
    assert!(get(c, &snap, &src, "f").await.unwrap() == body, "repaired copies don't hold the data");
    let fails: i64 = q::nodes(g.db()).await.iter().filter(|n| n.state == "active" && !victim_ids.contains(&n.id)).map(|n| n.audit_fail).sum();
    assert_eq!(fails, 0, "audits failed on honest hosts");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn v06_replicas_survive_loss_and_repair_without_owner() {
    loss_and_repair(p3(), 2).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn v06_erasure_coded_pieces_survive_loss_and_repair_without_owner() {
    loss_and_repair(ec48(), 4).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn v06_owner_returns_on_a_new_machine_with_only_the_phrase() {
    let g = Grid::start(fast_config()).await.unwrap();
    let (_, m) = g.pool_with_members(6, p3(), generous_quota(), false).await.unwrap();
    let body = noise(3 * MB as usize, 22);
    let (snap, src) = put(&m[0].client, "f", &body).await.unwrap();
    let id = m[0].client.id();
    let host = g.pieces_of(id).await.unwrap()[0].2;
    m.iter().flat_map(|x| x.nodes.iter()).find(|n| n.id == host).unwrap().kill();
    let db = g.db().clone();
    assert!(
        eventually(Duration::from_secs(30), || {
            let db = db.clone();
            async move { q::pieces_on(&db, id, host).await == 0 }
        })
        .await
    );
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("account"), format!("{id} {}", m[0].client.pool())).unwrap();
    let fresh = globalobject_client::Client::open(&g.url, &m[0].phrase, "", dir.path(), NetMode::Local).await.unwrap();
    assert!(fresh.snapshots().await.unwrap().contains(&snap));
    assert!(get(&fresh, &snap, &src, "f").await.unwrap() == body);
    // A wrong phrase on the same account gets nothing.
    let wrong = globalobject_client::Client::open(&g.url, "wrong phrase", "", dir.path(), NetMode::Local).await.unwrap();
    assert!(wrong.status().await.is_err());
}

// 7 ------------------------------------------------------------------ short outage

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn v07_short_outage_triggers_no_repair() {
    let mut cfg = fast_config();
    cfg.grace_secs = 60.0;
    let g = Grid::start(cfg).await.unwrap();
    let (_, m) = g.pool_with_members(6, p3(), generous_quota(), false).await.unwrap();
    let c = &m[0].client;
    let (snap, src) = put(c, "f", &noise(2 * MB as usize, 23)).await.unwrap();
    let before = g.pieces_of(c.id()).await.unwrap();
    let host = before[0].2;
    let nodes: Vec<_> = m.iter().flat_map(|x| x.nodes.iter().cloned()).collect();
    let victim = nodes.iter().find(|n| n.id == host).unwrap();
    victim.kill();
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(before, g.pieces_of(c.id()).await.unwrap(), "placement changed during a short outage");
    let repairs = q::repairs_in_flight(g.db()).await;
    assert_eq!(repairs, 0);
    victim.revive();
    assert!(get(c, &snap, &src, "f").await.is_ok());
    let done: u64 = nodes.iter().map(|n| n.stats.repairs_done.load(Ordering::Relaxed)).sum();
    assert_eq!(done, 0);
}

// 8 ----------------------------------------------------------------- cheating hosts

async fn cheating_host(cheat: impl Fn(&std::path::Path)) {
    let g = Grid::start(fast_config()).await.unwrap();
    let (_, m) = g.pool_with_members(8, p3(), generous_quota(), false).await.unwrap();
    let c = &m[0].client;
    let body = noise(2 * MB as usize, 31);
    let (snap, src) = put(c, "f", &body).await.unwrap();
    let nodes: Vec<_> = m.iter().flat_map(|x| x.nodes.iter().cloned()).collect();
    // Choose the host of the largest piece: small ones live inside the store's database.
    let pieces = g.pieces_of(c.id()).await.unwrap();
    let big = q::largest_segment(g.db(), c.id(), |s| s.piece_len).await;
    let target = pieces.iter().find(|p| p.0 == big).unwrap();
    let host = target.2;
    let bad = nodes.iter().find(|n| n.id == host).unwrap();
    // Just one piece. Passing audits on its other pieces must not excuse this.
    let path = piece_path(bad, &target.3);
    assert!(path.exists(), "expected the piece at {}", path.display());
    cheat(&path);
    let db = g.db().clone();
    let caught = eventually(Duration::from_secs(40), || {
        let db = db.clone();
        async move { q::node(&db, host).await.state == "disqualified" }
    })
    .await;
    assert!(caught, "cheating host wasn't disqualified");
    let id = c.id();
    let db = g.db().clone();
    let moved = eventually(Duration::from_secs(30), || {
        let db = db.clone();
        async move { q::pieces_on(&db, id, host).await == 0 }
    })
    .await;
    assert!(moved, "data wasn't moved off the cheating host");
    assert!(get(c, &snap, &src, "f").await.unwrap() == body);
    let others: i64 = q::nodes(g.db()).await.iter().filter(|n| n.id != host).map(|n| n.audit_fail).sum();
    assert_eq!(others, 0, "an honest host was blamed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn v08_discarded_data_is_caught() {
    cheating_host(|p| std::fs::write(p, b"").unwrap()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn v08_bit_flips_are_caught() {
    cheating_host(|p| {
        let mut b = std::fs::read(p).unwrap();
        for i in (0..b.len()).step_by(4096) {
            b[i] ^= 1;
        }
        std::fs::write(p, b).unwrap();
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn v08_replaced_data_is_caught() {
    cheating_host(|p| {
        let len = std::fs::metadata(p).unwrap().len() as usize;
        std::fs::write(p, noise(len, 99)).unwrap();
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn v08_reader_routes_around_a_corrupt_piece() {
    let mut cfg = fast_config();
    cfg.audit_every = Duration::from_secs(3600);
    let g = Grid::start(cfg).await.unwrap();
    let (_, m) = g.pool_with_members(5, p3(), generous_quota(), false).await.unwrap();
    let c = &m[0].client;
    let body = noise(2 * MB as usize, 33);
    let (snap, src) = put(c, "f", &body).await.unwrap();
    let nodes: Vec<_> = m.iter().flat_map(|x| x.nodes.iter().cloned()).collect();
    // Corrupt two of each three copies, where stored as files. One good copy must be enough.
    for p in g.pieces_of(c.id()).await.unwrap().iter().filter(|p| p.1 < 2) {
        let n = nodes.iter().find(|n| n.id == p.2).unwrap();
        let path = piece_path(n, &p.3);
        if let Ok(mut b) = std::fs::read(&path) {
            b[10] ^= 0xff;
            std::fs::write(&path, b).unwrap();
        }
    }
    assert!(get(c, &snap, &src, "f").await.unwrap() == body);
}

// 9 ------------------------------------------------------------------- coded pieces

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn v09_coded_pieces_differ() {
    let g = Grid::start(fast_config()).await.unwrap();
    let (_, m) = g.pool_with_members(10, ec48(), generous_quota(), false).await.unwrap();
    put(&m[0].client, "f", &noise(MB as usize, 41)).await.unwrap();
    let pieces = g.pieces_of(m[0].client.id()).await.unwrap();
    let seg = &pieces[0].0;
    let hashes: HashSet<&String> = pieces.iter().filter(|p| &p.0 == seg).map(|p| &p.3).collect();
    assert_eq!(hashes.len(), 8, "erasure-coded pieces must be distinct, so hosts can't dedupe them");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn v09_replicas_must_be_identical() {
    let g = Grid::start(fast_config()).await.unwrap();
    let (_, m) = g.pool_with_members(5, p3(), generous_quota(), false).await.unwrap();
    let c = &m[0].client;
    let raw = serde_json::to_vec(&BeginSegment {
        hashes: vec!["a".repeat(64), "b".repeat(64), "c".repeat(64)],
        ..serde_json::from_slice(&begin_body(c, MB)).unwrap()
    })
    .unwrap();
    assert_eq!(c.call("POST", "/v1/segments/begin", Some(&raw)).await.unwrap().status(), 400, "replicas must be identical");
}

// 10 ----------------------------------------------------------- shrink and leave

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn v10_shrinking_a_slice_drains_before_release() {
    let g = Grid::start(fast_config()).await.unwrap();
    let (_, m) = g.pool_with_members(7, p3(), generous_quota(), false).await.unwrap();
    let c = &m[0].client;
    let body = noise(8 * MB as usize, 51);
    let (snap, src) = put(c, "f", &body).await.unwrap();
    let host = g.pieces_of(c.id()).await.unwrap()[0].2;
    let owner = m.iter().find(|x| x.nodes.iter().any(|n| n.id == host)).unwrap();
    let node = owner.nodes.iter().find(|n| n.id == host).unwrap();
    assert!(node.used_bytes() > 0);
    let pledged_before = q::node(g.db(), host).await.pledged;
    owner.client.set_pledge(host, 0).await.unwrap();
    let still = q::node(g.db(), host).await.pledged;
    assert_eq!(still, pledged_before, "the old pledge holds until the data has moved");
    let db = g.db().clone();
    let released = eventually(Duration::from_secs(30), || {
        let db = db.clone();
        async move { q::node(&db, host).await.pledged == 0 }
    })
    .await;
    assert!(released, "pledge never released");
    assert!(eventually(Duration::from_secs(10), || async { node.used_bytes() == 0 }).await, "node still holds {} bytes", node.used_bytes());
    assert!(get(c, &snap, &src, "f").await.unwrap() == body);
    owner.client.set_pledge(host, 4 * GB).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn v10_drain_waits_rather_than_dropping_redundancy() {
    // With four owners, excluding the data's owner and the shrinking host leaves only two.
    let g = Grid::start(fast_config()).await.unwrap();
    let (_, m) = g.pool_with_members(4, p3(), generous_quota(), false).await.unwrap();
    let c = &m[0].client;
    let (snap, src) = put(c, "f", &noise(MB as usize, 54)).await.unwrap();
    let host = g.pieces_of(c.id()).await.unwrap()[0].2;
    let owner = m.iter().find(|x| x.nodes.iter().any(|n| n.id == host)).unwrap();
    let before = owner.nodes[0].used_bytes();
    owner.client.set_pledge(host, 0).await.unwrap();
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(owner.nodes[0].used_bytes(), before, "pieces dropped with nowhere to go");
    let pledged = q::node(g.db(), host).await.pledged;
    assert!(pledged > 0, "capacity released while still holding data");
    assert!(get(c, &snap, &src, "f").await.is_ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn v10_leavers_go_through_grace_read_only_then_deletion() {
    let g = Grid::start(fast_config()).await.unwrap();
    let (_, m) = g.pool_with_members(6, p3(), generous_quota(), false).await.unwrap();
    let c = &m[1].client;
    let (snap, src) = put(c, "f", &noise(2 * MB as usize, 52)).await.unwrap();
    assert_eq!(c.status().await.unwrap().state, "active");
    m[1].node().kill();
    let state = |id: i64| {
        let db = g.db().clone();
        async move { q::account(&db, id).await.state }
    };
    assert!(eventually(Duration::from_secs(20), || async { state(c.id()).await == "read_only" }).await, "never went read-only");
    let e = put(c, "g", &[1u8; 100]).await.err().unwrap();
    assert!(is(&e, |x| matches!(x, ClientError::Forbidden(_))), "{e:#}");
    let _ = (&snap, &src);
    assert!(eventually(Duration::from_secs(20), || async { state(c.id()).await == "purged" }).await, "never purged");
    let left = q::segments(g.db(), c.id()).await.len();
    assert_eq!(left, 0);
    let others: Vec<_> = m.iter().enumerate().filter(|(i, _)| *i != 1).flat_map(|(_, x)| x.nodes.iter().cloned()).collect();
    assert!(eventually(Duration::from_secs(10), || async { others.iter().all(|n| n.used_bytes() == 0) }).await, "space not reclaimed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn v10_read_only_accounts_can_still_restore() {
    let mut cfg = fast_config();
    cfg.read_only_secs = 3600.0;
    let g = Grid::start(cfg).await.unwrap();
    let (_, m) = g.pool_with_members(6, p3(), generous_quota(), false).await.unwrap();
    let c = &m[1].client;
    let body = noise(MB as usize, 55);
    let (snap, src) = put(c, "f", &body).await.unwrap();
    m[1].node().kill();
    let db = g.db().clone();
    let id = c.id();
    assert!(
        eventually(Duration::from_secs(20), || {
            let db = db.clone();
            async move { q::account(&db, id).await.state == "read_only" }
        })
        .await
    );
    assert!(get(c, &snap, &src, "f").await.unwrap() == body, "read-only must still restore");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn v10_returning_within_grace_restores_the_account() {
    let g = Grid::start(fast_config()).await.unwrap();
    let (_, m) = g.pool_with_members(5, p3(), generous_quota(), false).await.unwrap();
    let c = &m[1].client;
    put(c, "f", &noise(MB as usize, 53)).await.unwrap();
    m[1].node().kill();
    let over = |id: i64| {
        let db = g.db().clone();
        async move { q::account(&db, id).await.over_since.is_some() }
    };
    assert!(eventually(Duration::from_secs(20), || async { over(c.id()).await }).await);
    m[1].node().revive();
    assert!(eventually(Duration::from_secs(10), || async { !over(c.id()).await }).await, "never recovered");
    assert_eq!(c.status().await.unwrap().state, "active");
    put(c, "g", &[2u8; 100]).await.unwrap();
}

// 11 ----------------------------------------------------------------- pools

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn v11_pools_are_isolated() {
    let g = Grid::start(fast_config()).await.unwrap();
    let (_, a) = g.pool_with_members(4, p3(), generous_quota(), true).await.unwrap();
    let (_, b) = g.pool_with_members(4, p3(), generous_quota(), false).await.unwrap();
    assert_ne!(a[0].client.pool(), b[0].client.pool());
    put(&a[0].client, "a", &noise(3 * MB as usize, 61)).await.unwrap();
    put(&b[0].client, "b", &noise(3 * MB as usize, 62)).await.unwrap();

    let pool_of: std::collections::HashMap<i64, i64> = q::nodes(g.db()).await.iter().map(|n| (n.id, n.pool)).collect();
    let crossed =
        q::all_segments(g.db()).await.iter().flat_map(|s| s.pieces.iter().map(move |p| (s.pool, p.host))).filter(|(pool, h)| pool_of[h] != *pool).count();
    assert_eq!(crossed, 0);

    let seg = g.pieces_of(a[0].client.id()).await.unwrap()[0].0.clone();
    let raw = serde_json::to_vec(&ReadRequest { want: 1, exclude_pieces: vec![], endpoint: b[0].client.endpoint_id(), range: None }).unwrap();
    assert_eq!(b[0].client.call("POST", &format!("/v1/segments/{seg}/read"), Some(&raw)).await.unwrap().status(), 403);

    // A properly signed grant for pool A is refused by a host in pool B.
    let me = globalobject_net::bind(NetMode::Local, vec![]).await.unwrap();
    let forged = Grant {
        nonce: globalobject_crypto::random_hex(16),
        account: a[0].client.id(),
        pool: a[0].client.pool(),
        segment: seg,
        piece: 0,
        host: b[1].node().id,
        peer: me.id().to_string(),
        hash: "a".repeat(64),
        op: Op::Read,
        max_bytes: MB,
        rate_bps: 0,
        expires_at: now() + 60,
        range: None,
    };
    let token = Identity::from_seed(&SEED).token(T_GRANT, &forged);
    assert!(matches!(peer_call(&me, &b[1].node().addr(), &PeerRequest::Authorize { grant: token }).await.unwrap(), PeerReply::Refused { .. }));

    // A pool below three eligible owners can't accept writes, whatever other pools have.
    let (_, small) = g.pool_with_members(3, p3(), generous_quota(), true).await.unwrap();
    assert!(put(&small[0].client, "x", &[0u8; 10]).await.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn v11_private_admin_controls_membership_and_policy_but_not_the_floor() {
    let g = Grid::start(fast_config()).await.unwrap();
    let (p, m) = g.pool_with_members(6, p3(), generous_quota(), true).await.unwrap();
    let admin = &m[0].client;
    let member = &m[1].client;

    let r = member.call("POST", &format!("/v1/pools/{}/invites", p.pool), Some(b"null")).await.unwrap();
    assert_eq!(r.status(), 403);
    let path = format!("/v1/pools/{}", p.pool);
    let newp = serde_json::to_vec(&Policy { k: 1, n: 4, repair_at: 4, ..p3() }).unwrap();
    assert_eq!(member.call("PATCH", &path, Some(&newp)).await.unwrap().status(), 403);
    assert_eq!(admin.call("PATCH", &path, Some(&newp)).await.unwrap().status(), 200);
    let below = serde_json::to_vec(&Policy { k: 1, n: 2, repair_at: 2, min_failure_domains: 2, ..p3() }).unwrap();
    assert_eq!(admin.call("PATCH", &path, Some(&below)).await.unwrap().status(), 400);
    assert_eq!(admin.status().await.unwrap().policy.n, 4);

    // Removing a member drains their node, and data they host survives.
    let body = noise(2 * MB as usize, 63);
    let (snap, src) = put(admin, "keep", &body).await.unwrap();
    let remove = format!("/v1/pools/{}/members/{}/remove", p.pool, member.id());
    assert_eq!(member.call("POST", &remove, Some(b"null")).await.unwrap().status(), 403);
    assert_eq!(admin.call("POST", &remove, Some(b"null")).await.unwrap().status(), 200);
    let node = m[1].node().id;
    let db = g.db().clone();
    assert!(
        eventually(Duration::from_secs(30), || {
            let db = db.clone();
            async move { q::all_segments(&db).await.iter().flat_map(|s| &s.pieces).all(|p| p.host != node) }
        })
        .await,
        "member's node wasn't drained"
    );
    assert!(get(admin, &snap, &src, "keep").await.unwrap() == body);
    let e = put(member, "x", &[0u8; 10]).await.err().unwrap();
    assert!(is(&e, |x| matches!(x, ClientError::Forbidden(_))), "{e:#}");
}

// Additional scenarios from the threat model ----------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn t_false_pledge_surfaces_and_loses_standing() {
    let g = Grid::start(fast_config()).await.unwrap();
    let p = g.pool(false, p3(), generous_quota(), false).await.unwrap();
    let first = g.member(&p.admin_invite, Spec::default()).await.unwrap();
    let mut rest = vec![];
    for _ in 0..5 {
        let inv = first.client.invite().await.unwrap();
        rest.push(g.member(&inv, Spec::default()).await.unwrap());
    }
    // Pledges a lot and has 64 KiB. Its huge free space attracts placement first.
    let inv = first.client.invite().await.unwrap();
    let liar = g.member(&inv, Spec { pledged: 800 * GB, real_capacity: Some(64 * 1024), ..Spec::default() }).await.unwrap();
    let liar_id = liar.node().id;
    // Three different accounts upload, and find out. They still succeed, via other hosts.
    let mut backups = vec![];
    for (i, m) in rest.iter().take(3).enumerate() {
        let body = noise(3 * MB as usize, 70 + i as u8);
        let (snap, src) = put(&m.client, "f", &body).await.unwrap();
        backups.push((snap, src, body));
    }
    let db = g.db().clone();
    let caught = eventually(Duration::from_secs(10), || {
        let db = db.clone();
        async move { q::node(&db, liar_id).await.state == "disqualified" }
    })
    .await;
    assert!(caught, "false pledge never surfaced");
    assert_eq!(liar.client.status().await.unwrap().credited, 0, "a disqualified node earns no credit");
    // Regression: a retried upload reused its segment ID, so the abort's delayed deletion could
    // destroy the retry's copy on the same host, and honest hosts then failed audits.
    tokio::time::sleep(Duration::from_secs(3)).await;
    for (m, (snap, src, body)) in rest.iter().zip(&backups) {
        assert!(get(&m.client, snap, src, "f").await.unwrap() == *body, "a retried upload lost data");
    }
    let honest: i64 = q::nodes(g.db()).await.iter().filter(|n| n.id != liar_id).map(|n| n.audit_fail).sum();
    assert_eq!(honest, 0, "an honest host failed audits");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn t_one_client_cannot_disqualify_a_host_alone() {
    let g = Grid::start(fast_config()).await.unwrap();
    let (_, m) = g.pool_with_members(5, p3(), generous_quota(), false).await.unwrap();
    let target = m[1].node().id;
    put(&m[0].client, "f", &[1u8; 100]).await.unwrap();
    let seg = g.pieces_of(m[0].client.id()).await.unwrap()[0].0.clone();
    for _ in 0..10 {
        let raw = serde_json::to_vec(&CommitSegment { receipts: vec![], failed_hosts: vec![target] }).unwrap();
        let _ = m[0].client.call("POST", &format!("/v1/segments/{seg}/abort"), Some(&raw)).await;
    }
    tokio::time::sleep(Duration::from_millis(800)).await;
    let s = q::node(g.db(), target).await.state;
    assert_eq!(s, "active");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn t_hidden_snapshots_are_detected() {
    let g = Grid::start(fast_config()).await.unwrap();
    let (_, m) = g.pool_with_members(4, p3(), generous_quota(), false).await.unwrap();
    let c = &m[0].client;
    let (snap, _src) = put(c, "one", &[1u8; 100]).await.unwrap();
    put(c, "two", &[2u8; 100]).await.unwrap();
    assert_eq!(c.snapshots().await.unwrap().len(), 2);
    // A malicious control plane stops listing an earlier snapshot.
    g.db().objects.delete_one(mongodb::bson::doc! {"account": c.id(), "tpe": "snapshot", "object": &snap}).await.unwrap();
    let e = c.snapshots().await.unwrap_err();
    assert!(is(&e, |x| matches!(x, ClientError::Rollback(_))), "{e:#}");
}

// Partial reads, reclaim and local parity (docs/plans/reads-reclaim-repair.md) -------------

/// Regression for PR 1. A get request can name the *children* of a hash sequence, and the
/// provider then serves the blobs listed inside the requested one. A grant for one blob must
/// not unlock others held by the same host.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn v04_hash_sequence_requests_cannot_reach_other_blobs() {
    use iroh_blobs::{
        hashseq::HashSeq,
        protocol::{ChunkRangesSeq, GetRequest},
        store::mem::MemStore,
        BlobsProtocol, Hash,
    };
    let g = Grid::start(fast_config()).await.unwrap();
    let (_, m) = g.pool_with_members(4, p3(), generous_quota(), false).await.unwrap();
    let (victim, attacker) = (&m[0].client, &m[1].client);
    put(victim, "f", &noise(MB as usize, 80)).await.unwrap();
    let victim_pieces = g.pieces_of(victim.id()).await.unwrap();

    // The attacker serves one piece: a hash sequence naming every victim piece.
    let hashes: Vec<Hash> = victim_pieces.iter().map(|p| p.3.parse().unwrap()).collect();
    let evil = HashSeq::from_iter(hashes.clone()).into_inner();
    let evil_hash = globalobject_net::hash_of(&evil);
    let ep = globalobject_net::bind(NetMode::Local, vec![globalobject_net::BLOBS_ALPN.to_vec()]).await.unwrap();
    let store = MemStore::new();
    let _tag = store.add_bytes(evil.clone()).temp_tag().await.unwrap();
    let _router = iroh::protocol::Router::builder(ep.clone()).accept(globalobject_net::BLOBS_ALPN, BlobsProtocol::new(&store, None)).spawn();

    // Placed and committed through the real path, like any other upload.
    let segment = globalobject_crypto::random_hex(16);
    let begin = BeginSegment {
        segment: segment.clone(),
        object: ObjectRef { tpe: "pack".into(), id: globalobject_crypto::random_hex(32) },
        len: evil.len() as u64,
        k: 1,
        n: 3,
        local_groups: 0,
        hashes: vec![evil_hash.clone(); 3],
        endpoint: ep.id().to_string(),
        avoid: vec![],
    };
    let r = attacker.call("POST", "/v1/segments/begin", Some(&serde_json::to_vec(&begin).unwrap())).await.unwrap();
    assert!(r.status().is_success(), "{}", r.status());
    let plan: SegmentPlan = r.json().await.unwrap();
    let mut receipts = vec![];
    for p in &plan.pieces {
        match peer_call(&ep, &p.addr, &PeerRequest::Store { grant: p.grant.clone(), from: globalobject_net::addr_string(&ep) }).await.unwrap() {
            PeerReply::Stored { receipt } => receipts.push(receipt),
            other => panic!("store failed: {other:?}"),
        }
    }
    let commit = serde_json::to_vec(&CommitSegment { receipts, failed_hosts: vec![] }).unwrap();
    assert!(attacker.call("POST", &format!("/v1/segments/{segment}/commit"), Some(&commit)).await.unwrap().status().is_success());

    // A host holding both the evil piece and a victim piece.
    let evil_hosts: HashSet<i64> = plan.pieces.iter().map(|p| p.host).collect();
    let shared = victim_pieces.iter().find(|p| evil_hosts.contains(&p.2)).expect("some host holds both").2;
    let read = |exclude: Vec<u16>| {
        let raw = serde_json::to_vec(&ReadRequest { want: 3, exclude_pieces: exclude, endpoint: ep.id().to_string(), range: None }).unwrap();
        let seg = segment.clone();
        async move {
            let plan: ReadPlan = attacker.call("POST", &format!("/v1/segments/{seg}/read"), Some(&raw)).await.unwrap().json().await.unwrap();
            plan.pieces.into_iter().find(|p| p.host == shared).expect("a grant on the shared host")
        }
    };

    // An ordinary grant, then a request for the children instead of the blob itself.
    let p = read(vec![]).await;
    assert!(matches!(peer_call(&ep, &p.addr, &PeerRequest::Authorize { grant: p.grant.clone() }).await.unwrap(), PeerReply::Ok));
    let conn = ep.connect(globalobject_net::parse_addr(&p.addr).unwrap(), globalobject_net::BLOBS_ALPN).await.unwrap();
    let ranges =
        ChunkRangesSeq::from_ranges(std::iter::once(globalobject_net::ChunkRanges::empty()).chain(hashes.iter().map(|_| globalobject_net::ChunkRanges::all())));
    // The attacker made the hash sequence, so has it locally, and so knows which children arrive.
    let sink = MemStore::new();
    let _root = sink.add_bytes(evil.clone()).temp_tag().await.unwrap();
    let got = sink.remote().execute_get(conn.clone(), GetRequest { hash: evil_hash.parse().unwrap(), ranges }).await;
    conn.close(0u32.into(), b"done");
    let leaked: Vec<&Hash> = {
        let mut v = vec![];
        for h in &hashes {
            if matches!(
                sink.blobs().status(*h).await,
                Ok(iroh_blobs::api::blobs::BlobStatus::Complete { .. } | iroh_blobs::api::blobs::BlobStatus::Partial { .. })
            ) {
                v.push(h);
            }
        }
        v
    };
    assert!(leaked.is_empty(), "served other accounts' pieces through a hash sequence: {} of them ({got:?})", leaked.len());

    // Control: the same kind of grant still fetches the blob itself.
    let p = read(vec![]).await;
    assert!(matches!(peer_call(&ep, &p.addr, &PeerRequest::Authorize { grant: p.grant.clone() }).await.unwrap(), PeerReply::Ok));
    assert_eq!(fetch_verified(&ep, &p.addr, &p.hash, MB).await.unwrap(), evil.to_vec());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn v04_range_grants_limit_what_is_served() {
    let g = Grid::start(fast_config()).await.unwrap();
    let (_, m) = g.pool_with_members(4, p3(), generous_quota(), false).await.unwrap();
    let c = &m[0].client;
    put(c, "f", &noise(2 * MB as usize, 81)).await.unwrap();
    let seg = q::largest_segment(g.db(), c.id(), |s| s.len).await;
    let me = globalobject_net::bind(NetMode::Local, vec![]).await.unwrap();
    let grant = |range: (u64, u64)| {
        let raw = serde_json::to_vec(&ReadRequest { want: 1, exclude_pieces: vec![], endpoint: me.id().to_string(), range: Some(range) }).unwrap();
        let seg = seg.clone();
        async move {
            let r = c.call("POST", &format!("/v1/segments/{seg}/read"), Some(&raw)).await.unwrap();
            assert!(r.status().is_success(), "{}", r.status());
            let plan: ReadPlan = r.json().await.unwrap();
            assert_eq!(plan.pieces.len(), 1);
            plan.pieces.into_iter().next().unwrap()
        }
    };
    // The granted range arrives, verified, and charges about that much.
    let before = q::ledger_sum(g.db(), "read_charge").await;
    let p = grant((100_000, 140_000)).await;
    assert_eq!(p.range, Some((100_000, 140_000)));
    let charged = q::ledger_sum(g.db(), "read_charge").await - before;
    assert!(charged <= 64 * 1024, "charged {charged} bytes for 40 KB");
    assert!(matches!(peer_call(&me, &p.addr, &PeerRequest::Authorize { grant: p.grant.clone() }).await.unwrap(), PeerReply::Ok));
    let (bytes, sent) = globalobject_net::fetch_range(&me, &p.addr, &p.hash, 100_000, 140_000).await.unwrap();
    assert_eq!(bytes.len(), 40_000);
    assert!(sent <= 64 * 1024, "sent {sent}");

    // Outside the granted range: refused. So is the whole piece.
    let p = grant((100_000, 140_000)).await;
    assert!(matches!(peer_call(&me, &p.addr, &PeerRequest::Authorize { grant: p.grant.clone() }).await.unwrap(), PeerReply::Ok));
    assert!(globalobject_net::fetch_range(&me, &p.addr, &p.hash, 500_000, 540_000).await.is_err(), "served outside the range");
    let p = grant((100_000, 140_000)).await;
    assert!(matches!(peer_call(&me, &p.addr, &PeerRequest::Authorize { grant: p.grant.clone() }).await.unwrap(), PeerReply::Ok));
    assert!(fetch_verified(&me, &p.addr, &p.hash, 64 * MB).await.is_err(), "a range grant fetched the whole piece");
}

/// Reading one small file out of a large pack moves and charges about that file's bytes.
async fn partial_read(policy: Policy) {
    let g = Grid::start(fast_config()).await.unwrap();
    let (_, m) = g.pool_with_members(10, policy, generous_quota(), false).await.unwrap();
    let c = &m[0].client;
    let (big, small) = (noise(24 * MB as usize, 82), noise(20_000, 83));
    let src = Source::new(&[("big", &big), ("small", &small)]);
    let snap = c.backup(src.dir.path()).await.unwrap().snapshot;
    // Only the tree pack is small, so the small file's bytes sit inside a big data pack, and
    // reading that pack whole would cost megabytes.
    let packs: Vec<i64> = q::objects(g.db(), mongodb::bson::doc! {"account": c.id(), "tpe": "pack"}).await.iter().map(|o| o.len).collect();
    assert!(packs.iter().filter(|&&l| l < 4 * MB as i64).count() <= 1, "pack sizes {packs:?}");
    let nodes: Vec<_> = m.iter().flat_map(|x| x.nodes.iter().cloned()).collect();
    let served = || nodes.iter().map(|n| n.stats.bytes_served.load(Ordering::Relaxed)).sum::<u64>();
    let charged = || q::ledger_sum(g.db(), "read_charge");

    let (s0, c0) = (served(), charged().await);
    assert!(c.read_file(&snap, &src.file("small")).await.unwrap() == small);
    let (s1, c1) = (served() - s0, charged().await - c0);
    println!("{policy:?}: small file read served {s1} bytes, charged {c1}");
    // Repository metadata (config, keys, index, snapshot, tree) is read in full, and is small.
    assert!(s1 < 2 * MB, "served {s1} bytes to read a 20 KB file");
    assert!((c1 as u64) < 2 * MB, "charged {c1} bytes to read a 20 KB file");

    if policy.k > 1 {
        // Data pieces out of reach: the read falls back to decoding the whole segment.
        let first = &g.pieces_of(c.id()).await.unwrap()[0].0;
        let data_hosts: Vec<i64> = g.pieces_of(c.id()).await.unwrap().iter().filter(|p| &p.0 == first && (p.1 as u16) < policy.k).map(|p| p.2).collect();
        for n in nodes.iter().filter(|n| data_hosts.contains(&n.id)) {
            n.kill();
        }
        assert!(c.read_file(&snap, &src.file("small")).await.unwrap() == small, "no fallback when data pieces are down");
    }
    // Restores still work, mostly through whole-object reads.
    assert!(c.read_file(&snap, &src.file("big")).await.unwrap() == big);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn t_partial_read_fetches_only_the_range_with_replicas() {
    partial_read(p3()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn t_partial_read_fetches_only_the_range_with_erasure_coding() {
    partial_read(ec48()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn t_forget_and_prune_reclaim_space() {
    let g = Grid::start(fast_config()).await.unwrap();
    let (_, m) = g.pool_with_members(4, p3(), generous_quota(), false).await.unwrap();
    let c = &m[0].client;
    let (old, new) = (noise(8 * MB as usize, 84), noise(8 * MB as usize, 85));
    let src = Source::new(&[("f", &old)]);
    let first = c.backup(src.dir.path()).await.unwrap().snapshot;
    std::fs::write(src.file("f"), &new).unwrap();
    let second = c.backup(src.dir.path()).await.unwrap().snapshot;
    assert_eq!(c.snapshots().await.unwrap(), vec![first.clone(), second.clone()]);
    let nodes: Vec<_> = m.iter().flat_map(|x| x.nodes.iter().cloned()).collect();
    let held = || nodes.iter().map(|n| n.used_bytes()).sum::<u64>();
    let (used_before, held_before) = (c.status().await.unwrap().used_raw, held());

    let gone = c.forget(&globalobject_client::Retention { last: Some(1), ..Default::default() }).await.unwrap();
    assert_eq!(gone, vec![first.clone()]);
    // Forgetting isn't a rollback, so listing still works.
    assert_eq!(c.snapshots().await.unwrap(), vec![second.clone()]);
    let p = c.prune(true).await.unwrap();
    println!("prune: {p:?}");
    assert!(p.packs_removed >= 1, "{p:?}");

    // Quota comes back at once; hosts delete the pieces at their next heartbeat.
    let used_after = c.status().await.unwrap().used_raw;
    assert!(used_after + 3 * 7 * MB < used_before, "used {used_before} -> {used_after}");
    let freed = eventually(Duration::from_secs(10), || async { held() + 3 * 7 * MB < held_before }).await;
    assert!(freed, "hosts still hold {} of {held_before} bytes", held());

    // The newer snapshot is whole, through rustic's restore path.
    let dest = tempfile::tempdir().unwrap();
    c.restore(&second, dest.path()).await.unwrap();
    let root = dest.path().join(src.dir.path().strip_prefix("/").unwrap());
    assert!(std::fs::read(root.join("f")).unwrap() == new);
    assert!(c.restore(&first, dest.path()).await.is_err(), "the forgotten snapshot is gone");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn t_local_parity_repairs_from_the_group() {
    // 4 data in 2 groups, 2 global parities, 2 local: 8 pieces, as much space as 4-of-8.
    let policy = Policy::pyramid(4, 8, 2, 8);
    let g = Grid::start(fast_config()).await.unwrap();
    let (_, m) = g.pool_with_members(12, policy, generous_quota(), false).await.unwrap();
    let c = &m[0].client;
    let body = noise(6 * MB as usize, 86);
    let (snap, src) = put(c, "f", &body).await.unwrap();
    assert!(get(c, &snap, &src, "f").await.unwrap() == body);

    // Lose the host of data piece 0. Every segment of this account shares its placement group,
    // so each loses piece 0, and each can rebuild it from piece 1 and local parity 6 alone.
    let pieces = g.pieces_of(c.id()).await.unwrap();
    let victim = pieces.iter().find(|p| p.1 == 0).unwrap().2;
    let nodes: Vec<_> = m.iter().flat_map(|x| x.nodes.iter().cloned()).collect();
    let segments_hit: i64 = pieces.iter().filter(|p| p.2 == victim).count() as i64;
    let lost: i64 = q::segments(g.db(), c.id()).await.iter().map(|s| s.piece_len * s.pieces.iter().filter(|p| p.host == victim).count() as i64).sum();
    nodes.iter().find(|n| n.id == victim).unwrap().kill();
    let db = g.db().clone();
    let id = c.id();
    let repaired = eventually(Duration::from_secs(40), || {
        let db = db.clone();
        async move { q::pieces_on(&db, id, victim).await == 0 }
    })
    .await;
    assert!(repaired, "pieces still on the lost host");
    let fetched: u64 = nodes.iter().map(|n| n.stats.repair_bytes_fetched.load(Ordering::Relaxed)).sum();
    println!("repaired {segments_hit} pieces ({lost} bytes) by fetching {fetched} bytes; plain RS would fetch {}", 4 * lost);
    assert!(fetched as i64 <= 2 * lost + lost / 2, "fetched {fetched} bytes to rebuild {lost}: not group-local");
    assert!(fetched as i64 >= 2 * lost, "fetched {fetched}, less than the two group pieces each");

    // Any r + 1 = 3 losses are survivable. Take down three more hosts of the same segments.
    let now: Vec<i64> = g.pieces_of(c.id()).await.unwrap().iter().filter(|p| p.1 == 1 || p.1 == 2 || p.1 == 4).map(|p| p.2).collect();
    for n in nodes.iter().filter(|n| now.contains(&n.id)) {
        n.kill();
    }
    assert!(get(c, &snap, &src, "f").await.unwrap() == body, "not readable after r + 1 losses");
}

// U ------------------------------------------------------------------ ease of use

async fn json(r: reqwest::RequestBuilder) -> (u16, serde_json::Value) {
    let r = r.send().await.unwrap();
    let code = r.status().as_u16();
    (code, r.json().await.unwrap_or(serde_json::Value::Null))
}

/// The whole user path: a join link, `setup`, the agent's slice, a backup started from the
/// dashboard, a restore, and recovering the account on another machine from the phrase alone.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn u01_join_link_setup_agent_backup_restore() {
    use globalobject_client::{agent, config::Paths, setup};
    let g = Grid::start(fast_config()).await.unwrap();
    // Three members already, so a newcomer's three copies have somewhere to go.
    let (_, m) = g.pool_with_members(3, p3(), generous_quota(), false).await.unwrap();

    // The invite comes with a join link, and the link's page shows the one command to run.
    let (code, link) = m[0].client.invite_link().await.unwrap();
    let link = link.expect("join link");
    assert_eq!(link, format!("{}/join/{code}", g.url));
    let page = plain().get(&link).send().await.unwrap().text().await.unwrap();
    assert!(page.contains(&format!("curl -fsSL {}/install.sh | sh -s -- --join {link}", g.url)), "{page}");
    let script = plain().get(format!("{}/install.sh", g.url)).send().await.unwrap().text().await.unwrap();
    assert!(script.starts_with("#!/bin/sh"));
    assert_eq!(plain().get(format!("{}/join/00ff", g.url)).send().await.unwrap().status(), 404);

    let home = tempfile::tempdir().unwrap();
    let paths = Paths { config_dir: home.path().join("config"), data_dir: home.path().join("data") };
    let docs = home.path().join("Documents");
    std::fs::create_dir_all(&docs).unwrap();
    let body = noise(2 * MB as usize, 31);
    std::fs::write(docs.join("report.bin"), &body).unwrap();
    setup::run_with(
        &paths,
        setup::SetupArgs {
            join: Some(link.clone()),
            slice: Some("8GB".into()),
            paths: vec![docs.clone()],
            every_hours: Some(24),
            dashboard_port: Some(0),
            yes: true,
            no_service: true,
            local: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let (cfg, creds) = paths.load().unwrap();
    assert_eq!(cfg.slice.size, 8_000_000_000);
    assert_eq!(creds.phrase.split_whitespace().count(), 12, "a new 12-word phrase");
    // The invite is spent.
    assert_eq!(plain().get(&link).send().await.unwrap().status(), 410);

    let agent_task = tokio::spawn(agent::run(paths.clone()));
    let p2 = paths.clone();
    assert!(
        eventually(Duration::from_secs(20), || {
            let p = p2.clone();
            async move { agent::read_status(&p).is_some_and(|s| s.node.is_some()) }
        })
        .await
    );
    let st = agent::read_status(&paths).unwrap();
    let dash = st.dashboard.clone();
    // The control plane chose the node's site from its address, not the user.
    assert_eq!(q::node(g.db(), st.node.unwrap()).await.site, "127.0.0.0/24");

    // The dashboard: token, Host and CSRF checks.
    let c = plain();
    let bearer = format!("Bearer {}", creds.dashboard_token);
    assert_eq!(c.get(format!("{dash}/api/status")).send().await.unwrap().status(), 401);
    assert_eq!(c.get(format!("{dash}/api/status")).header("authorization", "Bearer nope").send().await.unwrap().status(), 401);
    assert_eq!(c.get(format!("{dash}/api/status")).header("authorization", &bearer).header("host", "evil.example").send().await.unwrap().status(), 403);
    assert_eq!(c.get(format!("{dash}/login?token=nope")).send().await.unwrap().status(), 401);
    let login = reqwest::Client::builder().no_proxy().redirect(reqwest::redirect::Policy::none()).build().unwrap();
    let r = login.get(format!("{dash}/login?token={}", creds.dashboard_token)).send().await.unwrap();
    assert_eq!(r.status(), 303);
    assert!(r.headers()["set-cookie"].to_str().unwrap().contains("HttpOnly; SameSite=Strict"));
    let (code, s) = json(c.get(format!("{dash}/api/status")).header("authorization", &bearer)).await;
    assert_eq!(code, 200, "{s}");
    assert_eq!(s["account"]["state"], "active");
    assert_eq!(c.post(format!("{dash}/api/backup")).header("authorization", &bearer).send().await.unwrap().status(), 403, "POST without x-globalobject");

    // Back up from the dashboard, and wait for it to finish.
    let post = |path: &str, body: serde_json::Value| c.post(format!("{dash}{path}")).header("authorization", &bearer).header("x-globalobject", "1").json(&body);
    assert_eq!(json(post("/api/backup", serde_json::json!({}))).await.0, 202);
    let ok = eventually(Duration::from_secs(60), || {
        let r = c.get(format!("{dash}/api/status")).header("authorization", &bearer);
        async move { json(r).await.1["agent"]["last_backup"]["ok"] == true }
    })
    .await;
    assert!(ok, "backup didn't finish: {:?}", agent::read_status(&paths).and_then(|s| s.last_backup));
    let (_, snaps) = json(c.get(format!("{dash}/api/snapshots")).header("authorization", &bearer)).await;
    let snap = snaps[0]["id"].as_str().unwrap().to_string();
    assert_eq!(snaps[0]["files"], 1);

    // Restore from the dashboard. Into a non-empty folder is refused.
    assert_eq!(json(post("/api/restore", serde_json::json!({"snapshot": snap, "dest": docs}))).await.0, 400);
    let dest = home.path().join("restored");
    assert_eq!(json(post("/api/restore", serde_json::json!({"snapshot": snap, "dest": dest}))).await.0, 202);
    let restored = dest.join(docs.strip_prefix("/").unwrap()).join("report.bin");
    let r2 = restored.clone();
    assert!(
        eventually(Duration::from_secs(60), || {
            let r = r2.clone();
            async move { std::fs::read(&r).is_ok_and(|b| b.len() == 2 * MB as usize) }
        })
        .await
    );
    assert!(std::fs::read(&restored).unwrap() == body);

    // Settings from the dashboard reach config.toml.
    assert_eq!(json(post("/api/settings", serde_json::json!({"every_hours": 6, "slice": "9GB"}))).await.0, 200);
    let (cfg2, _) = paths.load().unwrap();
    assert_eq!((cfg2.backup.every_hours, cfg2.slice.size), (6, 9_000_000_000));
    assert_eq!(json(post("/api/settings", serde_json::json!({"paths": ["relative/path"]}))).await.0, 400);
    let metrics = c.get(format!("{dash}/metrics")).header("authorization", &bearer).send().await.unwrap().text().await.unwrap();
    assert!(metrics.contains("globalobject_agent_last_backup_success 1"), "{metrics}");

    // A new machine, with only the phrase.
    let other = tempfile::tempdir().unwrap();
    let back = globalobject_client::Client::recover(&g.url, &creds.phrase, "", other.path(), NetMode::Local).await.unwrap();
    assert_eq!(back.id(), cfg_account(&paths));
    assert!(back.snapshots().await.unwrap().contains(&snap));
    let wrong = globalobject_client::Client::recover(&g.url, "not the phrase", "", tempfile::tempdir().unwrap().path(), NetMode::Local).await;
    assert!(wrong.is_err());
    agent_task.abort();
}

fn cfg_account(paths: &globalobject_client::config::Paths) -> i64 {
    std::fs::read_to_string(paths.state_dir().join("account")).unwrap().split_whitespace().next().unwrap().parse().unwrap()
}

/// What an operator sees: health, readiness, the admin API, metrics, and member removal.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn u02_operator_health_admin_api_and_metrics() {
    let g = Grid::start(fast_config()).await.unwrap();
    let (p, m) = g.pool_with_members(4, p3(), generous_quota(), true).await.unwrap();
    let c = plain();
    let op = |r: reqwest::RequestBuilder| r.header("x-globalobject-operator", OPERATOR);
    assert_eq!(c.get(format!("{}/healthz", g.url)).send().await.unwrap().status(), 200);
    assert_eq!(c.get(format!("{}/readyz", g.url)).send().await.unwrap().status(), 200);
    assert_eq!(c.get(format!("{}/admin", g.url)).send().await.unwrap().status(), 200);

    // Operator routes refuse anyone else.
    for path in ["/v1/admin/summary", "/v1/pools", "/v1/admin/nodes"] {
        assert_eq!(c.get(format!("{}{path}", g.url)).send().await.unwrap().status(), 403, "{path}");
        assert_eq!(c.get(format!("{}{path}", g.url)).header("x-globalobject-operator", "wrong").send().await.unwrap().status(), 403, "{path}");
    }
    let body = noise(3 * MB as usize, 9);
    let (snap, files) = put(&m[0].client, "f", &body).await.unwrap();

    let (code, s) = json(op(c.get(format!("{}/v1/admin/summary", g.url)))).await;
    assert_eq!(code, 200);
    assert_eq!(s["pools"], 1);
    assert_eq!(s["accounts"]["active"], 4);
    assert_eq!(s["nodes_online"], 4);
    assert!(s["stored_bytes"].as_i64().unwrap() > 0);
    assert_eq!(s["segments_unreadable"], 0);
    let (_, pools) = json(op(c.get(format!("{}/v1/pools", g.url)))).await;
    assert_eq!(pools[0]["members"], 4);
    assert_eq!(pools[0]["id"], p.pool);
    let (_, nodes) = json(op(c.get(format!("{}/v1/admin/nodes?pool={}", g.url, p.pool)))).await;
    assert_eq!(nodes.as_array().unwrap().len(), 4);
    assert!(nodes.as_array().unwrap().iter().any(|n| n["held"].as_i64().unwrap() > 0));

    // The operator can invite into a private pool, which members other than the admin can't.
    let (code, inv) = json(op(c.post(format!("{}/v1/pools/{}/invites", g.url, p.pool)))).await;
    assert_eq!(code, 200);
    assert!(inv["join_url"].as_str().unwrap().starts_with(&format!("{}/join/", g.url)));
    assert!(m[1].client.invite().await.is_err());

    // Metrics: request counts, grants, and the summary gauges.
    let text = c.get(format!("{}/metrics", g.url)).send().await.unwrap().text().await.unwrap();
    for series in ["globalobject_http_requests_total", "globalobject_grants_issued_total", "globalobject_nodes_online", "globalobject_segments_below_repair"] {
        assert!(text.contains(series), "{series} missing from:\n{text}");
    }
    assert!(text.contains(r#"route="/v1/segments/begin""#), "routes are labelled by pattern");

    // Losing a host shows up in the summary, and repair brings it back.
    let host = g.pieces_of(m[0].client.id()).await.unwrap()[0].2;
    m.iter().flat_map(|x| x.nodes.iter()).find(|n| n.id == host).unwrap().kill();
    let (cc, url) = (c.clone(), g.url.clone());
    assert!(
        eventually(Duration::from_secs(20), || {
            let r = op(cc.get(format!("{url}/v1/admin/summary")));
            async move { json(r).await.1["nodes_online"] == 3 }
        })
        .await
    );
    assert!(get(&m[0].client, &snap, &files, "f").await.unwrap() == body);

    // Removing a member: read-only at once, and its nodes drain.
    let victim = m[3].client.id();
    let (code, _) = json(op(c.post(format!("{}/v1/pools/{}/members/{victim}/remove", g.url, p.pool)))).await;
    assert_eq!(code, 200);
    assert_eq!(q::account(g.db(), victim).await.state, "read_only");
}
