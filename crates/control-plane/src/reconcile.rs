//! Background loops that compare desired state with reality, then act:
//! repair, drain, audit, probes and the over-quota lifecycle.

use crate::db::{self, bytes_by, find_all, int, to_bson, transaction, NodeDoc, RepairDoc, SegmentDoc};
use crate::{api::candidates, *};
use globalobject_policy::{place, PlacementRequest};
use mongodb::options::ReturnDocument;
use rand::{rngs::StdRng, seq::SliceRandom, Rng, SeedableRng};
use std::collections::HashMap;

pub fn spawn(s: Arc<Shared>) -> Vec<JoinHandle<()>> {
    fn every<F, Fut>(s: Arc<Shared>, d: Duration, name: &'static str, f: F) -> JoinHandle<()>
    where
        F: Fn(Arc<Shared>) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = DbResult<()>> + Send,
    {
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(d).await;
                if let Err(e) = f(s.clone()).await {
                    tracing::warn!("{name} loop: {e}");
                }
            }
        })
    }
    vec![
        every(s.clone(), s.cfg.reconcile_every, "repair", |s| async move { repair_round(&s).await }),
        every(s.clone(), s.cfg.reconcile_every, "lifecycle", |s| async move { lifecycle_round(&s).await }),
        every(s.clone(), s.cfg.reconcile_every, "refusals", |s| async move { refusals_round(&s).await }),
        every(s.clone(), s.cfg.reconcile_every, "pending", |s| async move { pending_round(&s).await }),
        every(s.clone(), s.cfg.audit_every, "audit", |s| async move { audit_round(&s).await }),
        every(s.clone(), s.cfg.probe_every, "probe", |s| async move { probe_round(&s, false).await }),
        // Frequent, because a new member is held to the unmeasured rate until this has run.
        every(s.clone(), s.cfg.reconcile_every.min(Duration::from_secs(2)), "probe-new", |s| async move {
            if s.cfg.probe_new_nodes {
                probe_round(&s, true).await?;
            }
            Ok(())
        }),
    ]
}

/// How long to wait before re-probing a new node whose first probe got no result.
const PROBE_RETRY_SECS: f64 = 60.0;

/// A piece is healthy if stored on an active host seen within the grace period.
fn healthy(state: &str, node: Option<&NodeDoc>, cutoff: f64) -> bool {
    state == "stored" && node.is_some_and(|n| n.state == "active" && n.last_seen >= cutoff)
}

async fn nodes_by_id(s: &Shared, ids: &[i64]) -> DbResult<HashMap<i64, NodeDoc>> {
    Ok(find_all(&s.db.nodes, doc! {"_id": {"$in": ids}}).await?.into_iter().map(|n| (n.id, n)).collect())
}

fn hosts_of(g: &SegmentDoc) -> Vec<i64> {
    g.pieces.iter().map(|p| p.host).collect()
}

/// Record a repair of `piece` onto `target`, unless one is already in flight.
async fn start_repair(s: &Shared, seg: &str, piece: i32, target: i64) -> DbResult<()> {
    let repair = RepairDoc { id: s.db.next_id("repairs").await?, target, started_at: fnow(), delivered: false };
    s.db.segments
        .update_one(doc! {"_id": seg, "pieces": {"$elemMatch": {"piece": piece, "repair": null}}}, doc! {"$set": {"pieces.$.repair": to_bson(&repair)}})
        .await?;
    Ok(())
}

/// Forget repair `id` of `piece`, so it can be planned again.
async fn clear_repair(s: &Shared, seg: &str, piece: i32, id: i64) -> DbResult<()> {
    s.db.segments.update_one(doc! {"_id": seg, "pieces": {"$elemMatch": {"piece": piece, "repair.id": id}}}, doc! {"$set": {"pieces.$.repair": null}}).await?;
    Ok(())
}

