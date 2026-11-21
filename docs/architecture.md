# GlobalObject architecture

GlobalObject is reciprocal cloud storage. Each user contributes a slice of their
own disk and receives encrypted, redundant storage on other users' disks. Remote
space is capped by what they contribute, and speed by what their link contributes.

Status: prototype. See `docs/status.md` for what is built. Numbers marked *provisional* are
starting values. `docs/sim-report.md` has the evidence and limits for simulated defaults.

## Goals

1. Data is encrypted before it leaves the owner's machine. No other party, including
   the operator, can read it.
2. Data lives on at least 3 independent failure domains. The policy is configurable.
3. Quota and bandwidth limits hold even if the user modifies their own client.
4. Repair needs neither the owner nor their keys.
5. Private groups can run their own isolated pool on the same service.

## Non-goals (for now)

Block devices, full POSIX semantics, databases or VM images on the mount, anonymity
from the operator, payments, and sharing between users.

## System overview

```
 User machine                                          Operator
┌───────────────────────────────┐              ┌──────────────────────────┐
│ Client: rustic_core (restic   │   metadata,  │ Control plane (no keys)  │
│ format) → encode → pieces     │    grants    │ identity · pools         │
│ Keys never leave this machine │◄────────────►│ placement · grants       │
├───────────────────────────────┤              │ ledger · audit · repair  │
│ Node: slice store · serve     │◄────────────►│ MongoDB (replica set)    │
└──────────────┬────────────────┘   liveness,  └──────────────────────────┘
               │ iroh QUIC, peer to peer        no file data passes through
               ▼
        other users' nodes: encrypted pieces only
```

**Client.** A restic-format repository through `rustic_core`, with a backend that stores its files on the grid. Holds the keys.

**Node.** Stores opaque pieces in an `iroh-blobs` store, capped to its slice, and serves them only
against a valid grant. It never needs to understand what it holds.

**Control plane.** Decides placement, signs grants, keeps the ledger, audits, and
drives repair. It holds no decryption keys and no file names.

## Terms

| Term | Meaning |
|---|---|
| Pool | Isolation boundary. Placement, ledger and policy are per pool, and credit never crosses pools |
| Slice | Disk capacity a node offers to one pool |
| Object | A file in the account's restic-format repository: a pack, index, snapshot, key or config |
| Segment | The unit of placement: one object's bytes |
| Piece | One of a segment's `n` stored fragments. Any `k` rebuild the segment |
| Grant | Short-lived signed permission for one operation on one piece at one host |
| Receipt | Signed proof that a host stored or served bytes |
| Raw bytes | Bytes on disk after redundancy. Quota is accounted in raw bytes |

## Backup path

1. **Back up.** `rustic_core` chunks, dedupes within the account, compresses, encrypts and packs. See `docs/dependencies.md`.
   The result is ordinary restic repository files, which were designed to sit on untrusted storage.
2. **Encode.** For each file, produce `n` pieces. `k=1` means identical copies, and `k>1` means Reed-Solomon.
   Encoding after encryption is what allows repair without keys.
3. **Hash.** BLAKE3 identifies each piece, and the control plane stores only that.
4. **Place.** Request placement. The control plane checks quota, chooses hosts, and returns signed grants.
5. **Transfer.** Each host fetches its piece from the client, verifying it as it arrives and refusing more than the grant
   allows, then returns a signed receipt.
6. **Commit.** The control plane verifies all `n` receipts, then lists the object in the account's catalogue.

## Restore path

1. `rustic_core` asks for repository files, as it would from any backend.
2. For each file's segment, request read grants for enough pieces to decode. The token bucket here enforces the bandwidth limit.
   A small read from a large object asks for just that byte range. Codes are systematic, so the range lives in the data pieces
   covering it, and only those bytes are granted, sent and charged. If a data piece is out of reach, the client decodes the whole
   segment instead.
3. Authorise with each host, then fetch, with the stream verified against the piece's hash.
4. Send a signed receipt, so the host earns credit.
5. Rebuild if needed. `rustic_core` then decrypts. A failed host means trying another piece.

The client holds whole files in memory, with limits on how many at once, so restores use about a few hundred MiB.

## Encryption and keys

Restic's scheme: a master key, wrapped in key files by a password through scrypt, and stored in the repository. Here, the password
comes from the recovery phrase. The account's signing key is derived separately from the same phrase with Argon2id, so the phrase alone
is enough to recover everything on a new machine.

- **No operator recovery.** Losing the phrase loses the data. The product must say so.
- **No cross-user dedupe.** Each account's repository has its own keys, which avoids the file-confirmation attacks known from
  convergent encryption, as documented by Tahoe-LAFS.

## Redundancy policy

```
policy: { k, n, repair_at, upload_extra, min_failure_domains: 3, domain_level: owner, local_groups: 0 }
```

