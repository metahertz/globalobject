//! Discrete-event churn simulation: hosts go up and down, die, and are repaired around.

use globalobject_policy::{place, Candidate, DomainLevel, PlacementRequest, Policy};
use rand::{rngs::SmallRng, Rng, SeedableRng};
use std::cmp::Reverse;
use std::collections::{BinaryHeap, VecDeque};

const HOUR: f64 = 1.0;
const DAY: f64 = 24.0;
const YEAR: f64 = 365.0 * DAY;
const SEGMENT_BYTES: f64 = 64.0 * 1048576.0;
/// Raw data per host assumed for traffic figures: a full 1 TB slice with 20% reserve.
const HELD_BYTES: f64 = 800e9;

#[derive(Clone, Copy, Debug)]
pub enum Class {
    AlwaysOn,
    /// Off for 8 hours each night, in sync with others in the region.
    Nightly,
    /// Short irregular sessions.
    Laptop,
}

#[derive(Clone, Debug)]
pub struct Mix {
    pub name: &'static str,
    pub always_on: f64,
    pub nightly: f64,
    /// Regional outages per region per year; each lasts 12-48 hours.
    pub regional_outages: f64,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub hosts: usize,
    pub segments: usize,
    pub policy: Policy,
    pub mix: Mix,
    pub grace_h: f64,
    pub years: f64,
    /// Permanent host loss rate per year.
    pub death_rate: f64,
    pub regions: usize,
    /// Share of upload used for repair, in Mbps per host at most.
    pub repair_mbps_per_host: f64,
    /// Segments per placement group, 1 = independent.
    pub group: usize,
    pub seed: u64,
}

#[derive(Debug, Default, Clone)]
pub struct Outcome {
    pub lost_segments: usize,
    pub segments: usize,
    pub seg_unavailable: f64,
    pub file_readable: f64,
    pub repaired_pieces: u64,
    pub repair_mbps_fleet: f64,
    pub repair_mbps_per_host: f64,
    pub max_backlog: usize,
    pub uptime: f64,
    pub infeasible: bool,
    pub runs_with_loss: usize,
    pub runs: usize,
}

#[derive(PartialEq)]
struct Ev(f64, u32, Kind);
impl Eq for Ev {}
impl PartialOrd for Ev {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Ev {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&o.0).then(self.1.cmp(&o.1))
    }
}
#[derive(PartialEq, Clone, Copy)]
enum Kind {
    Up(u32),
    Down(u32),
    Death(u32),
    GraceExpired(u32, u32),
    RegionDown(u32),
    RegionUp(u32),
    Tick,
}

struct Host {
    class: Class,
    region: u32,
    up: bool,
    dead: bool,
    /// How many region outages currently hold this host down.
    region_down: bool,
    wants_up: bool,
    /// Bumped each time the host goes down, to cancel stale grace timers.
    epoch: u32,
    suspect: bool,
    segments: Vec<u32>,
    free: u64,
}

struct Seg {
    /// Each piece: holder host index, or u32::MAX if vacated.
    pieces: Vec<u32>,
    lost: bool,
    queued: bool,
}