/// Schedule repairs for segments below their policy threshold.
async fn repair_round(s: &Shared) -> DbResult<()> {
    drain(s).await?;
    let cutoff = fnow() - s.cfg.grace_secs;
    // A target that went away before collecting its work would otherwise block this piece
    // until the timeout below. Re-plan it straight away. Repairs that never finished are
    // abandoned too, so they can be retried elsewhere.
    let reachable = (fnow() - s.cfg.reconcile_every.as_secs_f64() * 4.0 - 5.0).max(cutoff);
    let in_flight = db::aggregate(
        &s.db.segments,
        vec![
            doc! {"$match": {"pieces.repair": {"$ne": null}}},
            doc! {"$unwind": "$pieces"},
            doc! {"$match": {"pieces.repair": {"$ne": null}}},
            doc! {"$project": {"piece": "$pieces.piece", "repair": "$pieces.repair"}},
        ],
        None,
    )
    .await?;
    let in_flight: Vec<(String, i32, RepairDoc)> = in_flight
        .into_iter()
        .filter_map(|d| {
            let r: RepairDoc = mongodb::bson::from_bson(d.get("repair")?.clone()).ok()?;
            Some((d.get_str("_id").ok()?.to_string(), int(d.get("piece")) as i32, r))
        })
        .collect();
    let targets = nodes_by_id(s, &in_flight.iter().map(|x| x.2.target).collect::<Vec<_>>()).await?;
    for (seg, piece, r) in &in_flight {
        let gone = !r.delivered && targets.get(&r.target).is_none_or(|n| n.last_seen < reachable);
        if gone || r.started_at < fnow() - s.cfg.repair_timeout_secs {
            clear_repair(s, seg, *piece, r.id).await?;
        }
    }

    // Segments below the policy threshold, most at risk first. Policy is per pool.
    let mut at_risk: Vec<(usize, SegmentDoc)> = vec![];
    for pool in find_all(&s.db.pools, doc! {}).await? {
        let ok: Vec<i64> =
            s.db.nodes.distinct("_id", doc! {"pool": pool.id, "state": "active", "last_seen": {"$gte": cutoff}}).await?.iter().map(|b| int(Some(b))).collect();
        let healthy_count = doc! {"$size": {"$filter": {"input": "$pieces", "as": "p", "cond": {"$and": [
        {"$eq": ["$$p.state", "stored"]}, {"$in": ["$$p.host", &ok]}]}}}};
        let rows = db::aggregate(
            &s.db.segments,
            vec![
                doc! {"$match": {"pool": pool.id, "state": "committed"}},
                doc! {"$addFields": {"healthy": healthy_count}},
                doc! {"$match": {"$expr": {"$lt": ["$healthy", {"$min": [pool.policy.repair_at as i32, "$n"]}]}}},
                doc! {"$sort": {"healthy": 1}},
                doc! {"$limit": s.cfg.repairs_per_round},
            ],
            None,
        )
        .await?;
        for d in rows {
            let h = int(d.get("healthy")) as usize;
            at_risk.push((h, mongodb::bson::from_document(d)?));
        }
    }
    at_risk.sort_by_key(|x| x.0);
    at_risk.truncate(s.cfg.repairs_per_round as usize);

    let mut rng = StdRng::from_entropy();
    for (_, g) in at_risk {
        let (policy, _, distinct_sites) = pool_settings(&s.db, g.pool).await?;
        let nodes = nodes_by_id(s, &hosts_of(&g)).await?;
        let (cands, _) = candidates(s, g.pool, (s.cfg.reconcile_every.as_secs_f64() * 4.0 + 5.0).min(s.cfg.grace_secs)).await?;
        let candidate_of = |n: &NodeDoc| globalobject_policy::Candidate {
            host: n.id as u64,
            owner: n.account as u64,
            site: stable(&n.site),
            asn: stable(&n.asn),
            free_bytes: 0,
            reputation: 0.0,
        };
        let healthy_nodes: Vec<&NodeDoc> = g.pieces.iter().filter(|p| healthy(&p.state, nodes.get(&p.host), cutoff)).map(|p| &nodes[&p.host]).collect();
        let mut taken_domains: Vec<u64> = healthy_nodes.iter().map(|n| candidate_of(n).domain(policy.domain_level)).collect();
        let mut taken_sites: Vec<u64> = healthy_nodes.iter().map(|n| stable(&n.site)).collect();
        for p in &g.pieces {
            if healthy(&p.state, nodes.get(&p.host), cutoff) || p.repair.is_some() {
                continue;
            }
            let req = PlacementRequest {
                count: 1,
                piece_bytes: g.piece_len as u64,
                exclude_owner: Some(g.owner as u64),
                distinct_sites,
                taken_domains: taken_domains.clone(),
                taken_sites: taken_sites.clone(),
            };
            let Ok(chosen) = place(&cands, policy.domain_level, &req, &mut rng) else {
                tracing::warn!("no host available to repair {}/{}", g.id, p.piece);
                continue;
            };
            let c = cands[chosen[0]];
            taken_domains.push(c.domain(policy.domain_level));
            taken_sites.push(c.site);
            start_repair(s, &g.id, p.piece, c.host as i64).await?;
        }
    }
    Ok(())
}

