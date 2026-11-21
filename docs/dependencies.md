# Dependencies

GlobalObject builds its storage, crypto and transport on existing open-source projects. Custom code is limited
to what's specific to reciprocal storage.

| Job | Project | Licence | Maturity, as of 2026-11 |
|---|---|---|---|
| Chunking, dedupe, compression, encryption, keys, snapshots | [`rustic_core`](https://github.com/rustic-rs/rustic_core), restic's repository format | MIT or Apache-2.0 | **Pre-1.0**, API may change |
| Peer connections, encryption in transit, hole punching, relays | [`iroh`](https://github.com/n0-computer/iroh) | MIT or Apache-2.0 | 1.x |
| Verified transfers and range proofs (BLAKE3) | [`iroh-blobs`](https://github.com/n0-computer/iroh-blobs), `bao-tree` | MIT or Apache-2.0 | **Pre-1.0**. Its docs have warned that recent versions aren't production quality |
| Host blob storage | `iroh-blobs` filesystem store, on `redb` | MIT or Apache-2.0 | Same |
| Erasure coding, including local-parity (pyramid) codes built from its own calls | [`reed-solomon-erasure`](https://crates.io/crates/reed-solomon-erasure) | MIT | Established, but no release since 2022 |
| Snapshot retention (`forget`) and space reclaim (`prune`) | `rustic_core`, with `jiff` for its time rules | MIT or Apache-2.0; `jiff` MIT or Unlicense | As `rustic_core` |
| Grants, receipts, request auth | [`pasetors`](https://github.com/brycx/pasetors), PASETO v4.public | MIT | Pre-1.0, implementing a published spec |
| Deriving the signing key from a phrase | `argon2` (Argon2id) | MIT or Apache-2.0 | Established |
| Control-plane state | MongoDB 8.0 server, as a replica set, through the official [`mongodb`](https://crates.io/crates/mongodb) Rust driver | Driver Apache-2.0, `bson` MIT. Server SSPL: see Licences | Established |

## Still custom

This is the part to scrutinise. None of it invents cryptography.

| Code | Why it's custom |
|---|---|
| Reciprocal quota and bandwidth ledger | This is the product. No project found has it |
| Placement and failure-domain rules | Needs per-owner and per-pool rules, and existing placement code is either AGPL or tied to its system |
| Repair and audit scheduling | Same. Transfers and proofs underneath are `iroh-blobs` |
| Grant checks at hosts | Policy on top of `iroh-blobs`' request hooks, including byte ranges and refusing hash-sequence requests |
| Ranged verified fetch (`globalobject_net::fetch_range`) | A copy of `iroh-blobs`' own `get_blob` loop, for ranges. Proposed upstream as `get_blob_ranges` (`upstream/iroh-blobs-get-blob-ranges.md`) |
| Mapping repository files to segments | A thin backend for `rustic_core`, using its public trait |

## Quorum and consensus

Users' nodes never run a quorum protocol. Every decision goes through the control-plane API, and the MongoDB
database behind it is the source of truth. Reacting to failures doesn't need agreement among peers, only the control
plane noticing. MongoDB replica sets provide failover for that database, with three members rather than the one used in
development, but making the control-plane processes themselves highly available is still an operator concern, for later.

## Licences

Rule: permissive only, plus MPL-2.0 for two packages used unmodified. `scripts/check-licences.py` enforces this in CI.

| MPL-2.0 package | Comes from | If it had to go |
|---|---|---|
| `attohttpc` | `iroh`'s default port mapping | Disable the `portmapper` feature. More traffic may then need relays |
| `option-ext` | `rustic_core`, through `dirs` | Needs a change or patch upstream |

The rule covers code we ship or link. The MongoDB server is SSPL, which isn't permissive, but we only run it unmodified as
our own backend (or use Atlas), and nothing of it is linked into our binaries. That use was approved explicitly on
2026-11-08. SSPL's section 13 would apply only if we offered MongoDB itself as a service. If that ever has to change,
the wire protocol is also served by FerretDB (Apache-2.0), untested here.

## Replaced or removed

| Before | Now |
|---|---|
| Own chunked encryption format, padding and manifest | restic repository format |
| Own Merkle tree and audit proofs | BLAKE3 verified streaming |
| HTTP between peers | `iroh` QUIC, which also brings the NAT traversal that was next on the roadmap |
| Own signed grants and request signatures | PASETO tokens |
| Replica whitening | **Removed.** See the threat model for what this costs |
| Size-class padding | **Removed.** Pack sizes are visible, as with any restic backend |
| Signed root pointer | Restic names files by hash, so contents can't be forged. Hidden snapshots are caught by a check against a local record |
| Postgres, through `sqlx` | MongoDB, through the official driver. Data model in `crates/control-plane/src/db.rs` |

## Rejected

| Project | Why |
|---|---|
| Storj, Garage | AGPL. Excluded by the licence rule |
| Ceph, Rook | Needs about 10 ms between data sites, and trusted nodes |
| Tahoe-LAFS | GPL or TGPPL, with no recent upstream activity found |
| etcd, Patroni | Not suitable for users' nodes. Could be considered for the operator's own database later |
