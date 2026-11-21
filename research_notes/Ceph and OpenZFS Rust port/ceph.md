# Ceph components as Rust ports for GlobalObject

Scope: whether any Ceph component, used as-is or ported to Rust, would fix GlobalObject's listed issues 1-11 (from `docs/status.md`, `docs/threat-model.md`, `docs/architecture.md`, `docs/dependencies.md`), and where Ceph's design assumptions rule it out. Research date: 2026-11-07.

Method note: `docs.ceph.com`, `ceph.io`, `ceph.readthedocs.io` and `openhub.net` all refused connections from this environment ("proxy refused the connection"). Ceph documentation was therefore read from the doc sources in the `ceph/ceph` GitHub repo (`raw.githubusercontent.com`), on the `main` branch unless the URL says otherwise. Those `.rst` files are the source of docs.ceph.com, so they are primary. Points marked "search excerpt" come from search-engine summaries of pages I couldn't fetch, so they are less reliable.

## Q1. CRUSH: what it is, whether it is reusable pure computation, and how it compares with GlobalObject's placement

### Takeaway
CRUSH is a deterministic pseudo-random function: (object/PG id, cluster map, rule) → ordered list of devices. It is pure computation and could be reused or ported. But it solves a problem GlobalObject doesn't have, namely letting clients compute placement without asking a central directory. GlobalObject deliberately makes the control plane choose hosts, both so that clients can't bias placement (the Sybil defence) and so that weighting can use reputation and free space. A CRUSH port would therefore weaken the Sybil defence, or at best duplicate what the control plane already does.