- **Local groups** (optional) make the code a pyramid code. Of the `n` pieces, `k` are data in `local_groups` groups, each group
  has a local parity, and the rest are global parities. A single lost piece in a group is rebuilt from `k / local_groups`
  pieces instead of `k`, which roughly halves repair traffic over home uplinks at `local_groups = 2`. Any `r + 1` losses are
  survivable, where `r` is the number of global parities. Against plain `RS(k, n)` at the same `n`, that gives up guaranteed
  tolerance of the last `local_groups - 1` losses. The simulator hasn't yet weighed that trade, so no pool defaults to it. See
  `plans/reads-reclaim-repair.md`, PR 4.

- A segment is **healthy** when at least `repair_at` of its pieces are on hosts seen within
  the grace period (*provisional:* 72 h). Without this grace, every overnight shutdown
  would trigger repair.
- Writes are refused, never downgraded, when a pool has too few failure domains.
- **Defaults** (from `docs/sim-report.md`): `1-of-3` below 40 owners or under 75% mean uptime, otherwise
  `10-of-20`. `1-of-4` and `1-of-5` are worthwhile for important data. `10-of-16` shouldn't be offered.
- The simulator found EC's benefit is **space and durability, not availability**: under correlated churn a 1 GiB
  file is readable 98.6% of the time at 1-of-3 and 98.1% at 10-of-20. Below about `k/n` uptime, EC fails badly.
- The prototype sets policy per pool. Automatic switching and migration between policies aren't built yet.

## Placement

Failure-domain hierarchy: disk < host < owner < site < ASN < country.

At most one piece of a segment per domain at the pool's `domain_level`, and at most one per
site in the public pool. A user with three disks is therefore one domain. The owner's own
nodes are never eligible for their own pieces, because a local copy does nothing for the
failures redundancy exists to survive.

The control plane chooses hosts, never the client, weighted by free space and reputation.
Random selection limits a Sybil attacker: holding fraction `f` of nodes gives all 3 replicas
of about `f³` of segments.

Segments are assigned to placement groups of 16, so one file's segments share hosts. Otherwise
availability drops with file length: at 90% uptime a 1 GiB file of 256 independently placed
1-of-3 chunks is fully readable only 77% of the time. Simulated at 1-of-3 in the home mix, group
sizes of 1 and 16 give 96.9% and 98.6%.

## Identical replicas

With `k=1`, an operator holding several copies of a segment under different identities can keep just one. The prototype accepts this, and
relies on random placement and the per-site limit. An earlier design disguised replicas with a keystream, but that was a custom construction, and was
removed in favour of existing code. Erasure-coded pieces aren't affected, which is one more reason to use EC in larger pools.

## Quotas

The client is untrusted. A host stores or serves only with a control-plane grant, and only
granted actions affect the ledger.

```
credited = min(pledged, proven_held × headroom + probation_allowance) × availability
quota_raw = credited × (1 − reserve)
```

Simulated defaults: `reserve = 0.20`, `headroom = 1.5`. A pool at its limit recovers from losing a
share of hosts up to `reserve`.

| Contributed | Policy | Usable |
|---|---|---|
| 1 TB | 1-of-3 | about 267 GB |
| 1 TB | 10-of-20 | about 400 GB |

**Invariant: `headroom × (1 − reserve) > 1`.** The approved plan had no `headroom` term, in
effect `headroom = 1`. Then total stored data can never exceed `Σ allowance / reserve`,
however much is pledged: at 20% reserve, 4× the allowances. The pool stalls. The invariant
lets credit grow with held data until it reaches the pledge.

**False pledges.** Empty space can't be cheaply proven. Credit runs ahead of proof only by a
bounded factor, and placement favours emptier hosts, so a false pledge is soon tested. Refusing
a placement costs reputation and credit. Spot-check space proofs are a future option.

**Lifecycle when over quota.** Grace (14 days) → read-only (30 days) → deleted by lease expiry.
All provisional. Your data lasts only as long as your contribution, which removes the profit
from contributing, uploading, then leaving.

**Shrinking.** A node drains pieces elsewhere before capacity is released.

## Bandwidth

**Rule.** Aggregate download rate ≤ measured offered upload capacity. Bytes actually served
bank burst credit.

**Measurement.** Scheduled probes from several random peers, using the median, because an idle
pool has no real traffic to measure. One peer probes a target per round, because overlapping probes
split the link and under-measure it. New nodes are probed right after joining, rather than waiting for
the next round. Self-imposed limits and partial uptime lower the result.

**Enforcement.**

1. The control plane issues read grants from a token bucket at the entitled rate. This caps the
   aggregate across all hosts, which per-host limits can't. Reads are charged up front and may
   take the bucket into debt that later reads wait out. Requiring tokens beforehand would double
   charge alongside host pacing, and halve a sequential reader's rate.
2. Hosts pace each transfer to the rate in the grant, through `iroh-blobs`' throttling hook.

