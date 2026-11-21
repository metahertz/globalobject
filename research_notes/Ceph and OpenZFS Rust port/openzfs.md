# OpenZFS components as a Rust port for GlobalObject: what could fix issues 1-11, and where ZFS's design rules it out

Research date: 2026-11-07. Fetch notes: openzfs.github.io and despairlabs.com refused the connection from this environment. Man pages were read from raw.githubusercontent.com (`openzfs/zfs` master), which has the same source text. Other findings come from search-result excerpts, marked "(search excerpt)" where the page itself was not opened.

Project context, from `docs/status.md` and `docs/threat-model.md`: hosts are untrusted home machines. Restic packs are encrypted client-side with AES-256-CTR plus Poly1305. Pieces move over iroh-blobs with BLAKE3 verified streaming. The control plane places, grants and audits pieces. Pack sizes, piece hashes, access times and account IDs are visible to hosts.

## Q1. ZFS native encryption: what it leaks, its bugs, and whether it has been audited (issues 3 and 4)

### Takeaway
ZFS native encryption leaks more metadata than restic, not less. Dataset and snapshot names, properties, the dataset hierarchy, file sizes, holes and dedup tables all stay in plaintext. Its send/recv path had data-corruption bugs that took about four years to fix (2021 to 2025), and its maintainers said they did not fully understand them. I found no formal audit. It would not fix issue 3 or issue 4, and on both it would make things worse.

