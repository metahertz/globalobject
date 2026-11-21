# Partial reads, space reclaim and cheaper repair

Status: designed 2026-11-08. PRs 1–4 are implemented in the working tree, PR 5 is designed only, and the upstream items are drafts
that haven't been sent.

This follows the Ceph and OpenZFS research (`reports/Ceph and OpenZFS Rust port.md`). That research found three ideas worth
taking, and none of them needs code from Ceph or OpenZFS:

| Idea | Where it comes from | What it fixes here |
|---|---|---|
| Read only the bytes asked for | Ceph Tentacle "FastEC" partial reads | Issue 7 (whole-pack reads), and narrows issue 6 (coarse receipts) |
| Reclaim space from old snapshots | ZFS snapshot space accounting; restic `forget`/`prune` | Issue 8 (no forget/prune) |
| Repair one lost piece from a few neighbours | Ceph LRC plugin; Pyramid codes (Huang, Chen and Li, 2007) | Repair traffic over home uplinks |

## Where each change belongs

I read the pinned sources (`iroh-blobs` 0.104.1, `rustic_core` 0.13.0, `reed-solomon-erasure` 6.0.0) to see what is missing upstream.
Very little is:

- **`iroh-blobs`** already supports range requests (`GetRequest::blob_ranges`), and its provider hook gives the host the full
  request, ranges included. So range grants are enforced in our host gate, with no upstream change. Two small upstream
  contributions would help, and both are drafted under `docs/upstream/`:
  - a `get_blob_ranges` helper, so clients don't have to drive the low-level `fsm` API (we carry our own copy for now);
  - a docs fix, or a new event, about hash-sequence children. See PR 1.
- **`rustic_core`** already has `forget` (`KeepOptions`, `delete_snapshots`) and `prune` (`prune_plan`, `prune`). It deletes in two
  phases by default (`keep_delete`, 23 h), which keeps it safe against backups running at the same time. The backend trait already
  has `remove`. **Nothing upstream is needed.**
- **`reed-solomon-erasure`** is stable but hasn't had a release since 2022. The pyramid code below is built from its existing
  `encode` and `reconstruct_data` calls, with no field arithmetic of our own. **Nothing upstream is needed.** The crate has no
  local-parity code, and an unmaintained crate isn't a good place to add one. If we ever move to `reed-solomon-simd`, only
  `crates/segment` changes.

So all four built PRs are in our own repository.

## PR 1: hosts serve only single-blob requests (security fix)

**Found while designing PR 2.** In `iroh-blobs`, a get request can carry ranges for *children*. The provider then reads the
requested blob as a hash sequence and serves the blobs it names (`provider.rs`, `handle_get_impl`). The `GetRequestReceived`
event shows only the root hash. Our gate authorised on `(peer, root hash)`, so a client could:

1. upload a piece whose bytes are the 32-byte hashes of other pieces held by the same host (repair and audit grants reveal
   hashes, and so do probe blobs);
2. get an ordinary read grant for its own piece;
3. send a request with child ranges, and receive the other pieces without grants. That skips the bandwidth limit, earns no
   credit for the host, and confirms which blobs the host holds.

The data stays encrypted, but accounting, bandwidth limits and blob privacy are all bypassed. **Fix:** the gate accepts only
requests where `ranges.as_single()` is `Some((0, r))`, meaning the root blob alone, and refuses everything else.

**Test:** `v04_hash_sequence_requests_cannot_reach_other_blobs` builds exactly that piece, through the real placement and commit
path, and checks the host refuses the request.

## PR 2: range grants and partial reads

**Goal:** reading one file from a 48 MiB pack fetches, and is charged for, roughly that file's bytes, not the whole pack.

**Wire changes** (all `#[serde(default)]`, so old messages still parse):
- `Grant.range: Option<(u64, u64)>` is a byte range within the piece. `None` means the whole piece, as before.
- `ReadRequest.range: Option<(u64, u64)>` is a byte range within the object.
- `ReadPiece.range: Option<(u64, u64)>` is the range granted on that piece.

**Control plane** (`read_segment`):
- **Replicas (`k = 1`):** one healthy replica, with the same range.
- **Erasure coding:** codes are systematic, so data piece `i` holds bytes `[i·piece_len, (i+1)·piece_len)`. A range maps to the
  data pieces it covers, each with its own sub-range. If any of them is unreachable, the reply is 503, and the client falls back to
  a full read and decode. That fallback is the degraded read.
- **Charging:** the bucket and `max_bytes` are charged for the range rounded out to 16 KiB, which is `iroh-blobs`' chunk-group
  size and therefore the bytes really sent. Before, every read cost `piece_len`.

**Host gate:** the allowance carries the granted `ChunkRanges`, and a request is served only if its ranges are a subset.

**Client:** `read_partial` slices from the whole-object cache when it can. When it can't, it asks for a range if the read is
under half the object; otherwise it reads the whole object, which suits restore, since restore reads packs nearly
sequentially. Ranged fetches are verified by BLAKE3/Bao like whole ones.

**Receipts (issue 6):** a read receipt now covers a range. A reader who withholds one costs the host at most that range (rounded
to 16 KiB), not a whole piece. Restores still use large reads, so this narrows the issue rather than closing it.

**Tests:** `t_partial_read_fetches_only_the_range` (bytes served and charged stay far below the pack size, under both
policies) and `v04_range_grants_limit_what_is_served`.

## PR 3: `globalobject forget` and `globalobject prune`

**CLI:**
- `globalobject forget [--keep-last N] [--keep-daily N] [--keep-weekly N] [--keep-monthly N] [--prune]`
- `globalobject prune [--instant-delete]`

