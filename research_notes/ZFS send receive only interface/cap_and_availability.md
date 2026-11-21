# CAP, availability and durability if GlobalObject's only data interface became ZFS send/receive streams

Research date: 2026-11-20. Local inputs: `docs/architecture.md`, `docs/status.md`, `docs/threat-model.md`, `docs/sim-report.md`, `docs/protocol.md`, `README.md`, and the earlier note `research_notes/Ceph and OpenZFS Rust port/openzfs.md`.

Labels used throughout:
- **[Sourced]**: a fact with a citation.
- **[Computed]**: output of the python3 script reproduced under Q2. Inputs are the stated assumptions, not measurements.
- **[Inference]**: my reasoning from the sourced and computed material.
- **[Background]**: from general knowledge, not verified with a source this session.

Fetch notes: openzfs.github.io and helpcenter.veeam.com refused connections from this environment. OpenZFS man pages and source were read from raw.githubusercontent.com (`openzfs/zfs` master), which holds the same text. Veeam material comes from search-result excerpts and is marked as such.

Common model assumptions (used in the Q2 to Q5 calculations):
- Segment size 64 MiB (`docs/protocol.md`: "Packs are limited to 48 MiB, and segments to 64 MiB"). A 1 TiB full send is 16,384 segments.
- Incremental: 1% of 1 TiB changes per day, giving about 10 GiB, or 160 segments, per daily incremental. This is an assumption. ZFS incremental size depends on record size and the write pattern, and restic's on content-defined chunking.
- Host uptime p is independent per host, at 0.80, 0.90 or 0.95. Redundancy is 1-of-3 (any 1 of 3 replicas) or 10-of-20 (any 10 of 20 RS pieces).
- "Placement groups of 16" (pg16): GlobalObject puts 16 consecutive segments on the same host set (`architecture.md`, Placement). I model each group as one unit with the segment's availability. "Independent" treats every segment as placed separately, which is the worst case.

---

## Q1. Consistency: what ordering must the control plane enforce for a chain of ZFS streams, and what happens under partition?

### Takeaway
Today's catalogue is effectively a grow-only set of immutable, content-named objects. Concurrent or retried commits commute, so they need only per-object atomicity. A ZFS chain is an ordered log: incremental `i` is valid only if its `fromguid` equals the `toguid` of the committed head. The control plane must therefore run a linearizable compare-and-swap on a per-chain head pointer (no gaps, no forks, matching GUIDs). That is a strictly stronger requirement than today's. MongoDB can provide it with a conditional single-document update at `w: "majority"`. The CAP class doesn't change (the control plane is already CP and already stops writes when unreachable). But a partition mid-chain now leaves an ambiguous head that the client must reconcile before its next send.

