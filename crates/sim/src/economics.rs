//! Quota economics, attacks and bandwidth rule checks.

use globalobject_policy::{credited, place, quota_raw, Candidate, DomainLevel, PlacementRequest, Policy, QuotaParams};
use rand::{rngs::SmallRng, SeedableRng};

const GB: u64 = 1_000_000_000;
const TB: u64 = 1_000 * GB;

pub struct Grow {
    pub fill: f64,
    pub stalled: bool,
}

/// Every user wants as much as quota allows. Rounds of upload until nothing changes.
pub fn growth(users: usize, pledged: u64, q: &QuotaParams, policy: &Policy, seed: u64) -> Grow {
    let mut rng = SmallRng::seed_from_u64(seed);
    let mut held = vec![0u64; users];
    let mut used = vec![0u64; users];
    let piece = 64 * GB / policy.k as u64 / 64; // 1 GB of segment per k
    let mut rounds = 0;
    loop {
        rounds += 1;
        let mut progressed = false;
        for u in 0..users {
            let quota = quota_raw(credited(pledged, held[u], 1.0, q), q);
            let raw = piece * policy.n as u64;
            let mut budget = 64;
            while used[u] + raw <= quota && budget > 0 {
                let cands: Vec<Candidate> = (0..users)
                    .map(|h| Candidate { host: h as u64, owner: h as u64, site: h as u64, asn: h as u64, free_bytes: pledged - held[h], reputation: 1.0 })
                    .collect();
                let req = PlacementRequest { count: policy.n as usize, piece_bytes: piece, exclude_owner: Some(u as u64), ..Default::default() };
                let Ok(chosen) = place(&cands, DomainLevel::Owner, &req, &mut rng) else { break };
                for h in chosen {
                    held[h] += piece;
                }
                used[u] += raw;
                progressed = true;
                budget -= 1;
            }
        }
        if !progressed || rounds > 4000 {
            break;
        }
    }
    let total_held: u64 = held.iter().sum();
    let fill = total_held as f64 / (users as u64 * pledged) as f64;
    Grow { fill, stalled: fill < (1.0 - q.reserve) * 0.95 }
}

fn pct(x: f64) -> String {
    format!("{:.1}%", x * 100.0)
}

pub fn report() {
    println!("## 7. Does a pool actually fill? (headroom invariant)\n");
    println!("50 users pledge 1 TB each with a 10 GB allowance and unlimited demand, at 1-of-3. 'Full' means reaching (1 - reserve) of capacity.\n");
    println!("| reserve | headroom | headroom × (1 − reserve) | Final fill | Result |\n|---|---|---|---|---|");
    for (reserve, headroom) in [(0.2, 1.0), (0.2, 1.2), (0.2, 1.3), (0.2, 1.5), (0.2, 2.0), (0.1, 1.0), (0.1, 1.2), (0.3, 1.5)] {
        let q = QuotaParams { reserve, headroom, probation_allowance: 10 * GB };
        let g = growth(50, TB, &q, &Policy::replicate3(), 11);
        println!("| {reserve} | {headroom} | {:.2} | {} | {} |", headroom * (1.0 - reserve), pct(g.fill), if g.stalled { "**stalls**" } else { "fills" });
    }
    println!();

    println!("## 8. Can a full pool recover from host loss?\n");
    println!("Pool at its fill limit loses a share of hosts at once. Spare is what remains on survivors, Needed is the lost data to re-store.\n");
    println!("| reserve | Hosts lost | Spare | Needed | Result |\n|---|---|---|---|---|");
    for reserve in [0.05, 0.1, 0.2, 0.3] {
        for lost in [0.05, 0.1, 0.2, 0.3] {
            let fill = 1.0 - reserve;
            let spare = (1.0 - lost) * reserve;
            let needed = lost * fill;
            println!("| {reserve} | {} | {:.3} | {:.3} | {} |", pct(lost), spare, needed, if spare >= needed { "recovers" } else { "**short of space**" });
        }
    }
    println!();

    println!("## 9. Usable space per TB contributed\n");
    println!("| Policy | Expansion | Usable at 20% reserve | Usable at 10% reserve |\n|---|---|---|---|");
    for (name, p) in [
        ("1-of-3", Policy::replicate3()),
        ("1-of-5", Policy { k: 1, n: 5, repair_at: 4, ..Policy::replicate3() }),
        ("6-of-12", Policy::ec(6, 12, 9)),
        ("10-of-16", Policy::ec(10, 16, 13)),
        ("10-of-20", Policy::ec(10, 20, 15)),
    ] {
        let u = |reserve: f64| (1.0 - reserve) / p.expansion() * 1000.0;
        println!("| {name} | {:.1}x | {:.0} GB | {:.0} GB |", p.expansion(), u(0.2), u(0.1));
    }
    println!();

    attacks();
    bandwidth();
}