**Client:**
- **forget** applies `rustic_core`'s `KeepOptions` to all snapshots as one group, then calls `delete_snapshots`. It removes the
  forgotten IDs from the local list of seen snapshots *before* deleting them. Otherwise the next listing would look like a
  rollback.
- **prune** uses `prune_plan` and `prune` with rustic's defaults: repack at most 10%, tolerate 5% unused, and wait 23 h before
  deleting packs. `--instant-delete` is for single-device use and tests.

**Control plane:** none. `remove` already maps to `delete_object`, which queues piece deletion on hosts and stops counting the
segment against quota at once. Repacking downloads and uploads through the normal read and write paths, so it is charged and
limited like any other traffic. Users should know that pruning costs bandwidth.

**Why ZFS's "deadlist" bookkeeping isn't built:** in ZFS the pool knows which snapshot references which block. Here the snapshot
to pack mapping sits inside the encrypted index, which the control plane must not see, and rustic already computes it on the
client. Copying that bookkeeping to the control plane would leak structure for no gain.

**Tests:** `t_forget_and_prune_reclaim_space` covers two snapshots of different data, forgetting the older, pruning, then checks
`used_raw` drops, hosts delete the pieces, the newer snapshot still restores, and no rollback is reported.

## PR 4: local parity (pyramid codes)

**Policy:** `Policy.local_groups: u16`, defaulting to 0, which leaves existing pools unchanged. With `l` groups, a segment's
`n` pieces are:

| Index | Content |
|---|---|
| `0 .. k` | data, split into `l` contiguous groups |
| `k .. k+r` | global parities, where `r = n − k − l` |
| `k+r .. n` | one local parity per group |

**Construction:** a *basic pyramid code*.
1. Take the systematic Reed–Solomon code `RS(k, k + r + 1)` from `reed-solomon-erasure`.
2. Parity rows `1..=r` are the global parities.
3. Parity row 0 is split by group. Local parity `g` is row 0 computed with every other group's data set to zero, so the local
   parities sum (XOR) to the original row 0.

**What it buys:**
- **Single loss:** a lost data piece or local parity is rebuilt from its group alone, which is `k/l` reads instead of `k`.
- **Worst case:** any `r + 1` losses are recoverable. If every local parity survives, their sum gives back row 0, and the code is
  `RS(k, k+r+1)`. If one is lost, at most `r` other pieces are, and `RS(k, k+r)` is still MDS.
- **Same storage:** a plain XOR-local scheme with the same storage guarantees only `r`. The pyramid construction gets one more
  without any field arithmetic of our own.

**Trade against plain RS at the same storage:** `RS(10, 20)` tolerates 10 losses and repairs one piece by reading 10. Pyramid
`k=10, l=2, r=8` (still 20 pieces) tolerates any 9, plus many 10-loss patterns, and repairs one data piece by reading 5. Whether
halving single-loss repair traffic is worth giving up guaranteed tolerance of the 10th loss is a policy decision for the
simulator (`sim-report.md` follow-up). This PR only makes the choice available.

**Rules:**
- **Validation:** with `l > 0`, it requires `k ≥ 2l` and `r ≥ 1`.
- **Placement:** unchanged, with every piece in its own failure domain. Putting a group in one region would make its repair
  cheaper but correlated, and that isn't wanted.

**Code:**
- **`crates/segment`:** gets a `Layout` type and a single `recoverable` function that decides decodability for reads, repair
  sizing and tests, so they can't disagree with the decoder.
- **Reads:** the client stops once the pieces it holds can decode, not once it has `k` of them. The control plane offers data
  and global pieces first.
- **Repair:** the control plane lists the target's group members first. The node fetches in order until the target can be
  rebuilt, so a single loss costs `k/l` reads.

**Tests:**
- **Unit, exhaustive over every loss pattern:** with `k=4, l=2, r=2`, any 3 losses decode, every single loss repairs from its
  group, and the decoder agrees with `recoverable` on every pattern.
- **End to end:** `t_local_parity_repairs_from_the_group` covers round trip, repair of a lost data piece reading `k/l` pieces,
  and reads with `r + 1` hosts down.

## PR 5 (designed, not built): delete grace window

`delete_object` takes effect immediately, so anyone with the phrase can `forget --keep-last 0 && prune --instant-delete` and
destroy everything at once, ransomware included. A control-plane `delete_grace_secs` would work like this:
- **Hide, don't delete:** deletions mark objects deleted instead. They stop appearing in listings but keep counting against quota.
- **Reaper:** the reconcile loop deletes their pieces once the grace period passes.
- **Recovery:** an `undelete` call lists and restores marked objects, with a CLI to match.
- **Rewrites:** rewriting an object ID that is marked as deleted clears the mark.

It needs a decision on the default period, and on whether quota counts deleted data, so it stays a proposal.

## Upstream drafts (not sent)

| Draft | Project | Kind | File |
|---|---|---|---|
| U1 | `iroh-blobs` | PR: `get_blob_ranges` helper alongside `get_blob` | `docs/upstream/iroh-blobs-get-blob-ranges.md` |
| U2 | `iroh-blobs` | Issue first, then a docs or event PR: `GetRequestReceived` doesn't show hash-seq children | `docs/upstream/iroh-blobs-hashseq-children.md` |

Both should be checked against upstream `main` before sending, since 0.104 moves quickly. U2 is an API footgun rather than a
vulnerability in `iroh-blobs` itself, but raise it privately first in case other users gate on the root hash as we did.

## PR order

PR 1 must come first: it's a security fix, and PR 2 builds on the same gate code. PR 2 and PR 3 are independent of each other.
PR 4 touches the read loop that PR 2 also changes, so it goes after PR 2.