pub fn run(cfg: &Config) -> Outcome {
    let mut rng = SmallRng::seed_from_u64(cfg.seed);
    let p = &cfg.policy;
    let capacity_each = ((cfg.segments as f64 * SEGMENT_BYTES * p.expansion() * 1.5 / cfg.hosts as f64) as u64).max(1 << 40);
    let mut hosts: Vec<Host> = (0..cfg.hosts)
        .map(|i| {
            let r: f64 = rng.gen();
            let class = if r < cfg.mix.always_on {
                Class::AlwaysOn
            } else if r < cfg.mix.always_on + cfg.mix.nightly {
                Class::Nightly
            } else {
                Class::Laptop
            };
            Host {
                class,
                region: (i % cfg.regions) as u32,
                up: true,
                dead: false,
                region_down: false,
                wants_up: true,
                epoch: 0,
                suspect: false,
                segments: vec![],
                free: capacity_each,
            }
        })
        .collect();

    let cand =
        |i: usize, h: &Host| Candidate { host: i as u64, owner: i as u64, site: i as u64, asn: h.region as u64, free_bytes: h.free.max(1), reputation: 1.0 };
    let piece_bytes = (SEGMENT_BYTES / p.k as f64) as u64;
    let mut cands: Vec<Candidate> = hosts.iter().enumerate().map(|(i, h)| cand(i, h)).collect();

    if cfg.hosts < p.n as usize {
        return Outcome { infeasible: true, segments: cfg.segments, ..Default::default() };
    }

    // Initial placement, grouped.
    let mut segs: Vec<Seg> = Vec::with_capacity(cfg.segments);
    let mut group_hosts: Vec<usize> = vec![];
    for s in 0..cfg.segments {
        if s % cfg.group.max(1) == 0 {
            let req = PlacementRequest { count: p.n as usize, piece_bytes, ..Default::default() };
            group_hosts = place(&cands, DomainLevel::Owner, &req, &mut rng).expect("placement");
        }
        for &h in &group_hosts {
            hosts[h].segments.push(s as u32);
            hosts[h].free = hosts[h].free.saturating_sub(piece_bytes);
            cands[h].free_bytes = hosts[h].free.max(1);
        }
        segs.push(Seg { pieces: group_hosts.iter().map(|&h| h as u32).collect(), lost: false, queued: false });
    }

    let end = cfg.years * YEAR;
    let mut seq = 0u32;
    let mut q: BinaryHeap<Reverse<Ev>> = BinaryHeap::new();
    macro_rules! push {
        ($t:expr, $k:expr) => {{
            seq = seq.wrapping_add(1);
            let at: f64 = $t; // evaluate once: time expressions may draw randomness
            if at <= end {
                q.push(Reverse(Ev(at, seq, $k)));
            }
        }};
    }

    for (i, h) in hosts.iter().enumerate() {
        let i = i as u32;
        push!(exp(&mut rng, YEAR / cfg.death_rate.max(1e-9)), Kind::Death(i));
        match h.class {
            Class::AlwaysOn => push!(exp(&mut rng, 30.0 * DAY), Kind::Down(i)),
            Class::Nightly => {
                // Evening shutdown, aligned by region so outages correlate within it.
                let start = 22.0 + h.region as f64 * 24.0 / cfg.regions as f64 + rng.gen::<f64>();
                push!(start % DAY, Kind::Down(i));
            }
            Class::Laptop => push!(exp(&mut rng, 6.0), Kind::Down(i)),
        }
    }
    for r in 0..cfg.regions as u32 {
        if cfg.mix.regional_outages > 0.0 {
            push!(exp(&mut rng, YEAR / cfg.mix.regional_outages), Kind::RegionDown(r));
        }
    }
    push!(HOUR, Kind::Tick);

    // The simulation tracks far fewer segments than real hosts would hold. Traffic figures and
    // the repair budget are scaled as if each host held HELD_BYTES of raw data.
    let raw_per_host = cfg.segments as f64 * piece_bytes as f64 * p.n as f64 / cfg.hosts as f64;
    let data_scale = HELD_BYTES / raw_per_host;
    let mut out = Outcome { segments: cfg.segments, ..Default::default() };
    let mut queue: VecDeque<u32> = VecDeque::new();
    let (mut samples, mut unavailable, mut file_samples, mut file_ok, mut up_samples, mut up_sum) = (0u64, 0u64, 0u64, 0u64, 0u64, 0u64);
    let mut repair_bytes = 0.0;
    let mut lost_count = 0usize;
    const FILE_SEGS: usize = 16;

    while let Some(Reverse(Ev(t, _, kind))) = q.pop() {
        match kind {
            Kind::Up(i) => {
                let h = &mut hosts[i as usize];
                h.wants_up = true;
                if !h.dead && !h.region_down {
                    h.up = true;
                    h.suspect = false;
                }
                if !h.dead {
                    let d = match h.class {
                        Class::AlwaysOn => exp(&mut rng, 30.0 * DAY),
                        Class::Nightly => 16.0,
                        Class::Laptop => exp(&mut rng, 6.0),
                    };
                    push!(t + d, Kind::Down(i));
                }
            }
            Kind::Down(i) => {
                let h = &mut hosts[i as usize];
                if h.dead {
                    // Must not bump the epoch, or the death's grace timer is cancelled.
                    continue;
                }
                h.wants_up = false;
                h.up = false;
                h.epoch = h.epoch.wrapping_add(1);
                let (d, e) = match h.class {
                    Class::AlwaysOn => (exp(&mut rng, 4.0), h.epoch),
                    Class::Nightly => (8.0, h.epoch),
                    Class::Laptop => (exp(&mut rng, 18.0), h.epoch),
                };
                push!(t + d, Kind::Up(i));
                push!(t + cfg.grace_h, Kind::GraceExpired(i, e));
            }
            Kind::Death(i) => {
                let h = &mut hosts[i as usize];
                h.dead = true;
                h.up = false;
                h.epoch = h.epoch.wrapping_add(1);
                let e = h.epoch;
                push!(t + cfg.grace_h, Kind::GraceExpired(i, e));
                // Data on a dead host is gone: check for loss now.
                let segs_here = hosts[i as usize].segments.clone();
                // A new member joins to replace them, so pool size stays steady.
                let region = hosts[i as usize].region;
                let class = hosts[i as usize].class;
                let idx = hosts.len() as u32;
                hosts.push(Host {
                    class,
                    region,
                    up: true,
                    dead: false,
                    region_down: false,
                    wants_up: true,
                    epoch: 0,
                    suspect: false,
                    segments: vec![],
                    free: capacity_each,
                });
                let new = cand(idx as usize, &hosts[idx as usize]);
                cands.push(new);
                push!(t + exp(&mut rng, YEAR / cfg.death_rate.max(1e-9)), Kind::Death(idx));
                let first = match class {
                    Class::AlwaysOn => exp(&mut rng, 30.0 * DAY),
                    Class::Nightly => 12.0,
                    Class::Laptop => exp(&mut rng, 6.0),
                };
                push!(t + first, Kind::Down(idx));
                for s in segs_here {
                    let seg = &mut segs[s as usize];
                    let alive = seg.pieces.iter().filter(|&&x| x != u32::MAX && !hosts[x as usize].dead).count();
                    if !seg.lost && alive < p.k as usize {
                        seg.lost = true;
                        lost_count += 1;
                    }
                }
            }
            Kind::GraceExpired(i, e) => {
                let h = &mut hosts[i as usize];
                if h.epoch == e && !h.up {
                    h.suspect = true;
                    for &s in &hosts[i as usize].segments.clone() {
                        let seg = &mut segs[s as usize];
                        if !seg.lost && !seg.queued {
                            let healthy = healthy(seg, &hosts);
                            if healthy < p.repair_at as usize {
                                seg.queued = true;
                                queue.push_back(s);
                            }
                        }
                    }
                }
            }
            Kind::RegionDown(r) => {
                for (idx, h) in hosts.iter_mut().enumerate() {
                    if h.region == r && !h.dead {
                        h.region_down = true;
                        if h.up {
                            h.up = false;
                            h.epoch = h.epoch.wrapping_add(1);
                            let e = h.epoch;
                            seq = seq.wrapping_add(1);
                            let at = t + cfg.grace_h;
                            if at <= end {
                                q.push(Reverse(Ev(at, seq, Kind::GraceExpired(idx as u32, e))));
                            }
                        }
                    }
                }
                let dur = 12.0 + rng.gen::<f64>() * 36.0;
                push!(t + dur, Kind::RegionUp(r));
                push!(t + dur + exp(&mut rng, YEAR / cfg.mix.regional_outages), Kind::RegionDown(r));
            }
            Kind::RegionUp(r) => {
                for h in hosts.iter_mut() {
                    if h.region == r {
                        h.region_down = false;
                        if !h.dead && h.wants_up {
                            h.up = true;
                            h.suspect = false;
                        }
                    }
                }
            }
            Kind::Tick => {
                // Repair with the fleet's budget for this hour.
                let live_up = hosts.iter().filter(|h| h.up).count();
                let mut budget = live_up as f64 * cfg.repair_mbps_per_host * 1e6 / 8.0 * 3600.0 / data_scale;
                for (idx, h) in hosts.iter().enumerate() {
                    cands[idx].reputation = if h.up { 1.0 } else { 0.0 };
                }
                let mut attempts = queue.len();
                while budget > 0.0 && attempts > 0 {
                    attempts -= 1;
                    let Some(s) = queue.pop_front() else { break };
                    let seg_idx = s as usize;
                    segs[seg_idx].queued = false;
                    if segs[seg_idx].lost {
                        continue;
                    }
                    let readable = segs[seg_idx].pieces.iter().filter(|&&x| x != u32::MAX && hosts[x as usize].up).count();
                    if readable < p.k as usize {
                        // Can't rebuild right now. Retry next hour.
                        segs[seg_idx].queued = true;
                        queue.push_back(s);
                        continue;
                    }
                    let keep: Vec<u32> =
                        segs[seg_idx].pieces.iter().copied().filter(|&x| x != u32::MAX && !hosts[x as usize].dead && !hosts[x as usize].suspect).collect();
                    let need = (p.n as usize).saturating_sub(keep.len());
                    if need == 0 {
                        continue;
                    }
                    let req = PlacementRequest { count: need, piece_bytes, taken_domains: keep.iter().map(|&x| x as u64).collect(), ..Default::default() };
                    let Ok(chosen) = place(&cands, DomainLevel::Owner, &req, &mut rng) else {
                        segs[seg_idx].queued = true;
                        queue.push_back(s);
                        continue;
                    };
                    // Remove old membership.
                    for &old in segs[seg_idx].pieces.clone().iter() {
                        if old != u32::MAX && !keep.contains(&old) {
                            hosts[old as usize].segments.retain(|&x| x != s);
                        }
                    }
                    let mut pieces = keep;
                    for &c in &chosen {
                        pieces.push(c as u32);
                        hosts[c].segments.push(s);
                        hosts[c].free = hosts[c].free.saturating_sub(piece_bytes);
                        cands[c].free_bytes = hosts[c].free.max(1);
                    }
                    segs[seg_idx].pieces = pieces;
                    // Upload traffic: sources send k pieces to one rebuilder, which sends the rest.
                    let cost = (p.k as usize + need - 1) as f64 * piece_bytes as f64;
                    budget -= cost;
                    repair_bytes += cost;
                    out.repaired_pieces += need as u64;
                }
                out.max_backlog = out.max_backlog.max(queue.len());

                // Sample availability once per hour.
                up_sum += hosts.iter().filter(|h| h.up).count() as u64;
                up_samples += hosts.iter().filter(|h| !h.dead).count() as u64;
                for _ in 0..200 {
                    let s = rng.gen_range(0..segs.len());
                    samples += 1;
                    if !readable(&segs[s], &hosts, p.k) {
                        unavailable += 1;
                    }
                }
                for _ in 0..50 {
                    // A 1 GiB file: 16 consecutive segments, which share a group when grouped.
                    let start = rng.gen_range(0..segs.len().saturating_sub(FILE_SEGS).max(1));
                    file_samples += 1;
                    if (start..(start + FILE_SEGS).min(segs.len())).all(|s| readable(&segs[s], &hosts, p.k)) {
                        file_ok += 1;
                    }
                }
                push!(t + HOUR, Kind::Tick);
            }
        }
    }

    out.lost_segments = lost_count;
    out.seg_unavailable = unavailable as f64 / samples.max(1) as f64;
    out.file_readable = file_ok as f64 / file_samples.max(1) as f64;
    let secs = cfg.years * 365.0 * 86400.0;
    out.repair_mbps_fleet = repair_bytes * data_scale * 8.0 / 1e6 / secs;
    out.repair_mbps_per_host = out.repair_mbps_fleet / cfg.hosts as f64;
    out.uptime = up_sum as f64 / up_samples.max(1) as f64;
    out
}

