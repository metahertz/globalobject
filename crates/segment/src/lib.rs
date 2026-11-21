//! `(k, n)` coding of already-encrypted data. Reed-Solomon comes from `reed-solomon-erasure`.
//! `k = 1` means identical copies, through the same interface.
//!
//! With local groups, the code is a basic pyramid code (Huang, Chen and Li, 2007), built only
//! from the library's own `encode` and `reconstruct_data`:
//!
//! - pieces `0..k` are data, split into `local` contiguous groups;
//! - pieces `k..k+r` are parity rows `1..=r` of `RS(k, k+r+1)`;
//! - pieces `k+r..n` are parity row 0, computed per group with the other groups' data set to zero.
//!
//! The local parities XOR back to row 0. So a single lost piece in a group is rebuilt from that
//! group alone, and any `r + 1` losses are recoverable, one more than an XOR-local code of the
//! same size guarantees.

use reed_solomon_erasure::galois_8::ReedSolomon;

pub const MAX_SEGMENT: usize = 64 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum SegmentError {
    #[error("not enough pieces: have {have}, need {need}")]
    TooFewPieces { have: usize, need: usize },
    #[error("these pieces can't be decoded together")]
    Undecodable,
    #[error("coding error: {0}")]
    Coding(String),
}

/// How a segment's `n` pieces are laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub k: usize,
    pub n: usize,
    /// Local groups. Zero is plain Reed-Solomon, or replication when `k = 1`.
    pub local: usize,
}

impl Layout {
    pub fn new(k: usize, n: usize, local: usize) -> Layout {
        Layout { k, n, local }
    }
    pub fn plain(k: usize, n: usize) -> Layout {
        Layout { k, n, local: 0 }
    }
    /// Global parity pieces.
    pub fn r(&self) -> usize {
        self.n - self.k - self.local
    }
    pub fn is_valid(&self) -> bool {
        self.k >= 1 && self.k < self.n && self.n <= 255 && (self.local == 0 || (self.k >= 2 * self.local && self.n > self.k + self.local))
    }
    /// The local group a data piece belongs to.
    fn group_of_data(&self, i: usize) -> usize {
        i * self.local / self.k
    }
    fn local_index(&self, g: usize) -> usize {
        self.k + self.r() + g
    }
    /// Data pieces in group `g`.
    pub fn group_data(&self, g: usize) -> Vec<usize> {
        (0..self.k).filter(|&i| self.group_of_data(i) == g).collect()
    }
    /// The group a piece belongs to: data and local parity pieces have one, global parities don't.
    pub fn group_of(&self, piece: usize) -> Option<usize> {
        if self.local == 0 {
            None
        } else if piece < self.k {
            Some(self.group_of_data(piece))
        } else if piece >= self.k + self.r() {
            Some(piece - self.k - self.r())
        } else {
            None
        }
    }
    /// Every piece of group `g`: its data and its local parity.
    pub fn group_members(&self, g: usize) -> Vec<usize> {
        let mut m = self.group_data(g);
        m.push(self.local_index(g));
        m
    }
    /// Local parities help only within their group, so readers fetch them last.
    pub fn is_local_parity(&self, piece: usize) -> bool {
        self.local > 0 && piece >= self.k + self.r()
    }

    /// Which pieces can be derived from those `present`. The single source of truth for
    /// "can this be decoded", shared by readers, repair and tests, and mirroring `reconstruct`.
    pub fn recoverable(&self, present: &[bool]) -> Vec<bool> {
        let mut have = present.to_vec();
        have.resize(self.n, false);
        if self.local == 0 {
            if have.iter().filter(|&&p| p).count() >= self.k {
                return vec![true; self.n];
            }
            return have;
        }
        loop {
            if (0..self.k).all(|i| have[i]) {
                return vec![true; self.n];
            }
            let mut changed = false;
            for g in 0..self.local {
                let members = self.group_members(g);
                let missing: Vec<usize> = members.iter().copied().filter(|&i| !have[i]).collect();
                if missing.len() == 1 {
                    have[missing[0]] = true;
                    changed = true;
                }
            }
            let locals = (0..self.local).all(|g| have[self.local_index(g)]);
            let usable = (0..self.k + self.r()).filter(|&i| have[i]).count() + usize::from(locals);
            if usable >= self.k {
                for h in have.iter_mut().take(self.k) {
                    *h = true;
                }
                changed = true;
            }
            if !changed {
                return have;
            }
        }
    }
    pub fn can_decode(&self, present: &[bool]) -> bool {
        self.recoverable(present)[..self.k].iter().all(|&p| p)
    }
    pub fn can_rebuild(&self, target: usize, present: &[bool]) -> bool {
        self.recoverable(present)[target]
    }
}

