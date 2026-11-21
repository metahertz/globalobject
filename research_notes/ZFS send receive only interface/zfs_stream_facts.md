# OpenZFS send/receive streams as the sole data interface of an encrypted, distributed backup grid

Research date: 2026-11-20. Primary sources are the OpenZFS `master` man pages and source, fetched from GitHub on that date (the openzfs.github.io docs site could not be reached from the research environment; the man pages there are generated from the same repo files). Latest OpenZFS releases at that date: zfs-2.4.4, 2.3.9 and 2.2.11, all published 2026-08-21 ([GitHub releases API](https://api.github.com/repos/openzfs/zfs/releases)).

## Stream format, in-stream checksums, integrity checks on receive, and cross-version/platform stability

### Takeaway
A send stream is a sequence of fixed-size `dmu_replay_record_t` headers (BEGIN, OBJECT, FREEOBJECTS, WRITE, FREE, END, WRITE_BYREF, SPILL, WRITE_EMBEDDED, OBJECT_RANGE, REDACT), each followed by an optional payload. A running fletcher-4 checksum covers the whole stream. It is stored in every non-BEGIN header and again in the END record, and `zfs receive` checks it, so truncation and accidental corruption are detected. Fletcher-4 is not cryptographic, so it does not protect against deliberate tampering. OpenZFS says the stream format is "committed". Whether a receiver can accept a stream depends on per-stream feature flags and on which pool features the receiving pool has enabled.

### Cited Findings
- The record types are `DRR_BEGIN, DRR_OBJECT, DRR_FREEOBJECTS, DRR_WRITE, DRR_FREE, DRR_END, DRR_WRITE_BYREF, DRR_SPILL, DRR_WRITE_EMBEDDED, DRR_OBJECT_RANGE, DRR_REDACT`. Each header is a `dmu_replay_record_t` with `drr_type`, `drr_payloadlen` and a union of per-type structs — [zfs_ioctl.h](https://raw.githubusercontent.com/openzfs/zfs/master/include/sys/zfs_ioctl.h)
- `drr_begin` holds `drr_magic` (`DMU_BACKUP_MAGIC 0x2F5bacbacULL`), `drr_versioninfo` (header type plus a 56-bit feature-flag field), `drr_creation_time`, `drr_type`, `drr_flags`, `drr_toguid`, `drr_fromguid` and `drr_toname[MAXNAMELEN]`. The snapshot name is carried in plaintext in the stream header — [zfs_ioctl.h](https://raw.githubusercontent.com/openzfs/zfs/master/include/sys/zfs_ioctl.h)
- Header types are `DMU_SUBSTREAM` (0x1) and `DMU_COMPOUNDSTREAM` (0x2). Compound streams are the "stream packages" produced by `-R`, `-I` and so on — [zfs_ioctl.h](https://raw.githubusercontent.com/openzfs/zfs/master/include/sys/zfs_ioctl.h)
- Checksums: "drr_checksum is overlaid with all record types except DRR_BEGIN"; it is the "fletcher-4 checksum of everything preceding the checksum". `drr_end` carries `drr_checksum` and `drr_toguid` — [zfs_ioctl.h](https://raw.githubusercontent.com/openzfs/zfs/master/include/sys/zfs_ioctl.h)
- How receive verifies the stream: `receive_read_payload_and_next_header()` updates a running fletcher-4 over the payload and the next header. It returns `ECKSUM` if the header's checksum is non-zero and does not match. A header checksum of zero is not checked (`if (!ZIO_CHECKSUM_IS_ZERO(cksump)) && !ZIO_CHECKSUM_EQUAL(...)`). At `DRR_END`, a mismatch between the running checksum and `drr_end.drr_checksum` returns `ECKSUM`. A short read returns `ZFS_ERR_STREAM_TRUNCATED`, and the code comments that "ECKSUM or ZFS_ERR_STREAM_TRUNCATED indicates that the receive was interrupted and can potentially be resumed" — [dmu_recv.c](https://raw.githubusercontent.com/openzfs/zfs/master/module/zfs/dmu_recv.c)
- A second `DRR_BEGIN` inside a substream is rejected with EINVAL. Recent master also checks fields more strictly ("DRR_OBJECT has invalid checksum type", "Reject malformed DRR_SPILL before reading payload") — [dmu_recv.c](https://raw.githubusercontent.com/openzfs/zfs/master/module/zfs/dmu_recv.c)
- Endianness: receive detects a byte-swapped stream and swaps records (`byteswap_record`, `fletcher_4_incremental_byteswap`), so streams can move between big- and little-endian hosts — [dmu_recv.c](https://raw.githubusercontent.com/openzfs/zfs/master/module/zfs/dmu_recv.c)
- The `zfs-send(8)` man page says: "The format of the stream is committed. You will be able to receive your streams on future versions of ZFS." — [zfs-send.8](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zfs-send.8)
- Stream feature flags in `drr_versioninfo`: DEDUP, DEDUPPROPS, SA_SPILL, EMBED_DATA, LZ4, LARGE_BLOCKS, RESUMING, REDACTED, COMPRESSED, LARGE_DNODE, RAW, ZSTD, HOLDS, SWITCH_TO_LARGE_BLOCKS, LONGNAME and LARGE_MICROZAP. `DMU_STREAM_SUPPORTED(x)` rejects any flag outside the receiver's `DMU_BACKUP_FEATURE_MASK`, so an older receiver refuses a stream that uses newer features. Bits 0–1 (DEDUP/DEDUPPROPS) are not in the current mask — [zfs_ioctl.h](https://raw.githubusercontent.com/openzfs/zfs/master/include/sys/zfs_ioctl.h)
- The header warns: "Older software that does not support this flag [SWITCH_TO_LARGE_BLOCKS] may encounter a bug when switching to large blocks, which causes files to incorrectly be zeroed." Changing `-L` use between incrementals is a known hazard — [zfs_ioctl.h](https://raw.githubusercontent.com/openzfs/zfs/master/include/sys/zfs_ioctl.h)
- Pool-feature requirements on the receiver. For `-e`: "The receiving system must have the embedded_data feature enabled. If the lz4_compress or zstd_compress features are active on the sending system, then the receiving system must have the corresponding features enabled as well." — [zfs-send.8](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zfs-send.8)
- Recent receive-compatibility fixes: "Fix activating large_microzap on receive" (2.4.1, Feb 2026), "Fix receive of split large blocks with a short trailing chunk #18749" (2.4.4, Aug 2026) and "dmu_recv: Avoid potential null deref #18848" (2.4.4/2.3.9) — [OpenZFS releases](https://api.github.com/repos/openzfs/zfs/releases)
- Cross-platform encryption interop has an open bug: "Encrypted dataset created under current SmartOS/Illumos cannot be mounted on Linux, gives Input/output error" (#13301, opened 2022, still open) — [GitHub issue search](https://api.github.com/search/issues?q=repo:openzfs/zfs+label:%22Component:+Encryption%22+state:open+send)

### Inferences
- The in-stream fletcher-4 detects truncation and random bit-rot in a stored segment. It gives no authentication: anyone who changes the stream can recompute fletcher-4, or zero out per-record checksums, which receive then skips. The grid therefore needs its own cryptographic MAC/AEAD per segment and a signed manifest over the ordered segment list. The ZFS checksum should be treated as a last line of defence, not as the integrity mechanism.
- Corruption is only discovered while receiving, after earlier records have already been written. Without `-s` the partial receive is discarded. One bad segment in a full stream means the whole stream must be fetched again or rebuilt from erasure-coded parity.
- "Committed format" is a promise that newer versions can read older streams. It does not promise that older versions can read newer streams. Streams sent with `-c/-L/-e/-w` from a new pool that has zstd, large_dnode, longname, large_microzap and similar features active may not be receivable on an older or ported implementation (Windows/macOS ports lag upstream; see the adoption section). A restore host therefore needs an OpenZFS version and pool feature set at least as capable as the sender's.

### Gaps
- I did not find a primary OpenZFS source on Oracle Solaris ZFS incompatibility. My understanding (unsourced) is that Oracle's closed ZFS diverged after pool version 28 / filesystem version 5, and that OpenZFS uses feature flags (pool "version 5000"). Streams from Oracle Solaris 11 pools with newer versions, or with Oracle's own encryption, are generally not receivable by OpenZFS, and the reverse holds too. This needs confirming against Oracle documentation.
- I found no formal compatibility matrix for streams across Linux/FreeBSD/illumos/macOS/Windows. Interoperability is implied by the shared codebase and the byteswap handling, but is not documented as guaranteed.

## Full vs incremental streams, bookmarks, holds, dependency chain, and merging/rebasing

### Takeaway
An incremental stream applies only on top of a destination whose most recent snapshot matches the stream's source (checked by GUID). Restoring snapshot N needs the full base plus every incremental in order. OpenZFS has no tool that merges or rebases streams offline; consolidation means receiving into a pool and sending again.

### Cited Findings
- `-i snapshot|bookmark` creates an incremental from the source to the target. "The incremental source must be an earlier snapshot in the destination's history." — [zfs-send.8](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zfs-send.8)
- `-I` creates "a stream package that sends all intermediary snapshots"; `-I @a fs@d` "is similar to `-i @a fs@b; -i @b fs@c; -i @c fs@d`" — [zfs-send.8](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zfs-send.8)
- Receive rule: "If an incremental stream is received, then the destination file system must already exist, and its most recent snapshot must match the incremental stream's source." — [zfs-receive.8](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zfs-receive.8)
- Matching is by GUID: `drr_fromguid` / `drr_toguid` in BEGIN, `drr_toguid` in END, and receive compares `drrb->drr_toguid` with existing snapshot GUIDs — [zfs_ioctl.h](https://raw.githubusercontent.com/openzfs/zfs/master/include/sys/zfs_ioctl.h), [dmu_recv.c](https://raw.githubusercontent.com/openzfs/zfs/master/module/zfs/dmu_recv.c)
- A bookmark can be the incremental source (`-i snapshot|bookmark`), so the sender can delete the old snapshot and keep only a bookmark — [zfs-send.8](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zfs-send.8)
- `-h/--holds` includes snapshot holds in the stream package and tells receive to apply them (stream feature `DMU_BACKUP_FEATURE_HOLDS`) — [zfs-send.8](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zfs-send.8), [zfs_ioctl.h](https://raw.githubusercontent.com/openzfs/zfs/master/include/sys/zfs_ioctl.h)
- `zfs send -S/--saved` sends a partially received ("saved") dataset and uses its incremental source if one exists — [zfs-send.8](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zfs-send.8)
- The `zstream` subcommands are `decompress`, `drop_records`, `dump`, `raw`, `recompress`, `redup` and `token`. None of them merges or rebases incrementals — [zstream.8](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zstream.8)

### Inferences
- Retention and pruning in the grid has to work on whole chains. Dropping an old incremental breaks every later one, unless a host with a pool receives the chain and sends a new full stream (or a new base) to "rebase" it. This is unlike restic, where prune rewrites packs without a filesystem.
- Restore time and bandwidth grow with chain length. A practical design takes periodic new full sends (for example monthly full plus daily `-i`), which costs extra storage.
- A "synthetic full" made by receiving into a scratch pool on a trusted machine is possible but needs that machine to fetch the whole chain. With `-w` the machine needs no key (see the next section), so in principle an untrusted host could do it, at the cost of seeing the plaintext metadata described below.

### Gaps
- I found no third-party tool that merges ZFS incremental streams offline. The absence is inferred from searching crates.io and the zstream feature list, not from an authoritative statement that none exists.

## Raw encrypted send (`-w`): what stays visible, key requirements, and known bugs

### Takeaway
`zfs send -w` sends blocks exactly as they are on disk, so data stays encrypted and authenticated and the receiver never needs the key. The structure stays visible: dataset and snapshot names, hierarchy, properties, file sizes, holes, dedup tables, and stream-level object/offset/size metadata. The long-running native-encryption corruption issue (#12014) was closed in May 2025 by a fix to non-raw send of encrypted datasets. Some encryption panics remain open.

### Cited Findings
- `-w/--raw`: "For encrypted datasets, send data exactly as it exists on disk. This allows backups to be taken even if encryption keys are not currently loaded. The backup may then be received on an untrusted machine since that machine will not have the encryption keys to read the protected data or alter it without being detected. Upon being received, the dataset will have the same encryption keys as it did on the send side, although the keylocation property will be defaulted to prompt if not otherwise provided." — [zfs-send.8](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zfs-send.8)
- Without `-w`, an encrypted dataset is sent decrypted and may be re-encrypted with a different key on the receiver, "which will disable the ability to do a raw send to that system for incrementals" — [zfs-send.8](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zfs-send.8)
- What native encryption covers and leaves exposed: "ZFS will encrypt file and volume data, file attributes, ACLs, permission bits, directory listings, FUID mappings, and userused/groupused data. ZFS will not encrypt metadata related to the pool structure, including dataset and snapshot names, dataset hierarchy, properties, file size, file holes, and deduplication tables (though the deduplicated data itself is encrypted)." — [zfs-load-key.8 (Encryption section)](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zfs-load-key.8)
- Key management: changing the user's key does not re-encrypt data, and datasets "can be scrubbed, resilvered, renamed, and deleted without the encryption keys being loaded" — [zfs-load-key.8](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zfs-load-key.8)
- Raw stream restrictions on receive: raw streams "may only be received as is, and cannot be re-encrypted, decrypted, or recompressed". "ZFS will not allow a mix of raw receives and non-raw receives. Specifically, any raw incremental receives that are attempted after a non-raw receive will fail." Raw send transfers the IV set; to recover, "roll[] the destination back to the most recent snapshot that was received raw … or by receiving a new full raw stream." Encryption properties cannot be overridden on raw receive — [zfs-receive.8](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zfs-receive.8)
- Plaintext stream framing: `drr_object` exposes `drr_object` (object number), `drr_type`, `drr_bonustype`, `drr_blksz`, `drr_bonuslen`, `drr_checksumtype`, `drr_compress`, `drr_dn_slots` and `drr_raw_bonuslen`. WRITE/FREE records carry object/offset/length. These header fields are not encrypted by `-w`; only payloads are — [zfs_ioctl.h](https://raw.githubusercontent.com/openzfs/zfs/master/include/sys/zfs_ioctl.h)
- `-w` cannot be combined with redacted send ("raw sends and redacted sends cannot be combined at this time"), and `zstream raw` (zvol stream to image) does not support encrypted streams — [zfs-send.8](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zfs-send.8), [zstream raw commit / PR #18565](https://api.github.com/repos/openzfs/zfs/commits?path=man/man8/zstream.8)
- An open feature request: "`zstream` should be able to wrap/unwrap encrypted streams" (#16183) — [#12014 timeline](https://api.github.com/repos/openzfs/zfs/issues/12014/timeline)
- Issue #12014, "ZFS corruption related to snapshots post-2.0.x upgrade", was opened 2021-05-08 and closed 2025-05-19 by PR #17340, "Fix 2 bugs in non-raw send with encryption". Bisecting showed that redacted-send code made `dsl_dataset_hold_obj_flags()` take a DECRYPT key mapping that was never released, so later `arc_untransform` failed with ECKSUM, and leaked mappings caused panics on export/unload "after a non-raw send from an encrypted filesystem". The fix shipped in 2.3.3 (2025-06-19) and 2.2.8 (2025-06-12) — [issue #12014](https://api.github.com/repos/openzfs/zfs/issues/12014), [PR #17340](https://api.github.com/repos/openzfs/zfs/pulls/17340), [releases](https://api.github.com/repos/openzfs/zfs/releases)
- Before the fix, the man pages carried an "Encryption: document snapshot corruption" warning (PR #16745, Nov 2024), citing #12014 and FreeBSD bug 282622 — [PR #16745](https://api.github.com/repos/openzfs/zfs/pulls/16745)
- User reports in #12014 (early 2025) said that switching syncoid to raw sends (`--sendoptions=R`/`Rw`) stopped the errors, and that replicating without encryption removed the problem (user wohali). This is consistent with the bug being in the non-raw-from-encrypted path — [#12014 comments](https://api.github.com/repos/openzfs/zfs/issues/12014/timeline?per_page=100&page=4)
- Still open as of Nov 2026: #16065 "PANIC: unencrypted block in encrypted object set" (opened 2024-04), #14330 (same symptom, 2022), #12732 "PANIC at dmu_recv.c on receiving snapshot to encrypted file system" (2021), #6793 "zfs receive -F cannot be used to destroy an encrypted filesystem" and #15102 (change-key can lose access to local and backup data) — [GitHub issue search](https://api.github.com/search/issues?q=repo:openzfs/zfs+label:%22Component:+Encryption%22+state:open+send), [issue #16065](https://api.github.com/repos/openzfs/zfs/issues/16065)
- 2.2.3 (Feb 2024) changed one case to "Don't panic on unencrypted block in encrypted dataset #15677". 2.4.0 (Dec 2025) added a `send:encrypted` delegation permission (#17543) — [releases](https://api.github.com/repos/openzfs/zfs/releases)

### Inferences
- If the grid encrypts each segment with its own key before distribution, as proposed, the metadata leaks above are not exposed to storage hosts. They would only matter if raw streams were stored without that outer layer. Outer encryption is therefore mandatory, and ZFS native encryption becomes an optional inner layer. It still helps: a restore or rebase host can receive a `-w` stream without the user's key.
- If `-w` is used, the user must keep the ZFS wrapping key (passphrase or keyfile) outside the grid as well as the grid key, or no restore is possible. #15102 and #15952 ("Tool for Emergency Master-Key Recovery") show that key mishaps are a real failure mode.
- Raw incrementals cannot follow a non-raw receive. The design must pick one mode (raw or non-raw) for each chain.
- Encrypted datasets have a history of corruption bugs and still have open panics. The design team should treat native encryption plus send as carrying more risk than unencrypted send plus grid-level encryption.

### Gaps
- I found no formal third-party cryptographic analysis of OpenZFS native encryption metadata leakage newer than the man page. I could not reach the OpenZFS docs site or Klara articles from the research environment.
- I did not verify whether the objset-level MAC in a raw stream authenticates the dnode metadata end to end, nor whether receive checks MACs at receive time or only when the key is later loaded and data is read.

## Compressed (-c), large-block (-L), embedded (-e), deduplicated (-D) and resumable sends

### Takeaway
`-c`, `-L` and `-e` reproduce on-disk compression, records larger than 128 KiB and embedded blocks in the stream, at the cost of receiver feature requirements. Deduplicated send (`-D`) is no longer supported; the flag is accepted but ignored. Resumable receive (`-s` plus `receive_resume_token` plus `zfs send -t`) keeps the partial state on the receiving pool and resumes from the last record written.

### Cited Findings
- `-D/--dedup`: "Deduplicated send is no longer supported. This flag is accepted for backwards compatibility, but a regular, non-deduplicated stream will be generated." — [zfs-send.8](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zfs-send.8)
- `zstream redup` converts old deduplicated streams. It "requires a file; it rereads earlier parts of the stream to resolve deduplicated records, so it cannot accept input from a pipe." — [zstream.8](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zstream.8)
- `-L/--large-block` "Generate[s] a stream which may contain blocks larger than 128 KiB"; it has no effect unless the large_blocks feature is active on the dataset — [zfs-send.8](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zfs-send.8)
- `-c/--compressed` creates compressed WRITE records for blocks compressed on disk. Received blocks stay compressed as they were on the sender. "Datasets that are sent with this flag may not be received as an encrypted dataset" (encrypted datasets cannot use embedded_data) applies to `-e` — [zfs-send.8](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zfs-send.8)
- `-e/--embed` uses WRITE_EMBEDDED records, and the receiver needs the `embedded_data`, lz4 and zstd features if they are active on the sender — [zfs-send.8](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zfs-send.8)
- `zfs receive -s`: "If the receive is interrupted, save the partially received state, rather than deleting it. Interruption may be due to premature termination of the stream …, a checksum error in the stream, termination of the zfs receive process, or unclean shutdown of the system. The receive can be resumed with a stream generated by `zfs send -t token`, where the token is the value of the receive_resume_token property." It requires the `extensible_dataset` pool feature — [zfs-receive.8](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zfs-receive.8)
- `zfs receive -A` aborts an interrupted receive and deletes the saved state. `zstream token` decodes a resume token — [zfs-receive.8](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zfs-receive.8), [zstream.8](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zstream.8)
- "Update resume token at object receive #15927" landed in 2.2.4 (May 2024) — [releases](https://api.github.com/repos/openzfs/zfs/releases)
- Corrective receive: `zfs receive -c` (2.2.0+) heals data blocks in an existing dataset from a stream; "Metadata can not be healed by corrective receive" — [zfs-receive.8](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zfs-receive.8), [2.2.0 release notes](https://api.github.com/repos/openzfs/zfs/releases)
- A contributor in #12014 saw I/O errors and pool suspension while repeatedly interrupting and restarting `receive -s` on a full pool, and suggested the `%recv` partial state might be involved. This was unconfirmed — [#12014 comments](https://api.github.com/repos/openzfs/zfs/issues/12014/timeline?per_page=100&page=4)

### Inferences
- Resume tokens are generated by the receiver and require the sender (with the source pool) to produce a new stream from the resume point (`zfs send -t`). A stored, pre-generated stream cannot be "resumed" by `zfs send -t` at restore time. For restore from the grid, resumption is simpler: the grid re-feeds the byte stream from the right offset, or from the start, into `zfs receive -s`. Since resume tokens encode object and offset, the restorer could in principle skip forward in the stored stream, but this is not a supported OpenZFS workflow.
- Using `-c` with `-w` keeps streams small; raw streams are already as-compressed-on-disk. Inner compression makes the stream incompressible, so the grid should not compress it again.
- Do not mix `-L` and non-`-L` within a chain, per the SWITCH_TO_LARGE_BLOCKS warning.

### Gaps
- I did not pin down the exact OpenZFS release in which `-D` generation was removed. It is outside my sourced material; the current man page only says it is "no longer supported".

## Partial or single-file restore without receiving into a pool; stream parsing tools

### Takeaway
OpenZFS has no supported way to pull a single file out of a filesystem send stream without receiving it into a pool. `zstream dump` shows records and checksums. A new `zstream raw` command (merged to master July 2026) can apply zvol streams, but not encrypted ones, to a raw image or block device without a pool.

### Cited Findings
- `zstream dump [-Cvd]` "Prints information about the specified send stream, including headers and record counts". `-C` suppresses checksum validation, `-v` prints per-record metadata and `-d` dumps data. `zstreamdump` is an alias — [zstream.8](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zstream.8)
- `zstream raw [-v] [-b max_buffers] [-g fromguid] image|device [file]` "Applies a zvol send stream … to a raw image or block device", with incremental support, `-g` fromguid checking, and printing of the final toguid. The commit says: "Encrypted (raw) streams are not supported, but compression, large blocks, and embedded write records are handled" (PR #18565, Klara/Railway, committed 2026-07-02) — [zstream.8](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zstream.8), [commit history](https://api.github.com/repos/openzfs/zfs/commits?path=man/man8/zstream.8)
- `zstream decompress` and `drop_records` repair or drop individual records identified by `object,offset`, and `recompress` changes stream compression. The CLI was reorganised in PR #19109 (Aug 2026) — [zstream.8](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zstream.8), [commit history](https://api.github.com/repos/openzfs/zfs/commits?path=man/man8/zstream.8)
- The stream's plaintext framing carries object numbers and offsets but not path names. Directory contents are ZAP objects inside the payloads, and for `-w` streams those payloads are encrypted ("directory listings" are encrypted) — [zfs_ioctl.h](https://raw.githubusercontent.com/openzfs/zfs/master/include/sys/zfs_ioctl.h), [zfs-load-key.8](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zfs-load-key.8)

### Inferences
- Single-file restore from a filesystem stream means receiving the whole chain into a pool (a temporary file-backed pool works), then mounting and copying. Restore needs disk space for the full dataset and a ZFS-capable restore machine. This is a significant UX regression compared with restic's `restore --include` and `mount`.
- A custom parser could rebuild files from a non-raw stream by interpreting ZPL dnodes, SA bonus buffers and directory ZAPs. That means re-implementing a large part of ZPL on-disk semantics, and it is impossible for `-w` streams without implementing ZFS crypto key unwrapping too.
- `zstream raw` makes zvol-based backups restorable to plain image files without a pool, but only on very new OpenZFS (master in 2026; it is not listed in the 2.4.4 notes reviewed) and not for raw-encrypted streams.

### Gaps
- I did not confirm whether `zstream raw` shipped in a tagged release by Nov 2026. The 2.4.x release notes I checked did not mention it.
- I found no maintained third-party filesystem-stream extractor.

## Rust crates and libraries, and licensing (CDDL, linking vs CLI)

### Takeaway
No Rust crate found on crates.io parses or produces ZFS send streams natively. The active crates shell out to the `zfs` CLI. The older FFI bindings to libzfs/libzfs_core are stale. OpenZFS is CDDL-licensed; running the `zfs` binary as a separate process avoids linking to CDDL code.

### Cited Findings
- `zfskit` 0.3.0 (2026-10-06) is an "Async toolkit for OpenZFS: datasets, snapshots, send/recv streams, holds, bookmarks, pools — via the zfs/zpool CLI with JSON output" (repo github.com/okhsunrog/zfskit) — [crates.io API](https://crates.io/api/v1/crates/zfskit)
- Other 2026 crates also orchestrate the CLI: `zrb` 0.3.0 ("Incremental ZFS snapshot replication over SSH with resumable transfers"), `chithi` 0.2.0 ("OpenZFS replication tools") and `zfsctl` — [crates.io search](https://crates.io/api/v1/crates?q=zfs+send)
- Stale FFI bindings: `libzfs` 0.6.16 (2020), `libzfs-sys` 0.5.11 (2019), `libzfs_core-sys` 0.1.0 (2017), `razor-libzfs`/`-sys` 0.12.0 (2022), `libzetta` 0.5.0 (2023) and `libzetta-zfs-core-sys` 0.5.2 (2023) — [crates.io search](https://crates.io/api/v1/crates?q=libzfs)
- Pure-Rust on-disk readers exist but do not handle streams: `zfs-forensic-core` ("From-scratch pure-Rust ZFS on-disk reader"), `lamzfs` (read-only no_std pool reader) and `filer-zfs`. The only send-stream parser found is `btrfs-stream`, which is for btrfs — [crates.io search](https://crates.io/api/v1/crates?q=openzfs), [crates.io search](https://crates.io/api/v1/crates?q=send+stream+parser)
- OpenZFS files, including man pages and headers, carry the Common Development and Distribution License (CDDL) 1.0 header. A 2026 commit "convert[ed] all 'fenced' CDDL headers to standard form" (#18904) — [zfs-send.8](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zfs-send.8), [commit history](https://api.github.com/repos/openzfs/zfs/commits?path=man/man8/zstream.8)

### Inferences
- The stream format is a simple record framing (fixed header plus payload, fletcher-4), so a clean-room Rust reader for splitting, validating and indexing streams (checking the running checksum, extracting BEGIN/END GUIDs and feature flags) is small to write from the public format description. Such a reader works on the stream as data and does not link CDDL code. Whether reusing struct layouts from CDDL headers creates licence obligations needs a licensing review.
- Running `zfs send`/`zfs receive` as a child process and piping stdout/stdin is the usual pattern (zfskit, zrb, syncoid). It keeps the grid's code separate from CDDL code in the same way as running any system tool. This matches the project's existing rule that separately-run services or tools are acceptable and linked code is what the licence rule covers. That is my reading of the project memory, not legal advice.

### Gaps
- I found no authoritative legal analysis of CDDL implications for calling the CLI versus linking libzfs_core from a non-CDDL project.

## ZFS availability on end-user machines (how many users this design can serve)

### Takeaway
On desktops and laptops, ZFS is first-class only on FreeBSD and on Ubuntu, where the installer option is experimental and LUKS is recommended. It is standard on NAS/hypervisor platforms (TrueNAS, Proxmox). On macOS it is a third-party kernel extension, and on Windows it is still beta (release candidates only). Requiring ZFS restricts the grid to a small, technical minority of home machines.

### Cited Findings
- Ubuntu 26.04 Desktop documentation lists "Erase disk and use ZFS with encryption" but recommends LUKS for encryption. Third-party guides describe the ZFS option as "Experimental", hidden under advanced options, with no mirror or RAIDZ support. The Ubuntu Server (Subiquity) installer has no ZFS-root GUI option — [Ubuntu Desktop docs: Advanced disk setup](https://ubuntu.com/desktop/docs/en/26.04/reference/advanced-disk-setup-features/), [LinuxConfig 26.04 ZFS root](https://linuxconfig.org/how-to-install-ubuntu-26-04-desktop-on-zfs-root)
- Ubuntu 24.04 brought back easy ZFS root install with encryption — [Phoronix](https://phoronix.com/news/OpenZFS-Ubuntu-24.04-LTS)
- Reports say Ubuntu 26.10 may remove ZFS/RAID/encryption support from GRUB. This is speculative, not confirmed — [It's FOSS](https://itsfoss.com/news/ubuntu-26-10-grub-overhaul.md)
- OpenZFS on Windows is still in release candidates only: zfswin-2.4.4rc9 (2026-10-17), 2.4.1rc17 (2026-08-29) and 2.3.1rc14 (2025-05). The project wiki (May 2026) calls it a "beta release candidate" — [openzfsonwindows releases](https://api.github.com/repos/openzfsonwindows/openzfs/releases), [OpenZFS on Windows wiki](https://github.com/openzfsonwindows/openzfs/wiki)
- The last GitHub release under openzfsonosx/openzfs is zfs-macOS-2.1.99 (2022-08-23). Newer macOS builds (for example zfs-macOS-2.2.3) are announced through the Windows/macOS maintainer's channels — [openzfsonosx releases](https://api.github.com/repos/openzfsonosx/openzfs/releases), [newreleases.io](https://newreleases.io/project/github/openzfsonwindows/openzfs/release/zfswin-2.2.3rc5)

### Inferences
- Realistic ZFS hosts are Linux enthusiasts (Ubuntu ZFS root, Arch/NixOS with out-of-tree modules), FreeBSD users, and home-lab NAS/hypervisor owners (TrueNAS, Proxmox, Unraid with ZFS). Mainstream Windows and macOS users, the majority of home machines, are effectively excluded. Windows/macOS ports lag upstream by one or more minor versions, which matters for receiving streams that use new features.
- Storage hosts in the grid do not need ZFS at all, because they hold opaque encrypted segments. Only data sources and restore targets need ZFS. Restore on a non-ZFS machine (for example after a laptop dies and is replaced by a Mac) would need a Linux VM or live USB with OpenZFS.

### Gaps
- I did not fetch the FreeBSD Handbook, TrueNAS or Proxmox documentation directly. Their ZFS-default status is stated from general knowledge and should be verified.
- I found no quantitative data on the share of desktop installs using ZFS, such as Ubuntu installer telemetry for ZFS selection.
