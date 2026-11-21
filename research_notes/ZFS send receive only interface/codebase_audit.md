# Codebase audit: what changes if ZFS send/receive streams become the only client interface

Scope: the local repository at `/home/metahertz/projects/globalobject`, read on 2026-11-20 without changes. Sources that start with `repo:` are files in that repository; line numbers are from that date. **Measured** means the figure came from `wc -l` or a script run over the files or `Cargo.lock`. **Estimated** means an engineering judgement, and it's labelled each time. ZFS behaviour is cited from the OpenZFS man pages.

Headline figures (measured): the workspace has **8,888 lines of Rust** across 10 crates, excluding `target/`. `Cargo.lock` has **637 packages**. `rustic_core`'s transitive closure is **231 packages**. Only **70 of them are reachable solely through `rustic_core`, `jiff` and `bytesize`**, so those 70 would leave the build and 567 would stay. All of the format-specific code is in one crate, `crates/client`, plus a single constant in the control plane. Every other crate is agnostic to the backup format.

## 1. What each crate does today, and which depend on rustic_core and the restic format

### Takeaway
Only `globalobject-client` depends on `rustic_core`, `jiff` and `bytesize`. Restic shows up elsewhere in exactly two places: a whitelist of restic file types in the control plane, and a doc comment in `proto`. Segment, crypto, net, node, policy, sim and the control-plane logic are format-agnostic. The restic dependency is narrow, but it is load-bearing. **All file encryption, chunking, dedupe, compression and snapshot handling comes from `rustic_core`.** `crates/crypto` does no data encryption.

### Cited Findings

Lines of code per crate and file (measured, `wc -l` on `crates/*/src` and `tests`):

| Crate | Files (LOC) | Total | Role | rustic/restic dependency |
|---|---|---|---|---|
| client | lib.rs 804, main.rs 132 | **936** | CLI `globalobject register/account/status/invite/backup/snapshots/restore/pledge/forget/prune`; `GridBackend` implements rustic's `ReadBackend`/`WriteBackend`; piece upload/download, read grants, receipts | **Yes**: `rustic_core`, `jiff`, `bytesize` in Cargo.toml |
| control-plane | api.rs 749, reconcile.rs 590, db.rs 427, lib.rs 259, main.rs 56 | **2,081** | MongoDB-backed placement, grants, quota, ledger, audits, repair, drain, membership | Only `OBJECT_TYPES = ["config","index","key","snapshot","pack"]` (api.rs:21-22) and a hex-ID check on object IDs (api.rs:343) |
| e2e | src/lib.rs 306, tests/grid.rs 1,237 | **1,543** | In-process grid test harness; 44 end-to-end tests | Indirectly, through `Client::backup/read_file/restore/snapshots/forget/prune` and `"pack"`/`"snapshot"` literals |
| sim | churn.rs 619, economics.rs 171, main.rs 51 | **841** | Durability and economics simulator | No |
| node | lib.rs 664, main.rs 60 | **724** | Storage node: iroh-blobs store, capacity limit, grant gate, repair, audits, probes | No. Its doc comment says "The node never holds a decryption key. Repair works on ciphertext alone." |
| segment | lib.rs 451 | **451** | Reed-Solomon and pyramid local parity; `MAX_SEGMENT = 64 MiB` | No (uses `reed-solomon-erasure` only) |
| policy | lib.rs 432 | **432** | Placement, quota and token-bucket rules | No |
| proto | lib.rs 358 | **358** | Wire types (grants, receipts, `ObjectRef{tpe,id}`, `BeginSegment`, `ReadPlan`) | Doc comment only: "A file in the account's restic-format repository" (proto:229) |
| net | lib.rs 196 | **196** | iroh endpoint, verified fetch, `fetch_range`, peer RPC | No |
| crypto | lib.rs 126 | **126** | PASETO tokens, Ed25519 identity, Argon2id phrase-to-seed, BLAKE3 | No. Its doc comment says "File encryption lives in `rustic_core`, and transport encryption in `iroh`." |
| **Total** | | **8,888** | | |