**Accounting.** Reads debit the reader on grant issue. Receipts credit the host. A reader who withholds a receipt gains nothing, but
the host loses credit for that grant: a whole piece, or for a partial read, the range rounded to 16 KiB. Fabricating served bytes with a friend is zero-sum.

**Other traffic.** Uploads are limited by the user's own uplink. Repair draws on a rate-limited
pool reserve, not the owner's entitlement, since the owner may be offline.

## Audits and reputation

- The auditor picks a random range of a random piece and verifies it against the piece's hash, using BLAKE3 verified streaming. 300 audits catch
  1% loss with 95% probability. 3,000 catch 0.1%.
- New hosts begin on probation.
- Failures add up, and passes on other pieces don't cancel them. Enough of them leads to disqualification and repair. Audits prove possession, not an operator's
  real-world independence.

## Pools

| | Public | Private |
|---|---|---|
| Joining | Accountable invite tree, open signup later | Admin invites |
| Policy | Operator | Admin, floor of 3 failure domains |
| Reputation | Strict | Same audits, softer thresholds |

Admins never hold keys and can't disable encryption. Contribution-based quota applies everywhere.
Removing a member drains their pieces first. Most private pools stay on `1-of-3`, since EC needs
about 20 independent hosts.

## Control plane

Stateless services over MongoDB, built as reconcile loops that compare desired redundancy with
reality and then act. The pattern is borrowed from Kubernetes operators such as Rook.

Documents follow the access patterns (`crates/control-plane/src/db.rs`):

| Collection | Holds | Embedded |
|---|---|---|
| `segments` | One stored object's placement and state | Its pieces (at most `n`), and any repair in flight for each |
| `accounts` | Identity, standing, token bucket, banked credit | Placement groups, one per `(k, n)` |
| `nodes` | Address, pledge, standing, audits, measured rate | Latest probe samples (capped), accounts that reported refusals |
| `pools`, `invites` | Policy and quota settings, invitations | |
| `objects` | Repository file name → segment | |
| `ledger`, `read_grants` | Append-only accounting, issued read grants | |
| `pending_deletes`, `pending_probes`, `counters` | Work queued for nodes, numeric id sequences | |

Embedding pieces makes committing a segment, claiming a repair and finishing it each one atomic update to one document.
Transactions remain where several documents change together: registering (invite and account), starting an upload
(quota check, placement group and new segment, serialised per account by a write to its document), committing (segment
and file name), finishing a repair and removing a segment (segment, delete queue and file name). They need a replica
set, which a single node satisfies in development.

Known scaling limits, acceptable for the prototype: each repair round scans a pool's committed segments, and each audit
round samples across all stored pieces, as the Postgres queries before them did.

| Failure | Result |
|---|---|
| Control plane down | No new grants. Data stays safe but inaccessible until it returns |
| Control plane compromised | Attacker can delete, misplace or roll back, but can't read |
| Operator disappears | Clients keep an encrypted piece map. Owner-signed recovery grants are future work |

## Access layers

The client is snapshot-based backup: `globalobject backup`, `globalobject snapshots` and `globalobject restore`. Restic's format suits this, and the simulator suggests
it's also the product's best fit. Rustic's read paths could later support a read-only mount. A writable S3 gateway would be harder on this
format. A hosted gateway would break end-to-end encryption, so we won't offer one.

## Network

Peers use `iroh`: encrypted QUIC, addressed by public key, with hole punching and relay fallback. The best independent measurement we found
was about 70% hole-punch success, conditional on relay setup succeeding, so relays carry real traffic. `iroh` uses public relays by default, and running
our own is possible. Only loopback has been tested so far.

## Prior art

| System | Taken | Left |
|---|---|---|
| Storj | Ideas: central metadata, vetting, audits, repair threshold, segments | AGPL code, token payouts, very wide codes |
| Tahoe-LAFS | Client-side crypto, dumb servers, leases | Cross-user convergent encryption |
| Ceph | Failure-domain hierarchy, placement groups | RADOS: about 10 ms RTT guidance between data sites |
| Rook | Reconcile loops | Kubernetes on user machines |
| ZFS | Merkle integrity ideas, now provided by BLAKE3 | Dependency on one filesystem |
| Garage | Zone-aware placement ideas | AGPL, and its trusted-node model |
| restic | **Its whole repository format, through `rustic_core`** | Its own transport and backends |
| Samsara | Reciprocal claims | Purely peer enforcement |
| Sia, Filecoin | Proofs of holding data | Blockchain |
| Wuala, Symform | Reciprocal storage has been sold; users need an exit | Demand remains unproven |

## Open risks

1. **Legal.** Hosts hold content they can't inspect, across jurisdictions. Get legal advice on
   takedown, liability, terms and data location before public signup.
2. **Demand.** About 267 GB per contributed TB, and days to upload. Best fit is backup and archive.
3. **Host traffic.** ISP data caps and terms of service.
4. **Correlated outages.** Independent-failure maths is a best case.