fn stable(s: &str) -> u64 {
    u64::from_le_bytes(globalobject_crypto::hash(s.as_bytes())[..8].try_into().unwrap())
}

/// Mark pieces on shrinking or leaving hosts as needing a new home, until the host fits.
async fn drain(s: &Shared) -> DbResult<()> {
    let nodes = find_all(&s.db.nodes, doc! {"target_pledged": {"$ne": null}}).await?;
    for n in nodes {
        let (id, Some(target)) = (n.id, n.target_pledged) else { continue };
        let held = bytes_by(&s.db, "pieces.host", &[id], doc! {}, None).await?.get(&id).copied().unwrap_or(0);
        if held <= target {
            // Everything moved: release the capacity, and a leaving host is done.
            let state = if n.state == "draining" { "left" } else { n.state.as_str() };
            s.db.nodes.update_one(doc! {"_id": id}, doc! {"$set": {"pledged": target, "target_pledged": null, "state": state}}).await?;
            continue;
        }
        // Move enough pieces, in one pass, onto other hosts.
        let mut rows =
            s.db.segments
                .find(doc! {"state": "committed", "pieces": {"$elemMatch": {"host": id, "state": "stored", "repair": null}}})
                .limit(s.cfg.repairs_per_round)
                .await?;
        let in_flight = bytes_by(&s.db, "pieces.host", &[id], doc! {"repair": {"$ne": null}}, None).await?.get(&id).copied().unwrap_or(0);
        let mut remaining = held - in_flight - target;
        let mut rng = StdRng::from_entropy();
        use futures::TryStreamExt;
        while let Some(g) = rows.try_next().await? {
            if remaining <= 0 {
                break;
            }
            let Some(p) = g.pieces.iter().find(|p| p.host == id && p.state == "stored" && p.repair.is_none()) else { continue };
            let (policy, _, distinct_sites) = pool_settings(&s.db, g.pool).await?;
            let existing = nodes_by_id(s, &hosts_of(&g)).await?;
            let mut taken_domains = vec![];
            let mut taken_sites = vec![];
            for e in existing.values() {
                let c = globalobject_policy::Candidate {
                    host: e.id as u64,
                    owner: e.account as u64,
                    site: stable(&e.site),
                    asn: stable(&e.asn),
                    free_bytes: 0,
                    reputation: 0.0,
                };
                taken_domains.push(c.domain(policy.domain_level));
                taken_sites.push(c.site);
            }
            let (cands, _) = candidates(s, g.pool, (s.cfg.reconcile_every.as_secs_f64() * 4.0 + 5.0).min(s.cfg.grace_secs)).await?;
            let req =
                PlacementRequest { count: 1, piece_bytes: g.piece_len as u64, exclude_owner: Some(g.owner as u64), distinct_sites, taken_domains, taken_sites };
            let Ok(chosen) = place(&cands, policy.domain_level, &req, &mut rng) else { continue };
            start_repair(s, &g.id, p.piece, cands[chosen[0]].host as i64).await?;
            remaining -= g.piece_len;
        }
    }
    Ok(())
}