fn rs(k: usize, parity: usize) -> Result<ReedSolomon, SegmentError> {
    ReedSolomon::new(k, parity).map_err(|e| SegmentError::Coding(format!("{e:?}")))
}

fn coding(e: reed_solomon_erasure::Error) -> SegmentError {
    SegmentError::Coding(format!("{e:?}"))
}

pub fn piece_len(len: usize, k: usize) -> usize {
    len.div_ceil(k)
}

/// Split data into `k` equal pieces, zero-padding the last.
fn split(data: &[u8], k: usize) -> Vec<Vec<u8>> {
    let size = piece_len(data.len(), k);
    (0..k)
        .map(|i| {
            let mut s = vec![0u8; size];
            let start = (i * size).min(data.len());
            let end = (start + size).min(data.len());
            s[..end - start].copy_from_slice(&data[start..end]);
            s
        })
        .collect()
}

pub fn encode(data: &[u8], layout: Layout) -> Result<Vec<Vec<u8>>, SegmentError> {
    if layout.k == 1 {
        return Ok(vec![data.to_vec(); layout.n]);
    }
    encode_pieces(split(data, layout.k), layout)
}

/// All `n` pieces from the `k` data pieces.
fn encode_pieces(data: Vec<Vec<u8>>, layout: Layout) -> Result<Vec<Vec<u8>>, SegmentError> {
    let (k, size) = (layout.k, data[0].len());
    if layout.local == 0 {
        let mut shards = data;
        shards.resize(layout.n, vec![0u8; size]);
        rs(k, layout.n - k)?.encode(&mut shards).map_err(coding)?;
        return Ok(shards);
    }
    let r = layout.r();
    let code = rs(k, r + 1)?;
    let mut full = data.clone();
    full.resize(k + r + 1, vec![0u8; size]);
    code.encode(&mut full).map_err(coding)?;
    let mut out: Vec<Vec<u8>> = data.clone();
    out.extend(full.drain(k + 1..)); // rows 1..=r
    for g in 0..layout.local {
        out.push(local_parity(&code, &data, layout, g)?);
    }
    Ok(out)
}

/// Parity row 0 over group `g`'s data alone.
fn local_parity(code: &ReedSolomon, data: &[Vec<u8>], layout: Layout, g: usize) -> Result<Vec<u8>, SegmentError> {
    let (k, size) = (layout.k, data[0].len());
    let mut shards: Vec<Vec<u8>> = (0..k)
        .map(|i| if layout.group_of_data(i) == g { data[i].clone() } else { vec![0u8; size] })
        .chain(std::iter::repeat_n(vec![0u8; size], layout.r() + 1))
        .collect();
    code.encode(&mut shards).map_err(coding)?;
    Ok(shards.swap_remove(k))
}

/// Rebuild all `n` pieces. `pieces[i]` is `None` when missing.
pub fn reconstruct(pieces: Vec<Option<Vec<u8>>>, layout: Layout) -> Result<Vec<Vec<u8>>, SegmentError> {
    let n = layout.n;
    if layout.k == 1 {
        let copy = pieces.into_iter().flatten().next().ok_or(SegmentError::TooFewPieces { have: 0, need: 1 })?;
        return Ok(vec![copy; n]);
    }
    let data = decode_data(pieces, layout)?;
    encode_pieces(data, layout)
}