### Cited Findings
- [Sourced] The send-stream BEGIN record carries both GUIDs: `struct drr_begin { … uint64_t drr_toguid; uint64_t drr_fromguid; char drr_toname[MAXNAMELEN]; }`. The END record carries `zio_cksum_t drr_checksum; uint64_t drr_toguid;`. Raw streams set the feature flag `DMU_BACKUP_FEATURE_RAW (1 << 24)`. The header file doesn't explain either GUID field in a comment. — [zfs_ioctl.h](https://raw.githubusercontent.com/openzfs/zfs/master/include/sys/zfs_ioctl.h)
- [Sourced] On receive, `recv_begin_check_existing_impl` searches the destination's snapshots for one whose `ds_guid == fromguid`. If none is found (`obj == 0`), or the match is in another dataset directory, it returns `ENODEV`. Without `-F`, it returns `ETXTBSY` if the dataset was modified since that snapshot ("If we are not forcing, there must be no changes since fromsnap"). A new snapshot name that already exists returns `EEXIST`. `fromguid == 0` is treated as a full stream. — [dmu_recv.c](https://raw.githubusercontent.com/openzfs/zfs/master/module/zfs/dmu_recv.c)
- [Sourced] At `DRR_END`, the receiver compares its running checksum with `drr_end.drr_checksum` and returns `ECKSUM` on mismatch. On failure, "If receive is not resumable, destroy what we created, so we don't leave it in the inconsistent state." — [dmu_recv.c](https://raw.githubusercontent.com/openzfs/zfs/master/module/zfs/dmu_recv.c)
- [Sourced] zfs-receive(8): for an incremental stream, the destination's "most recent snapshot must match the incremental stream's source". `-F` forces "a rollback of the file system to the most recent snapshot before performing the receive operation". With `-s`, "If the receive is interrupted, save the partially received state", which can later be resumed through `receive_resume_token`. — [zfs-receive.8](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zfs-receive.8)
- [Sourced] zfs-send(8): "The incremental source must be an earlier snapshot in the destination's history". A send "may be incremental from a bookmark". `-w`: "For encrypted datasets, send data exactly as it exists on disk". "The format of the stream is committed." — [zfs-send.8](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zfs-send.8)
- [Sourced] Restic's repository: "All files in a repository are only written once and never modified afterwards". Files are named by the SHA-256 of their contents, and "A snapshot references a tree by the SHA-256 hash". Backups take non-exclusive locks. "A client removing data must acquire an exclusive lock first." — [restic design.rst](https://raw.githubusercontent.com/restic/restic/master/doc/design.rst)
- [Sourced] GlobalObject's commit step: "The control plane verifies all `n` receipts, then lists the object in the account's catalogue". Uploads per account are serialised by a deliberate write conflict on the account document. The read token bucket is "a compare-and-set on the account document". — `docs/architecture.md`, `docs/status.md` (local)
- [Sourced] MongoDB's linearizable read concern returns data reflecting all majority-acknowledged writes completed before the read. It applies only to a filter that uniquely identifies one document, and only on the primary. The docs advise `maxTimeMS` in case a majority is unavailable. The older pattern is a `findAndModify` with `w: "majority"` on a uniquely indexed document. — [MongoDB read concern linearizable](https://www.mongodb.com/docs/v8.0/reference/read-concern-linearizable/), [findAndModify linearizable reads](https://www.mongodb.com/docs/manual/tutorial/perform-findAndModify-linearizable-reads/)
- [Sourced] CAP (Gilbert and Lynch, 2002): consistency means linearizability (an "atomic" data object), and a partition-tolerant register can't be both consistent and available in the asynchronous model. — [Gilbert & Lynch, SIGACT News 33(2), DOI 10.1145/564585.564601](https://www.comp.nus.edu.sg/~gilbert/pubs/BrewersConjecture-SigAct.pdf) (summary via search excerpt and [mwhittaker notes](https://mwhittaker.github.io/papers/html/gilbert2002brewer.html))
- [Background, not fetched] PACELC (Abadi, IEEE Computer 2012) adds that without a partition a system still trades latency for consistency. A majority-acknowledged CAS pays that latency.

### Inferences
- [Inference] **What the control plane must enforce.** Keep one document per chain: `{chain_id, dataset_id, head_toguid, head_seq, segments[] or stream ids}`. Commit stream `s` with the conditional update `{_id: chain, head_toguid: s.fromguid, head_seq: s.seq-1} → {head_toguid: s.toguid, head_seq: s.seq}`, plus `fromguid == 0` and `seq == 0` for a new chain. This rules out:
  - gaps (fromguid must equal the current head),
  - forks (two streams sharing a fromguid: only the first CAS wins),
  - reordering (seq is monotonic),
  - duplicate toguids, which an index on `(chain_id, toguid)` prevents.

  This is a linearizable append to a single-writer log, the same pattern as a per-chain register CAS. A single MongoDB document update at `w: "majority"` on the primary is atomic. Combined with uniqueness, it gives this guarantee without a multi-document transaction. Today's segment commit already uses a transaction (segment plus file name), so the head CAS could join that transaction.
- [Inference] **GUID verification at commit time.** OpenZFS checks the chain only at receive time, which means at restore, possibly years later. To catch a broken chain early, the control plane, or a host, must read `drr_fromguid` and `drr_toguid` from the BEGIN record at upload. It should also check `drr_end.drr_toguid` and the stream checksum at the end. [Background] In raw sends, the replay-record headers stay plaintext (only block payloads are encrypted), so this needs no keys. That is consistent with the earlier note that raw streams expose object and record structure. But a client could lie about which segment holds the BEGIN record. The control plane would need the first segment's first bytes, or a host's attestation of them. That is a new, small trust surface.
- [Inference] **Today versus ZFS.** Restic objects are immutable and content-named, and snapshots are independent files. So two devices backing up at once, or a retried upload, produce extra objects, never a conflict. The catalogue behaves like a grow-only set (CRDT-like), and correctness needs only per-object atomic commit. Note that the brief describes a "root/index pointer updated by CAS". I couldn't find such a single pointer in `architecture.md`: commits list objects individually. The ZFS design introduces a genuinely ordered, conflict-prone head that doesn't exist today.
- [Inference] **Partition between client and control plane mid-chain.** Three cases:
  1. *Before the CAS.* The stream's pieces may already be on hosts, but uncommitted. They are orphans that need garbage collection, as aborted uploads are today. The client must keep its local from-snapshot, or a **bookmark**, since `zfs send -i` accepts one, until the next CAS succeeds. Otherwise its next send must be a full send. While partitioned, the client keeps taking local snapshots. After healing it sends one `-i` (or `-I`, to keep intermediates) from the last committed GUID.
  2. *CAS outcome unknown* (request sent, reply lost). The CAS must be idempotent: if the retry finds `head_toguid == s.toguid` with the same stream id, report success. Otherwise the client could re-send a stream whose fromguid no longer matches, and get a spurious fork rejection.
  3. *Control-plane primary partitioned from its majority.* Writes and linearizable reads fail, so no CAS and no grants. This matches today's "Control plane down: No new grants" row. Availability in the CAP sense is unchanged, because the control plane already chooses C over A for grants and commits.
- [Inference] **Multi-device and forks.** Today two devices can write snapshots to one restic repository at once, using non-exclusive locks. With ZFS, each dataset has exactly one writer chain, because a second device has different snapshot GUIDs. Each device needs its own chain, or the second device's sends are rejected as forks.
- [Inference] **Rollback attacks.** The threat model already notes a compromised control plane can "hide or roll back snapshots". With chains it can also truncate a chain, or serve an older head, to a new device. The client's remembered head GUID is the same defence as today's remembered snapshot list. It is no weaker and no stronger.

### Gaps
- I didn't confirm from source that the raw-stream BEGIN header and record headers are unencrypted. That comes from background knowledge and the earlier note's statement that raw streams expose record structure. Check with `zstream dump` on a raw stream.
- I found no check of `drr_end.drr_toguid` against `drr_begin.drr_toguid` in the parts of `dmu_recv.c` I read.

---

## Q2. Availability of a restore: chain versus restic, with binomial calculations

### Takeaway
For a **full restore of the latest state**, the chain adds only the incremental segments to what restic would also need. The penalty is modest: at 1-of-3, 90% uptime, pg16, P(all available at once) falls from 0.359 (restic, or ZFS with no incrementals) to 0.266 with 30 daily incrementals. For a **single-file or partial restore**, the gap is enormous. Restic needs about 1 segment (P ≈ 0.999 at 1-of-3, p = 0.9). ZFS needs the whole chain (P ≈ 0.27). At 1-of-3, a 1 TiB chain is never "all online at once" with any useful probability below 95% uptime. 10-of-20 at 90% or more keeps even year-long chains above 99.6% with pg16. Rebasing caps chain length but costs about 4.6 days of 20 Mbps uplink per TB.

### Cited Findings
- [Sourced] Restic: "A snapshot references a tree", and "Trees and data are saved in pack files". A snapshot depends only on the packs holding blobs reachable from its tree. — [restic design.rst](https://raw.githubusercontent.com/restic/restic/master/doc/design.rst)
- [Sourced] GlobalObject's partial reads: "Reading a 20 KB file from a 24 MiB pack charged 37–39 KB under both policies." — `docs/status.md` (local)
- [Sourced] The analytic check in the sim report gives 1-of-3 at 90% uptime, 256 independent chunks: 77.40%. Correlated churn in the home mix gives 98.6% (1-of-3) and 98.1% (10-of-20) for a 1 GiB file with pg16. Placement group size 1 gives 96.9%, and 16 gives 98.6%. — `docs/sim-report.md`, `docs/architecture.md` (local)
- [Sourced] Restore and upload time: "4.6 days per TB on 20 Mbps up without banked credit." — `docs/sim-report.md` (local)
- [Sourced] An incremental needs its source: "The incremental source must be an earlier snapshot in the destination's history." — [zfs-send.8](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zfs-send.8)

### Computed results
Formulas:
- Per-segment availability A:
  - 1-of-3: A = 1 − (1 − p)^3.
  - 10-of-20: A = Σ_{i=10}^{20} C(20, i) p^i (1 − p)^(20−i).
- P(restore) = A^U, where U is the number of independent units the restore needs. U = S segments when placement is independent, or ⌈S/16⌉ with placement groups. For a chain of base plus N incrementals, S = 16,384 + 160·N.

[Computed] Per-segment availability (U = 1 − A):

| Policy | p = 0.80 | p = 0.90 | p = 0.95 |
|---|---|---|---|
| 1-of-3 | 0.992 (U = 8.0e-3) | 0.999 (1.0e-3) | 0.999875 (1.25e-4) |
| 10-of-20 | 0.999437 (5.6e-4) | 0.99999929 (7.1e-7) | 0.9999999995 (5.4e-10) |
| 1-of-4 | 0.9984 | 0.9999 | 0.99999375 |
| 1-of-5 | 0.99968 | 0.99999 | 0.9999996875 |

Sanity check: 0.999^256 = 77.40%, which matches the sim report's analytic 77.40%.

[Computed] P(all segments of snapshot N available at once). The pg16 column is first in each cell; the independent-placement value is in brackets.

| Policy, uptime | N = 0 (also restic full restore) | N = 30 | N = 90 | N = 365 |
|---|---|---|---|---|
| 1-of-3, 80% | 0.0003 [7e-58] | ≈0 | ≈0 | ≈0 |
| 1-of-3, 90% | 0.359 [7.6e-8] | 0.266 [6.2e-10] | 0.146 [4.2e-14] | 0.0093 [3e-33] |
| 1-of-3, 95% | 0.880 [0.129] | 0.848 [0.071] | 0.786 [0.021] | 0.558 [8.7e-5] |
| 10-of-20, 80% | 0.562 [9.8e-5] | 0.474 [6.5e-6] | 0.338 [2.9e-8] | 0.072 [5e-19] |
| 10-of-20, 90% | 0.9993 [0.9885] | 0.9991 [0.9851] | 0.9986 [0.9784] | 0.9967 [0.9484] |
| 10-of-20, 95% | 1.0000 | 1.0000 | 1.0000 | 1.0000 |

[Computed] One-file restore, restic versus ZFS:

| Policy, uptime | Restic, 1 segment | ZFS, N = 30, pg16 |
|---|---|---|
| 1-of-3, 80% | 0.992 | ≈0 |
| 1-of-3, 90% | 0.999 | 0.266 |
| 1-of-3, 95% | 0.99988 | 0.848 |
| 10-of-20, 80% | 0.99944 | 0.474 |
| 10-of-20, 90% | 0.999999 | 0.9991 |

[Computed] Largest chain (in independent units) that is all-available with probability ≥ 99% / ≥ 90%:

| Policy | p = 0.80 | p = 0.90 | p = 0.95 |
|---|---|---|---|
| 1-of-3 | 1 / 13 | 10 / 105 | 80 / 843 |
| 1-of-4 | 6 / 66 | 100 / 1,054 | 1,608 / 16,860 |
| 1-of-5 | 31 / 329 | 1,005 / 10,540 | 32,160 / 337,200 |
| 10-of-20 | 18 / 187 | 14,180 / 148,600 | 1.9e7 / 2.0e8 |

A 1 TiB base is 1,024 pg16 units, so only 10-of-20 at 90% or more, and 1-of-5 at 95%, keep it at the 99% level.

[Computed] Upload cost of rebasing (1 TB = 1e12 bytes):

| Uplink | Full send | Daily 10 GiB incremental |
|---|---|---|
| 10 Mbps | 9.26 d | 2.39 h |
| 20 Mbps | 4.63 d | 1.19 h |
| 50 Mbps | 1.85 d | 0.48 h |
| 100 Mbps | 0.93 d | 0.24 h |

At 20 Mbps, a full send every R days takes this share of uplink time, and caps the chain at R incrementals:

| R | 30 d | 60 d | 90 d | 180 d | 365 d |
|---|---|---|---|---|---|
| Uplink share | 15.4% | 7.7% | 5.1% | 2.6% | 1.3% |

Code (run with host python3; full output reproduced above):
```python
from math import comb, exp, log, ceil
def seg_avail(k, n, p):
    return sum(comb(n, i) * p**i * (1-p)**(n-i) for i in range(k, n+1))
def prod(a, m): return exp(m*log(a)) if a < 1 else 1.0
SEG = 64 * 2**20; BASE = 2**40 // SEG; INC = 160      # 16384 base segs, ~10 GiB/day
for k, n in [(1,3),(10,20)]:
    for p in [0.80, 0.90, 0.95]:
        a = seg_avail(k, n, p)
        for N in [0, 30, 90, 365]:
            S = BASE + N*INC
            print(k, n, p, N, prod(a, S), prod(a, ceil(S/16)))   # independent, placement groups of 16
# upload: 1e12*8/(mbps*1e6)/86400 days; uplink share of fulls = days_per_full / R
```
(The full script also computes the storage, durability and blast-radius tables. It is at `/tmp/claude-1000/-home-metahertz-projects-globalobject/7f649a04-68d0-44b2-818a-d178f6ae2e9a/scratchpad/chain.py`.)

### Inferences
- [Inference] **These "all at once" figures overstate the practical harm for large restores.** A 1 TiB restore takes about 4.6 days at 20 Mbps anyway. Hosts that are offline at one moment come back, so a restore that waits rather than fails will usually finish. The binomial product measures "can the restore stream without stalling", not "is the data recoverable". Durability (Q3) is the permanent-loss measure. The difference matters more for ZFS, because `zfs receive` consumes the stream strictly in order. One unavailable segment stalls the whole pipeline: the client can prefetch later segments, but can't apply them. Restic can restore files in any order and skip around unavailable packs. Resumable receive (`-s`) avoids restarting from zero after an abort.
- [Inference] The sim report's correlated-churn figures (98.6% for 1 GiB with pg16) are better than the independent model for a single file. For whole-chain restores, though, the chain length multiplies whatever per-unit unavailability correlated churn produces. The relative ranking (restic beats ZFS for partial reads; similar for full latest-state restores) should hold under correlated churn. The absolute numbers would need the simulator.
- [Inference] **Rebasing** bounds N, and so bounds both the chain penalty and the blast radius (Q3). Its cost is uplink time: 15% of a 20 Mbps uplink for monthly fulls of 1 TB. That competes with the user's other traffic and with ISP caps (an open risk in `architecture.md`). It also drops dedupe, since each full re-sends unchanged data. Restic never needs a periodic full.

### Gaps
- No measured per-host uptime distribution: 80/90/95% are scenario values. The correlated-churn simulator wasn't run for chains.
- No empirical comparison of ZFS incremental size with restic's deduplicated churn on real home datasets. The 1%/day figure is assumed for both.

---

## Q3. Durability under host churn, and the blast radius of one lost segment

### Takeaway
In a chain, a permanently lost segment in the base makes **every snapshot of the chain unrestorable in full**. In incremental `j`, it breaks snapshots j..N. `zfs receive` has no way to skip a damaged record. With 30 daily incrementals on a 1 TiB base, a random lost segment makes on average 89% of the chain's snapshots unrestorable (the whole dataset each time). In restic, the same loss damages only the files with blobs in that 64 MiB pack, about 0.006% of 1 TiB, across whichever snapshots reference them. Using the sim's (unvalidated) 1-of-3 loss rate, a 1 TiB chain has about a 6% chance per year of being broken.

### Cited Findings
- [Sourced] At DRR_END a checksum mismatch returns `ECKSUM`. A non-resumable failed receive destroys what it created: "so we don't leave it in the inconsistent state". An incremental receive needs a destination snapshot with GUID == fromguid, or it returns `ENODEV`. — [dmu_recv.c](https://raw.githubusercontent.com/openzfs/zfs/master/module/zfs/dmu_recv.c)
- [Sourced] Restic `repair snapshots` can "remove all inaccessible data from the snapshots", which "will result in a limited amount of data loss". `repair packs` repairs damaged packs and updates the index. — [restic troubleshooting](https://raw.githubusercontent.com/restic/restic/master/doc/077_troubleshooting.rst)
- [Sourced] Sim result: 1-of-3 lost "0.0003% of segments per year", which is "about 0.045 lost segments per year per TB stored (15,000 segments of 64 MiB)", with "loss arrives in groups". "Don't advertise a durability figure from this." 10-of-20 had no loss in the baseline runs, but did lose data in some harsher configurations. — `docs/sim-report.md` (local)
- [Sourced] Veeam: when a health check finds corrupted metadata in an incremental, Veeam removes "that incremental restore point and the subsequent ones" from the configuration. Corruption is fixed by building a new full from source data. — Veeam Help Center health-check pages ([v13](https://helpcenter.veeam.com/docs/vbr/userguide/backup_health_check.html?ver=13), [Hyper-V](https://helpcenter.veeam.com/docs/backup/hyperv/backup_health_check.html)). Search excerpt only: the page refused direct fetch.
- [Sourced] Earlier research found raw send/recv has a history of corruption bugs (zfs#12014, #8852, and #12614 still open per a search excerpt), with fixes into 2025. — `research_notes/Ceph and OpenZFS Rust port/openzfs.md` (local), citing [openzfs-docs#494](https://github.com/openzfs/openzfs-docs/issues/494) and [zfs#12614](https://github.com/openzfs/zfs/issues/12614)

### Computed results
- [Computed] One uniformly random lost segment in a chain of a 16,384-segment base plus N × 160-segment incrementals:

  | N | P(lost segment is in base) | Expected snapshots made unrestorable |
  |---|---|---|
  | 30 | 77.3% | 27.5 of 31 (89%) |
  | 90 | 53.2% | 69.7 of 91 (77%) |
  | 365 | 21.9% | 223 of 366 (61%) |

  Each affected snapshot loses the whole dataset, unless the client has a local copy.
- [Computed] Restic: one lost 64 MiB segment is at most 64 MiB of pack data, about 6.1e-5 of 1 TiB.
- [Computed] With λ = 0.045/15,000 per segment-year (sim's 1-of-3 figure, a Poisson approximation, P = 1 − e^(−λ·S·T)):

  | Chain | 1 year | 3 years |
  |---|---|---|
  | 30 incrementals | 6.2% | 17.4% |
  | 90 incrementals | 8.8% | 24.2% |
  | 365 incrementals | 20.1% | 49.0% |

  Restic over the same storage: about 0.05 segments per year per TiB, or about 3.2 MiB of pack data, confined to the files in those packs.

### Inferences
- [Inference] **The expected bytes lost are similar in both designs, but the loss distribution is very different.** In restic, the expected loss of about 3 MiB per year spreads as small, file-level, repairable damage. In a chain, it shows up as about a 6% annual chance of losing restorability of the whole TiB, and every snapshot in the chain at once. For backup, a heavy-tailed, all-or-nothing loss is much worse. It also turns "a lost segment" from a minor incident into a must-repair-or-rebase emergency.
- [Inference] **Restic's blast radius isn't zero across snapshots.** Dedupe means one data blob is often referenced by every snapshot. [Background] Restic keeps tree blobs and data blobs in separate packs, so a lost tree pack can hide a directory subtree in every snapshot sharing that tree. Even so, `repair snapshots` salvages the rest. ZFS has no equivalent partial salvage short of `zfs receive` with an intact stream. Corrective receive (`-c`) needs a good copy of the stream.
- [Inference] **Mitigations in GlobalObject terms.**
  1. Raise redundancy for base segments, for example 1-of-5 or 10-of-20 for chain bases. They have the largest blast radius.
  2. Rebase periodically. It caps chain lifetime exposure, at the uplink cost in Q2.
  3. Keep two overlapping chains, so a break in one leaves the other.
  4. Have repair prioritise chain segments by how many snapshots depend on them.

  These all cost space or uplink that restic doesn't need.
- [Inference] Receive and rebase use raw send/recv code paths with a recent history of corruption bugs. That adds software-side risk that a host-side redundancy calculation doesn't capture.

### Gaps
- The sim's loss rate is explicitly not a durability claim, and loss arrives in correlated groups. So the Poisson chain-break probabilities are illustrative only. Correlated loss would make the chain-break probability lower, for the same expected segments lost, but each event larger.
- I didn't find a source quantifying restic's per-snapshot blast radius as a function of dedupe ratio.

---

## Q4. Space reclamation: keeping whole chains versus restic forget and prune

### Takeaway
You can't delete a base or a middle incremental while a later snapshot is retained, so retention works at chain granularity. For "keep 30 daily" with 1%/day change, the peak overhead is 1.76× restic at monthly rebases, 2.2× at 90-day rebases and 4.3× with yearly rebases. Restic's prune is close to the live set plus the retention window's churn. Only full sends (rebases) let old chains be dropped, which ties storage and uplink cost together.

### Cited Findings
- [Sourced] An incremental needs its source snapshot in the destination's history. There is no supported way to merge or remove a middle incremental from a set of stored streams. — [zfs-send.8](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zfs-send.8), [zfs-receive.8](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zfs-receive.8)
- [Sourced] Restic prune removes unreferenced data and repacks partly used packs. `--max-unused` (default 5%) sets how much unused space is tolerated, and `--max-repack-size` bounds repacking. — [restic-prune man page](https://www.mankier.com/1/restic-prune), [restic forum](https://forum.restic.net/t/restic-not-obeying-max-unused-parameter-on-prune/8879) (search excerpts)
- [Sourced] GlobalObject already ships `forget`/`prune` through `rustic_core`. In `t_forget_and_prune_reclaim_space`, 2 packs (8.4 MB) were deleted, quota came back at once, hosts freed the pieces, and the newer snapshot restored. — `docs/status.md` (local)
- [Sourced] Bacula's VirtualFull "consolidate[s] the previous Full backup plus the most recent Differential backup and any subsequent Incremental backups into a new Full backup", "without requesting any data from the client". — [Bacula Systems docs](https://docs.baculasystems.com/AdvancedFeaturesUsage/VirtualFullJobs/index.html) (search excerpt)

### Computed results
[Computed] Model: 1 TiB live data, 10.24 GiB/day of changed data that is entirely new, kept for 30 daily snapshots. Restic uses about 1,024 + 30 × 10.24 ≈ 1,331 GiB. For ZFS with a rebase every R days, the peak comes just after a rebase, when the old chain (base + R−1 incrementals) is still needed for in-window snapshots alongside the new base.

| R (days) | Peak storage | vs restic | Just before rebase |
|---|---|---|---|
| 30 | 2,345 GiB | 1.76× | 1,321 GiB |
| 60 | 2,652 GiB | 1.99× | 1,628 GiB |
| 90 | 2,959 GiB | 2.22× | 1,935 GiB |
| 180 | 3,881 GiB | 2.92× | 2,857 GiB |
| 365 | 5,775 GiB | 4.34× | 4,751 GiB |

These are logical bytes. Raw bytes multiply by 3 for 1-of-3, or 2 for 10-of-20.

### Inferences
- [Inference] Quota is charged on raw bytes, so a 1.8 to 4.3× logical overhead directly cuts the usable backup size per contributed TB. That is about 267 GB at 1-of-3 today (`architecture.md`). Usable *protected live data* would fall to roughly 267/1.76 ≈ 150 GB at monthly rebases.
- [Inference] Short R reduces storage overhead and chain fragility, but raises uplink cost. Long R does the reverse. No R matches restic on both.
- [Inference] A keyless "synthetic full" is possible in principle. Raw streams can be received on an untrusted machine without keys ("The backup may then be received on an untrusted machine", per the earlier note citing zfs-send.8), and an encrypted dataset can be raw-sent with keys unloaded. So a helper node running ZFS could `zfs receive` the chain and `zfs send -w` a new full of the head, like Bacula's VirtualFull or Veeam's synthetic full, without the owner's uplink. Costs and risks:
  - It downloads and re-uploads about 1 TB over home links from the repair reserve.
  - The helper needs a ZFS pool at least as large as the dataset.
  - The helper isn't trusted. Block MACs in raw streams make tampering detectable at restore, but omission or substitution of whole objects may not be detectable without keys. I didn't verify this, so treat it as an open question.
  - It runs exactly the raw recv/send paths with a bug history.

### Gaps
- No source on whether a raw re-sent full produced by a third party can be verified as complete without keys.
- The storage model assumes all daily change is new data. Overwrites that shrink or repeat data would favour restic's dedupe more.

---

## Q5. Partial reads: restoring one file, bandwidth entitlement and restore time

### Takeaway
With streams as the only interface, a one-file restore costs a full-chain restore: download base + incrementals, then `zfs receive` into a local pool with enough free space, then copy the file out. At 20 Mbps entitlement, a 20 KB file costs about 1.3 TB of granted reads and about 6 days. Restic, as built, charges 37–39 KB.

### Cited Findings
- [Sourced] GlobalObject's range grants: "A small read from a large object asks for just that byte range… only those bytes are granted, sent and charged." Measured: 20 KB from a 24 MiB pack charged 37–39 KB. — `docs/architecture.md`, `docs/status.md` (local)
- [Sourced] Bandwidth rule: "Aggregate download rate ≤ measured offered upload capacity". Reads are debited up front at grant issue. Restoring 1 TB at the entitled rate takes 4.6 d at 20 Mbps up, or 0.2 to 1.2 d with banked credit. — `docs/architecture.md`, `docs/sim-report.md` (local)
- [Sourced] "Raw sends and redacted sends cannot be combined at this time." — earlier note citing [zfs-send.8](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zfs-send.8) (`research_notes/Ceph and OpenZFS Rust port/openzfs.md`)
- [Sourced] Raw streams "may only be received as is, and cannot be re-encrypted, decrypted, or recompressed by the receive process." — [zfs-receive.8](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zfs-receive.8)

### Computed results
- [Computed] Base plus 30 daily incrementals is 1 TiB + 300 GiB, about 1.43e12 bytes. At 20 Mbps that is about 6.6 days. The relative cost for a 20 KB file is about 3.7e7× restic's 38 KB charge.
- [Computed] At 100 Mbps the same transfer is about 1.3 days.

### Inferences
- [Inference] **The entitlement rule becomes a much harsher constraint.** One file-level restore spends the user's whole banked credit, and debits the up-front token bucket by about 1.3 TB. That blocks other reads for days. Owners would rationally avoid small restores, or keep a full local replica, which defeats the purpose.
- [Inference] **Restores need a ZFS pool locally**, with free space of at least the dataset size. Raw receive keeps the data encrypted, so keys are loaded after receive. Restore therefore isn't possible on non-ZFS machines, such as a new laptop after a disaster, without first installing ZFS or a userspace DMU (see the earlier note's portability findings).
- [Inference] An "index on the side" would restore partial reads: for example, the client also uploads an encrypted file-to-object map plus per-object offsets in the stream. But stream records depend on prior state (frees, object reallocations across incrementals), so reconstructing one file from raw records without `zfs receive` means reimplementing the DMU replay. In practice that rebuilds restic's index under a less suitable format. This contradicts the premise that streams are the *only* interface.
- [Inference] **Repair and audit are unaffected.** They work on encoded segments and pieces, which are opaque either way.

### Gaps
- No measurement of `zfs receive` throughput on home hardware for raw streams. Restore time assumes the network is the bottleneck.

---

## Q6. Does the control plane get simpler, and does its CAP role change?

### Takeaway
Somewhat simpler at the catalogue layer: one chain record per dataset replaces many pack, index and snapshot object names. But the **dominant metadata** (one `segments` document per 64 MiB, with embedded pieces, repair and audit state) is unchanged, because a stream is still split into segments for placement. The CAP role is still CP: no grants or commits without a majority. What changes is that correctness now *depends* on linearizable ordering (head CAS), where today it needs only per-object atomic inserts. Confidentiality is unchanged with raw sends: neither hosts nor the control plane see keys. Metadata leakage grows (dataset and snapshot names, properties, and structure in the clear).

### Cited Findings
- [Sourced] Collections today: `segments` (pieces embedded), `objects` ("Repository file name → segment"), `accounts`, `nodes`, `ledger`, `read_grants` and others. Commit is a transaction over segment and file name. "Control plane down: No new grants. Data stays safe but inaccessible." — `docs/architecture.md` (local)
- [Sourced] `-w`: "send data exactly as it exists on disk. This allows backups to be taken even if encryption keys are not currently loaded. The backup may then be received on an untrusted machine." — earlier note citing [zfs-send.8](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zfs-send.8)
- [Sourced] ZFS native encryption "will not encrypt metadata related to the pool structure", including dataset and snapshot names, hierarchy, properties, file size and holes. — earlier note citing [zfs-load-key.8](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zfs-load-key.8)
- [Sourced] CAP: linearizable registers can't stay available under partition (Gilbert & Lynch, cited in Q1). MongoDB linearizable operations need the primary to reach a majority, and time out otherwise ([MongoDB docs](https://www.mongodb.com/docs/v8.0/reference/read-concern-linearizable/)).

### Inferences
- [Inference] **Object counts, 1 TiB with 30 dailies.**
  - Restic: about 1,331 GiB / ≤48 MiB packs ≈ 28k pack objects (more in practice, since packs aren't full), plus index files and 30 snapshot files.
  - ZFS: 31 stream records in one chain document. But the streams still occupy about 21k segments, each needing its own placement and pieces document.
  - So `objects` shrinks by about 3 orders of magnitude, while `segments` (repair scans, audits, placement) stays the same size or grows with the Q4 storage overhead. The net control-plane load doesn't drop materially.
- [Inference] **New control-plane duties:**
  1. Per-chain head CAS and fork rejection.
  2. Verifying fromguid and toguid from the stream header at commit.
  3. Retention by chain, refusing deletes that would orphan dependants. This is a referential-integrity check today's prune doesn't need from the control plane.
  4. Repair priority weighted by dependants.
  5. Rebase scheduling and orphan-stream garbage collection.

  Today's restic design leaves snapshot semantics entirely to the client, so the control plane stays semantics-free. The ZFS design pushes backup-chain semantics into the control plane.
- [Inference] **CAP role.** Writes, meaning commits and the head CAS, need the primary and a majority. That is CP, as today. Reads needing grants are also unavailable during a control-plane partition, as today. Under PACELC, every commit already pays majority-write latency, and the head CAS adds no extra round trip if folded into the commit transaction.

  One subtle improvement: because restore follows a known chain of GUIDs, a client that cached its chain map and piece locations could in principle restore during control-plane unavailability, if owner-signed recovery grants existed (listed as future work in `architecture.md`). The same is true of restic plus a cached piece map, so this isn't a differentiator.
- [Inference] **Rollback and fork detection get easier to state.** The head is one GUID, so a client or auditor can pin it. Truncation by a compromised control plane is detected by any client that remembers its last toguid, as with remembered snapshot IDs today.

### Gaps
- I didn't measure actual restic pack-count distributions from `rustic_core` (pack fill factor), so the object-count comparison is approximate.

---

## Q7. Literature and practice on incremental-chain fragility and mitigations

### Takeaway
Enterprise backup treats forever-incremental chains as fragile. The standard mitigations are:
- periodic active fulls,
- synthetic or virtual fulls built server-side from existing backups (Bacula VirtualFull, Veeam synthetic full),
- reverse incremental, where the newest point is always a full,
- scheduled health checks that, on corruption, drop the damaged point *and all later ones* and force a new full.

Restic-style content-addressed repositories sidestep the chain entirely. In GlobalObject, server-side synthesis can only be done keylessly by a ZFS-capable helper, at about 1 TB of transfer each way.

### Cited Findings
- [Sourced] Veeam reverse incremental: "the most recent restore point in the backup chain is always a full backup", rebuilt each session. Older points are reverse-incremental (VRB) files applied to the full (VBK). It is deprecated for new jobs from Veeam B&R v13. — [Veeam reverse incremental (v12)](https://helpcenter.veeam.com/docs/backup/hyperv/reversed_incremental_backup.html?ver=120), [v13 "Deprecated" page](https://helpcenter.veeam.com/docs/vbr/userguide/reversed_incremental_backup.html) (search excerpts)
- [Sourced] Veeam health check on forever-forward chains: on corruption it builds a new full from source. Corrupted incremental metadata leads Veeam to remove that restore point "and the subsequent ones". If the latest point is incomplete, an active full is needed. — [Veeam health check v13](https://helpcenter.veeam.com/docs/vbr/userguide/backup_health_check.html?ver=13), [Hyper-V](https://helpcenter.veeam.com/docs/backup/hyperv/backup_health_check.html) (search excerpts)
- [Sourced, opinion] Practitioners argue long incremental chains and synthetic-only fulls raise corruption exposure, and that "a periodic active full restarts the backup chain". These are user opinions, not vendor guidance. — [Veeam community](https://community.veeam.com/discussion-boards-66/question-about-forever-forward-backups-5662), [serverfault mirror](https://kiwix.lema.org/serverfault.com_en_all_2023-11/questions/947616/veeam-target-how-to-detect-crc-errors-in-time) (search excerpts)
- [Sourced] Bacula VirtualFull consolidates Full + Differential + Incrementals into a new Full "without requesting any data from the client". It "will automatically discard jobs that are not related to the current Job". — [Bacula Systems docs](https://docs.baculasystems.com/AdvancedFeaturesUsage/VirtualFullJobs/index.html) (search excerpt)

### Inferences
- [Inference] Mapping enterprise mitigations to GlobalObject's ZFS variant:

  | Mitigation | Cost in GlobalObject |
  |---|---|
  | Active full | The owner's uplink: 4.6 d/TB at 20 Mbps |
  | Synthetic full | A ZFS helper with keyless raw recv/send; about 1 TB down and 1 TB up from the repair reserve; trust and verification open (Q4) |
  | Reverse incremental | Needs a component that can rewrite the full each night. Keyless that means the helper again, daily. Infeasible over home links |
  | Health checks | Feasible: periodic `zfs receive` dry-runs by a helper or the owner. Detection only; repair still needs a full |

- [Inference] These products assume a trusted, well-connected backup server that can read and rewrite backup files. GlobalObject's untrusted, bandwidth-poor, keyless hosts are the worst environment for chain maintenance. That is why a content-addressed, independently restorable snapshot format (restic) fits better.
- [Inference] Veeam deprecating reverse incremental in v13 suggests that even with good infrastructure, the industry is moving towards forward chains plus synthetic fulls on block-cloning filesystems (ReFS/XFS fast clone). Those are server-side operations GlobalObject's hosts can't perform on encrypted data without ZFS.

### Gaps
- I found no peer-reviewed quantitative study of incremental-chain reliability versus chain length. The sources are vendor docs and practitioner opinion.
- The Veeam pages couldn't be fetched directly. Quotes come from search excerpts, so check the wording against the live pages before quoting in a final report.
- The Commvault documentation on synthetic fulls wasn't consulted.
