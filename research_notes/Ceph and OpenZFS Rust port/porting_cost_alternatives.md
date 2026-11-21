# Porting Ceph/OpenZFS to Rust: maturity outcome, cost, and mature Rust alternatives

Research date: 2026-11-07. Method notes: WebFetch to openhub.net was refused by the proxy, and a second attempt via the sandbox was blocked, so openhub figures come from search-result excerpts only. Crate metadata (versions, dates, downloads, licences) was pulled directly from the crates.io JSON API on 2026-11-07 (https://crates.io/api/v1/crates/<name>). The iroh-blobs README was read directly from GitHub. Most other items are from search-engine summaries of the cited pages, not full-page reads; treat quoted wording as approximate.

## Do large C/C++ to Rust ports reach production quality, how long does it take, and does a port inherit the original's production history?

### Takeaway
The ports that did ship took years and still produced new, behaviour-mismatch bugs when they first reached wide deployment: uutils and sudo-rs in Ubuntu 25.10. Fish, a 57K-line C++ codebase, took about two years with more than 200 contributors. A port starts its production history at zero. Its memory-safety gains don't carry over the original's years of field-tested edge-case behaviour.

### Cited Findings
**fish shell (C++ to Rust)**
- Fish 4.0 shipped in February 2025 with its core ported from C++ to Rust. The port went one subsystem at a time through the autocxx bridge, and the last C++ was removed in early 2024. Regular C++ releases (for example 3.7.0) kept coming out during the migration. — [Ubuntu Handbook](https://ubuntuhandbook.org/index.php/2025/02/fish-shell-4-0-released-with-rust-port-new-bind-key-notation); [fish 4b blog](https://fishshell.com/blog/fish-4b/)
- Scale: 2,604 commits by more than 200 authors over almost 2 years, taking about 57K lines of C++ to about 75K lines of Rust. Sources disagree on duration: one says about 14 months of development, the forum summary of the official post says almost 2 years. — [fish rustport blog](https://fishshell.com/blog/rustport/) (via search excerpt and forum quote); [Linuxiac](https://linuxiac.com/fish-shell-4-0-rust-is-coming/)
- The motivation was threading safety ("Send and Sync statically enforcing rules around threading") and making the project fun for contributors. The approach was a "Fish Of Theseus" port that stayed a working fish at every stage. — [fish rustport blog](https://fishshell.com/blog/rustport/)
- Problems reported: portability, tooling that ignored other targets, and localization. Cargo was too simplistic for installation, so fish still uses CMake. Cygwin support was lost. Verdict: "overall, it went pretty dang well." — [Phoronix coverage via HN reader](https://hn.jakechampion.name/reader/https://www.phoronix.com/news/Fish-Shell-Rust-Challenges)

**uutils coreutils (rewrite of GNU coreutils), default in Ubuntu 25.10**
- md5sum differences broke Makeself archives on Ubuntu 25.10, and switching back to GNU coreutils fixed them. Launchpad bug 2125535 also covers a dd "partial write to slow reader" issue. — [Phoronix, Sept 2025](https://www.phoronix.com/news/Ubuntu-25.10-Coreutils-Makeself); [Launchpad/foundations-bugs list](https://lists.snapcraft.io/archives/foundations-bugs/2025-October/543205.html)
- In uutils `date`, the parser accepted a flag (`-r`) whose logic was never wired up. This broke unattended-upgrades on Ubuntu 25.10, which was fixed in rust-coreutils 0.2.2-0ubuntu2.1 in Oct 2025. — [It's FOSS](https://itsfoss.com/news/ubuntu-25-10-rust-automatic-upgrade-bug/); [Slashdot/OMG Ubuntu, Nov 2025](https://news.slashdot.org/story/25/11/03/0049215)
- There was also an autopkgtest regression (rsyslog on s390x) tied to a rust-coreutils update in Oct 2025. — [foundations-bugs list](https://lists.snapcraft.io/archives/foundations-bugs/2025-October/543205.html)
- GNU test-suite compatibility (the project began in 2013 per general knowledge, and is still not at 100%):
  - 0.6 (Feb 2026): 96.28%
  - 0.9: about 90.4%, after GNU 9.11 added tests
  - 0.10 (Aug 2026): 93.48%, 645 of 690
  - 0.12.0 (Sept 2026): 653 pass and 21 fail. That is 94.36% of all tests, or 96.9% excluding skipped ones.
  — [Phoronix 0.10](https://www.phoronix.com/news/Rust-Coreutils-0.10); [newreleases 0.12.0](https://newreleases.io/project/github/uutils/coreutils/release/0.12.0); [Phoronix 0.12](https://phoronix.com/news/Rust-Coreutils-0.12-Released); [heise 0.6](https://heise.de/-11163476)

**sudo-rs (Trifecta Tech / ISRG Prossimo), default in Ubuntu 25.10**
- In Nov 2025, two moderate vulnerabilities were found in sudo-rs: a possible password leak when sudo times out or is killed, and a wrong user ID in timestamps when targetpw/rootpw is set. One is CVE-2025-64170. They were fixed in sudo-rs 0.2.10 and Ubuntu USN-7867-1 (10 Nov 2025). — [Phoronix](https://phoronix.com/news/sudo-rs-security-ubuntu-25.10); [WebProNews (lower quality)](https://www.webpronews.com/rusts-sudo-overhaul-backfires-security-flaws-rock-ubuntu-25-10)
- The two 2025 C sudo flaws, CVE-2025-32462 and CVE-2025-32463, were in features that sudo-rs doesn't implement. — [It's FOSS](https://itsfoss.com/news/sudo-rs-issue-ubuntu.md); [ISRG/Prossimo](https://www.memorysafety.org/blog/sudo-rs-headed-to-ubuntu)

**Rust in the Linux kernel**
- At the Dec 2025 Maintainers Summit (Tokyo), Rust was declared no longer experimental. As of April 2025 the kernel had about 34M lines of C and about 25K lines of Rust. Android 16 on kernel 6.12 ships a Rust ashmem allocator. Dave Airlie said DRM was about a year from requiring Rust for new code. — [LWN 1050174, 13 Dec 2025](https://lwn.net/Articles/1050174/); [DevClass](https://devclass.com/2025/12/15/rust-boosted-by-permanent-adoption-for-linux-kernel-code/)

**Microsoft and DARPA (automated translation)**
- In Dec 2025, Galen Hunt (Microsoft Distinguished Engineer) posted a goal to "eliminate every line of C and C++ from Microsoft by 2030" using AI plus algorithms, with a "North Star" of "1 engineer, 1 month, 1 million lines of code". He later edited the post to say Windows is NOT being rewritten in Rust with AI, and that this is a research project. — [The Register, 24 Dec 2025](https://www.theregister.com/2025/12/24/microsoft_rust_codebase_migration); [TechRadar](https://www.techradar.com/computing/cloud-computing/microsoft-engineer-clarifies-speculation-around-plans-to-eliminate-c-c-languages-by-2030)
- DARPA TRACTOR (Translating All C to Rust) was announced in Aug 2024. Program manager Dan Wallach said LLMs can convert "some C code" but the output is not error-free. In July 2025, UW-Madison and UIUC received a $5M ForCLift award for formally verified lifting. I found no public 2026 results. — [Dark Reading](https://darkreading.com/application-security/darpa-aims-to-ditch-c-code-move-to-rust); [UIUC CSL](https://csl.illinois.edu/news-and-media/translating-legacy-code-for-a-safer-future-darpa-backs-effort-to-convert-c-to-rust)

### Inferences
- The two Ubuntu 25.10 port failures I found in detail (md5sum/dd output differences, and `date` ignoring a flag) were behaviour or completeness mismatches, not memory-safety bugs. Both sudo-rs CVEs were logic bugs too. That fits the expectation that a port removes one bug class and adds compatibility bugs. I found no source that measures the share of port bugs that are behaviour mismatches, so this is anecdotal.
- A port does not inherit production history. Ubuntu found its bugs only after years of upstream development, at the point of mass deployment. A Rust Ceph or OpenZFS would be a new storage system with zero field hours. For storage, where the failure mode is silent data loss, that is the opposite of "mature".
- Automated translation (c2rust/TRACTOR/Microsoft) is research-stage. Raw c2rust output is mostly `unsafe` Rust, and its correctness still has to be proven by testing. It doesn't shortcut maturity.
- Porting also wouldn't change the architectural reasons Ceph was rejected in docs/dependencies.md: it needs about 10 ms between data sites and trusted nodes.

### Gaps
- I couldn't fetch the fish rustport post in full, so I don't have its exact person-effort figures.
- I found no study quantifying "fraction of port bugs due to behaviour mismatch".
- I found no current (2026) status for c2rust or TRACTOR outputs.

## How big are Ceph and OpenZFS, and what would a port cost?

### Takeaway
Ceph is about 2.3M lines (Open Hub). OpenZFS is about 0.5M lines (Coverity Scan). Basic COCOMO puts writing them at roughly 690 and 135 person-years. A port costs less than writing from scratch but stays in the tens to hundreds of person-years, before any time to earn production trust.

### Cited Findings
- Open Hub lists Ceph at about 2.34M lines of code, with 249 current contributors. I couldn't read the page directly because the fetch was refused, so this comes from a search excerpt, and the language breakdown and analysis date weren't visible. — [Open Hub (search excerpt)](https://openhub.net/accounts/rossturk/projects)
- A GitHub mirror shows the Ceph repo as about 70% C++, with Python, Shell and C making up most of the rest. This is a secondary source. — [ceph_study mirror](https://github.com/zxlnoodle/ceph_study)
- Coverity Scan analyses about 493,098 lines for openzfs/zfs. The kernel module component is about 145,385 lines and the commands about 44,221. The scan date isn't shown. — [Coverity Scan openzfs-zfs](https://scan.coverity.com/projects/openzfs-zfs)
- Open Hub's cost estimates use Basic COCOMO with a=2.4, b=1.05. For example, git at 607,427 lines comes out at 167 person-years. — [Open Hub git estimated cost](https://openhub.net/p/git/estimated_cost)
- Fish's port took 57K lines of C++ to 75K lines of Rust over about 1.2–2 years, with more than 200 authors. — [fish rustport blog](https://fishshell.com/blog/rustport/)

### Inferences
These are my own calculations using Open Hub's formula, effort = 2.4 × KLOC^1.05 person-months:
- Ceph, 2,340 KLOC: about 8,300 person-months, or about 690 person-years.
- OpenZFS, 493 KLOC: about 1,600 person-months, or about 135 person-years.
- Fish, 57 KLOC: about 168 person-months, or about 14 person-years.

COCOMO estimates new development. A port reuses the design, so it could cost less, but fish's line count rose by about 30% in Rust, and the work also needs test parity and a long soak period. Scaling from fish (57K lines in about 2 calendar years with a large volunteer group) suggests that a 0.5M to 2.3M line port is a multi-year, multi-team programme.

These figures are rough. COCOMO ignores domain difficulty, and kernel and distributed-storage code is harder than shell code.

### Gaps
- I couldn't read the Open Hub Ceph or OpenZFS pages directly. There is no Open Hub figure for OpenZFS.
- I found no published person-year estimate for any Rust port of storage software.

## What mature, permissively licensed Rust alternatives exist for the pieces GlobalObject needs?

### Takeaway
The cryptographic and coding primitives are already mature or widely used, and none needs a port:
- BLAKE3 is 1.x.
- reed-solomon-erasure, reed-solomon-simd and reed-solomon-novelpoly are all ≥1.0.
- rustls, quinn and the RustCrypto AEADs are 0.x but each has hundreds of millions of downloads, and some have been audited.

The genuinely immature parts are the higher layers GlobalObject relies on, rustic_core and iroh-blobs (see the last question). No permissively licensed Rust CRUSH exists. The only crate found is GPL/LGPL.

### Cited Findings
Figures from the crates.io API, 2026-11-07. Licences are for the latest version.

| Crate | Latest stable (date) | Licence | Total / recent (90-day) downloads | First release |
|---|---|---|---|---|
| reed-solomon-erasure | 6.0.0 (2022-09-23) | MIT | 6.76M / 1.30M | 2017 |
| reed-solomon-simd | 3.1.0 (2025-10-14) | MIT AND BSD-3-Clause | 2.49M / 0.62M | 2023 (2.0.0) |
| reed-solomon-novelpoly | 2.0.0 (2024-01-25) | Apache-2.0 AND MIT | 5.96M / 0.48M | 2021 |
| isa-l (bindings) | 0.2.0 (2020-06-25) | n/a | 7.7K total | n/a |
| blake3 | 1.8.7 (2026-08-20) | CC0-1.0 OR Apache-2.0 (OR with LLVM exception) | 221M / 60M | 2019 |
| chacha20poly1305 | 0.11.0 (2026-06-28) | Apache-2.0 OR MIT | 110M / 38M | 2016 |
| aes-gcm | 0.11.1 (2026-08-21) | Apache-2.0 OR MIT | 205M / 71M | 2019 |
| rustls | 0.23.46 stable; 0.24.0-dev.2 prerelease (2026-10-14) | Apache-2.0 OR ISC OR MIT | 1.09B / 274M | 2018 (on crates.io) |
| quinn | 0.11.12 (2026-09-14) | MIT OR Apache-2.0 | 405M / 149M | 2018 |
| age | 0.12.1 (2026-07-14) | MIT OR Apache-2.0 | 7.7M / 4.4M | 2019 |
| rage | 0.12.1 (2026-07-14) | MIT OR Apache-2.0 | 148K / 2K | n/a |
| pasetors | 0.8.1 (2026-08-30) | MIT | 10.4M / 1.67M | 2020 |
| redb | 4.5.0 (2026-11-05) | n/a | 15.5M / 6.75M | n/a |
| iroh | 1.4.1 (2026-10-21) | n/a | 4.04M / 2.43M | n/a |
| libcrush (CRUSH) | 1.0.1 (2025-10-09) | GPL-3.0-or-later OR LGPL-2.1-or-later | n/a | 2025-10-08 |

Source for all rows: [crates.io API](https://crates.io/api/v1/crates/blake3), queried per crate. "n/a" means the field wasn't captured.

- reed-solomon-erasure hasn't had a release since Sept 2022 but is still heavily downloaded. — crates.io API (above)
- NCC Group reviewed the RustCrypto AES-GCM and ChaCha20Poly1305 crates. MobileCoin funded the review, engaged NCC in Dec 2019, and the report was published Feb 2020. Two consultants spent 5 person-days, focused on timing side channels, and found no significant issues. — [NCC Group public report](https://www.nccgroup.com/research/public-report-rustcrypto-aesgcm-and-chacha20pluspoly1305-implementation-review/); [aes-gcm docs](https://docs.rs/crate/aes-gcm/^0.10.0)
- Cure53 audited rustls in May–June 2020, covering v0.16+ along with ring, webpki and sct, funded by the CNCF. The results were positive. — [Cure53 report](https://cure53.de/pentest-report_rustls.pdf); [jbp.io](https://jbp.io/2020/06/14/rustls-audit/)
- libcrush describes itself as Rust CRUSH, but its code was taken from Ceph and it is GPL/LGPL. That excludes it under the project's licence rule. crushtool only reads and writes crushmaps. — [libcrush README](https://docs.rs/crate/libcrush/latest/source/README.md); [crushtool](https://docs.rs/crushtool)
- Proxmox Backup Server is described by its vendor as written entirely in Rust, and by Wikipedia as "mostly" Rust. It does client-side AES-256-GCM encryption, fixed or variable chunk deduplication with SHA-256, and an RSA master key for key recovery. The server can be untrusted. — [Proxmox features](https://proxmox.com/en/products/proxmox-backup-server/features); [Wikipedia mirror](https://www.wikiwand.com/en/articles/Proxmox_Backup_Server)
- rustic, the application, is at 0.11.4 (2026-08-18) with 42K downloads (crates.io API, rustic-rs). Its README still contains wording about being "unstable" at line 134. I didn't read the context of that line. — [rustic README](https://github.com/rustic-rs/rustic)

### Inferences
- Version number doesn't track maturity in this ecosystem. rustls and quinn sit behind most Rust HTTPS and QUIC traffic, and the AEAD crates are audited, yet all are 0.x. The licences fit the project's rule, apart from libcrush.
- reed-solomon-simd is the stronger-maintained choice for erasure coding, being newer and SIMD-accelerated. Its BSD-3-Clause component is permissive. reed-solomon-erasure is stable but dormant since 2022.
- No permissive Rust CRUSH exists. The project's decision to write its own placement logic stands. Alternatives include rendezvous or HRW hashing, which is simple enough to implement and audit, but I didn't research specific crates for it.
- Kopia (Go), Borg (Python/C) and restic (Go) aren't Rust. Using them would mean a separate process or a reimplementation, not a crate.
- Proxmox Backup Server is a Rust backup system with production use, but it's a server product, not a library. I didn't check its licence here, though it's understood to be AGPL-3.0, which would be excluded. Verify this.
- bupstash wasn't researched. I hit the tool budget before getting to it.

### Gaps
- Proxmox Backup Server licence not verified in this session.
- bupstash status not researched.
- No 2026 audit found for RustCrypto AEAD 0.11 or for rustls since 2020. A newer rustls audit is plausible but unconfirmed. The search found no 7ASecurity report.
- reed-solomon-simd has no audit found.

## Is "pre-1.0" a meaningful risk signal in Rust, and what mitigates it?

### Takeaway
On its own, "pre-1.0" is a weak signal in Rust. Cargo treats 0.x minor bumps as breaking, so the main cost is API churn on upgrade, not unreliability. The stronger signals are explicit maintainer warnings (like iroh-blobs'), release cadence, users and audits. Pinning, vendoring and keeping 0.x types out of GlobalObject's public API cover most of the risk.

### Cited Findings
- Cargo treats 0.x → 0.(x+1) as incompatible and 0.x.y → 0.x.z as compatible. That isn't strict semver, but Cargo users can rely on it. Steve Klabnik has defended this. — [users.rust-lang.org](https://users.rust-lang.org/t/why-are-the-version-numbers-on-crates-io-so-low/8284); [r/rust thread](https://redlib.hbubli.cc/r/rust/comments/n4ni2d/arent_many_rust_crates_abusing_semantic_versioning/gwwpt37/?context=3)
- Tony Arcieri (RustCrypto) argued in 2020 that a mostly-0.x ecosystem looks immature and that maintainers should move to 1.0. — [tonyarcieri.com](https://tonyarcieri.com/rust-2020-towards-a-1-0-crate-ecosystem)
- 0.x status spreads to dependents unless the 0.x types stay out of the public API. — [rust-lang/api-guidelines#267](https://github.com/rust-lang/api-guidelines/issues/267)
- Examples of widely used 0.x crates (crates.io API, 2026-11-07):
  - rand is still 0.10.3, with 2.1B downloads.
  - rustls 0.23 has 1.09B and quinn 0.11 has 405M.
  - tokio is 1.54.1. It reached 1.0 in Dec 2020, which is from general knowledge and not re-verified here.

### Inferences
- The crates GlobalObject depends on fall into two groups:
  - Low risk despite the 0.x number, because the API sits on a published spec and adoption is wide: pasetors (0.8, 10M downloads, PASETO spec) and bao-tree (1.16M downloads).
  - Real risk, where the 0.x number goes with explicit warnings or small adoption: iroh-blobs (see the next question) and rustic_core (114K downloads, 26 releases in 3 years).
- Mitigations, in rising order of cost:
  1. Pin exact versions with `=` and commit Cargo.lock.
  2. `cargo vendor`, plus cargo-deny or cargo-vet for supply-chain checks.
  3. Wrap each risky crate behind an internal trait. This is already done for the rustic_core backend.
  4. Fork if upstream stalls.
  5. Sponsor maintainers, or fund an audit (the MobileCoin/NCC model cost 5 person-days).
- Each of these costs far less than porting Ceph or OpenZFS.

### Gaps
- I found no dataset correlating 0.x status with defect rates.

## Status of rustic_core and iroh-blobs (as of Nov 2026)

### Takeaway
Both crates are still pre-1.0 and changing quickly. iroh-blobs' own README says its current line isn't production quality and recommends 0.35 from May 2025. rustic_core is 0.13.0 and I found no 1.0 roadmap. Neither would become more mature through a Ceph or OpenZFS port, because they solve different problems.

### Cited Findings
- iroh-blobs:
  - Latest version is 0.104.1, released 2026-11-04. It has had 46 releases since 2024-05, with 942K total and 560K recent downloads. Licence is MIT OR Apache-2.0. 0.35.0 was released 2025-05-12. — crates.io API
  - The README on main reads: "this version of iroh-blobs is not yet considered production quality. For now, if you need production quality, use iroh-blobs 0.35". — [iroh-blobs README](https://github.com/n0-computer/iroh-blobs) (read directly 2026-11-07)
- iroh roadmap: a blobs 1.0 API overhaul in v0.34 (Mar 2025), with 1.0 targeted for H2 2025 and blobs expected to reach 1.0 around the same time. iroh itself is now 1.4.1, released 2026-10-21, but iroh-blobs hasn't followed to 1.0. — [iroh roadmap](https://iroh.computer/roadmap); [iroh 0.35 blog](https://iroh.computer/blog/iroh-0-35-prepping-for-1-0); crates.io API
- A byteiota article calls iroh-blobs one of three "production-ready" protocols at iroh 1.0. That conflicts with the README. — [byteiota](https://byteiota.com/?p=16605)
- bao-tree is 0.17.0, released 2026-10-22, with 1.16M downloads. — crates.io API
- rustic_core:
  - Latest version is 0.13.0, released 2026-08-16. First release was 0.1.0 in 2023-08, with 26 versions since. It has 114K total and 20K recent downloads. Licence is Apache-2.0 OR MIT. — crates.io API
  - I found no 1.0 roadmap or list of production users in searches. The README grep found only MSRV text and no stability statement. — [rustic_core README](https://github.com/rustic-rs/rustic_core)

### Inferences
- The iroh-blobs line that GlobalObject would use (0.9x–0.10x) is explicitly disclaimed. The options are:
  - Pin 0.35. This is stable but old, and its API differs.
  - Pin a current 0.104.x and accept the risk.
  - Contribute to or fund the 1.0 stabilisation.
- rustic_core's risk is in its API, not the data. Its on-disk format is restic's, which has a long production history, and restic itself (Go) can read the repositories. That gives a mature fallback for recovery that a Ceph or OpenZFS port couldn't offer. This is my inference from the project's choice of the restic format, and I didn't verify cross-compatibility tests.

### Gaps
- I couldn't confirm production users of rustic_core or iroh-blobs.
- I found no official iroh-blobs 1.0 date after the roadmap's H2 2025 target was missed.