/// Work for one node, returned in its heartbeat reply.
pub async fn tasks_for(s: &Shared, node: i64) -> DbResult<Vec<Task>> {
    let mut tasks = vec![];
    let me = s.db.nodes.find_one(doc! {"_id": node}).await?.ok_or_else(|| missing("node"))?;
    let (account, pool, my_endpoint) = (me.account, me.pool, me.endpoint);

    // Delete tasks are idempotent, so one handed out twice by overlapping heartbeats is harmless.
    let deletes = find_all(&s.db.pending_deletes, doc! {"host": node}).await?;
    for d in &deletes {
        let grant = sign_grant(s, account, pool, &d.segment, d.piece, node, &my_endpoint, "", Op::Delete, 0, 0);
        tasks.push(Task::Delete { segment: d.segment.clone(), piece: d.piece as u16, grant: grant.token });
        s.db.pending_deletes.delete_one(doc! {"host": node, "segment": &d.segment, "piece": d.piece}).await?;
    }

    for _ in 0..2 {
        let Some(p) = s.db.pending_probes.find_one_and_delete(doc! {"prober": node}).await? else { break };
        let Some(t) = s.db.nodes.find_one(doc! {"_id": p.target}).await? else { continue };
        let grant = sign_grant(s, account, pool, "probe", 0, p.target, &my_endpoint, &t.probe_hash, Op::Probe, t.probe_bytes as u64, 0);
        tasks.push(Task::Probe { target: p.target, addr: t.addr, grant: grant.token, hash: t.probe_hash, bytes: t.probe_bytes as u64 });
    }

    // Up to four repairs not yet handed out, each claimed by a conditional update so that
    // overlapping heartbeats can't both deliver it.
    let waiting = find_all(&s.db.segments, doc! {"pieces": {"$elemMatch": {"repair.target": node, "repair.delivered": false}}}).await?;
    let mut claimed = 0;
    for g in waiting {
        for p in &g.pieces {
            let Some(r) = p.repair.as_ref().filter(|r| r.target == node && !r.delivered) else { continue };
            if claimed >= 4 {
                break;
            }
            let res =
                s.db.segments
                    .update_one(
                        doc! {"_id": &g.id, "pieces": {"$elemMatch": {"piece": p.piece, "repair.id": r.id, "repair.delivered": false}}},
                        doc! {"$set": {"pieces.$.repair.delivered": true, "pieces.$.repair.started_at": fnow()}},
                    )
                    .await?;
            if res.modified_count == 0 {
                continue;
            }
            claimed += 1;
            if let Some(t) = repair_task(s, &g, p.piece, r.id, node, &my_endpoint).await? {
                tasks.push(t);
            }
        }
    }
    Ok(tasks)
}