### Cited Findings
- CRUSH is described as "a scalable pseudorandom data distribution function" that maps objects to devices without a central directory. It needs only a compact, hierarchical cluster description and a placement policy. SC'06 paper by Weil, Brandt, Miller and Maltzahn, November 2006 (search excerpt) — [Weil et al., CRUSH SC'06 PDF](https://www.ssrc.ucsc.edu/Papers/weil-sc06.pdf); [IEEE Xplore](https://ieeexplore.ieee.org/document/4090205)
- Ceph architecture doc: "The client uses its copy of the cluster map and the CRUSH algorithm to compute precisely which OSD it will use…" and "Object locations must be computed." — [ceph/ceph doc/architecture.rst (reef)](https://raw.githubusercontent.com/ceph/ceph/reef/doc/architecture.rst)
- CRUSH rules can place EC locality groups by failure domain, for example `crush-steps='[ [ "choose", "rack", 2 ], [ "chooseleaf", "host", 4 ] ]'`, so that "recovering from the loss of a single OSD does not require using bandwidth between the two racks." — [doc/rados/operations/erasure-code-lrc.rst](https://raw.githubusercontent.com/ceph/ceph/main/doc/rados/operations/erasure-code-lrc.rst)
- Existing Rust CRUSH crates (search excerpts, not verified):
  - `libcrush`, 0.1.2 and 1.0.1. Its README says the sources were "extracted from Ceph on January 2017" under LGPL-2.1, and also calls itself a C library — [docs.rs libcrush README](https://docs.rs/crate/libcrush/latest/source/README.md)
  - `crush-sys` 1.0.1, updated 2025-10-09 — [rust-digger crush-sys](https://rust-digger.code-maven.com/crates/crush-sys)
  - `crushtool` 0.3.10 decompiles binary crushmaps (straw and straw2 bucket types). It is not a placement engine — [docs.rs crushtool](https://docs.rs/crushtool)
- GlobalObject's own placement:
  - Domain hierarchy disk < host < owner < site < ASN < country, with one piece per domain at `domain_level`, and the owner's own nodes excluded.
  - The control plane chooses hosts (never the client), weighted by free space and reputation.
  - Placement groups of 16 segments.
  - Sources: `docs/architecture.md` §Placement, and `docs/threat-model.md` ("Bias placement: Client never chooses hosts").
- Ceph was already credited in the project's prior-art table for "Failure-domain hierarchy, placement groups" — `docs/architecture.md` §Prior art.

### Inferences
- The project already uses CRUSH's two big ideas, the hierarchy and placement groups. What CRUSH adds beyond them is **decentralised computability**: anyone holding the map can recompute placement. In GlobalObject that is a liability rather than an asset.
  - The Sybil defence relies on placement being an unpredictable, server-side random choice. With CRUSH, anyone with the map can predict placement, so an attacker can grind object IDs or join with node IDs that land on wanted PGs. In restic, pack IDs are content hashes the client controls.
  - Mitigation would require a server-held secret salt in the hash input. That turns it back into control-plane placement.
- CRUSH's weights are static (capacity). GlobalObject weights dynamically by reputation, free space and probation. These could be encoded as CRUSH weights, but every reputation change would move data. CRUSH's "minimal movement on map change" helps here, but a lookup table in Postgres has no movement cost at all.
- A small potential use: a CRUSH-like straw2 deterministic selection would let the control plane recompute placement for repair verification without storing per-segment host lists. Postgres already stores those lists cheaply, so the gain is small.
- **Licence:** Ceph code, and Ceph-derived `libcrush`, is LGPL. The project's rule is permissive-only, plus two MPL packages (`docs/dependencies.md`). A port of Ceph's `crush/` code would inherit LGPL. A clean-room implementation from the SC'06 paper would avoid that. The licence of Ceph as a whole wasn't fetched here, so that claim is unsourced. The LGPL-2.1 figure for libcrush is sourced above.
- **Verdict:** CRUSH addresses none of issues 1-11 directly. At most it is an optional refactor of placement, and it would weaken the Sybil defence (issue 5 context).

### Gaps
- Didn't verify maintenance status, straw2 coverage or the licence of `crush-sys` and `libcrush`. The search summary of `libcrush` was self-contradictory (it calls itself both Rust and C).
- Couldn't fetch the SC'06 PDF itself, so the paper's quantitative claims (movement on reweight, speed) aren't quoted.

## Q2. Erasure-coding plugins and FastEC (Tentacle): partial reads, repair bandwidth, overwrites

### Takeaway
Ceph's EC plugins are thin wrappers around codes: ISA-L and Jerasure (Reed-Solomon), LRC (local parity), CLAY (regenerating codes with sub-chunk repair) and SHEC. Tentacle's FastEC adds partial reads and partial writes at the OSD/PG layer, not in the codes.
- CLAY and LRC directly target repair bandwidth. Repair bandwidth is relevant to GlobalObject because home uplinks are the bottleneck. But CLAY is deprecated and slated for removal.
- Partial reads (issue 7) in GlobalObject are blocked by the restic pack format and whole-piece BLAKE3 hashing, not by the RS code. `reed-solomon-erasure` is systematic, so a partial read of the data shards is already possible in principle.

### Cited Findings
- `allow_ec_overwrites` enables partial writes for RBD, CephFS and librados on EC pools. It has been available since Luminous, and "This can be enabled only on a pool residing on BlueStore OSDs." — [doc/rados/operations/erasure-code.rst](https://raw.githubusercontent.com/ceph/ceph/main/doc/rados/operations/erasure-code.rst)
- FastEC (`allow_ec_optimizations`), from Tentacle — same source:
  - Adds partial reads, partial writes and reduced padding.
  - Can't be disabled once enabled, and needs all MONs and OSDs on Tentacle or later.
  - The stripe unit must be a multiple of 4 KiB, with at least 16K recommended.
- Plugin support for FastEC — same source:
  - The doc says "Optimizations are currently only supported with the ISA-L plugin". It also says Jerasure works with `reed_sol_van` only, which is a slight internal inconsistency in the doc.
  - CLAY, LRC and SHEC are blocked with an error.
  - ISA-L is the default for pools created on Tentacle or later.
  - "the Jerasure library is no longer maintained."
- Ceph blog "Fast Erasure Coding for Tentacle Performance Updates" (2025), search excerpt:
  - Partial reads mean "only read the minimal data to honour the client request". Wasted I/O on small reads grows with K.
  - It also covers partial writes, parity-delta writes, and a 16K stripe unit recommendation (default 4K).
  - The gains target block and file workloads, with possible benefits for small S3 objects or random reads.
  - [ceph.io blog, 2025](https://www.ceph.io/en/news/blog/2025/tentacle-fastec-performance-updates/) (fetch blocked)
- Tentacle is v20.2.0 — [v20.2.0 Tentacle released](https://ceph.io/en/news/blog/2025/v20-2-0-tentacle-released) (search excerpt)
- CLAY — [doc/rados/operations/erasure-code-clay.rst](https://raw.githubusercontent.com/ceph/ceph/main/doc/rados/operations/erasure-code-clay.rst):
  - Repair contacts d helpers, k+1 ≤ d ≤ k+m−1.
  - At k=8, m=4, d=11, repairing 1 GiB downloads about 250 MiB from each of 11 OSDs, 2.75 GiB in total, versus 8 GiB for Jerasure or ISA.
  - Disk I/O is d·S/(d−k+1), versus k·S for RS.
  - Sub-chunk count is q^((k+m)/q) with q = d−k+1. Reads are non-contiguous, and sub-chunks should be at least about 4 KB.
  - **"Support for this plugin will be removed in the Vampire release."**
- LRC — [doc/rados/operations/erasure-code-lrc.rst](https://raw.githubusercontent.com/ceph/ceph/main/doc/rados/operations/erasure-code-lrc.rst):
  - At k=8, m=4, l=4, a single-OSD loss needs 4 surviving OSDs instead of 8.
  - Storage overhead is higher than RS. The CLAY doc compares k=10, m=4: CLAY's 3.25·S I/O at 0.4 overhead versus LRC's 7·S at 0.6.
- Third-party commentary (everywan.com) argues that FastEC narrows the gap with replication rather than closing it (search excerpt, opinion) — [everywan blog](https://everywan.com/en/blog/fast-ec-ships-switched-off-ceph-tentacle)

### Inferences
- **Issue 7, whole-pack reads only.** Ceph FastEC does not transfer.
  - It is a RADOS-level feature built around Ceph's stripe-unit layout and an OSD primary that reads from the shards. GlobalObject reads whole packs because:
    1. The `rustic_core` backend API asks for whole files.
    2. Each piece is one BLAKE3 blob.
  - `iroh-blobs` already supports verified range requests, which the auditor uses. So partial reads need range-aware grants plus mapping a pack byte range to data-shard ranges. With a systematic RS code that is shard index = offset / shard_len.
  - The Ceph idea worth borrowing is the *design* (stripe-unit layout, so small reads touch one shard), not code.
- **Repair bandwidth over home uplinks** isn't on the 1-11 list, but it's a real cost: repair "draws on a rate-limited pool reserve".
  - Porting LRC or CLAY could cut repair traffic about 2-3× at 10-of-20.
  - CLAY is deprecated in Ceph, and its many non-contiguous sub-chunk reads would map badly onto per-piece BLAKE3 blobs and per-range grants, because there are many range requests per helper.
  - LRC is simpler, but raises storage overhead, which directly reduces the usable-GB-per-TB figure that is already a demand risk.
- **Overwrites (`allow_ec_overwrites`)** are irrelevant. Restic objects are immutable and content-addressed.
- **No Rust port of the plugins is needed for RS.** `reed-solomon-erasure` is MIT and established. ISA-L is a C library (BSD-3, unsourced here) that could be bound via FFI if speed mattered.

### Gaps
- Couldn't fetch the ceph.io FastEC blog for exact numbers or date. Its sentences come from search snippets.
- No Rust implementations of CLAY or LRC were looked for or found.
- The internal inconsistency in the Ceph doc (ISA-L only versus Jerasure `reed_sol_van` also supported) is unresolved.

## Q3. Scrub and deep scrub: do they defend against a lying OSD? Any proof of storage?

### Takeaway
No. Scrub is run *by the OSDs themselves* on their own data, comparing replicas and checksums. It detects bit rot and inconsistency, not a dishonest OSD. An OSD that lies about its scrub results, or recomputes checksums over fabricated data, isn't caught. There's no challenge-response proof of storage. GlobalObject's control-plane audits with BLAKE3 range proofs are already stronger against malicious hosts than anything in Ceph.

### Cited Findings
- "Ceph OSD Daemons compare the metadata of their own local objects against the metadata of the replicas of those objects", and "Ceph OSD Daemons perform deeper scrubbing by comparing the data in objects, bit-for-bit, against their checksums." — [doc/architecture.rst (reef)](https://raw.githubusercontent.com/ceph/ceph/reef/doc/architecture.rst)
- Light scrub "checks the object size and attributes", typically daily. Deep scrub "reads the data and uses checksums to ensure data integrity", typically weekly. Ceph "compares each primary object to its replicas". — [doc/rados/configuration/osd-config-ref.rst](https://raw.githubusercontent.com/ceph/ceph/main/doc/rados/configuration/osd-config-ref.rst)
- Fault handling is crash-model: OSDs report on neighbours to the monitors. A search for Ceph Byzantine or malicious-OSD handling turned up no such mechanism — [doc/architecture.rst (reef)](https://raw.githubusercontent.com/ceph/ceph/reef/doc/architecture.rst); search summary over [docs.ceph.com architecture](https://docs.ceph.com/en/latest/architecture/)
- GlobalObject's current audit and its known residual risks:
  - A random range of a random piece, verified by BLAKE3 streaming. 300 audits catch 1% loss with 95% probability.
  - "Fetch from a peer when audited" is not fully prevented, and audit timeouts don't count as failures.
  - Sources: `docs/architecture.md`, `docs/threat-model.md`.

### Inferences
- **Issue 9, audit detection rate and proof of possession.** Ceph offers nothing to port.
  - Measuring detection rate is a test-harness and simulator task.
  - Stronger proofs (fetch-from-peer resistance, Sybil copy-dedupe) need proof-of-replication or proof-of-retrievability constructions, as in Filecoin and Sia (prior art the project already lists), or timing-bound challenges. Nothing in Ceph does this.
- Ceph's design assumes OSDs are inside one administrative trust domain, sharing cephx secrets with the MONs. Its scrub is an fsck, not an audit.

### Gaps
- Didn't find an explicit Ceph statement saying "OSDs are trusted" or "no Byzantine tolerance". The conclusion rests on the crash-fault design and the shared-secret auth below, so it is an inference, though a strong one.
- Default values of `osd_scrub_min_interval` and `osd_deep_scrub_interval` weren't in the fetched text.

## Q4. msgr2 secure mode, cephx, and encryption at rest

### Takeaway
- msgr2 secure mode gives AES-GCM transport encryption after cephx authentication.
- cephx is symmetric shared-secret Kerberos-style ticketing, in which the monitors hold every client's secret and the MONs and OSDs share a secret. That is fundamentally unsuitable when hosts are untrusted.
- Ceph's client-side encryption is RBD-only: LUKS in librbd, AES-XTS, from Pacific onward. It is unauthenticated block-device encryption, weaker in kind than restic's AEAD for untrusted storage.
- None of this improves on iroh, PASETO and restic for issues 3 or 11.

### Cited Findings
- msgr2 — [doc/rados/configuration/msgr2.rst](https://raw.githubusercontent.com/ceph/ceph/main/doc/rados/configuration/msgr2.rst):
  - crc mode gives "a crc32c integrity check to protect against bit flips", with no secrecy and no MITM protection.
  - Secure mode gives "full encryption of all post-authentication traffic, including a cryptographic integrity check" and "uses the AES-GCM stream cipher".
  - Modes are set by `ms_cluster_mode`, `ms_service_mode` and `ms_client_mode`. The defaults weren't stated on that page.
- cephx — [doc/architecture.rst (reef)](https://raw.githubusercontent.com/ceph/ceph/reef/doc/architecture.rst):
  - "`cephx` uses shared secret keys for authentication. This means that both the client and the monitor cluster keep a copy of the client's secret key."
  - "Ceph Monitors and OSDs share a secret".
  - "The `cephx` protocol does not address data encryption in transport (for example, SSL/TLS) or encryption at rest."
- With signatures on, "data is not encrypted in flight". Signatures give "some limited protection against messages being tampered with in flight". — [doc/rados/configuration/auth-config-ref.rst](https://raw.githubusercontent.com/ceph/ceph/main/doc/rados/configuration/auth-config-ref.rst)
- RBD client-side encryption — [doc/rbd/rbd-encryption.rst](https://raw.githubusercontent.com/ceph/ceph/main/doc/rbd/rbd-encryption.rst):
  - "Starting with the Pacific release, image-level encryption can be handled internally by RBD clients."
  - LUKS1 and LUKS2, AES-128 or AES-256, and "xts-plain64 is currently the only supported encryption mode".
  - krbd isn't supported, and neither are journaled images.

### Inferences
- **Issue 3, restic crypto unaudited.** Porting Ceph crypto doesn't help.
  - RBD's AES-XTS has no integrity tag and is deterministic per sector, so it's unsuitable for untrusted hosts.
  - cephx is a symmetric ticket system. msgr2 AES-GCM is transport only.
  - Paths that do help: commission an audit, or move to a format with an audited spec. Neither relates to Ceph.
- **Issue 11, no native TLS on the control plane.** Ceph's msgr2 is a bespoke protocol, and porting it would be pointless. The Rust ecosystem fix is `rustls` with `axum-server` or `tokio-rustls`, an off-the-shelf solution (my inference, not researched here).
- **cephx's model is incompatible with GlobalObject's threat model.**
  - The MONs hold clients' secrets, and every OSD shares a secret with the MONs, so a single malicious OSD holds key material that lets it validate tickets.
  - GlobalObject's PASETO v4.public grants are asymmetric. Hosts only verify and can't forge, which is strictly better for untrusted hosts.
- On encryption at rest on OSDs, ceph-volume can put OSDs on dm-crypt, from my background knowledge, **unsourced here**. That protects against disk theft only, not against the OSD operator, so it's irrelevant to a host-is-adversary model.
- RGW server-side encryption, including SSE-C, encrypts at the RGW gateway, which sees plaintext (background knowledge, **unsourced here**). That fails GlobalObject's goal 1.

### Gaps
- msgr2 default modes weren't confirmed.
- I didn't fetch dm-crypt (ceph-volume) or RGW SSE docs, because docs.ceph.com was blocked. The statements above are background knowledge.

## Q5. Monitors (Paxos) as control-plane HA, and latency or WAN assumptions

### Takeaway
Ceph MONs are a Paxos-replicated, strongly consistent store for small cluster maps, and they need a majority quorum. They're an existence proof of an HA control store, but porting them gains nothing over Postgres HA (Patroni or similar) or an embedded Rust Raft (`openraft`, `raft-rs`; not researched here). It would also clash with the project's "prefer existing OSS" rule.

Ceph's stretch guidance assumes at most 10 ms RTT between data sites, which home links can't meet. That rules RADOS out as the data plane across home users, but doesn't matter for a control plane run by one operator in one data centre.

### Cited Findings
- "Ceph uses the Paxos algorithm and a majority of monitors" to establish consensus (for example 2 of 3 or 3 of 5) — [doc/architecture.rst (reef)](https://raw.githubusercontent.com/ceph/ceph/reef/doc/architecture.rst)
- Red Hat Ceph Storage 8 and 9 admin guides say latency between the two data sites should stay at or under 10 ms RTT (search excerpt) — [RHCS 9 stretch clusters](https://docs.redhat.com/en/documentation/red_hat_ceph_storage/9/html/administration_guide/stretch-clusters-for-ceph-storage); [RHCS 8](https://docs.redhat.com/en/documentation/red_hat_ceph_storage/8/html/administration_guide/stretch-clusters-for-ceph-storage)
- Ceph blog: each write pays at least the inter-site RTT, and jitter must also be low (search excerpt) — [Ceph Stretch Clusters Part 2, 2025](https://www.ceph.io/en/news/blog/2025/stretch-cluuuuuuuuusters-part2/)
- Upstream stretch-mode doc — [doc/rados/operations/stretch-mode.rst](https://raw.githubusercontent.com/ceph/ceph/main/doc/rados/operations/stretch-mode.rst):
  - The tiebreaker "can have higher network latency to the OSD zone(s) than OSD zone(s) can have to each other". **No numeric limit is given.**
  - "Stretch clusters have a higher likelihood of (possibly asymmetric) network splits."
  - A netsplit can leave OSDs unable to be marked down, which blocks I/O.
- **The "tiebreaker up to 100 ms" figure couldn't be confirmed** in either upstream or Red Hat search results.

### Inferences
- **Issue 11, HA.** The Ceph MON is the wrong thing to port.
  - Its value is tied to the OSDMap, PGMap and other Ceph map types.
  - The project already concluded that users' nodes never run quorum, and that control-plane failover is an operator concern (`docs/dependencies.md` §Quorum).
  - Existing options (Postgres with Patroni, or CloudNativePG; `openraft` if embedding) match the "prefer existing OSS" memory rule. A Paxos port from Ceph would be custom consensus code.
- Ceph's MON quorum requires the MONs to be well connected and low latency, with the tiebreaker as the only relaxation. This confirms MONs could never run on home machines.

### Gaps
- The 100 ms tiebreaker figure is unverified. Numbers in the Red Hat docs couldn't be fetched directly (search excerpt only).

## Q6. Ceph's trust model, Byzantine tolerance, and behaviour over WAN, flapping and NAT

### Takeaway
Ceph assumes a single administrative domain of trusted, well-connected daemons: crash-fault tolerance, shared-secret auth, and OSD self-reporting of failures and scrubs. There's no Byzantine fault tolerance. Its failure detector and peering are tuned for data-centre links, and flapping or NATed home nodes would trigger constant remapping and backfill. These are design assumptions, not implementation details, so a Rust port doesn't change them.

### Cited Findings
- OSDs detect neighbouring failures and report them to the MONs. MONs mark OSDs down after missed heartbeats (crash model) — [doc/architecture.rst (reef)](https://raw.githubusercontent.com/ceph/ceph/reef/doc/architecture.rst)
- Shared secrets between the MONs and OSDs, and MONs holding client secrets — same source.
- Netsplits in stretch clusters can block I/O — [stretch-mode.rst](https://raw.githubusercontent.com/ceph/ceph/main/doc/rados/operations/stretch-mode.rst)
- A search for Ceph BFT or malicious-OSD defences returned only general architecture material, with no BFT mechanism (search, 2026-11) — [docs.ceph.com architecture](https://docs.ceph.com/en/latest/architecture/)
- GlobalObject's assumption is the opposite: "Every peer is untrusted" — `docs/threat-model.md`
- The project already rejected Ceph for "about 10 ms between data sites, and trusted nodes" — `docs/dependencies.md` §Rejected

### Inferences
- RADOS addressing assumes routable daemon addresses published in the OSDMap. There's no NAT traversal or relay concept (background knowledge; I didn't find a source).
- **Issue 10, NAT and relays.** Ceph has nothing to offer. iroh's hole punching and relays remain the right layer, and the gap is testing, not code.
- A GlobalObject with 72 h grace for overnight shutdowns is deliberately the opposite of Ceph's fast down/out marking.

### Gaps
- No primary Ceph doc found that states "no BFT" explicitly.
- No source on msgr2 behind NAT.

## Q7. Existing Rust Ceph efforts, Crimson, and codebase size

### Takeaway
The Rust side is FFI bindings to the C `librados`, not ports. There's no pure-Rust RADOS client and no maintained pure-Rust CRUSH of note. Crimson is a C++/Seastar rewrite of the OSD, still a tech preview in Tentacle. Ceph is about 2.3 M lines (search excerpt), so a "Ceph port to Rust" is not a realistic project-sized task. Only small, self-contained algorithms (CRUSH, the LRC or CLAY maths) are portable.

### Cited Findings
- crates.io API query `q=ceph`, 2026-11-07 — [crates.io API](https://crates.io/api/v1/crates?q=ceph&per_page=20):

| Crate | Version | Last updated | Downloads | Notes |
|---|---|---|---|---|
| `ceph` | 3.2.5 | 2021-04-05 | ~54k | "Official Rust interface to Ceph librados and admin sockets", repo github.com/ceph/ceph-rust |
| `ceph-rust` | 0.1.17 | 2017-06-07 | — | Old name of the same crate |
| `ceph-async` | 4.0.5 | 2024-06-19 | — | Fork with async support |
| `async-ceph`, `rados-sys`, `rbd-sys`, `cephfs-sys`, `ceph-bindings-gen` | 0.1.0 | 2026-10-05 | Tens of downloads each | New FFI bindings, repo github.com/steliaai/async-ceph |
| `ceph-rbd` | 0.3.2 | 2019 | — | |
| `rad` | 0.5.0 | 2017 | — | Built on ceph-rust |

- Licences of these crates weren't retrieved.
- `libcrush` (LGPL-2.1, extracted from Ceph in 2017), `crush-sys` (2025-10) and `crushtool` (map decompiler) — see Q1.
- Crimson — [v20.2.0 Tentacle released](https://ceph.io/en/news/blog/2025/v20-2-0-tentacle-released) (search excerpt); [Red Hat Ceph 8 Crimson](https://docs.redhat.com/it/documentation/red_hat_ceph_storage/8/html/administration_guide/crimson):
  - Squid was the first Crimson tech preview.
  - In Tentacle, SeaStore is deployable alongside Crimson-OSD "mainly for early testing and experimentation".
  - Red Hat Ceph 8 docs limit Crimson testing to RBD on replicated pools, with "Do not use Crimson for production data".
- Codebase size: Open Hub shows about 2.34 M lines of code for the main Ceph project. This comes from a search snippet of a contributor's Open Hub account page, with an undated snapshot. The Open Hub project page itself was blocked — [Open Hub account page](https://openhub.net/accounts/rossturk/projects)

### Inferences
- **Issue 1, `rustic_core` pre-1.0, and issue 2, `iroh-blobs` and `bao-tree` pre-1.0.** Ceph doesn't help.
  - Replacing `rustic_core` with RADOS would mean a whole different storage model with no client-side AEAD, plus trusted OSDs.
  - The Ceph Rust crates are FFI wrappers over C/C++ `librados`, which is more mature but brings LGPL, a large native dependency and Ceph's trust model.
  - The newest bindings (`async-ceph`, 0.1.0, October 2026) are themselves brand new.
- Crimson shows that even Ceph's own team takes years to re-implement the OSD in another C++ framework. A Rust port of a component larger than CRUSH or an EC plugin is out of scope.

### Gaps
- Licences and maturity of `ceph` 3.2.5 and `async-ceph` weren't fetched. Upstream ceph-rust repo activity after 2021 wasn't checked.
- No exact, dated LOC from Open Hub, which was blocked.

## Summary matrix: issues 1-11 against Ceph (inferences drawn from the findings above)

| # | Issue | Ceph component that could help | Verdict |
|---|---|---|---|
| 1 | `rustic_core` pre-1.0 | None. RADOS, RGW and RBD aren't client-side-encrypted backup formats | **No** |
| 2 | `iroh-blobs` and `bao-tree` pre-1.0 | None. Ceph has no verified-streaming content addressing. Scrub checksums are internal to the OSD | **No** |
| 3 | Restic crypto unaudited | RBD LUKS (AES-XTS, no auth tag) and msgr2 AES-GCM are transport only. cephx is shared-secret | **No.** Weaker, or the wrong layer |
| 4 | Pack sizes visible to hosts | None. Ceph doesn't hide object sizes from OSDs. EC striping into fixed stripe units hides sizes only to within a stripe | **No.** Padding (a project decision) or fixed-size segments are the fix |
| 5 | Sybil replica dedupe at k=1 | None. Ceph trusts OSDs, has no proof-of-replication, and CRUSH's predictability would make grinding easier | **No.** Harder with CRUSH. EC (already supported) or PoRep-style constructions are the paths |
| 6 | Coarse read receipts | FastEC partial reads at stripe-unit granularity show the idea, but receipts are an accounting construct Ceph lacks | **No** (design idea only) |
| 7 | Whole-pack reads only | FastEC partial reads (Tentacle, ISA-L or Jerasure `reed_sol_van` only) | **Design idea only.** The code is tied to RADOS. Fix via range grants plus systematic RS shard mapping on top of `iroh-blobs` ranges |
| 8 | No forget/prune | None. Ceph's deletes are trivial because the OSDs are trusted. Prune is restic logic | **No.** Already in `rustic_core` |
| 9 | Audit rate unmeasured, proof of possession | Scrub and deep scrub are self-checks by trusted OSDs | **No.** GlobalObject's BLAKE3 audits are already stronger |
| 10 | NAT and relays untested | None. RADOS assumes routable, low-latency data-centre links | **No** |
| 11 | Control-plane TLS and HA | MON Paxos quorum is the right pattern but the wrong code. msgr2 isn't TLS | **No.** Use `rustls`, and Postgres HA or `openraft` |
| — | Repair bandwidth (related, not listed) | LRC and CLAY plugins | **Possible design input.** CLAY is deprecated (removed in Vampire). LRC costs storage overhead. Re-implementation, not a port, given LGPL |
| — | Placement | CRUSH | **Optional.** Pure function, portable from the paper, but undermines control-plane random placement against Sybils |