Other measured sizes: scripts total 167 lines (grid.sh 89, test.sh 41, check-licences.py 37), and docs total 1,288 lines.

- The `rustic_core`, `jiff` and `bytesize` dependencies appear only in `crates/client/Cargo.toml`. — repo:crates/*/Cargo.toml
- The client's module doc says "Encryption, chunking, dedupe, compression and snapshots all come from `rustic_core`, using restic's repository format… This crate supplies a backend that stores those repository files on the grid." — repo:crates/client/src/lib.rs:1-3
- `docs/dependencies.md` assigns "Chunking, dedupe, compression, encryption, keys, snapshots" and "Snapshot retention (`forget`) and space reclaim (`prune`)" to `rustic_core`, and lists it as **pre-1.0**. The custom part is "Mapping repository files to segments: A thin backend for `rustic_core`, using its public trait". — repo:docs/dependencies.md
- One repository file becomes one catalogue object, stored as one segment. "Packs are limited to 48 MiB, and segments to 64 MiB." — repo:docs/protocol.md:79; repo:crates/client/src/lib.rs:35 (`PACK_LIMIT`); repo:crates/segment/src/lib.rs:17 (`MAX_SEGMENT`)
- The control plane keys its catalogue as `ObjectDoc{account, tpe, object, len, segment}`, which db.rs describes as "The segment currently holding one repository file". — repo:crates/control-plane/src/db.rs:145-154
- MongoDB has 11 collections: pools, invites, accounts, nodes, segments, objects, read_grants, ledger, pending_deletes, pending_probes, counters. Only `objects` stores anything named after restic concepts. — repo:crates/control-plane/src/db.rs:222-232

Cargo.lock closure, measured with a Python `tomllib` script over `Cargo.lock`, since `cargo tree` isn't available:

| Measure | Packages |
|---|---|
| Total in Cargo.lock | 637 (status.md says 638 for the licence check, which may count the root differently) |
| Transitive closure of `rustic_core 0.13.0` | 231 |
| Reachable only through `rustic_core`, `jiff` or `bytesize`, so removed with them | **70** |
| Remaining | **567** |
| Closure per crate | client 579, control-plane 571, node 491, net 461, proto 102, crypto 95, segment 53, sim 46, policy 45, e2e 636 |

- The 70 packages that would go are: aes256ctr_poly1305aes, array-init, binrw, binrw_derive, bstr, bytemuck, bytesize, cached (+2 proc-macro crates), cachedir, cipher, crossbeam-deque, darling (two versions plus core/macro), defmt (+2), derive_setters, dirs, dirs-sys, dunce, ecow, enum-map (+derive), enumset (+derive), filetime, gethostname, globset, one hashbrown version, ignore, inout, integer-sqrt, itertools, jiff (+core, static, tzdb, tzdb-platform), libredox, nix, **option-ext (MPL-2.0)**, ordered-float, owo-colors, pariter, path-dedot, pkg-config, poly1305, portable-atomic-util, quick_cache, rayon, rayon-core, redox_users, runtime-format, rustic_cdc, rustic_core, salsa20, scrypt, serde-aux, serde-value, shell-words, xattr, **zstd, zstd-safe, zstd-sys (C library)**. — measured from repo:Cargo.lock
- `option-ext` is one of only two MPL-2.0 exceptions, and it "Comes from `rustic_core`, through `dirs`". Its "If it had to go" entry reads "Needs a change or patch upstream". — repo:docs/dependencies.md (Licences)

### Inferences
- `rustic_core` pulls in 231 packages, but 161 of them are shared with `iroh`, `tokio`, `reqwest` and `mongodb`. **The real reduction is 70 packages, about 11% of the lockfile.** That removes one of the two MPL exceptions and the only C compression library (zstd-sys).
- The format coupling is concentrated: about 310 lines in `client/src/lib.rs`, about 70 in `client/src/main.rs`, and one constant in the control plane (detail in section 2).

### Gaps
- No build was run, so it's unconfirmed that removing these dependencies leaves no feature-unification side effects. For example, `aes` stays because something else uses it.

## 2. What would be removed or simplified, with line counts

### Takeaway
About **380 lines of client code would be deleted** (measured by line range; the classification is a judgement). That covers the rustic backend, backup/snapshots/restore/forget/prune, the retention structs and the snapshot-rollback ledger. About 60 more lines become simpler sequential reads. The control plane loses almost nothing; it needs additions instead. The forget/prune feature (PR 3) and file-level partial reads (PR 2's client half) lose their purpose. PR 2's range-grant machinery in the control plane and node stays valid but would see little use.

### Cited Findings

`crates/client/src/lib.rs` (804 lines), classified by line range (ranges measured, classification judged):

| Lines | Content | Count | Verdict |
|---|---|---|---|
| 1-31 | Module doc, imports (rustic import block at 20-23) | 31 | Simplified: drop about 4 lines of rustic imports |
| 32-74 | Constants, `ClientError`, `Inner` (cache, semaphores, `last_error` workaround "since rustic's errors flatten it") | 43 | Simplified: `PACK_LIMIT`, `CACHE_OBJECTS` and the `last_error` workaround go (about 10 lines) |
| 75-100 | `Retention`, `PruneSummary`, `BackupStats` | 26 | **Removed**, replaced by stream/chain stats |
| 101-212 | register/open/build/call/status/invite/set_pledge | 112 | **Unchanged** |
| 213-281 | `upload_object`: RS-encode, `/v1/segments/begin`, iroh store, commit/abort/retry | 69 | **Unchanged** core; called per stream segment |
| 282-417 | authorize, receipts, `download_piece/range`, `read_segment`, `read_segment_range`, `request_read` | 136 | **Unchanged**; `read_segment_range` loses its main caller |
| 418-573 | backend(), repo_options, backup, snapshots, seen-snapshot ledger, `check_rollback`, `read_file`, `restore` | 156 | **Removed** (rollback check is rewritten for chains) |
| 574-636 | `remember_error`, `read_object` (3-object LRU), `read_object_range` | 63 | **Simplified** to sequential whole-segment reads for restore |
| 637-705 | `forget` (KeepOptions), `prune` (prune_plan, 23 h delayed delete) | 69 | **Removed** |
| 706-780 | `GridBackend`, `ReadBackend`/`WriteBackend` impls | 75 | **Removed** |
| 781-804 | http_client, `is_unavailable`, `decode_or_error` | 24 | Unchanged |

Totals, estimated from these ranges: **removed ≈ 326 + ≈14 import/constant lines ≈ 340**; simplified ≈ 63; unchanged ≈ 400.

`crates/client/src/main.rs` (132 lines, measured): the `Backup`, `Snapshots`, `Restore`, `Forget` (with keep-last/daily/weekly/monthly, `--prune`, `--instant-delete`), `Prune` subcommands and `print_prune` take about 70 lines. All of them would be replaced. `Register`, `Account`, `Status`, `Invite` and `Pledge` stay. — repo:crates/client/src/main.rs

Other crates:

| Item | Verdict | Evidence |
|---|---|---|
| control-plane `OBJECT_TYPES` and the hex-ID check | Simplified or replaced (2-3 lines) | repo:crates/control-plane/src/api.rs:21-22, 343 |
| control-plane `objects` catalogue (`list_objects`, `get_object`, `delete_object`, about 30 lines) | Kept as the segment index, but needs a chain layer above it (section 4) | repo:api.rs:671-697 |
| control-plane range reads in `read_segment` (`data_ranges`, the "read the whole object" 503 fallback) | Unchanged but rarely used, since restores of whole streams read whole segments | repo:api.rs:532-590 |
| `segment::data_ranges`, `net::fetch_range` | Unchanged. Audits use ranges anyway: "requests a random 16 KiB range of the piece" | repo:docs/protocol.md (Audits) |
| proto `ObjectRef{tpe,id}` | Simplified/renamed, plus new stream types | repo:crates/proto/src/lib.rs:229-235 |
| crypto | Unchanged, but see section 4: new data encryption is needed unless only `-w` is accepted | repo:crates/crypto/src/lib.rs:3 |

- PR 3's forget/prune is implemented entirely in `rustic_core` (`KeepOptions`, `delete_snapshots`, `prune_plan`, `prune`), and "Control plane: none" was needed for it. Removing it therefore touches only the client. — repo:docs/plans/reads-reclaim-repair.md:25-26, 82-104
- Client-side dedupe disappears along with `rustic_cdc`. Today's guarantee is "No cross-user dedupe… Each account's repository has its own keys." — repo:docs/architecture.md:93
- Snapshot-tree handling disappears: `node_from_snapshot_path`, `ls`, `prepare_restore`, `LocalDestination`, `read_file`. — repo:crates/client/src/lib.rs:538-572

### Inferences
- The tested partial-read result ("Reading a 20 KB file from a 24 MiB pack charged 37–39 KB", repo:docs/status.md) loses its user-facing meaning. A ZFS stream can't yield one file without replaying the full chain into a pool. — repo:research_notes/Ceph and OpenZFS Rust port/openzfs.md:52
- The client would be roughly 600 lines **before** new work (936 − ≈340). It would then grow by the new work in section 4.

### Gaps
- Whether segment-range reads would still be worth exposing, for example to verify a stream header without downloading it, is a design choice, not something the code settles.

## 3. What stays regardless (confirmed in the code)

### Takeaway
Everything below the "object → segment" boundary works on opaque ciphertext and stays as it is. That covers the control plane (MongoDB placement, grants, quota, ledger, audits, repair, drain, membership), the node, erasure coding and local parity, iroh/iroh-blobs BLAKE3 transport, read receipts and the bandwidth token bucket. About **6,000 lines of Rust are unaffected** (estimated, section 7).

### Cited Findings
- Control-plane routes: pools, invites, members, accounts, nodes, heartbeat, `segments/begin|commit|abort|read`, `receipts/read`, `objects/:tpe[/:id][/delete]`, `tasks/repair-done`, `tasks/probe-result`. Only the three `objects` routes carry the restic type names. — repo:crates/control-plane/src/api.rs:727-747
- Quota is enforced per segment at begin: `if u.used_raw + raw > u.quota_raw { … "quota exceeded" }`. It is independent of format. — repo:api.rs:382-383
- The bandwidth token bucket is `globalobject_policy::TokenBucket` on the account document. — repo:api.rs:599; repo:docs/status.md (MongoDB section)
- The node holds iroh-blobs and serves "only against grants", and repair "works on ciphertext alone". — repo:crates/node/src/lib.rs:1-3
- `upload_object` does `encode(&data, layout)`, BLAKE3 `hash_of` per piece, `BeginSegment`, `PeerRequest::Store`, then commit or abort. A new random segment ID per attempt prevents the earlier data-loss bug. — repo:crates/client/src/lib.rs:213-281; repo:docs/status.md (Bugs table)
- Audits, repair, drain, disqualification and the leaver lifecycle are in `reconcile.rs` (590 lines), with no format references found (grep for restic/rustic/pack/snapshot/dedup). — measured grep over `crates/`

### Inferences
- Segment-level invariants carry over directly to fixed-size stream segments: placement, failure domains, EC/replica policy, local parity repair, and audits.

### Gaps
- None for this question. The reconcile loop wasn't read line by line, but the grep found no format coupling.

## 4. New work needed for ZFS-only

### Takeaway
The ZFS-only client is **not** smaller once finished. It deletes about 340 lines and depends on about 70 fewer packages, but it needs an estimated **1,200–1,900 new lines** across client, control plane, proto and tests, plus a new data-encryption layer. The biggest new design problems all come from chain semantics:

- an incremental is useless without its base;
- deleting a base needs dependents gone first;
- reclaiming space means a full re-send;
- restore must be ordered and can't resume from our side mid-stream.

### Cited Findings (ZFS behaviour)
- `-w`/`--raw`: "For encrypted datasets, send data exactly as it exists on disk… backups to be taken even if encryption keys are not currently loaded." — [zfs-send.8](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zfs-send.8)
- `-i`: "The incremental source must be an earlier snapshot in the destination's history". The synopsis allows `-i snapshot|bookmark`, so an incremental can be sent "from a bookmark". — [zfs-send.8](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zfs-send.8)
- Incremental receive: "the destination file system must already exist, and its most recent snapshot must match the incremental stream's source". — [zfs-receive.8](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zfs-receive.8)
- Resume: `zfs receive -s` saves "the partially received state", and "The receive can be resumed with a stream generated by zfs send -t token, where the token is the value of the receive_resume_token property". `-A` aborts it. — [zfs-receive.8](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zfs-receive.8)
- Raw streams "may only be received as is", and "ZFS will not allow a mix of raw receives and non-raw receives". — [zfs-receive.8](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zfs-receive.8)
- Sending an encrypted dataset without `-w` means the data "will be sent unencrypted". "Deduplicated send is no longer supported." — [zfs-send.8](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zfs-send.8), via repo:research_notes/Ceph and OpenZFS Rust port/openzfs.md:44-47
- ZFS native encryption "will not encrypt metadata related to the pool structure", including dataset and snapshot names, properties, file size and holes, and dedup tables. — [zfs-load-key.8](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zfs-load-key.8), via repo:reports/Ceph and OpenZFS Rust port.md:13
- Encrypted send/recv had corruption bugs from 2021 that weren't fixed until 2025, and no formal audit of ZFS encryption was found. — repo:reports/Ceph and OpenZFS Rust port.md:13, citing [openzfs-docs#494](https://github.com/openzfs/openzfs-docs/issues/494)

### New work, itemised (sizes are estimates)

| # | Work item | Where | Est. LOC | Notes |
|---|---|---|---|---|
| 1 | **Stream ingestion**: read stdin, cut into fixed-size segments (for example 48 MiB, pad the last), upload each with the existing `upload_object`, with bounded memory and parallelism | client | 150–250 | Fixed size plus padding *fixes* today's pack-size leak at the segment level |
| 2 | **Data encryption** of the stream (needed for non-raw sends, and advisable for raw ones too) | client, possibly crypto | 100–200 plus 1–3 new crates | Today `crates/crypto` has none: "File encryption lives in `rustic_core`". The user's memory rule says to use existing OSS (for example RustCrypto `chacha20poly1305` STREAM or the `age` crate), not custom crypto. Restic's scrypt-wrapped master key goes too, so key derivation from the phrase must be redefined (Argon2id is already a dependency) |
| 3 | **Encrypted stream manifest** (dataset, snapshot name, GUIDs, from-snapshot, raw/compressed flags, segment list, total length, stream hash) | client + control plane | 100–150 | Keeps names and GUIDs off the control plane; the control plane sees only opaque IDs and edges |
| 4 | **Chain catalogue**: new `streams` collection {account, stream_id, parent_stream_id or null for full, segments[], len, state, created}, plus indexes | control-plane db/api | 200–300 | Replaces the restic `OBJECT_TYPES`. Must be transactional with segment commit (existing pattern in db.rs:395) |
| 5 | **Deletion semantics**: refuse to delete a stream while dependents exist, cascade delete of a whole chain, and the delete grace window (PR 5) | control plane | 100–200 | PR 5 is still "designed, not built", and it matters more now (section 5) |
| 6 | **Consolidation/rebase**: accept a new full stream for a dataset, then retire the old chain once the new one commits | client CLI + control plane | 80–150 | Needs about 2× quota for the overlap period and a full upload over home uplinks |
| 7 | **Restore ordering**: resolve the chain from the base, stream each in order to stdout for `zfs receive` (with `-F` or `-s` guidance), verify the stream hash | client | 100–150 | Sequential whole-segment reads; prefetch the next segment |
| 8 | **Upload resume**: checkpoint committed segments locally and continue an interrupted upload | client + control plane (stream `state=partial`) | 80–150 | ZFS `-t` tokens resume a *receive*, so they can't help a pipe into our client unless the user re-runs `zfs send` and we skip bytes. Whether `zfs send` output is byte-identical across runs is unverified (see Gaps). Partial streams must count against quota and expire |
| 9 | **Restore resume** | client | 30–80 | `zfs receive -s` saves partial state, but `zfs send -t` needs the original source pool, which a restore doesn't have. Realistically: abort (`-A`) and restart that stream, keeping the chain links that already completed |
| 10 | **Quota for chains**: per-chain usage in `status`, refusal messages that name which chain to consolidate, and a reservation for a rebase | control plane + CLI | 50–100 | Raw quota accounting per segment already works unchanged |
| 11 | **Rollback/hidden-stream detection**: a local record of chain heads per dataset, replacing `seen-snapshots` | client | 40–60 | ZFS's own GUID check on incremental receive catches a missing *middle* link at restore time, but not a hidden newest one |
| 12 | **CLI**: `globalobject send <dataset>` (stdin), `receive <stream or chain>` (stdout), `streams`/`chains`, `delete-chain`, `consolidate` | client main.rs | 80–120 | |
| 13 | **Holds/bookmarks guidance**: document `zfs hold` or `zfs bookmark` on the last uploaded snapshot, so the next `-i` source survives local snapshot pruning. Optionally the client records which bookmark the next incremental must use | docs + client | 20–50 | |
| 14 | **Tests**: chain integrity, base-deletion refusal, rebase, upload resume, restore order, chain quota, plus an optional real-ZFS integration test | e2e | 400–600 | Most tests can use synthetic byte streams because the client treats streams as opaque. A real-ZFS test needs the zfs kernel module in the sandbox, which wasn't checked |
| | **Total new (estimated)** | | **≈1,330–2,560 (central ≈1,700)** | |

### Inferences
- **Should raw `-w` be trusted alone?** No, in my reading. Raw streams leave dataset and snapshot names, properties, file sizes and holes in plaintext. Today's test invariant "No plaintext on any disk or the control plane" (v01) would then fail for metadata. Encrypting every stream client-side with an existing AEAD crate keeps that invariant, and it costs one extra encryption pass.
- ZFS-only shifts work to the user: they need a ZFS source dataset, a ZFS pool to restore into, and must manage holds and bookmarks. Restore to non-ZFS machines and single-file restore become impossible.

### Gaps
- Whether `zfs send` of the same snapshot pair produces byte-identical output across runs isn't established here, and it decides whether upload resume by byte offset is safe. A safer design re-uploads the whole stream, or checks a hash per segment.
- The sandbox container's ability to load the ZFS kernel module, needed for a real integration test, wasn't checked.
- The LOC estimates are judgements, scaled from the size of comparable existing code (for example, `upload_object` is 69 lines and the PR 3 forget/prune was about 70).

## 5. Which docs/status.md issues disappear, persist or get worse

### Takeaway
One issue disappears: the `rustic_core` pre-1.0 dependency and restic crypto with it. One improves: segment sizes become fixed. Several persist unchanged. Three get worse: forget/prune flexibility becomes chain-only deletion, partial reads become impossible, and the delete grace window becomes more urgent. A fourth, metadata leakage, gets worse unless every stream is wrapped in client-side encryption.

### Cited Findings and verdicts

| Issue (source: repo:docs/status.md, repo:docs/threat-model.md) | Verdict | Reason |
|---|---|---|
| "Two pre-1.0 foundations. `rustic_core` and `iroh-blobs`" | **Half disappears** | `rustic_core` and its 70 exclusive packages go; `iroh-blobs` stays |
| "Restic's crypto has an informal public review rather than a formal audit" | **Disappears, but is replaced** | Either ZFS native crypto (no audit found, a 2021–2025 send/recv bug record) or a new AEAD crate plus a key scheme we design. The "never custom crypto" rule still applies |
| "Pack sizes are visible to the control plane and hosts" | **Improves** at the segment level, with fixed segments and padding | Total stream size and incremental size per snapshot remain visible, which shows how much changed. Restic also leaks `bytes_added`, so this is about equal |
| Metadata leak: control plane sees "repository object types, IDs and sizes, snapshot counts" | **Persists, or worse with raw-only** | A chain graph (parent edges) and per-dataset streams are visible. With un-wrapped `-w`, hosts would also see names, properties, file sizes and holes |
| PR 2 partial reads ("whole-pack reads") | **Worse: the feature is gone** | Restore granularity becomes a whole chain. Range grants survive only for audits |
| PR 3 forget/prune (retention keep-daily/weekly/monthly; repack) | **Worse/simplified** | No mid-chain thinning. Reclaim means delete whole chains, or rebase with a full re-send that costs about 2× quota during the overlap |
| "No delete grace window yet, so anyone with the phrase can delete everything at once" (PR 5) | **Persists, more urgent** | Deleting one base kills every dependent, so one deletion destroys more |
| Hidden/rolled-back snapshots: "A new device has no record" | **Persists, changes shape** | ZFS GUID checks catch missing middle links at receive time, but not a hidden newest stream |
| Sybil replica dedupe at `k=1` | **Persists** | Below the format boundary |
| "Receipts are coarser… restores mostly use whole-piece grants" | **Persists** (restores are entirely whole-piece now) | |
| Unexplained `t_false_pledge_surfaces_and_loses_standing` audit failure | **Persists** | Audit path unchanged |
| Control-plane TLS, HA, `$jsonSchema` validators, untested Compose/CI, loopback-only | **Persists** | Unrelated to the format |
| MPL exception `option-ext` from `rustic_core` | **Disappears** | Only `attohttpc` (iroh portmapper) remains |
| Demand: "Contributing 1 TB gives 267 to 400 GB, with days to upload and restore" | **Worse** | ZFS-only shrinks the audience to ZFS users, and periodic full re-sends add days of upload |
| Within-account dedupe and compression | **Lost / changed** | `zfs send -c` keeps on-disk compression; there is no cross-snapshot dedupe beyond incrementals |

### Inferences
- The project's own earlier conclusion holds: "Incremental chains can't be pruned remotely… Restic's prune model is a better fit for remote opaque storage." — repo:research_notes/Ceph and OpenZFS Rust port/openzfs.md:53, 150

### Gaps
- No public audit status was checked for candidate AEAD or `age` crates. Check before choosing one.

## 6. Tests in crates/e2e/tests/grid.rs

### Takeaway
There are **44 end-to-end tests** (measured: 44 `#[tokio::test]` attributes in 1,237 lines), plus 27 unit tests in `src/` (measured: crypto 3, node 3, policy 12, segment 7, sim 2). None of the unit tests touch rustic. Of the 44:

- **5** don't depend on the format;
- **30** use it only through the `put`/`get` helpers in `crates/e2e/src/lib.rs`, so their bodies stay if the helpers are reimplemented over streams;
- **5** must be rewritten;
- **4** should be removed or replaced.

### Cited Findings
The helpers `put` (calls `c.backup`) and `get` (calls `c.read_file`) in repo:crates/e2e/src/lib.rs:274-282 are about 8 lines. Changing them carries most tests across.

| Class | Count | Tests |
|---|---|---|
| A. Format-independent (at most a `"pack"` literal change) | 5 | v03_policy_below_the_floor_is_refused, v04_parallel_uploads_cannot_race_past_quota (its `begin_body` uses `tpe:"pack"`), v04_hosts_refuse_bad_grants, v05_new_members_are_measured_soon_after_joining, v09_replicas_must_be_identical |
| B. Unchanged bodies once `put`/`get` are rewritten | 30 | v01_no_plaintext ×2 (valid only if streams are client-encrypted), v03 ×2, v04_quota, v04_blobs_not_served, v04_requests_signed, v04_hash_sequence (also a `"pack"` literal), v04_range_grants, v05 ×3, v06 replica/EC loss ×2, v07, v08 ×4, v09_coded_pieces_differ, v10 ×5, v11 ×2, t_false_pledge, t_one_client_cannot_disqualify, t_local_parity |
| C. Rewrite (snapshot API semantics) | 5 | v01_others_cannot_read_an_account (`snapshots()`), v02_files_round_trip ×2 (backup/read_file/restore become a stream round trip), v06_owner_returns_on_a_new_machine (`snapshots()`), t_hidden_snapshots_are_detected (deletes a `tpe:"snapshot"` object; becomes a hidden chain head) |
| D. Remove or replace | 4 | v02_repeated_content_dedupes_within_an_account (no dedupe), t_partial_read_fetches_only_the_range ×2 (no file-level reads), t_forget_and_prune_reclaim_space (becomes a chain-deletion and rebase test) |

The classification came from a script that scanned each test body for `put(`, `get(`, `.backup(`, `.read_file(`, `.restore(`, `.snapshots(`, `.forget(`, `.prune(` and the restic type literals, then from reading the matches. — measured over repo:crates/e2e/tests/grid.rs

Other test assets:
- `scripts/grid.sh` (89 lines) calls `backup`, `snapshots` and `restore` at lines 48-87. About 15 lines need rewriting. — repo:scripts/grid.sh
- Verification-plan item 2, "Files from 0 bytes to multi-GB round trip", becomes a stream round trip. Item 9 is unaffected. — repo:docs/status.md

### Inferences
- About 9 tests (C+D, ≈260 lines by function length) need real work. About 6 new chain tests are needed (section 4, item 14).
- Class B's two `no_plaintext` tests are a useful guard. They would *fail* under raw-only-without-wrapping if the needle is a dataset or file name, which documents the leak.

### Gaps
- Whether `v04_requests_must_be_signed_for_exactly_what_they_do` uses the `get` helper or an HTTP GET wasn't confirmed: the regex matched `get(`. It's counted in B either way, so the totals don't change.

## 7. Summary roll-up

### Takeaway
Measured: 8,888 lines of Rust and 637 lockfile packages. Estimated classification: about 340 lines removed, about 135 simplified, about 8,400 unchanged, and about 1,700 new (range 1,300–2,600). That is a net growth of roughly 1,000–2,200 lines, with 70 fewer packages.

### Cited Findings

| Crate | LOC (measured) | Removed | Simplified | Unchanged | New work (estimated) |
|---|---|---|---|---|---|
| client | 936 | ≈340 (lib) + ≈70 (main, replaced) | ≈63 | ≈460 | ≈650–1,100 (ingest, encryption, manifest, restore, resume, rollback, CLI) |
| control-plane | 2,081 | ≈3 | ≈30 (objects routes) | ≈2,050 | ≈350–600 (streams collection, chain deletion, grace window, rebase, quota reporting) |
| proto | 358 | 0 | ≈7 (`ObjectRef`) | ≈350 | ≈40–80 (stream types) |
| crypto | 126 | 0 | 0 | 126 | 0–100 (if AEAD/key derivation goes here) |
| segment | 451 | 0 | 0 | 451 | 0 (padding could live in client) |
| net | 196 | 0 | 0 | 196 | 0 |
| node | 724 | 0 | 0 | 724 | 0 |
| policy | 432 | 0 | 0 | 432 | 0–30 (chain quota rule, if not in control plane) |
| sim | 841 | 0 | 0 | 841 | 0 |
| e2e | 1,543 | ≈150 (4 tests) | ≈8 helpers + ≈110 (5 rewrites) | ≈1,270 | ≈400–600 |
| Dependencies | 637 pkgs | **70 pkgs** (measured) | | 567 | +1–3 crates (AEAD/`age`), each with small closures (not measured) |

### Inferences
- Moving to ZFS-only wouldn't simplify the system much. It removes a narrow adapter (the restic backend and CLI verbs) and keeps the large core. It then adds chain management, a new encryption layer and new deletion rules. It also gives up features that the project explicitly built and tested in November 2026: partial reads, retention-based forget/prune, and dedupe.

### Gaps
- Line counts for new work are estimates. No prototype was written.