async fn repair_task(s: &Shared, g: &SegmentDoc, piece: i32, id: i64, node: i64, my_endpoint: &str) -> DbResult<Option<Task>> {
    let (k, n) = (g.k, g.n);
    let layout = globalobject_segment::Layout::new(k as usize, n as usize, g.local_groups as usize);
    // Any reachable copy can be a source, including the one being moved off a leaving host.
    let online = fnow() - s.cfg.reconcile_every.as_secs_f64().max(1.0) * 3.0 - 3.0;
    let nodes = nodes_by_id(s, &hosts_of(g)).await?;
    let mut rows: Vec<_> = g
        .pieces
        .iter()
        .filter(|p| p.state == "stored" && nodes.get(&p.host).is_some_and(|n| n.last_seen >= online && (n.state == "active" || n.state == "draining")))
        .collect();
    rows.shuffle(&mut StdRng::from_entropy());
    // When the target's local group is all reachable, it rebuilds the target alone: put it
    // first, and the repairing host stops fetching once it can rebuild.
    // A piece being moved off a draining host is itself a source, and the cheapest one.
    let reachable: Vec<bool> = (0..n).map(|i| rows.iter().any(|r| r.piece == i)).collect();
    let group = layout.group_of(piece as usize).map(|g| layout.group_members(g)).filter(|g| g.iter().all(|&i| i == piece as usize || reachable[i]));
    rows.sort_by_key(|r| (r.piece != piece, !group.as_ref().is_some_and(|g| g.contains(&(r.piece as usize)))));
    let Some(expected_hash) = g.pieces.iter().find(|p| p.piece == piece).map(|p| p.hash.clone()) else { return Ok(None) };
    if !layout.can_rebuild(piece as usize, &reachable) {
        // Too few reachable now. Clear it, and a later round retries.
        clear_repair(s, &g.id, piece, id).await?;
        return Ok(None);
    }
    let sources = rows
        .iter()
        .map(|r| {
            let grant = sign_grant(s, g.owner, g.pool, &g.id, r.piece, r.host, my_endpoint, &r.hash, Op::RepairRead, g.piece_len as u64, 0);
            RepairSource { addr: nodes[&r.host].addr.clone(), piece: r.piece as u16, grant: grant.token, hash: r.hash.clone() }
        })
        .collect();
    let write_grant = sign_grant(s, g.owner, g.pool, &g.id, piece, node, my_endpoint, &expected_hash, Op::Write, g.piece_len as u64, 0);
    Ok(Some(Task::Repair {
        repair_id: id,
        segment: g.id.clone(),
        piece: piece as u16,
        k: k as u16,
        n: n as u16,
        local_groups: g.local_groups as u16,
        piece_len: g.piece_len as u64,
        sources,
        expected_hash,
        write_grant: write_grant.token,
    }))
}

pub async fn finish_repair(s: &Shared, node: i64, req: &RepairDone) -> DbResult<()> {
    let Some(g) = s.db.segments.find_one(doc! {"pieces.repair.id": req.repair_id}).await? else { return Ok(()) };
    let Some(p) = g.pieces.iter().find(|p| p.repair.as_ref().is_some_and(|r| r.id == req.repair_id)) else { return Ok(()) };
    let target = p.repair.as_ref().map_or(0, |r| r.target);
    let (seg, piece, old) = (g.id.clone(), p.piece, p.host);
    let key = s.db.nodes.find_one(doc! {"_id": node}).await?.ok_or_else(|| missing("node"))?.public_key;
    let Ok(rc) = globalobject_crypto::open_token::<StoreReceipt>(&key, T_STORE, &req.receipt) else {
        tracing::warn!("rejected repair receipt from node {node}");
        return Ok(());
    };
    if target != node || rc.segment != seg || rc.piece as i32 != piece || rc.host != node || rc.hash != p.hash {
        tracing::warn!("repair receipt mismatch from node {node}");
        return Ok(());
    }
    let moved = transaction!(s.db, sess, {
        let r =
            s.db.segments
                .update_one(
                    doc! {"_id": &seg, "pieces": {"$elemMatch": {"piece": piece, "repair.id": req.repair_id}}},
                    doc! {"$set": {"pieces.$.host": node, "pieces.$.state": "stored", "pieces.$.repair": null}},
                )
                .session(&mut sess)
                .await?;
        if r.matched_count == 0 {
            // Removed, or cleared and re-planned, meanwhile.
            return Ok(false);
        }
        // The old holder removes its copy now if here, or on return. Deleted only after the move.
        queue_delete(s, old, &seg, piece, Some(&mut sess)).await?;
        Ok::<_, db::Error>(true)
    })?;
    if moved {
        s.metrics.repairs_completed.inc();
        ledger(&s.db, g.pool, g.owner, "repair", rc.bytes as i64, &seg).await?;
    }
    Ok(())
}

async fn queue_delete(s: &Shared, host: i64, seg: &str, piece: i32, session: Option<&mut mongodb::ClientSession>) -> DbResult<()> {
    let f = doc! {"host": host, "segment": seg, "piece": piece};
    let op = s.db.pending_deletes.update_one(f.clone(), doc! {"$setOnInsert": f}).upsert(true);
    match session {
        Some(sess) => op.session(sess).await?,
        None => op.await?,
    };
    Ok(())
}