/// Rebuild only the `k` data pieces, which is all a reader needs.
pub fn reconstruct_data(pieces: Vec<Option<Vec<u8>>>, layout: Layout) -> Result<Vec<Vec<u8>>, SegmentError> {
    if layout.k == 1 {
        return Ok(vec![pieces.into_iter().flatten().next().ok_or(SegmentError::TooFewPieces { have: 0, need: 1 })?]);
    }
    decode_data(pieces, layout)
}

fn decode_data(mut pieces: Vec<Option<Vec<u8>>>, layout: Layout) -> Result<Vec<Vec<u8>>, SegmentError> {
    let (k, n) = (layout.k, layout.n);
    pieces.resize(n, None);
    let present: Vec<bool> = pieces.iter().map(Option::is_some).collect();
    if !layout.can_decode(&present) {
        let have = present.iter().filter(|&&p| p).count();
        return Err(if have < k { SegmentError::TooFewPieces { have, need: k } } else { SegmentError::Undecodable });
    }
    if layout.local == 0 {
        let mut shards = pieces;
        rs(k, n - k)?.reconstruct_data(&mut shards).map_err(coding)?;
        return Ok(shards.into_iter().take(k).map(|p| p.expect("reconstructed")).collect());
    }
    let r = layout.r();
    let code = rs(k, r + 1)?;
    let size = pieces.iter().flatten().next().map(Vec::len).unwrap_or(0);
    // Same steps as `Layout::recoverable`, on real bytes.
    while (0..k).any(|i| pieces[i].is_none()) {
        let mut changed = false;
        for g in 0..layout.local {
            let members = layout.group_members(g);
            let missing: Vec<usize> = members.iter().copied().filter(|&i| pieces[i].is_none()).collect();
            if missing.len() != 1 {
                continue;
            }
            let m = missing[0];
            if m >= k {
                let data: Vec<Vec<u8>> = (0..k).map(|i| pieces[i].clone().unwrap_or_else(|| vec![0u8; size])).collect();
                pieces[m] = Some(local_parity(&code, &data, layout, g)?);
            } else {
                // Group g's data, zeros elsewhere, and its local parity as row 0.
                let mut shards: Vec<Option<Vec<u8>>> =
                    (0..k).map(|i| if layout.group_of_data(i) == g { pieces[i].clone() } else { Some(vec![0u8; size]) }).collect();
                shards.push(pieces[layout.local_index(g)].clone());
                shards.extend(std::iter::repeat_n(None, r));
                code.reconstruct_data(&mut shards).map_err(coding)?;
                pieces[m] = shards[m].take();
            }
            changed = true;
        }
        let locals: Option<Vec<u8>> = (0..layout.local).try_fold(vec![0u8; size], |mut acc, g| {
            let p = pieces[layout.local_index(g)].as_ref()?;
            acc.iter_mut().zip(p).for_each(|(a, b)| *a ^= b);
            Some(acc)
        });
        let usable = (0..k + r).filter(|&i| pieces[i].is_some()).count() + usize::from(locals.is_some());
        if (0..k).any(|i| pieces[i].is_none()) && usable >= k {
            let mut shards: Vec<Option<Vec<u8>>> = pieces[..k].to_vec();
            shards.push(locals);
            shards.extend(pieces[k..k + r].iter().cloned());
            code.reconstruct_data(&mut shards).map_err(coding)?;
            for (i, s) in shards.into_iter().take(k).enumerate() {
                pieces[i] = s;
            }
            changed = true;
        }
        if !changed {
            return Err(SegmentError::Undecodable);
        }
    }
    Ok(pieces.into_iter().take(k).map(|p| p.expect("decoded")).collect())
}

/// Join data pieces back into the original bytes.
pub fn join(pieces: &[Vec<u8>], k: usize, len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    for p in pieces.iter().take(k) {
        out.extend_from_slice(p);
    }
    out.truncate(len);
    out
}