fn attacks() {
    println!("## 10. Attacks\n");
    println!("**Sybil share of segments, using real placement** (1,000 hosts, 100,000 placements)\n");
    println!("| Attacker hosts | All replicas of 1-of-3 | Analytic f³ | More than 10 of 20 pieces (enough to destroy a 10-of-20 segment) |\n|---|---|---|---|");
    let mut rng = SmallRng::seed_from_u64(21);
    for f in [0.01, 0.1, 0.33] {
        let cands: Vec<Candidate> = (0..1000).map(|h| Candidate { host: h, owner: h, site: h, asn: h, free_bytes: TB, reputation: 1.0 }).collect();
        let bad = (1000.0 * f) as usize;
        let trials = 100_000;
        let (mut all3, mut ec_kill) = (0, 0);
        for _ in 0..trials {
            let r = place(&cands, DomainLevel::Owner, &PlacementRequest { count: 3, piece_bytes: 1, ..Default::default() }, &mut rng).unwrap();
            if r.iter().all(|&i| i < bad) {
                all3 += 1;
            }
            let r = place(&cands, DomainLevel::Owner, &PlacementRequest { count: 20, piece_bytes: 1, ..Default::default() }, &mut rng).unwrap();
            if r.iter().filter(|&&i| i < bad).count() > 10 {
                ec_kill += 1;
            }
        }
        println!("| {} | {:.4}% | {:.4}% | {:.4}% |", pct(f), all3 as f64 / trials as f64 * 100.0, f * f * f * 100.0, ec_kill as f64 / trials as f64 * 100.0);
    }
    println!();

    println!("**Sybil using one site for its identities.** With the per-site rule, a single-site attacker holds at most one piece of any segment regardless of identity count. The prototype enforces this in the public pool and tests it.\n");

    println!("**False pledge.** Attacker pledges 10 TB and really has 1 TB, with reserve 20% and headroom 1.5. Credit is bounded by proof:\n");
    println!("| Really holding | Credit | Raw quota | Note |\n|---|---|---|---|");
    let q = QuotaParams::default();
    for held in [0, 100 * GB, 500 * GB, TB] {
        let c = credited(10 * TB, held, 1.0, &q);
        println!(
            "| {} GB | {} GB | {} GB | {} |",
            held / GB,
            c / GB,
            quota_raw(c, &q) / GB,
            if held == TB { "next placement is refused, and the lie surfaces" } else { "honest so far" }
        );
    }
    println!("\nThe attacker's gain is `1.2 × held + 8 GB` raw while holding `held`, compared with `0.8 × held` for an honest participant with a true pledge at capacity. At the moment the disk fills, a liar has 1,208 GB raw against an honest 800 GB, an overdraft of about 51%. Once placements start failing, credit falls back and the lifecycle moves them to read-only. Lower `headroom` reduces this, but section 7 shows it can't go below about 1.3 at 20% reserve.\n");

    println!("**Hit and run.** A leaver's data lasts through grace and read-only (44 days provisional). The network pays one repair of the bytes they held, and they keep nothing. At 20%/yr honest loss, this is ordinary churn unless many do it at once.\n");

    println!("**Identical replicas.** An operator holding several copies of a segment under different identities needs to keep only one. This is an accepted risk at `k=1`, and erasure-coded pieces aren't affected. This table gives how much disk the attack saves.\n");
    println!("| Attacker share f | Expected pieces per segment on attacker | Segments with at least one | Attacker's disk saved |\n|---|---|---|---|");
    for f in [0.01f64, 0.1, 0.33] {
        let pieces = 3.0 * f;
        let any = 1.0 - (1.0 - f).powi(3);
        println!("| {} | {:.3} | {} | {} |", pct(f), pieces, pct(any), pct((pieces - any) / pieces));
    }
    println!("\nThe saving is roughly `f`: whenever an attacker holds two or more copies, it needs to keep just one. It also collapses the real number of independent copies, so the durability harm exceeds the disk saving.\n");
}

fn bandwidth() {
    println!("## 11. The bandwidth rule\n");
    println!("Download limited to measured upload (with no banked credit), and 1 TB of logical data at 1-of-3.\n");
    println!("| Link | Upload time (3 TB) | Restore 1 TB at entitled rate | Restore with banked credit |\n|---|---|---|---|");
    for (name, down, up) in [
        ("500/20", 500.0, 20.0),
        ("100/20", 100.0, 20.0),
        ("80/20", 80.0, 20.0),
        ("1000/100", 1000.0, 100.0),
        ("1000/1000", 1000.0, 1000.0),
        ("50/10", 50.0, 10.0),
    ] {
        let days = |bytes: f64, mbps: f64| bytes * 8.0 / (mbps * 1e6) / 86400.0;
        // Bank = serving at 10% of upload for 90 days.
        let bank: f64 = up * 1e6 / 8.0 * 0.10 * 90.0 * 86400.0;
        let remaining = (1e12 - bank).max(0.0);
        let with_bank = days(remaining, up) + days(1e12 - remaining, down);
        println!("| {name} | {:.1} d | {:.1} d | {:.1} d |", days(3e12, up), days(1e12, up), with_bank);
    }
    println!("\nThe last column assumes 90 days of serving at 10% of upload banked, and spending it at the link's download speed.\n");
    println!("**Everyone restores at once** (regional disaster): sustained demand can't exceed supply, because each reader's entitled rate equals what they offer. That argument assumes accurate measurement, and it ignores repair traffic, offline hosts and banked bursts, all of which can make real supply fall short for a while. It isn't tested by this simulator.\n");
}