pub async fn remove_segment(s: &Shared, seg: &str, owner: Option<i64>, only_state: Option<&str>) -> DbResult<()> {
    transaction!(s.db, sess, {
        let Some(g) = s.db.segments.find_one(doc! {"_id": seg}).session(&mut sess).await? else { return Ok(()) };
        if owner.is_some_and(|x| x != g.owner) || only_state.is_some_and(|x| x != g.state) {
            return Ok(());
        }
        for p in &g.pieces {
            queue_delete(s, p.host, seg, p.piece, Some(&mut sess)).await?;
        }
        s.db.segments.delete_one(doc! {"_id": seg}).session(&mut sess).await?;
        // The file name pointing at it goes too.
        s.db.objects.delete_many(doc! {"segment": seg}).session(&mut sess).await?;
        Ok::<_, db::Error>(())
    })
}

/// Uploads that never committed release their quota and space.
async fn pending_round(s: &Shared) -> DbResult<()> {
    let stale = s.db.segments.distinct("_id", doc! {"state": "pending", "created_at": {"$lt": fnow() - s.cfg.pending_timeout_secs}}).await?;
    for seg in stale.iter().filter_map(|b| b.as_str()) {
        remove_segment(s, seg, None, Some("pending")).await?;
    }
    Ok(())
}

/// Over quota → grace → read-only → deletion. Your data lasts as long as your contribution.
async fn lifecycle_round(s: &Shared) -> DbResult<()> {
    let accounts = find_all(&s.db.accounts, doc! {"state": {"$ne": "purged"}}).await?;
    for a in accounts {
        let (id, pool) = (a.id, a.pool);
        let (_, q, _) = pool_settings(&s.db, pool).await?;
        let u = usage(s, id, &q).await?;
        let t = fnow();
        if u.used_raw <= u.quota_raw {
            // Back in credit. A removed member stays read-only, since their nodes have left.
            let removed = s.db.nodes.count_documents(doc! {"account": id, "state": {"$in": ["draining", "left"]}}).await? > 0;
            if !removed && (a.over_since.is_some() || a.state == "read_only") {
                s.db.accounts.update_one(doc! {"_id": id}, doc! {"$set": {"over_since": null, "read_only_since": null, "state": "active"}}).await?;
            }
            continue;
        }
        match (a.state.as_str(), a.over_since, a.read_only_since) {
            ("active", None, _) => {
                s.db.accounts.update_one(doc! {"_id": id}, doc! {"$set": {"over_since": t}}).await?;
            }
            ("active", Some(since), _) if t - since >= s.cfg.over_quota_grace_secs => {
                s.db.accounts.update_one(doc! {"_id": id}, doc! {"$set": {"state": "read_only", "read_only_since": t}}).await?;
            }
            ("read_only", _, Some(since)) if t - since >= s.cfg.read_only_secs => {
                let segs = s.db.segments.distinct("_id", doc! {"owner": id}).await?;
                for seg in segs.iter().filter_map(|b| b.as_str()) {
                    remove_segment(s, seg, Some(id), None).await?;
                }
                s.db.accounts.update_one(doc! {"_id": id}, doc! {"$set": {"state": "purged"}}).await?;
                ledger(&s.db, pool, id, "purged", u.used_raw as i64, "").await?;
            }
            _ => {}
        }
    }
    Ok(())
}

/// Several different accounts reporting refusals is how a false pledge surfaces.
async fn refusals_round(s: &Shared) -> DbResult<()> {
    // `refused_by` is a set of reporting accounts: at least N of them means element N-1 exists.
    let nth = format!("refused_by.{}", (s.cfg.refusals_to_disqualify - 1).max(0));
    let hosts = find_all(
        &s.db.nodes,
        doc! {"state": "active", "last_seen": {"$gte": fnow() - s.cfg.reconcile_every.as_secs_f64() * 4.0 - 5.0}, nth: {"$exists": true}},
    )
    .await?;
    for n in hosts {
        tracing::warn!("host {} refused placements from several accounts; disqualifying", n.id);
        s.db.nodes.update_one(doc! {"_id": n.id}, doc! {"$set": {"state": "disqualified"}}).await?;
    }
    Ok(())
}

