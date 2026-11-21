# Prior art: ZFS send/receive as a backup interface (live-pool receivers vs opaque stream/object stores)

Research date: 2026-11-20. Method notes: the WebFetch proxy refused most non-GitHub hosts (rsync.net, zfs.rent, news.ycombinator.com, forum.restic.net, zrepl.github.io). For those, findings come from search-result snippets and are marked "(snippet)". GitHub-hosted READMEs/docs and GitHub API metadata (release dates, push dates, stars) were read directly. Treat "(snippet)" items as lower-confidence. Repo activity was checked through api.github.com on 2026-11-20.

## 1. Services that receive into a live pool (rsync.net, zfs.rent, Klara guidance): encryption, quotas, chains, retention

### Takeaway
The commercial "ZFS-native offsite" market consists of a few providers that give the customer a remote zpool, or a VM with a pool, and run `zfs recv` there. rsync.net (dedicated zfs-send account) and zfs.rent (colocate your own disk with a VM) are the main ones. Confidentiality depends on raw sends (`zfs send -w`) of datasets that are already natively encrypted. Chain integrity, retention and quotas are left to the customer's tooling and to ordinary ZFS semantics on the remote pool. The provider does not manage chains.

### Cited Findings
- rsync.net requires a special zfs-send-capable account. Its pricing page lists per-GB tiered pricing (for example 0.75 cents/GB in the 100–999 TB band) and says transfer and usage are free. (snippet) — [rsync.net Cloud Storage With ZFS](https://rsync.net/products/zfs.html)
- An rsync.net staff member posted on the TrueNAS forum that the platform supports ZFS native encryption and raw send, so customers can send encrypted snapshots without the provider holding keys. (snippet) — [TrueNAS forum: rsync.net now supports ZFS send and receive](https://www.truenas.com/community/goto/post?id=661449)
- Reported rsync.net minimums and prices conflict and are dated. One user cites a 1 TB minimum at $0.03/GB, another a 5 TB minimum at $60/month, and another "just north of $30/month for 1TB" after a price cut. A user also calls it "on the pricier side" at 4 TB for $100/month. (snippets; the figures conflict) — [TrueNAS forum thread](https://www.truenas.com/community/goto/post?id=401132); [TrueNAS forum thread](https://www.truenas.com/community/goto/post?id=527817); [Lemmy "rsync.net offers ZFS send/receive"](https://lemmy.klein.ruhr/post/232443/941808)
- rsync.net also sells a cheaper "no-frills borg" product, reported at about 20% of the cost of the ZFS product. It does not preserve ZFS snapshots. (snippet) — [Lemmy thread](https://feddit.it/post/28656287/19447187)
- zfs.rent offers fixed-rate colocation of the customer's own 3.5" SAS/SATA drive in a Tier 2 datacenter, with a dedicated VM. Drives are passed through verbatim, so the customer can build a mirror or raidz from several drives. Pricing conflicts: $20/slot-month on the homepage, $10/month/drive on a pricing page, and a $240 12-month plan. 1 TB/month of transfer is included and extra is $5/TB. Customers can pre-load data before shipping to avoid the initial transfer. (snippet) — [zfs.rent](https://zfs.rent/); [zfs.rent pricing](https://zfs.rent/pricing.html)
- A TrueNAS community poster called zfs.rent "arguably the best zfs remote replication deal out there" and asked how the service is secured. The question went unanswered in the snippet. (snippet) — [TrueNAS forum: Interesting off-site replication option](https://forums.truenas.com/t/interesting-off-site-replication-option/66459)
- Other ZFS-native replication targets exist, for example Velocity Host (Australia) "KISS Cloud ZFS Mirror", which says its arrays are built to at least raidz3. (snippet) — [Velocity Host](https://velocityhost.com.au/?p=96170)
- Klara Systems guidance on encryption: a plain `zfs send` decrypts encrypted datasets. A raw send (`-w`) means "the receiving side does not need the key at all". Klara recommends a layered pattern: replicate locally to a trusted encrypted target, then raw-send to the remote untrusted site. (snippet) — [Klara: Keeping data safe with OpenZFS security, encryption, delegation](https://klarasystems.com/articles/keeping-data-safe-with-openzfs-security-encryption-delegation/)
- A raw send keeps data encrypted only if the source dataset is already natively encrypted. Raw-sending an unencrypted dataset gives an unencrypted replica. — [TrueNAS forum: ZFS replication to untrusted location](https://forums.truenas.com/t/zfs-replication-to-untrusted-location-options-and-question/5939); [mtlynch.io: Back up encrypted ZFS data without unlocking it](https://mtlynch.io/zfs-encrypted-backups/)

### Inferences
- These services all push chain management (which snapshot is the incremental base, retention) onto the client. The receiving pool enforces consistency, because `zfs recv` refuses a stream whose base is missing. The provider's only lever is a quota on a dataset or pool.
- Existing ZFS-native offsite offerings are priced as premium or niche: dedicated accounts, minimum sizes, or bring-your-own-disk. That suggests a gap for a cheap, multi-tenant, encrypted-by-default ZFS-stream store. The pricing data is anecdotal, so this is not a sized market.
- The "untrusted remote holds raw ciphertext" model is already standard practice (Klara, rsync.net, zrepl's "encrypted-send-to-untrusted-receiver" mode). Our design is not novel there. What is new is storing the stream as opaque segments instead of a live pool.

### Gaps
- Current rsync.net ZFS pricing, minimums and per-account mechanics (VM? jailed zpool? zfs delegation?) could not be fetched directly. The proxy refused rsync.net.
- I found no zfs.rent security, SLA, or failure postmortem material.
- I found no "Klara offsite backup service" as a product. Klara appears to offer consulting and guidance only.

## 2. Tools that store streams as files/objects, and pool-to-pool tools: chain length, pruning, restore, corruption, resume, verification

### Takeaway
Two families exist:
- **Pool-to-pool tools** (zrepl, syncoid/sanoid, zfs_autobackup, znapzend) are actively maintained. They solve chain safety with bookmarks, holds and resume tokens, and they rely on the receiver being a live pool.
- **Stream-to-object tools** (zfsbackup-go, z3, zfs_uploader, zfsbackrest) mostly look stale or experimental. They handle chains with naming or metadata conventions plus "full every N" or full/diff/incr levels. Dependency-aware pruning, verification without a pool, and corruption tolerance are weak or missing.

No mature, maintained open-source "ZFS send to object storage" tool was found.

### Cited Findings

**Maintenance status, from the GitHub API, 2026-11-20**
- zrepl: latest release v0.7.0 on 2026-02-15, last push 2026-05-19, about 1.2k stars. Active. — [api.github.com/repos/zrepl/zrepl](https://github.com/zrepl/zrepl)
- sanoid/syncoid: latest release v2.3.0 on 2025-06-11, last push 2026-06-05, about 3.9k stars. Active. — [github.com/jimsalterjrs/sanoid](https://github.com/jimsalterjrs/sanoid)
- zfs_autobackup: v4.0-rc2 on 2026-10-07, last push 2026-11-13. Very active. — [github.com/psy0rz/zfs_autobackup](https://github.com/psy0rz/zfs_autobackup)
- znapzend: v0.23.6 on 2026-07-06. Active. — [github.com/oetiker/znapzend](https://github.com/oetiker/znapzend)
- zfsbackup-go: latest release v0.4-rc1 on 2020-05-26, last push 2024-05-27. **Stale.** — [github.com/someone1/zfsbackup-go](https://github.com/someone1/zfsbackup-go)
- z3 (Presslabs): latest release v0.1.14 on 2016-11-25, last push 2024-04-17. **Stale.** It was tested on Python 2.7/boto 2. — [github.com/presslabs/z3](https://github.com/presslabs/z3)
- zfs_uploader: 0.9.0 on 2023-08-16, last push 2024-01. **Stale.** — [github.com/ddebeau/zfs_uploader](https://github.com/ddebeau/zfs_uploader)
- zfsbackrest: v0.2.0 on 2025-11-14, 44 stars. Self-described "Experimental … something I wrote over a weekend". — [github.com/gargakshit/zfsbackrest](https://github.com/gargakshit/zfsbackrest)

**zfsbackup-go (closest analogue to "chunked, encrypted segments in object storage")**
- Splits the send stream into volumes (`--volsize`, default 200 MiB, "not guaranteed"). Each volume is optionally compressed (pgzip or an external xz/bzip2), PGP-encrypted (AES-256, cipher not configurable) and signed, then uploaded in chunks of 5–100 MiB. — [zfsbackup-go README](https://github.com/someone1/zfsbackup-go)
- Writes manifests under a `manifests/` prefix. `clean` deletes objects that no manifest references. A delete/retention feature is listed as a TODO, so it has **no dependency-aware pruning**. — [zfsbackup-go README](https://github.com/someone1/zfsbackup-go)
- Chain handling: `--full`, `--increment` (base = newest snapshot already on the target) and `--fullIfOlderThan <duration>` for a periodic full. Restore with `--auto` works out the missing snapshots **by snapshot name**, so renamed snapshots break it. It does not track intermediary snapshots. — [zfsbackup-go README](https://github.com/someone1/zfsbackup-go)
- Resume: `--resume` retries a failed backup, but only if the caller passes identical arguments. Verification: SHA256 + CRC32C per volume, plus backend upload hash checks. Setting `--maxFileBuffer=0` disables those checks. Supported backends: GCS, S3 and S3-compatible, Azure, B2, file, SFTP, with multiple destinations at once. — [zfsbackup-go README](https://github.com/someone1/zfsbackup-go)

**z3 (ZFS to S3)**
- Stores one S3 object per snapshot. Object metadata records `isfull` and `parent`. Multipart chunk size is auto-sized to stay under 9,999 parts. Encryption is S3/KMS server-side, or gpg as a "compressor". — [z3 README](https://github.com/presslabs/z3)
- Health model: "an incremental snapshot is healthy only if its parent exists and is healthy". Backup and restore abort on an unhealthy chain. There is no pruning and no maximum chain length. — [z3 README](https://github.com/presslabs/z3)

**zfs_uploader**
- Configured as a maximum number of incrementals per full backup. The full backup is not removed while it is still needed. (snippet) — [ecosyste.ms: zfs_uploader](https://awesome.ecosyste.ms/projects/github.com%2Fddebeau%2Fzfs_uploader)

**zfsbackrest (pgbackrest-style)**
- Three levels:
  - `full`: standalone.
  - `diff`: incremental from the latest full.
  - `incr`: incremental from the latest diff.

  This caps chain depth at 3. Per-level expiry durations are configured (example: full 14 days, diff 5 days, incr 1 day). "Child backups expire if the parent expires." age encryption is mandatory. The tool distinguishes "orphans" left by incomplete or cancelled uploads (`cleanup --orphans`) from expired backups. It holds the source snapshot with `zfs hold`. — [zfsbackrest README](https://github.com/gargakshit/zfsbackrest)

**zrepl (pool-to-pool; sink mode is the closest analogue to a central receiver)**
- In push mode the active client connects to a passive `sink`. The sink maps each client identity to its own subtree `root_fs/${client_identity}`. — [zrepl docs: overview.rst](https://github.com/zrepl/zrepl/blob/master/docs/configuration/overview.rst)
- zrepl guarantees by default "that incremental replication is always possible and that started replication steps can always be resumed if they are interrupted". It does this with three mechanisms:
  - A **replication cursor**: a send-side bookmark of the last successfully replicated snapshot.
  - A **last-received-hold** on the receiver.
  - **Step holds** on `@from` and `@to` during each step.

  Holds make `zfs destroy` fail, so pruning cannot delete a base that is still needed. — [zrepl docs: overview.rst](https://github.com/zrepl/zrepl/blob/master/docs/configuration/overview.rst)
- An incremental send can use a bookmark as `from` on the sender, but "the receiving side must always have the actual snapshot @from". A resumed incremental needs `@to` plus either `@from` or a bookmark of it. — [zrepl docs: overview.rst](https://github.com/zrepl/zrepl/blob/master/docs/configuration/overview.rst)
- `send.encrypted=true` forces raw sends (`-w`). It refuses to send datasets that are not natively encrypted, as "a safeguard to prevent unintended sends of unencrypted filesystems in raw mode". zrepl explicitly supports an "encrypted-send-to-untrusted-receiver" use case in which the receiver doesn't have the key loaded. Placeholder datasets must be created with `-o encryption=off`. — [zrepl docs: sendrecvoptions.rst](https://github.com/zrepl/zrepl/blob/master/docs/configuration/sendrecvoptions.rst)

**syncoid/sanoid**
- Incrementals go from one sync snapshot to the next. If target-side pruning removes every common snapshot, "you probably need to start over" with a full resend. The advice is to keep longer retention on the target than on the source and never delete `@syncoid_` snapshots. `--force-delete` destroys target datasets when no common snapshot or bookmark exists. — [sanoid discussion #887](https://github.com/jimsalterjrs/sanoid/discussions/887); [syncoid man page](https://manpages.ubuntu.com/manpages/noble/man8/syncoid.8.html)
- An iXsystems bug, "Interrupted full replications are silently failing to resume", shows resume handling is a recurring failure point even in commercial products. (snippet; title only) — [jira.ixsystems.com NAS-109720](https://jira.ixsystems.com/browse/NAS-109720)

**zfs_autobackup**
- Uses bookmarks and holds to protect needed snapshots. Features listed: "progressive thinning", automatic resume, encrypt/decrypt/re-encrypt during transfer, and the `zfs-autoverify`/`zfs-check` verification tools. — [zfs_autobackup README](https://github.com/psy0rz/zfs_autobackup)
- `zfs-check` writes one SHA1 per data chunk (default 100 MB) of a mounted snapshot or zvol and compares the source with the replica. Its stated rationale: "some ZFS bugs can corrupt data during zfs send/recv … The checksums are then correct, but for the corrupt data." It cites openzfs #12762 and #6224. **This verification requires the data to be mounted or decrypted**, not just the stream. — [zfs_autobackup wiki: zfs-check](https://github.com/psy0rz/zfs_autobackup/wiki/zfs-check)

**Verification without a pool**
- `zstream dump` (formerly `zstreamdump`) reads a stream from a file or stdin and verifies the stream checksums by default. It reports "Expected checksum differs from checksum in stream". The END record carries a checksum of the whole stream. (snippet; source code) — [zstream_dump.c (FreeBSD contrib)](https://kernelsources.org/source/raw/freebsd/sys/contrib/openzfs/cmd/zstream/zstream_dump.c); [OpenZFS wiki Documentation/ZfsSend](https://openzfs.org/wiki/Special:MobileDiff/217)

**TrueNAS Cloud Sync and Proxmox Backup Server**
- TrueNAS Cloud Sync is rclone-based and copies files or objects, not ZFS streams. TrueNAS recommends ZFS replication when the target runs ZFS, especially for zvols. — [TrueNAS docs: disaster recovery](https://www.truenas.com/docs/solutions/optimizations/disasterrecovery); [DATAZONE: TrueNAS cloud sync](https://www.datazone.de/en/news/truenas-cloud-sync-offsite-backup/)
- Proxmox Backup Server uses its own chunked, deduplicated format and does not ingest `zfs send` streams. Users who want ZFS-native copies run sanoid/syncoid separately. (snippet) — [r/Proxmox thread](https://redlib.hbubli.cc/r/Proxmox/comments/1khzmbf/proxmox_backup_server_truenas/mrcqrm2)

### Inferences
- The pool-to-pool tools solve the hard problems (base protection, resumability, raw encryption) with **pool-side primitives**: holds, bookmarks and `receive_resume_token`. A stream-blob store has no pool, so the control plane must reimplement each of these as metadata:
  - holds become reference counts or dependency edges between stream objects;
  - the replication cursor becomes "last fully-durable stream per dataset";
  - resume tokens become segment-level upload checkpoints.
- The sender side still needs bookmarks (or a held snapshot) of the last stored snapshot, so it can generate the next incremental after local pruning. The sender also loses the guarantee that the "receiver has @from", because nothing receives until restore. zrepl's sender-side replication-cursor bookmark is directly reusable.
- zfsbackrest's bounded full/diff/incr hierarchy, with "child expires when parent expires", is the simplest prior pattern for capping restore-replay length and making pruning dependency-safe. zfsbackup-go's `--fullIfOlderThan` is the simpler alternative.
- All the object-store tools match by snapshot name. The more robust key is the snapshot GUID and the stream's `fromguid`/`toguid`, which zrepl uses in cursor names (`#zrepl_CURSOR_G_<GUID>_J_<job>`).
- `zstream dump` gives structural and checksum verification of a stream **without a pool and, for raw sends, without keys**. That makes it the natural primitive for "verify a stored stream on an untrusted node or at the control plane". It is weaker than a trial `zfs recv` and cannot catch the logical send/recv bugs that zfs-check targets.

### Gaps
- I did not verify whether zrepl v0.7.0 added any object-storage or "file sink" target. The docs read are the master-branch configuration docs.
- I found no documentation of how any stream-to-object tool behaves when one middle segment is lost or corrupted (partial-restore semantics).
- I did not read znapzend's chain/retention docs in detail. It is a pool-to-pool tool like syncoid.

## 3. Failure stories: broken chains, raw-send encryption bugs, version incompatibility, unrestorable backups

### Takeaway
The recurring failure modes are:
- a single flipped bit invalidating a stored stream, plus every later incremental that depends on it;
- deletion or pruning of the common base snapshot;
- OpenZFS native-encryption send/recv bugs, mostly in **non-raw** sends of encrypted datasets, but with some raw-receive and resume/key-handling issues too;
- receivers lacking the pool features the stream needs.

Practitioners overwhelmingly advise receiving into a pool rather than storing stream files. The dissenters rely on platform-level integrity, checksums and restore tests.

### Cited Findings
- "All it takes is 1 single bit (anywhere in the stream) to flip … and the entire stream is corrupt and unusable." There is error detection but "no error correction", and PAR2 parity is suggested as a mitigation. (snippet) — [FreeBSD forums: how to make a zfs backup](https://forums.freebsd.org/threads/how-to-make-a-zfs-backup.63042/)
- "One bit error in the backup (i.e. incremental set) may render the whole backup set unusable." (snippet) — [zfs-discuss 2017-08: Options for local and offsite incremental backups](http://list.zfsonlinux.org/pipermail/zfs-discuss/2017-August/028982.html)
- On Hacker News, a commenter warns that incremental streams are "completely useless without everything they're based on" and that stream files lack selective restore and indexes. Another commenter stored full send streams of archived file systems in S3-compatible storage, relying on platform integrity plus SHA checksums and restore tests. (snippets) — [HN item 33079152](https://news.ycombinator.com/item?id=33079152)
- A practitioner write-up describes a failed restore caused by deleting a snapshot that was still the base for the next incremental. The fix was to send a new full, or an incremental from the last common snapshot. (snippet) — [cr0x.net: ZFS incremental send](https://cr0x.net/en/zfs-incremental-send-backups/)
- OpenZFS docs issue #494: at least two bugs in **non-raw** send of encrypted datasets were found and fixed, targeted for 2.2.8 and 2.3.3. A raw send "in some cases" avoids the issue but is not guaranteed to. FreeBSD issued errata FreeBSD-EN-25:10.zfs for the same class of problem. (snippet) — [openzfs-docs #494](https://github.com/openzfs/openzfs-docs/issues/494)
- openzfs #6224: snapshots of encrypted datasets report errors after a quick succession of snapshot + non-raw send. The bug is reportedly not triggered with raw send. (snippet) — [openzfs/zfs #6224](https://github.com/openzfs/zfs/issues/6224); [Lawrence Systems forum: Encrypted zfs plus zfs-send not fit for production?](https://forums.lawrencesystems.com/t/encrypted-zfs-plus-zfs-send-not-fit-for-production/20483)
- Raw-receive corruption reports exist. FreeBSD Bug 282622 (July 2025 list thread) concerns "zfs snapshot corruption when using encryption", and openzfs #8852 is older. A 2024 Reddit thread suggested the worst issues were fixed in 2.2.0+ and asked for reproductions on 2.2.5+. (snippets) — [freebsd-fs 2025-07: Bug 282622](https://lists.freebsd.org/archives/freebsd-fs/2025-July/004526.html); [openzfs/zfs #8852](https://github.com/openzfs/zfs/issues/8852); [r/freebsd thread](https://lr.us.psf.lt/r/freebsd/comments/1gqwfqn/openzfs_encryption_and_zfssend8_potential)
- openzfs #14153: passing `--raw` on a resumed send can put an encrypted stream into a different state, flagged as a security concern. openzfs #12614: an incremental receive can overwrite the replica's master key and cause permission-denied errors on remount. (snippets) — [openzfs/zfs #14153](https://github.com/openzfs/zfs/issues/14153); [openzfs/zfs #12614](https://github.com/openzfs/zfs/issues/12614)
- zfs_autobackup's author cites #12762 and #6224 as send/recv bugs where "the checksums are then correct, but for the corrupt data". — [zfs_autobackup wiki: zfs-check](https://github.com/psy0rz/zfs_autobackup/wiki/zfs-check)
- Slashdot/Phoronix, Feb 2024: "OpenZFS native encryption use has new(ish) data corruption bug". (snippet; headline only) — [Slashdot](https://hardware.slashdot.org/story/24/02/17/0613215/openzfs-native-encryption-use-has-newish-data-corruption-bug?sdsrc=prev)
- Version and feature incompatibility:
  - Streams made with `-L` need `large_blocks` on the receiver, and `-e` streams need `embedded_data`.
  - Embedded-data streams cannot be received as encrypted datasets ("incompatible embedded data stream feature with encrypted receive").
  - A Ubuntu 19.10 to FreeBSD 12.0 backup broke over `large_dnode`.
  - Solaris 11.2 streams are rejected with "stream has unsupported feature".

  (snippets) — [zfs-send(8), Debian](https://manpages.debian.org/testing/zfsutils-linux/zfs-send.8); [OpenZFS on OS X forum](https://openzfsonosx.org/forum/viewtopic.php?p=12473); [Launchpad #1854982](https://bugs.launchpad.net/ubuntu/+source/zfs-linux/+bug/1854982); [TrueNAS forum: Cannot receive ZFS stream from Solaris 11.2](https://www.truenas.com/community/threads/cannot-receive-zfs-stream-from-solaris-11-2.30307/post-195143)
- Deduplicated send streams (`zfs send -D`) are deprecated. Receiving them is slated for removal, and `zstream redup` is the conversion path. This is a real example of a stored-stream format losing receive support over time. — [zstream(8), Debian](https://manpages.debian.org/unstable/zfsutils-linux/zstream.8)

### Inferences
- In a segmented blob store, one lost or corrupt segment invalidates that stream and every descendant. Per-segment integrity (hash in the manifest), redundancy across untrusted nodes (replication or erasure coding), and bounded chain depth are therefore mandatory, not optional.
- Version risk is concrete. The control plane should record the sender's OpenZFS version and the stream feature flags for each stream (`zstream dump` exposes the header flags), and should refuse or warn when a restore target is older. `-D` streams should never be accepted.
- Encryption bug history argues for: requiring raw sends only (as zrepl's `encrypted=true` does); recommending OpenZFS ≥ 2.2.8 / 2.3.3; and periodic **trial restores** into a scratch pool by a trusted party, since only a real `zfs recv` plus a content check catches logical send bugs.

### Gaps
- I did not confirm the current fix status of the raw-receive reports (#8852, FreeBSD 282622) against OpenZFS 2.3.x/2.4 release notes.
- I found no single public postmortem from a commercial service about losing customer ZFS backup chains. The evidence is forum anecdotes.

## 4. Deduplicating or chunking ZFS streams (restic/borg on `zfs send`)

### Takeaway
Feeding `zfs send` into restic or borg works (`--stdin`), but no benchmark of dedup ratios was found. Community reports say dedup of send streams is poor. The design rationale is that stream record ordering and headers shift between snapshots, and compression or raw encryption destroys cross-stream byte similarity. For raw-encrypted sends, dedup across snapshots should be essentially nil, except that an identical stream re-sent dedups. Incrementals are already "deduped" by ZFS itself.

### Cited Findings
- restic supports stdin backup via `--stdin` / `--stdin-filename`, and its CDC chunker is Rabin-based. — [restic docs: Backing up](https://restic.readthedocs.io/en/v0.16.4/040_backup.html)
- A Proxmox forum tutorial on piping zfs send (raw) into restic says deduplication does not work with zfs send snapshots. It recommends backing up the raw VM stream instead. (snippet; German) — [Proxmox forum: zfs_send / RAW -> restic](https://forum.proxmox.com/threads/zfs_send-raw-restic.184948/)
- A restic-forum thread "Backing up ZFS snapshots: good idea?" exists, but its contents could not be fetched. (title only) — [restic forum #9604](https://forum.restic.net/t/backing-up-zfs-snapshots-good-idea/9604)
- A general data point: compressed dumps change byte-for-byte nightly and do not dedup in restic. Switching to uncompressed stdin let restic store about 180 KiB for a one-row change in a 20 MB dump. — [mrviduus/textstack PR #700](https://github.com/mrviduus/textstack/pull/700)
- Typical practice is to back up the mounted snapshot (`.zfs/snapshot/...`) with borg/restic at file level rather than the send stream. Example: borgsnap, which also integrates with rsync.net. — [borgsnap](https://github.com/scotte/borgsnap)
- A Lobsters commenter suggested feeding send streams into a chunking deduplicator. It is an untested proposal. (snippet) — [lobste.rs thread](https://lobste.rs/s/w2l7hb/migrating_zfs_pool_from_raidz1_raidz2)
- A Borg mailing-list thread asks about reducing VM-image backup times with zfs send instead of borg-reading raw images, a sign of interest in combining the two. — [borgbackup list 2022q2](https://www.python.org/pipermail/borgbackup/2022q2/002012.html)

### Inferences
- Two caveats here are not measured. First, with **raw sends of natively encrypted datasets**, block payloads are ciphertext with per-block IVs/salts, so content-defined chunking across different snapshots should find little shared content beyond what ZFS incrementals already exclude. Second, full sends of a large, mostly static, unencrypted dataset *might* dedup reasonably under CDC, because WRITE records for unchanged blocks are emitted in similar order. That is untested.
- Practical implication: for this design, chunking is for **distribution and repair**, not dedup. Fixed-size segments are simpler and sufficient. ZFS incrementals already provide change-only transfer. Untrusted nodes cannot dedup ciphertext without leaking equality, so convergent encryption should be avoided unless that leak is accepted.
- The cost of periodic full sends (to bound chain length) cannot be offset by dedup. Bounded chain depth through a full/diff/incr hierarchy (zfsbackrest pattern) is the cheaper lever.

### Gaps
- I found no published benchmark of restic/borg dedup ratios on successive full or incremental `zfs send` streams, plain or raw. This would need a local experiment.

## 5. Pricing and market signals for ZFS-native offsite backup

### Takeaway
Demand is persistent but niche. Recurring TrueNAS, FreeBSD and Lemmy threads ask for "cloud backup with zfs send/recv". Answers cluster on rsync.net (seen as pricey, with a dedicated-account minimum), zfs.rent (cheap but bring-your-own-drive), self-rented VPS/boxes, or giving up ZFS-native and using borg/restic or object storage. New hobby tools keep appearing (zfsbackrest, 2025), which signals unmet demand for "ZFS streams to cheap object storage with encryption".

### Cited Findings
- A long-running TrueNAS thread, "Cloud based backup with ZFS send and receive?", has posts spanning several years. — [TrueNAS forum](https://www.truenas.com/community/goto/post?id=326221)
- rsync.net's ZFS offering was re-discussed on Lemmy/fediverse ("rsync.net offers ZFS send/receive"). Commenters weigh price against borg alternatives. (snippets) — [scribe.disroot.org](https://scribe.disroot.org/post/8429644/10604712); [campfyre.nickwebster.dev](https://campfyre.nickwebster.dev/post/3274480/8854901)
- A FreeBSD forum "cheap remote backup" thread: users rent a cheap remote box (for example 2×2 TB HDD, 8 GB RAM for under $20/month) to receive snapshots over SSH. (snippet) — [FreeBSD forums: cheap remote backup](https://forums.freebsd.org/threads/cheap-remote-backup.87238/latest)
- zfsbackrest, "pgbackrest style encrypted backups for ZFS filesystems", was posted to Hacker News in 2025. — [HN item 45092605](https://news.ycombinator.com/item?id=45092605); [zfsbackrest](https://github.com/gargakshit/zfsbackrest)
- A blog post describes adventures in backing up data with ZFS (2021): pain points of finding ZFS-native offsite. (title only) — [gavinhoward.com](https://gavinhoward.com/2021/02/adventures-in-backing-up-data-with-zfs)
- Practical ZFS Discourse (Jim Salter's community) has a "3-2-1 Backup of ZFS in a Home Server Environment" thread, a sign of homelab demand. (title only) — [discourse.practicalzfs.com](https://discourse.practicalzfs.com/t/3-2-1-backup-of-zfs-in-a-home-server-environment/4174)
- Star counts as a rough interest signal: sanoid about 3.9k, zrepl about 1.2k, zfs_autobackup about 0.8k, z3 about 0.4k, zfsbackup-go about 0.4k (GitHub API, 2026-11-20). — [github.com/jimsalterjrs/sanoid](https://github.com/jimsalterjrs/sanoid); [github.com/presslabs/z3](https://github.com/presslabs/z3)

### Inferences
- The fact that two stream-to-object tools (z3, zfsbackup-go) each gathered about 400 stars and then went stale suggests real demand that no project sustained. The cause is plausibly the hard problems above: pruning, verification and restore UX.
- Our target user (homelab ZFS owners with spare disk) overlaps with zfs.rent's "send a drive" customers and with the "rent a cheap box" crowd. A peer-hosted model competes on price with both. It must overcome the "untrusted storage" objection that even zfs.rent drew ("how is it secured?").

### Gaps
- No quantitative market data (number of rsync.net ZFS customers, revenue, survey data) was found.
- I could not read HN discussion contents directly (fetch blocked), so comment-level sentiment on zfsbackrest and rsync.net is not captured.