### Cited Findings
- From zfs-load-key(8): ZFS encrypts file and volume data, file attributes, ACLs, permission bits, directory listings, FUID mappings and userused/groupused data. It "will not encrypt metadata related to the pool structure", which includes dataset and snapshot names, the dataset hierarchy, properties, file size and holes. Dedup tables are also left unencrypted. — [zfs-load-key.8 (master)](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zfs-load-key.8)
- On dedup, the same page says: "datasets will only deduplicate against themselves, their snapshots, and their clones", and "Deduplication with encryption will leak information about which blocks are equivalent in a dataset". — [zfs-load-key.8](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zfs-load-key.8)
- Compression runs before encryption, so datasets "may be vulnerable to a CRIME-like attack if applications accessing the data allow for it". — [zfs-load-key.8](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zfs-load-key.8)
- For encrypted blocks, the block-pointer checksum is "128 bits of the user-chosen checksum and 128 bits of MAC from the encryption suite". Datasets "can be scrubbed, resilvered, renamed, and deleted without the encryption keys being loaded." Changing the user key doesn't re-encrypt data, and the old wrapped master key isn't overwritten on disk. — [zfs-load-key.8](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zfs-load-key.8)
- The ciphers are aes-128/192/256 in CCM or GCM mode, and the default is aes-256-gcm. Encryption must be set when a dataset is created and can't be changed later. Encrypted datasets can't use `copies=3`. — [zfsprops.7 (master)](https://raw.githubusercontent.com/openzfs/zfs/master/man/man7/zfsprops.7)
- In 2018, during FreeBSD's call for testing of native encryption, the implementer said the dedup table "needs to be in plaintext so that we can repair it even when keys are not loaded", and that sparse regions remain holes, "technically ... a small leak". Alan Somers argued that "watermarking attacks will still be possible based on the size and sparsity of a file". The Datto developer replied that the team judged "the security risks here are easily outweighed by the usability and performance benefits". — [freebsd-fs, Aug 2018](https://lists.freebsd.org/pipermail/freebsd-fs/2018-August/026588.html) (search excerpt); [thread](https://lists.freebsd.org/pipermail/freebsd-fs/2018-August/026583.html)
- A forensics vendor, writing in 2021, says pool and file-system size, dataset names and dedup tables can be read without the key. — [Elcomsoft blog, Aug 2021](https://blog.elcomsoft.com/2021/08/nas-forensics-truenas-encryption-overview/) (search excerpt)
- **Corruption bugs.** openzfs-docs issue #494 was opened on 12 Feb 2024 and closed on 7 Oct 2025. It asked for warnings against using native encryption with send/recv, citing zfs#12014 (snapshot corruption after upgrading to 2.0.x), #11688, #11679 and #15474. A reproducer was bisected to commit 30af21b in April 2025. At least two bugs in non-raw encrypted send were fixed in PR #17340 and backported for 2.2.8 and 2.3.3. — [openzfs-docs#494](https://github.com/openzfs/openzfs-docs/issues/494)
- In the same thread, maintainer amotin (Alexander Motin) said of the fix: "I don't think there is still a proper understanding of what's going on." Contributor rincebrain said he had "given up trying to fix native encryption issues". — [openzfs-docs#494](https://github.com/openzfs/openzfs-docs/issues/494)
- **Raw-send bugs.** In issue #12614, a raw receive can reset a child's pbkdf2salt while the child stays in the same encryptionroot, without rewrapping the key. It was still open according to the search result. Issue #8852 reported corrupted data from raw send/recv, caused by a missing check before truncating an object's block count. — [zfs#12614](https://github.com/openzfs/zfs/issues/12614), [zfs#8852](https://github.com/openzfs/zfs/issues/8852) (search excerpts)
- A blog post, "Mind the encryptionroot", describes raw-replicated backups that would no longer mount, and how the author recovered them. It was linked in #494 in October 2025. — [sambowman.tech](https://sambowman.tech/blog/posts/mind-the-encryptionroot-how-to-save-your-data-when-zfs-loses-its-mind) (not fetched; date unconfirmed)
- Phoronix reported in February 2024: "Making use of zfs send on an encrypted dataset can cause one or more snapshots to report errors." — [Phoronix](https://Phoronix.com/news/OpenZFS-Encrypt-Corrupt), [Slashdot](https://hardware.slashdot.org/story/24/02/17/0613215/openzfs-native-encryption-use-has-newish-data-corruption-bug?sdsrc=prev)
- **No audit found.** A 2023 freebsd-security post says neither GELI nor the rest of FreeBSD's crypto subsystem "has ever undergone anything close to a real security audit". That statement covers FreeBSD broadly, not ZFS specifically. #494 contains no statement about an audit. — [freebsd-security, Aug 2023](https://lists.freebsd.org/archives/freebsd-security/2023-August/000206.html) (search excerpt); [openzfs-docs#494](https://github.com/openzfs/openzfs-docs/issues/494)
- OpenZFS 2.4.0, released 18 Dec 2025, added AVX2 acceleration for AES-GCM. Point releases 2.4.1 to 2.4.3 followed between Feb and Jun 2026. — [zfs-2.4.0 release](https://github.com/openzfs/zfs/releases/tag/zfs-2.4.0); point-release dates from [releasealert.dev](https://releasealert.dev/github/openzfs/zfs) (search excerpt)

### Inferences
- **Issue 3 (restic crypto unaudited).** Swapping in ZFS crypto would replace an informally reviewed design with one that has no audit I could find and a recent history of bugs in exactly the replication path a backup system would use. It would not help.
- **Issue 4 (pack sizes visible).** A raw ZFS stream exposes, by design, per-file size and holes, block sizes, the dataset and snapshot namespace and properties. Restic packs bundle many blobs and hide file boundaries better. This makes issue 4 worse.
- A GlobalObject port would have to reimplement the ZFS encryption key hierarchy (wrapping key, master key, per-objset keys, IV/salt handling). That is the subsystem with the worst bug record, and the user's rule is "use existing OSS for crypto, not custom implementations". A Rust rewrite of it would be custom crypto.

### Gaps
- I couldn't confirm that 2.2.8 and 2.3.3 shipped with the #17340 fixes, because I didn't open the release notes. #494 was closed as completed, which suggests they did.
- I found no academic cryptanalysis of ZFS's on-disk encryption format.

## Q2. Could `zfs send`/`recv` streams replace restic packs as the backup transport? (issues 1, 4, 7, 8)

### Takeaway
A raw send stream is a committed, versioned format that a host can store without keys. That is close to what GlobalObject needs. But it's a serialisation of the ZFS DMU, and only ZFS can produce or consume it. The client would need a ZFS dataset as its source, and restoring needs ZFS (or a userspace DMU) to apply it. Incrementals form a chain, so a stream can't be read in part or pruned on its own. It would not fix issues 1, 4, 7 or 8 better than restic does.

### Cited Findings
- `-w/--raw`: "For encrypted datasets, send data exactly as it exists on disk. This allows backups to be taken even if encryption keys are not currently loaded. The backup may then be received on an untrusted machine." After receive, "the dataset will have the same encryption keys as it did on the send side". — [zfs-send.8 (master)](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zfs-send.8)
- If an encrypted dataset is sent without `-w`, the data "will be sent unencrypted", which disables raw incrementals to that system afterwards. — [zfs-send.8](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zfs-send.8)
- "The format of the stream is committed. You will be able to receive your streams on future versions of ZFS." Some streams need receiver pool features (lz4/zstd, embedded_data, large_blocks). — [zfs-send.8](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zfs-send.8)
- `-i` takes an incremental source that "must be an earlier snapshot in the destination's history". `-I` sends all intermediate snapshots. — [zfs-send.8](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zfs-send.8)
- "Raw sends and redacted sends cannot be combined at this time." "Deduplicated send is no longer supported" (`-D`). — [zfs-send.8](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zfs-send.8)

### Inferences
- **Does it need ZFS on both ends?** On the sending side, yes: the stream is generated from a ZFS snapshot, so every backup client would need its data on a ZFS dataset. Most home Windows and macOS machines don't have one (see Q5).
- A host could store a raw stream as an opaque file without ZFS. But restore means `zfs recv` into a pool, or a reimplemented DMU in Rust. The `zstream` tool can inspect and redup streams, but it isn't a restore engine. (I know this from background knowledge and didn't fetch a source for `zstream`.)
- **Issue 7 (whole-pack reads only).** A send stream is sequential, and incrementals depend on the base snapshot and every increment in between. Restoring one file means replaying the full plus incremental chain. Granularity is worse than restic packs plus index.
- **Issue 8 (no forget/prune).** You can't delete a middle incremental from a chain of streams stored on remote hosts. Rebasing needs a fresh full send, which forgoes deduplication. Restic's prune model is a better fit for remote opaque storage.
- **Issue 1 (pre-1.0 dependencies).** The stream format is stable, but the producers and consumers are the C kernel module and libzfs. A Rust port of send/recv would be new, unreviewed code. That swaps a pre-1.0 Rust crate for a 0.x reimplementation of a huge C subsystem.
- **Issue 4.** See Q1. Raw streams expose object and record structure, so they leak more than packs.

### Gaps
- I didn't find a documented independent specification of the send stream format beyond the source and man page (the `dmu_replay_record` structures in the code).

## Q3. Block-pointer checksums (a Merkle tree) and scrub: defence against disk corruption vs an adversarial host (issues 6, 7, 9)

### Takeaway
ZFS's self-checksumming tree, with the MAC folded into block pointers when encryption is on, is aimed at one host checking its own disks, which it trusts. It doesn't give remote proof that someone else's disk still holds the data. GlobalObject already has an equivalent per-piece Merkle tree (BLAKE3/bao) for verified streaming. Neither scrub nor the checksum tree gives proof-of-possession against a host that can lie (issue 9). The parts worth borrowing are ideas, not code.

### Cited Findings
- Encrypted blocks carry a 128-bit checksum plus a 128-bit MAC in the block pointer, giving "additional protection against maliciously altered data". Scrub and resilver work without keys. — [zfs-load-key.8](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zfs-load-key.8)
- 2.4.0 added time-range scrubs (`zpool scrub -S -E`) and `-a` for all pools. — [zfs-2.4.0 release](https://github.com/openzfs/zfs/releases/tag/zfs-2.4.0) (search excerpt)

### Inferences
- **Issue 9 (audit detection rate, proof-of-possession).** Scrub reads every block locally and compares it against parent pointers. The verifier and the storage are the same trusted kernel. An adversarial host would just report "scrub clean". The audit problem (challenging a remote party to prove it holds bytes, without the verifier holding them) is outside ZFS's threat model. Note that the threat model already says that hosts which fetch from a peer when audited are "not fully prevented". ZFS has nothing for this. Better fits are proofs of retrievability or random BLAKE3 range challenges against bao outboards, which iroh-blobs already supports.
- **Issue 7 (whole-pack reads).** ZFS's indirect-block tree allows random block reads locally. The equivalent in GlobalObject is bao-tree range requests. The cost comes from restic's pack and index granularity combined with GlobalObject's grant and receipt design, not from a missing Merkle tree.
- **Issue 6 (coarse read receipts).** Not addressed. Receipts are an accounting protocol between two untrusted parties, and ZFS has no counterpart.
- The useful idea to borrow is that a background verifier should not need keys (ZFS scrubs without keys). GlobalObject already audits ciphertext without owner keys.

### Gaps
- I didn't fetch a primary OpenZFS document describing the block-pointer tree (openzfs.github.io refused the connection).

## Q4. Dedup (including fast dedup), snapshots, space freeing and block cloning as an analogue of prune (issues 5 and 8)

### Takeaway
ZFS frees space by refcounting blocks across snapshots, clones and the dedup table (DDT) inside one pool. That design assumes one transactional allocator owns all the blocks. It doesn't carry over to blocks spread across untrusted hosts, where "free" is a distributed garbage-collection and authorisation problem. Fast dedup (2.3) is a single-pool DDT performance change, and with encryption, dedup only works within one dataset family. It doesn't help Sybil replica detection (issue 5).

### Cited Findings
- OpenZFS 2.3.0 was released on 13 Jan 2025. Its headline features were Fast Dedup (#15896), "a major performance upgrade to the original OpenZFS deduplication functionality", plus RAIDZ expansion, Direct IO, JSON output and long names. — [zfs-2.3.0 release](https://github.com/openzfs/zfs/releases/tag/zfs-2.3.0) (search excerpt)
- TrueNAS says the fast dedup table can be constrained to fit in RAM or on dedicated flash. Block cloning arrived in 2.2. — [TrueNAS blog](https://truenas.com/blog/electric-eel-openzfs-23) (search excerpt)
- With encryption on, dedup is limited to a dataset, its snapshots and its clones, and the DDT is in plaintext. — [zfs-load-key.8](https://raw.githubusercontent.com/openzfs/zfs/master/man/man8/zfs-load-key.8)

### Inferences
- **Issue 8 (forget/prune).** The ZFS model of birth transaction groups (txgs) plus deadlists per snapshot is an elegant way to compute exactly which blocks a snapshot deletion frees, without scanning everything. GlobalObject could borrow the idea: track for each pack the earliest and latest snapshots referencing it. But rustic_core already implements forget/prune for the restic format, and the missing piece is wiring it to GlobalObject's control plane (deleting pieces on hosts, credit, grace periods). Porting ZFS code wouldn't help. This is the clearest case where the fix is "call the existing rustic prune", not a ZFS port.
- **Issue 5 (Sybil, k=1).** ZFS dedup is the opposite of what's needed. It detects identical blocks so it can store them once. GlobalObject's problem is a host pretending to store distinct copies while keeping one. Since dedup only works within one encryption family, there's nothing to reuse. Fixes are EC (already the recommendation), per-replica encoding, or proofs of replication. None of them come from ZFS.
- Block cloning (reflink-style references) is a local allocator feature, so it doesn't apply.

### Gaps
- I didn't fetch the 2.3 fast-dedup design docs or Klara's write-ups on them.

## Q5. RAIDZ/dRAID, the single-host kernel design, userspace libzpool, and Windows/macOS ports (issues 10 and 11)

### Takeaway
RAIDZ and dRAID are erasure codes over local vdevs, run synchronously within one transaction group. They assume all disks are in one machine under one trusted kernel. They don't solve what GlobalObject's EC needs: placement across hosts that churn, repair without the owner, and audits. reed-solomon-erasure already covers the maths. ZFS has no networking, NAT traversal or HA control plane, so it doesn't apply to issues 10 or 11. Windows and macOS ports exist but are still release candidates or fork-maintained, so ZFS couldn't be assumed on home clients.

### Cited Findings
- OpenZFS on Windows is still in release candidates. A forum thread lists 2.3.1 rc14 as "still a release candidate/beta, but very good". Known issues include import problems and driver conflicts. — [openzfsonwindows releases](https://github.com/openzfsonwindows/openzfs/releases); [ComputerBase forum](https://computerbase.de/forum/threads/openzfs-for-windows.2229681) (search excerpts, undated)
- OpenZFS on OS X is maintained by Jorgen Lundman as a separate fork. It has 2.3.x tags, and 2.4.1/2.4.3 release candidates appear on the releases page. It supports macOS 10.8 to 15 on Intel and Apple Silicon. — [openzfsonosx releases](https://github.com/openzfsonosx/openzfs-fork/releases); [wiki](https://github.com/openzfsonosx/openzfs-fork/wiki/OpenZFS-on-OS-X) (search excerpts)
- In 2.4.0, mainline OpenZFS supports Linux 4.18 to 6.18 and FreeBSD 14 to 16. — [zfs-2.4.0 release](https://github.com/openzfs/zfs/releases/tag/zfs-2.4.0)
- A FreeBSD forum thread notes that the Linux kernel made a needed symbol EXPORT_SYMBOL_GPL, which out-of-tree ZFS can't use. This is a recurring maintenance burden of being a kernel module. — [FreeBSD forums](https://forums.freebsd.org/goto/post?id=717021) (search excerpt)
- Rob Norris (OpenZFS developer at Klara) has written that most of his plans for OpenZFS are "stymied by the code not being particularly modular". — [despairlabs.com, 12 Mar 2025](https://despairlabs.com/blog/posts/2025-03-12-we-should-improve-libzfs-somewhat/) (search excerpt; direct fetch refused)

### Inferences
- **Distributed EC.** RAIDZ's parity layout (variable stripe width per block, parity computed at write time within one txg) depends on synchronous, atomic writes to every member. Across hosts that come and go, that would turn into a write-availability problem. dRAID's distributed spare idea (spreading spare capacity so rebuilds are parallel) is conceptually similar to GlobalObject's random placement plus repair, but there's no code to reuse.
- **libzpool/ztest.** These run the DMU and SPA in userspace for testing. In principle that's a route to a userspace "receive and restore" without a kernel module. In practice it's C, it isn't packaged as a library with a stable API, and it's CDDL-licensed (background knowledge, not verified this session). Under the user's "prefer existing OSS" rule, binding to libzpool via FFI would beat porting it. Even so, it pulls the whole ZFS core into the client.
- **Issue 10 (NAT/relays) and issue 11 (control-plane TLS/HA).** Not applicable. ZFS replication assumes an external transport (usually ssh) and has no coordinator.

### Gaps
- I found no first-party statement of the final OpenZFS on Windows release state as of November 2026. The latest I saw was 2.3.1 RC-series forum reports.
- I didn't verify the licence from LICENSE files during this session.

## Q6. Existing Rust ZFS work, OpenZFS code size, and Rust in OpenZFS (issue 1, porting cost)

### Takeaway
There is no maintained pure-Rust ZFS implementation or reader. What exists is FFI bindings to libzfs_core and nvpair (Delphix, Razor) and an abandoned 2016 stub crate. Redox gave up on porting ZFS because of its "monolithic design" and wrote its own filesystem (TFS, later RedoxFS). OpenZFS is about 0.5M lines of C, and I found no Rust in it. A Rust port of any meaningful ZFS subsystem would be a large, unreviewed new codebase. That's the opposite of fixing issue 1 (immature dependencies).

### Cited Findings
- crates.io `zfs` 0.1.0 (ticki) was described as "an implementation of the ZFS file system". It was created and last updated in November 2016. — [docs.rs zfs 0.1.0](https://docs.rs/crate/zfs/0.1.0); [rust-digger](https://rust-digger.code-maven.com/crates/zfs) (search excerpt)
- Delphix `rust-libzfs` provides bindings to libzfs_core and nvpair, not a reimplementation. The `zfs-core` crate was last updated in December 2020. — [delphix/rust-libzfs](https://github.com/delphix/rust-libzfs); [rust-digger zfs-core](https://rust-digger.code-maven.com/crates/zfs-core) (search excerpt)
- Razor (`razor-rs`) is "Rust OpenZFS bindings and tools", also wrapping the C. — [razor-zfs/razor-rs](https://github.com/razor-zfs/razor-rs) (search excerpt)
- LCPFS is a self-described copy-on-write filesystem written in Rust with "ZFS-equivalent features" (version 2026.1). Its claims are unverified, and it isn't ZFS-compatible as far as the excerpt shows. — [lcpfs via scour.ing](https://scour.ing/p/https://github.com/artst3in/lcpfs) (search excerpt)
- Redox's TFS README says it was made "as a replacement for ZFS, which proved to be slow to implement because of its monolithic design". TFS "was replaced by RedoxFS and is no longer maintained". — [redox-os/tfs](https://github.com/redox-os/tfs); [ticki/tfs](https://github.com/ticki/tfs) (search excerpt)
- **Code size.** Coverity Scan analyses 493,098 lines for openzfs/zfs, and has done since September 2022. Open Hub reports about 529,588 LOC for behlendorf's zfs fork, analysed about 10 months before the search. The Open Hub OpenZFS page shows C at 92% but gave no total in the excerpt. — [Coverity Scan](https://scan.coverity.com/projects/openzfs-zfs); [Open Hub behlendorfs_zfs](https://openhub.net/p/behlendorfs_zfs); [Open Hub openzfs](https://www.openhub.net/p/openzfs) (search excerpts)
- **Rust in OpenZFS.** I found no Rust in OpenZFS and no formal proposal. Rob Norris has said he'd be "perfectly happy to use Rust" in kernel and filesystem work if it were available. — [FreeBSD forums](https://forums.freebsd.org/goto/post?id=717420) (search excerpt)

### Inferences
- The tools are fragmented and stale (the newest binding updates I found are from 2020). Combined with the ~500k-line C base and Redox's experience, porting any ZFS subsystem to Rust is a multi-year effort. It would produce exactly the kind of custom, unaudited storage and crypto code GlobalObject removed in its migration.
- Binding via FFI (Delphix/Razor) only manages a local pool. It doesn't give GlobalObject a portable, kernel-free client.

### Gaps
- I couldn't confirm a current (2026) Open Hub total for openzfs/zfs.
- I found no evidence about other Rust ZFS readers (for example in bootloaders), so none are listed.

## Summary table by issue (inference, drawing on the findings above)

| # | Issue | Would an OpenZFS component or port help? | Why |
|---|---|---|---|
| 1 | rustic_core pre-1.0 | No | Its replacement would be a new Rust port of ~500k LOC of C. No mature Rust ZFS exists (Q6). The stream format is stable, but no Rust code exists to produce or consume it |
| 2 | iroh-blobs/bao pre-1.0 | No | ZFS has no network transport. Send streams go over ssh or similar. Block-pointer checksums only verify local data |
| 3 | Restic crypto unaudited | No, and worse | ZFS encryption has no audit I could find, plus send/recv corruption bugs from 2021 to 2025 that maintainers didn't fully understand (Q1) |
| 4 | Pack sizes visible | No, and worse | ZFS leaves file sizes, holes, names, properties and dedup tables in plaintext by design (Q1) |
| 5 | Sybil replica dedupe, k=1 | No | Dedup is local and only within a key family. Proof of replication is outside ZFS's threat model (Q4) |
| 6 | Coarse read receipts | No | This is accounting between untrusted parties, which ZFS has no counterpart for |
| 7 | Whole-pack reads | No | Send streams are sequential incremental chains, so granularity is worse. Range reads already exist in bao-tree (Q2, Q3) |
| 8 | No forget/prune | The idea, not the code | Deadlist and birth-txg accounting is a useful design pattern, but rustic_core already has prune. Incremental chains can't be pruned remotely (Q2, Q4) |
| 9 | Audit rate, proof-of-possession | No | Scrub trusts the local kernel and gives no remote proof (Q3) |
| 10 | NAT/relays untested | No | Not in scope for ZFS |
| 11 | Control plane TLS/HA | No | Not in scope for ZFS. ZFS is single-host |

The underlying design assumptions that rule ZFS out:
- One trusted kernel owns all the disks, the allocator and the transaction groups. There's no adversarial storage.
- Encryption protects data at rest, not the structure of the pool.
- Replication needs ZFS on the sending end, and ZFS (or its DMU) to restore.
- Mainline only supports Linux and FreeBSD. Windows is in release candidates and macOS is a fork.