async fn audit_round(s: &Arc<Shared>) -> DbResult<()> {
    let online = fnow() - s.cfg.audit_every.as_secs_f64() * 2.0 - 5.0;
    let nodes: HashMap<i64, NodeDoc> =
        find_all(&s.db.nodes, doc! {"state": "active", "last_seen": {"$gte": online}}).await?.into_iter().map(|n| (n.id, n)).collect();
    let ids: Vec<i64> = nodes.keys().copied().collect();
    if ids.is_empty() {
        return Ok(());
    }
    // Random stored pieces of committed segments, on hosts that are up.
    let rows = db::aggregate(
        &s.db.segments,
        vec![
            doc! {"$match": {"state": "committed", "pieces": {"$elemMatch": {"state": "stored", "host": {"$in": &ids}}}}},
            doc! {"$unwind": "$pieces"},
            doc! {"$match": {"pieces.state": "stored", "pieces.host": {"$in": &ids}}},
            doc! {"$sample": {"size": s.cfg.audits_per_round as i64}},
            doc! {"$project": {"piece": "$pieces.piece", "host": "$pieces.host", "hash": "$pieces.hash", "piece_len": 1, "pool": 1, "owner": 1}},
        ],
        None,
    )
    .await?;
    let me = s.endpoint.id().to_string();
    let mut set = tokio::task::JoinSet::new();
    for r in rows {
        let (Ok(seg), Ok(hash)) = (r.get_str("_id"), r.get_str("hash")) else { continue };
        let (seg, hash) = (seg.to_string(), hash.to_string());
        let (piece, host, piece_len, pool, owner) =
            (int(r.get("piece")) as i32, int(r.get("host")), int(r.get("piece_len")), int(r.get("pool")), int(r.get("owner")));
        let Some(addr) = nodes.get(&host).map(|n| n.addr.clone()) else { continue };
        let (shared, me) = (s.clone(), me.clone());
        set.spawn(async move {
            let attempt = || async {
                let grant = sign_grant(&shared, owner, pool, &seg, piece, host, &me, &hash, Op::Audit, 0, 0);
                tokio::time::timeout(Duration::from_secs(10), audit_one(&shared.endpoint, &addr, grant.token, &hash, piece_len as u64)).await.unwrap_or(None)
            };
            let mut outcome = attempt().await;
            if outcome == Some(false) {
                // A host going offline mid-audit looks the same as one failing a proof, and failures
                // add up. So only a failure repeated by a host that's still reachable counts.
                outcome = attempt().await;
            }
            (host, seg, piece, outcome)
        });
    }
    while let Some(res) = set.join_next().await {
        let Ok((host, seg, piece, passed)) = res else { continue };
        let Some(passed) = passed else { continue };
        if !passed {
            // The piece may have been moved away during the audit, which is no fault of the host's.
            let still = s.db.segments.count_documents(doc! {"_id": &seg, "pieces": {"$elemMatch": {"piece": piece, "host": host}}}).await? > 0;
            if !still {
                continue;
            }
        }
        let result = if passed { "pass" } else { "fail" };
        s.metrics.audits.get_or_create(&crate::ops::ResultLabels { result: result.into() }).inc();
        if passed {
            s.db.nodes.update_one(doc! {"_id": host}, doc! {"$inc": {"audit_ok": 1_i64}}).await?;
        } else {
            // Cumulative, not consecutive. Honest hosts don't fail proofs, and passes on other pieces
            // mustn't excuse a host that has corrupted one.
            let n =
                s.db.nodes
                    .find_one_and_update(doc! {"_id": host}, doc! {"$inc": {"audit_fail": 1_i64}})
                    .return_document(ReturnDocument::After)
                    .await?
                    .ok_or_else(|| missing("node"))?;
            tracing::warn!("host {host} failed an audit ({} so far)", n.audit_fail);
            if n.audit_fail >= s.cfg.disqualify_after as i64 {
                s.db.nodes.update_one(doc! {"_id": host}, doc! {"$set": {"state": "disqualified"}}).await?;
            }
        }
    }
    Ok(())
}