/// The data pieces holding bytes `[start, end)` of a segment, each with its range within the
/// piece. Codes are systematic, so data piece `i` holds bytes `[i·len, (i+1)·len)` unchanged.
pub fn data_ranges(start: u64, end: u64, piece_len: u64, k: usize) -> Vec<(usize, u64, u64)> {
    if k == 1 {
        return vec![(0, start, end)];
    }
    let first = (start / piece_len) as usize;
    let last = ((end - 1) / piece_len) as usize;
    (first..=last.min(k - 1))
        .map(|i| {
            let base = i as u64 * piece_len;
            (i, start.max(base) - base, end.min(base + piece_len) - base)
        })
        .collect()
}

/// Rebuild one piece from whatever is present, using its group alone when that's enough.
/// This is what a repairing host runs: it needs `target`, not the whole segment.
pub fn rebuild_piece(mut pieces: Vec<Option<Vec<u8>>>, target: usize, layout: Layout) -> Result<Vec<u8>, SegmentError> {
    pieces.resize(layout.n, None);
    if layout.k == 1 {
        return pieces.into_iter().flatten().next().ok_or(SegmentError::TooFewPieces { have: 0, need: 1 });
    }
    if let Some(g) = layout.group_of(target) {
        let members = layout.group_members(g);
        if members.iter().all(|&i| i == target || pieces[i].is_some()) {
            let size = pieces.iter().flatten().next().map(Vec::len).unwrap_or(0);
            let code = rs(layout.k, layout.r() + 1)?;
            if target >= layout.k {
                let data: Vec<Vec<u8>> = (0..layout.k).map(|i| pieces[i].clone().unwrap_or_else(|| vec![0u8; size])).collect();
                return local_parity(&code, &data, layout, g);
            }
            let mut shards: Vec<Option<Vec<u8>>> =
                (0..layout.k).map(|i| if layout.group_of_data(i) == g { pieces[i].clone() } else { Some(vec![0u8; size]) }).collect();
            shards.push(pieces[layout.local_index(g)].clone());
            shards.extend(std::iter::repeat_n(None, layout.r()));
            code.reconstruct_data(&mut shards).map_err(coding)?;
            return shards[target].take().ok_or(SegmentError::Undecodable);
        }
    }
    reconstruct(pieces, layout)?.into_iter().nth(target).ok_or(SegmentError::Undecodable)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i * 31 % 251) as u8).collect()
    }

    fn keep(pieces: &[Vec<u8>], present: &[bool]) -> Vec<Option<Vec<u8>>> {
        pieces.iter().zip(present).map(|(p, &k)| k.then(|| p.clone())).collect()
    }

    #[test]
    fn replication_is_the_same_interface() {
        let d = bytes(100_000);
        let l = Layout::plain(1, 3);
        let pieces = encode(&d, l).unwrap();
        assert_eq!(pieces.len(), 3);
        let rebuilt = reconstruct(vec![None, None, Some(pieces[2].clone())], l).unwrap();
        assert_eq!(join(&rebuilt, 1, d.len()), d);
    }

    #[test]
    fn any_k_of_n_rebuilds_all_pieces() {
        let d = bytes(1_000_003);
        let l = Layout::plain(4, 8);
        let pieces = encode(&d, l).unwrap();
        for keep_idx in [[0usize, 1, 2, 3], [4, 5, 6, 7], [0, 2, 5, 7], [1, 3, 4, 6]] {
            let partial = (0..8).map(|i| keep_idx.contains(&i).then(|| pieces[i].clone())).collect();
            let all = reconstruct(partial, l).unwrap();
            assert_eq!(all, pieces);
            assert_eq!(join(&all, 4, d.len()), d);
        }
        let partial = (0..8).map(|i| [1usize, 3, 4, 6].contains(&i).then(|| pieces[i].clone())).collect();
        assert_eq!(join(&reconstruct_data(partial, l).unwrap(), 4, d.len()), d);
        let few = (0..8).map(|i| (i < 3).then(|| pieces[i].clone())).collect();
        assert!(matches!(reconstruct(few, l), Err(SegmentError::TooFewPieces { have: 3, need: 4 })));
    }

    #[test]
    fn pyramid_local_parities_sum_to_the_first_parity_row() {
        let d = bytes(80_000);
        let l = Layout::new(4, 8, 2);
        let pieces = encode(&d, l).unwrap();
        let mut full = split(&d, 4);
        full.resize(7, vec![0u8; pieces[0].len()]);
        rs(4, 3).unwrap().encode(&mut full).unwrap();
        let sum: Vec<u8> = pieces[6].iter().zip(&pieces[7]).map(|(a, b)| a ^ b).collect();
        assert_eq!(sum, full[4], "row 0");
        assert_eq!(&pieces[4..6], &full[5..7], "rows 1 and 2 are the global parities");
        assert_eq!(&pieces[..4], &full[..4], "systematic");
    }

    /// Every loss pattern: the decoder agrees with `recoverable`, any r+1 losses decode, and
    /// every single loss can be repaired from its group alone.
    #[test]
    fn pyramid_exhaustive_loss_patterns() {
        for (k, n, local) in [(4, 8, 2), (6, 10, 2), (4, 7, 1), (6, 12, 3)] {
            let l = Layout::new(k, n, local);
            assert!(l.is_valid());
            let d = bytes(10_000 + k);
            let pieces = encode(&d, l).unwrap();
            let r = l.r();
            for mask in 0u32..(1 << n) {
                let present: Vec<bool> = (0..n).map(|i| mask & (1 << i) != 0).collect();
                let lost = n - mask.count_ones() as usize;
                let predicted = l.can_decode(&present);
                let got = reconstruct(keep(&pieces, &present), l);
                assert_eq!(got.is_ok(), predicted, "{l:?} present {present:?}");
                if let Ok(all) = got {
                    assert_eq!(all, pieces, "{l:?} {present:?}");
                }
                if lost <= r + 1 {
                    assert!(predicted, "{l:?} must survive any {} losses: {present:?}", r + 1);
                }
            }
            for target in 0..n {
                let Some(g) = l.group_of(target) else { continue };
                let present: Vec<bool> = (0..n).map(|i| i != target && l.group_members(g).contains(&i)).collect();
                assert!(l.can_rebuild(target, &present), "{l:?}: piece {target} from its group");
                assert_eq!(rebuild_piece(keep(&pieces, &present), target, l).unwrap(), pieces[target], "{l:?}: piece {target}");
            }
        }
    }

    #[test]
    fn group_repair_rebuilds_the_exact_piece() {
        let l = Layout::new(6, 10, 2);
        let d = bytes(123_457);
        let pieces = encode(&d, l).unwrap();
        for target in [0usize, 4, 9] {
            let g = l.group_of(target).unwrap();
            let sources: Vec<usize> = l.group_members(g).into_iter().filter(|&i| i != target).collect();
            assert_eq!(sources.len(), 3, "k/l reads");
            let rebuilt = rebuild_one(&pieces, &sources, target, l);
            assert_eq!(rebuilt, pieces[target]);
        }
    }

    fn rebuild_one(pieces: &[Vec<u8>], sources: &[usize], target: usize, l: Layout) -> Vec<u8> {
        let present: Vec<bool> = (0..l.n).map(|i| sources.contains(&i)).collect();
        rebuild_piece(keep(pieces, &present), target, l).unwrap()
    }

    #[test]
    fn data_ranges_map_onto_systematic_pieces() {
        assert_eq!(data_ranges(10, 20, 100, 1), vec![(0, 10, 20)]);
        assert_eq!(data_ranges(10, 20, 100, 4), vec![(0, 10, 20)]);
        assert_eq!(data_ranges(90, 210, 100, 4), vec![(0, 90, 100), (1, 0, 100), (2, 0, 10)]);
        assert_eq!(data_ranges(300, 400, 100, 4), vec![(3, 0, 100)]);
    }

    #[test]
    fn validity() {
        assert!(Layout::new(4, 8, 2).is_valid());
        assert!(!Layout::new(4, 6, 2).is_valid(), "needs a global parity");
        assert!(!Layout::new(3, 8, 2).is_valid(), "groups need two data pieces");
        assert!(Layout::plain(1, 3).is_valid());
    }
}
