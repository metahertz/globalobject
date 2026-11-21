//! Redundancy, placement and quota rules, shared by the control plane and the simulator.

use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DomainLevel {
    Host,
    Owner,
    Site,
    Asn,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Policy {
    pub k: u16,
    pub n: u16,
    /// Repair when fewer than this many pieces are healthy.
    pub repair_at: u16,
    /// Extra hosts attempted at upload, cancelling the slowest.
    pub upload_extra: u16,
    pub min_failure_domains: u16,
    pub domain_level: DomainLevel,
    /// Local parity groups (pyramid code). Zero is plain Reed-Solomon. With `l` groups, `n`
    /// counts `k` data, `n - k - l` global parities and `l` local parities. A lost piece in a
    /// group is rebuilt from `k / l` pieces instead of `k`.
    #[serde(default)]
    pub local_groups: u16,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum PolicyError {
    #[error("need 1 <= k < n <= 255")]
    BadShape,
    #[error("need k <= repair_at <= n")]
    BadRepairAt,
    #[error("min_failure_domains is below the floor of {0}")]
    BelowFloor(u16),
    #[error("n is smaller than min_failure_domains")]
    TooFewPieces,
    #[error("local groups need k >= 2 x groups and at least one global parity")]
    BadLocalGroups,
}

/// Every policy must span at least this many failure domains.
pub const FAILURE_DOMAIN_FLOOR: u16 = 3;

impl Policy {
    pub fn replicate3() -> Self {
        Policy { k: 1, n: 3, repair_at: 3, upload_extra: 0, min_failure_domains: 3, domain_level: DomainLevel::Owner, local_groups: 0 }
    }
    pub fn ec(k: u16, n: u16, repair_at: u16) -> Self {
        Policy { k, n, repair_at, upload_extra: 0, min_failure_domains: n, domain_level: DomainLevel::Owner, local_groups: 0 }
    }
    /// A pyramid code: `k` data, `local` groups each with a local parity, and the rest global.
    pub fn pyramid(k: u16, n: u16, local: u16, repair_at: u16) -> Self {
        Policy { local_groups: local, ..Policy::ec(k, n, repair_at) }
    }
    pub fn validate(&self) -> Result<(), PolicyError> {
        if self.k == 0 || self.k >= self.n || self.n > 255 {
            return Err(PolicyError::BadShape);
        }
        if self.repair_at < self.k || self.repair_at > self.n {
            return Err(PolicyError::BadRepairAt);
        }
        if self.min_failure_domains < FAILURE_DOMAIN_FLOOR {
            return Err(PolicyError::BelowFloor(FAILURE_DOMAIN_FLOOR));
        }
        if self.n < self.min_failure_domains {
            return Err(PolicyError::TooFewPieces);
        }
        let l = self.local_groups;
        if l > 0 && (self.k < 2 * l || self.n <= self.k + l) {
            return Err(PolicyError::BadLocalGroups);
        }
        Ok(())
    }
    pub fn expansion(&self) -> f64 {
        self.n as f64 / self.k as f64
    }
    /// Raw bytes per piece for a segment of `len` ciphertext bytes.
    pub fn piece_len(&self, len: u64) -> u64 {
        len.div_ceil(self.k as u64)
    }
    pub fn raw_bytes(&self, len: u64) -> u64 {
        self.piece_len(len) * self.n as u64
    }
}

/// Probability that at least `k` of `n` independent pieces are reachable.
pub fn piece_availability(host_uptime: f64, k: u16, n: u16) -> f64 {
    let (k, n) = (k as u32, n as u32);
    let mut sum = 0.0;
    for i in k..=n {
        sum += binom(n, i) * host_uptime.powi(i as i32) * (1.0 - host_uptime).powi((n - i) as i32);
    }
    sum.min(1.0)
}

fn binom(n: u32, k: u32) -> f64 {
    let k = k.min(n - k);
    (0..k).fold(1.0, |acc, i| acc * (n - i) as f64 / (i + 1) as f64)
}

// ---------------------------------------------------------------- placement

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Candidate {
    pub host: u64,
    pub owner: u64,
    pub site: u64,
    pub asn: u64,
    pub free_bytes: u64,
    /// In 0..=1. Zero excludes the host.
    pub reputation: f64,
}

impl Candidate {
    pub fn domain(&self, level: DomainLevel) -> u64 {
        match level {
            DomainLevel::Host => self.host,
            DomainLevel::Owner => self.owner,
            DomainLevel::Site => self.site,
            DomainLevel::Asn => self.asn,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct PlacementRequest {
    pub count: usize,
    pub piece_bytes: u64,
    /// Owner of the data. Their hosts never hold their pieces.
    pub exclude_owner: Option<u64>,
    /// Also keep pieces on distinct sites, in addition to the domain level.
    pub distinct_sites: bool,
    /// Domains and sites already holding pieces of this segment (for repair).
    pub taken_domains: Vec<u64>,
    pub taken_sites: Vec<u64>,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum PlacementError {
    #[error("only {available} eligible failure domains, need {needed}")]
    TooFewDomains { available: usize, needed: usize },
}

/// Weighted random choice without replacement, at most one host per failure domain.
/// Weight is `free_bytes × reputation`, so emptier hosts fill first and false pledges
/// are tested early. Returns indices into `candidates`.
pub fn place<R: Rng>(candidates: &[Candidate], level: DomainLevel, req: &PlacementRequest, rng: &mut R) -> Result<Vec<usize>, PlacementError> {
    let eligible = |c: &Candidate| c.reputation > 0.0 && c.free_bytes >= req.piece_bytes && Some(c.owner) != req.exclude_owner;
    let max_weight = candidates.iter().filter(|c| eligible(c)).map(|c| c.free_bytes as f64 * c.reputation).fold(0.0, f64::max);

    let mut domains: HashSet<u64> = req.taken_domains.iter().copied().collect();
    let mut sites: HashSet<u64> = req.taken_sites.iter().copied().collect();
    let mut chosen = Vec::with_capacity(req.count);

    // Rejection sampling keeps placement cheap in large pools.
    if max_weight > 0.0 {
        let limit = 64 * req.count.max(1);
        for _ in 0..limit {
            if chosen.len() == req.count {
                break;
            }
            let i = rng.gen_range(0..candidates.len());
            let c = &candidates[i];
            if !eligible(c)
                || domains.contains(&c.domain(level))
                || (req.distinct_sites && sites.contains(&c.site))
                || rng.gen::<f64>() * max_weight > c.free_bytes as f64 * c.reputation
            {
                continue;
            }
            domains.insert(c.domain(level));
            sites.insert(c.site);
            chosen.push(i);
        }
    }

    // Exact fallback for small or heavily constrained pools.
    while chosen.len() < req.count {
        let open: Vec<usize> = (0..candidates.len())
            .filter(|&i| {
                let c = &candidates[i];
                eligible(c) && !domains.contains(&c.domain(level)) && !(req.distinct_sites && sites.contains(&c.site))
            })
            .collect();
        if open.is_empty() {
            return Err(PlacementError::TooFewDomains { available: chosen.len(), needed: req.count });
        }
        let total: f64 = open.iter().map(|&i| candidates[i].free_bytes as f64 * candidates[i].reputation).sum();
        let mut pick = rng.gen::<f64>() * total;
        let mut selected = open[open.len() - 1];
        for &i in &open {
            pick -= candidates[i].free_bytes as f64 * candidates[i].reputation;
            if pick <= 0.0 {
                selected = i;
                break;
            }
        }
        let c = &candidates[selected];
        domains.insert(c.domain(level));
        sites.insert(c.site);
        chosen.push(selected);
    }
    Ok(chosen)
}

// -------------------------------------------------------------------- quota

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct QuotaParams {
    /// Fraction of credited space kept free for repair and churn.
    pub reserve: f64,
    /// Credit earned per byte actually held, before reaching the pledge.
    pub headroom: f64,
    pub probation_allowance: u64,
}

impl Default for QuotaParams {
    fn default() -> Self {
        QuotaParams { reserve: 0.20, headroom: 1.5, probation_allowance: 10 << 30 }
    }
}

impl QuotaParams {
    /// Without this, total stored data stalls at `Σ allowance / reserve`.
    pub fn can_grow(&self) -> bool {
        self.headroom * (1.0 - self.reserve) > 1.0
    }
}

/// Space credited for a contribution. `availability` is in 0..=1.
pub fn credited(pledged: u64, proven_held: u64, availability: f64, p: &QuotaParams) -> u64 {
    let earned = proven_held as f64 * p.headroom + p.probation_allowance as f64;
    ((pledged as f64).min(earned) * availability.clamp(0.0, 1.0)) as u64
}

/// Raw bytes this account may have stored on other people's disks.
pub fn quota_raw(credited: u64, p: &QuotaParams) -> u64 {
    (credited as f64 * (1.0 - p.reserve)) as u64
}

// ---------------------------------------------------------------- bandwidth

/// Bytes-per-second token bucket. Pure, so state can live in a database row.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TokenBucket {
    pub tokens: f64,
    pub updated_at: f64,
}

impl TokenBucket {
    /// Refill for elapsed time at `rate`, capped at `burst`.
    pub fn refill(&mut self, now: f64, rate: f64, burst: f64) {
        let elapsed = (now - self.updated_at).max(0.0);
        self.tokens = (self.tokens + elapsed * rate).min(burst);
        self.updated_at = now;
    }
    /// Charge for a read of `bytes`. Returns how much banked credit was spent, or the seconds
    /// to wait. Reads are charged up front, and can take the bucket into debt that later reads
    /// wait out. Requiring the tokens beforehand would be a double charge alongside host
    /// pacing: a sequential reader would wait T, then transfer for T, and get half their rate.
    pub fn take(&mut self, bytes: f64, credit: f64, rate: f64) -> Result<f64, f64> {
        if self.tokens >= bytes {
            self.tokens -= bytes;
            return Ok(0.0);
        }
        let short = bytes - self.tokens.max(0.0);
        if short <= credit && self.tokens >= 0.0 {
            self.tokens = 0.0;
            return Ok(short);
        }
        if self.tokens > 0.0 {
            self.tokens -= bytes;
            return Ok(0.0);
        }
        Err(if rate > 0.0 { (-self.tokens / rate).max(0.001) } else { f64::INFINITY })
    }
}

/// Median of measured throughput samples, the entitled rate.
pub fn median(samples: &[f64]) -> Option<f64> {
    if samples.is_empty() {
        return None;
    }
    let mut s = samples.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    let m = s.len() / 2;
    Some(if s.len() % 2 == 1 { s[m] } else { (s[m - 1] + s[m]) / 2.0 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::{rngs::SmallRng, SeedableRng};

    fn cand(host: u64, owner: u64, site: u64) -> Candidate {
        Candidate { host, owner, site, asn: site, free_bytes: 1 << 40, reputation: 1.0 }
    }

    #[test]
    fn availability_matches_the_plan_table() {
        let file = |a: f64, k, n| piece_availability(a, k, n).powi(256);
        assert!((file(0.8, 1, 3) - 0.1279).abs() < 0.001);
        assert!((file(0.9, 1, 3) - 0.7740).abs() < 0.001);
        assert!((file(0.9, 10, 20) - 0.9998).abs() < 0.0002);
        assert!((file(0.8, 10, 16) - 0.0010).abs() < 0.0002);
    }

    #[test]
    fn floor_of_three_domains_is_enforced() {
        let mut p = Policy::replicate3();
        assert_eq!(p.validate(), Ok(()));
        p.min_failure_domains = 2;
        assert_eq!(p.validate(), Err(PolicyError::BelowFloor(3)));
    }

    #[test]
    fn local_groups_are_validated() {
        assert_eq!(Policy::pyramid(4, 8, 2, 7).validate(), Ok(()));
        assert_eq!(Policy::pyramid(3, 8, 2, 7).validate(), Err(PolicyError::BadLocalGroups));
        assert_eq!(Policy::pyramid(4, 6, 2, 5).validate(), Err(PolicyError::BadLocalGroups));
        // Old stored policies, with no field, still parse as plain codes.
        let old: Policy = serde_json::from_str(r#"{"k":1,"n":3,"repair_at":3,"upload_extra":0,"min_failure_domains":3,"domain_level":"owner"}"#).unwrap();
        assert_eq!(old.local_groups, 0);
    }

    #[test]
    fn one_owner_with_many_disks_is_one_domain() {
        let mut rng = SmallRng::seed_from_u64(1);
        let cands: Vec<_> = (0..8).map(|h| cand(h, 1, h)).chain([cand(9, 2, 9)]).collect();
        let req = PlacementRequest { count: 3, piece_bytes: 1, ..Default::default() };
        assert_eq!(place(&cands, DomainLevel::Owner, &req, &mut rng), Err(PlacementError::TooFewDomains { available: 2, needed: 3 }));
    }

    #[test]
    fn owner_never_holds_their_own_pieces() {
        let mut rng = SmallRng::seed_from_u64(2);
        let cands: Vec<_> = (0..6).map(|h| cand(h, h, h)).collect();
        let req = PlacementRequest { count: 3, piece_bytes: 1, exclude_owner: Some(0), ..Default::default() };
        for _ in 0..200 {
            let got = place(&cands, DomainLevel::Owner, &req, &mut rng).unwrap();
            assert!(got.iter().all(|&i| cands[i].owner != 0));
            let set: HashSet<_> = got.iter().collect();
            assert_eq!(set.len(), 3);
        }
    }

    #[test]
    fn distinct_sites_and_repair_exclusions_hold() {
        let mut rng = SmallRng::seed_from_u64(3);
        let cands = vec![cand(0, 0, 5), cand(1, 1, 5), cand(2, 2, 6), cand(3, 3, 7)];
        let req = PlacementRequest { count: 2, piece_bytes: 1, distinct_sites: true, taken_sites: vec![7], taken_domains: vec![3], ..Default::default() };
        for _ in 0..100 {
            let got = place(&cands, DomainLevel::Owner, &req, &mut rng).unwrap();
            assert!(got.contains(&2));
            assert!(!got.contains(&3));
        }
    }

    #[test]
    fn placement_prefers_emptier_hosts() {
        let mut rng = SmallRng::seed_from_u64(4);
        let mut cands: Vec<_> = (0..4).map(|h| cand(h, h, h)).collect();
        cands[0].free_bytes = 100 << 40;
        let req = PlacementRequest { count: 1, piece_bytes: 1, ..Default::default() };
        let hits = (0..1000).filter(|_| place(&cands, DomainLevel::Owner, &req, &mut rng).unwrap()[0] == 0).count();
        assert!(hits > 900, "{hits}");
    }

    #[test]
    fn quota_invariant_detects_the_stall() {
        assert!(!QuotaParams { headroom: 1.0, ..Default::default() }.can_grow());
        assert!(QuotaParams::default().can_grow());
    }

    #[test]
    fn terabyte_contribution_gives_expected_usable_space() {
        let p = QuotaParams::default();
        let tb = 1_000_000_000_000u64;
        let q = quota_raw(credited(tb, tb, 1.0, &p), &p);
        let usable_3x = q as f64 / Policy::replicate3().expansion();
        assert!((usable_3x / 1e9 - 266.7).abs() < 0.1);
    }

    #[test]
    fn token_bucket_limits_and_spends_credit() {
        let mut b = TokenBucket { tokens: 0.0, updated_at: 0.0 };
        b.refill(10.0, 100.0, 500.0);
        assert_eq!(b.tokens, 500.0);
        assert_eq!(b.take(400.0, 0.0, 100.0), Ok(0.0));
        // Credit covers what the bucket can't.
        assert_eq!(b.take(200.0, 150.0, 100.0), Ok(100.0));
        assert_eq!(b.tokens, 0.0);
        // Nothing left: a short wait, not a grant.
        assert!(b.take(300.0, 100.0, 100.0).is_err());
    }

    #[test]
    fn token_bucket_debt_gives_sequential_readers_the_full_rate() {
        // Each read of 100 bytes at 100 B/s takes 1s of paced transfer. With nothing else
        // going on, the second read must be grantable as soon as the first has finished.
        let mut b = TokenBucket { tokens: 0.0, updated_at: 0.0 };
        let (rate, burst) = (100.0, 100.0);
        let mut now = 0.001;
        for _ in 0..10 {
            b.refill(now, rate, burst);
            b.take(100.0, 0.0, rate).expect("granted");
            now += 1.0; // transfer time
        }
    }

    #[test]
    fn token_bucket_debt_still_caps_the_aggregate() {
        // A greedy client asking constantly gets no more than the rate over time.
        let mut b = TokenBucket { tokens: 0.0, updated_at: 0.0 };
        let (rate, burst, mut granted, mut now) = (100.0, 100.0, 0.0, 0.0);
        while now < 100.0 {
            now += 0.01;
            b.refill(now, rate, burst);
            if b.take(100.0, 0.0, rate).is_ok() {
                granted += 100.0;
            }
        }
        assert!(granted <= rate * 100.0 + 2.0 * burst, "{granted}");
        assert!(granted >= rate * 100.0 * 0.9, "{granted}");
    }
}