/// Some(true) passed, Some(false) failed, None if the host couldn't be reached (not counted here).
async fn audit_one(ep: &iroh::Endpoint, addr: &str, grant: String, hash: &str, piece_len: u64) -> Option<bool> {
    // 16 KiB at a random position. The proof is checked against the hash alone.
    const CHUNKS: u64 = 16;
    let total = piece_len.div_ceil(1024).max(1);
    let chunk = StdRng::from_entropy().gen_range(0..total.saturating_sub(CHUNKS).max(1));
    match globalobject_net::peer_call(ep, addr, &PeerRequest::Authorize { grant }).await {
        Ok(PeerReply::Ok) => {}
        Ok(PeerReply::Refused { reason }) if reason == "unavailable" => return None,
        Ok(other) => {
            tracing::warn!("audit of {hash} at {addr}: authorisation refused: {other:?}");
            return Some(false);
        }
        Err(_) => return None,
    }
    let r = globalobject_net::audit_range(ep, addr, hash, chunk, CHUNKS).await;
    if !matches!(r, Ok(true)) {
        tracing::warn!("audit of {hash} chunk {chunk}: {r:?}");
    }
    r.ok()
}

/// Ask a random peer to measure each node. One prober per target per round, because
/// overlapping probes share the link and would each under-measure it. Keep `probe_every`
/// well above the time one probe takes. Samples build up over rounds, and the median is used.
///
/// With `only_unmeasured`, covers just nodes with no measurement yet, so that a new member
/// isn't held at the unmeasured rate until the next scheduled round.
async fn probe_round(s: &Shared, only_unmeasured: bool) -> DbResult<()> {
    let t = fnow();
    let online = t - s.cfg.reconcile_every.as_secs_f64() * 4.0 - 5.0;
    let nodes = find_all(&s.db.nodes, doc! {"state": "active", "last_seen": {"$gte": online}}).await?;
    let mut rng = StdRng::from_entropy();
    for n in &nodes {
        // A probe on a slow link can take a while. Don't start another on top of it.
        if only_unmeasured && (n.measured_bps.is_some() || n.probe_requested_at.is_some_and(|r| t - r < PROBE_RETRY_SECS)) {
            continue;
        }
        if s.db.pending_probes.count_documents(doc! {"target": n.id}).await? > 0 {
            continue;
        }
        let peers: Vec<i64> = nodes.iter().filter(|p| p.id != n.id && p.pool == n.pool && p.account != n.account).map(|p| p.id).collect();
        if let Some(&prober) = peers.choose(&mut rng) {
            s.db.pending_probes.insert_one(db::PendingProbeDoc { prober, target: n.id }).await?;
            s.db.nodes.update_one(doc! {"_id": n.id}, doc! {"$set": {"probe_requested_at": t}}).await?;
        }
    }
    Ok(())
}

pub async fn record_probe(s: &Shared, _prober: i64, r: &ProbeResult) -> DbResult<()> {
    // Keep only the latest samples, so the array stays small.
    let n =
        s.db.nodes
            .find_one_and_update(doc! {"_id": r.target}, doc! {"$push": {"probe_samples": {"$each": [r.bps], "$slice": -s.cfg.probe_samples}}})
            .return_document(ReturnDocument::After)
            .await?;
    if let Some(m) = n.and_then(|n| globalobject_policy::median(&n.probe_samples)) {
        s.db.nodes.update_one(doc! {"_id": r.target}, doc! {"$set": {"measured_bps": m}}).await?;
    }
    Ok(())
}