/// Run several seeds in parallel and average them. Loss comes in rare, large events,
/// so one seed says little, and `runs_with_loss` is reported alongside the mean.
pub fn run_seeds(cfg: &Config, seeds: u64) -> Outcome {
    let results: Vec<Outcome> = std::thread::scope(|sc| {
        let handles: Vec<_> = (0..seeds)
            .map(|i| {
                let mut c = cfg.clone();
                c.seed = cfg.seed + i * 1000;
                sc.spawn(move || run(&c))
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let n = results.len() as f64;
    let mean = |f: &dyn Fn(&Outcome) -> f64| results.iter().map(f).sum::<f64>() / n;
    Outcome {
        lost_segments: results.iter().map(|r| r.lost_segments).sum::<usize>(),
        segments: results.iter().map(|r| r.segments).sum::<usize>(),
        seg_unavailable: mean(&|r| r.seg_unavailable),
        file_readable: mean(&|r| r.file_readable),
        repaired_pieces: results.iter().map(|r| r.repaired_pieces).sum(),
        repair_mbps_fleet: mean(&|r| r.repair_mbps_fleet),
        repair_mbps_per_host: mean(&|r| r.repair_mbps_per_host),
        max_backlog: results.iter().map(|r| r.max_backlog).max().unwrap_or(0),
        uptime: mean(&|r| r.uptime),
        infeasible: results.iter().any(|r| r.infeasible),
        runs_with_loss: results.iter().filter(|r| r.lost_segments > 0).count(),
        runs: results.len(),
    }
}

fn healthy(seg: &Seg, hosts: &[Host]) -> usize {
    seg.pieces.iter().filter(|&&x| x != u32::MAX && !hosts[x as usize].dead && !hosts[x as usize].suspect).count()
}
fn readable(seg: &Seg, hosts: &[Host], k: u16) -> bool {
    !seg.lost && seg.pieces.iter().filter(|&&x| x != u32::MAX && hosts[x as usize].up).count() >= k as usize
}
fn exp(rng: &mut SmallRng, mean: f64) -> f64 {
    -mean * (1.0 - rng.gen::<f64>()).ln()
}

fn base(hosts: usize, policy: Policy, mix: Mix) -> Config {
    Config {
        hosts,
        segments: (hosts * 120).min(40_000),
        policy,
        mix,
        grace_h: 72.0,
        years: 3.0,
        death_rate: 0.2,
        regions: 6,
        repair_mbps_per_host: 2.0,
        group: 16,
        seed: 7,
    }
}

pub fn mixes() -> Vec<Mix> {
    vec![
        Mix { name: "always-on", always_on: 1.0, nightly: 0.0, regional_outages: 0.0 },
        Mix { name: "home mix (60/30/10)", always_on: 0.6, nightly: 0.3, regional_outages: 1.0 },
        Mix { name: "mostly laptops (20/30/50)", always_on: 0.2, nightly: 0.3, regional_outages: 1.0 },
    ]
}

fn row(name: &str, o: &Outcome, years: f64) -> String {
    if o.infeasible {
        return format!("| {name} | not enough hosts | | | | | | |");
    }
    format!(
        "| {name} | {:.4}% | {}/{} | {:.3}% | {:.3}% | {:.0}% | {:.3} | {} |",
        o.lost_segments as f64 / o.segments as f64 * 100.0 / years,
        o.runs_with_loss,
        o.runs,
        o.seg_unavailable * 100.0,
        o.file_readable * 100.0,
        o.uptime * 100.0,
        o.repair_mbps_per_host,
        o.max_backlog,
    )
}

const HEAD: &str = "| Case | Segments lost per year | Runs with loss | Segment unavailable | 1 GiB file readable | Mean uptime | Repair Mbps/host (800 GB held) | Peak backlog |\n|---|---|---|---|---|---|---|---|";
const SEEDS: u64 = 8;

pub fn report(quick: bool) {
    let years = if quick { 0.5 } else { 3.0 };
    let policies: Vec<(&str, Policy)> = vec![
        ("1-of-3", Policy::replicate3()),
        ("1-of-4", Policy { k: 1, n: 4, repair_at: 3, ..Policy::replicate3() }),
        ("1-of-5", Policy { k: 1, n: 5, repair_at: 4, ..Policy::replicate3() }),
        ("6-of-12", Policy::ec(6, 12, 9)),
        ("10-of-16", Policy::ec(10, 16, 13)),
        ("10-of-20", Policy::ec(10, 20, 15)),
    ];

    println!("## 2. Policy against churn (200 hosts, {years} years, 72 h grace, 20%/yr host loss)\n");
    println!("Regions are time zones. Nightly hosts in one region shut down together, and each region has one 12-48 hour outage a year in the non-always-on mixes.\n");
    for mix in mixes() {
        println!("**{}**\n\n{HEAD}", mix.name);
        for (name, p) in &policies {
            let mut c = base(200, *p, mix.clone());
            c.years = years;
            println!("{}", row(name, &run_seeds(&c, SEEDS), c.years));
        }
        println!();
    }

    println!("## 3. Pool size (home mix, {years} years)\n\n{HEAD}");
    let home = mixes()[1].clone();
    let sizes: &[usize] = if quick { &[5, 10, 20, 40, 100, 1000] } else { &[5, 10, 20, 30, 40, 60, 100, 1000, 10_000, 100_000] };
    for &h in sizes {
        for (name, p) in
            [("1-of-3", Policy::replicate3()), ("1-of-5", Policy { k: 1, n: 5, repair_at: 4, ..Policy::replicate3() }), ("10-of-20", Policy::ec(10, 20, 15))]
        {
            let mut c = base(h, p, home.clone());
            c.years = if h >= 10_000 { years.min(1.0) } else { years };
            println!("{}", row(&format!("{h} hosts, {name}"), &run_seeds(&c, if h >= 10_000 { 2 } else { SEEDS }), c.years));
        }
    }
    println!();

    println!("## 4. Grace period (1-of-3 and 10-of-20, home mix)\n\n{HEAD}");
    for grace in [12.0, 24.0, 72.0, 168.0, 336.0] {
        for (name, p) in [("1-of-3", Policy::replicate3()), ("10-of-20", Policy::ec(10, 20, 15))] {
            let mut c = base(200, p, home.clone());
            c.years = years;
            c.grace_h = grace;
            println!("{}", row(&format!("{grace} h, {name}"), &run_seeds(&c, SEEDS), c.years));
        }
    }
    println!();

    println!("## 5. Placement group size (home mix)\n\n{HEAD}");
    for group in [1usize, 4, 16, 64, 256] {
        for (name, p) in [("1-of-3", Policy::replicate3()), ("10-of-20", Policy::ec(10, 20, 15))] {
            let mut c = base(200, p, home.clone());
            c.years = years;
            c.group = group;
            println!("{}", row(&format!("{group} segs/group, {name}"), &run_seeds(&c, SEEDS), c.years));
        }
    }
    println!();

    println!("## 6. Host loss rate (home mix)\n\n{HEAD}");
    for death in [0.1, 0.2, 0.5, 1.0] {
        for (name, p) in [("1-of-3", Policy::replicate3()), ("10-of-20", Policy::ec(10, 20, 15))] {
            let mut c = base(200, p, home.clone());
            c.years = years;
            c.death_rate = death;
            println!("{}", row(&format!("{:.0}%/yr, {name}", death * 100.0), &run_seeds(&c, SEEDS), c.years));
        }
    }
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn always_on() -> Mix {
        Mix { name: "t", always_on: 1.0, nightly: 0.0, regional_outages: 0.0 }
    }

    /// With no transient outages, repair must replace every piece on every dead host,
    /// and traffic should match `loss rate × data held`.
    #[test]
    fn repair_keeps_up_with_host_loss() {
        let mut c = base(200, Policy::replicate3(), always_on());
        c.years = 2.0;
        let o = run_seeds(&c, 4);
        // 20%/yr of 800 GB, as Mbps per host. Allow for early bias toward an empty new host.
        let expected = 0.2 * 800e9 * 8.0 / 1e6 / (365.0 * 86400.0);
        assert!(
            (o.repair_mbps_per_host - expected).abs() < expected * 0.5,
            "repair {} Mbps/host, expected about {expected}; {} pieces repaired",
            o.repair_mbps_per_host,
            o.repaired_pieces
        );
        // Rough bound: needs 3 of a set to die inside a repair window of a few days.
        assert_eq!(o.lost_segments, 0, "segments lost on always-on hosts");
    }

    #[test]
    fn nightly_shutdowns_alone_trigger_no_repair() {
        let mix = Mix { name: "t", always_on: 0.0, nightly: 1.0, regional_outages: 0.0 };
        let mut c = base(100, Policy::replicate3(), mix);
        c.death_rate = 1e-6;
        c.years = 0.25;
        let o = run(&c);
        assert_eq!(o.repaired_pieces, 0);
        assert!((o.uptime - 16.0 / 24.0).abs() < 0.03, "uptime {}", o.uptime);
    }
}
