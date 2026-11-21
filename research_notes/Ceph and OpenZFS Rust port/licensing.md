# Licensing of Ceph-to-Rust and OpenZFS-to-Rust ports under a permissive-only policy

Research date: 2026-11-07. This is not legal advice. Facts with a source are kept apart from inference. Tool limits: opensource.org and spdx.org could not be fetched (proxy refused), so the CDDL text was read from the OpenZFS repo's own LICENSE file. I did not fetch the gnu.org GPL FAQ or license list, the LGPL text, or the curia.europa.eu judgment text; those claims rest on search excerpts and secondary sources, as marked.

Policy under test: "permissive only (MIT/Apache-2.0/BSD/ISC etc.); MPL-2.0 allowed only for unmodified dependencies; no GPL/AGPL."

**Verdict.** A line-by-line Rust port of Ceph would still be LGPL-2.1/3 code. A line-by-line Rust port of OpenZFS would still be CDDL-1.0 code. Neither meets the policy. Only a clean-room reimplementation is open to the project: one built from papers, documentation and observed behaviour, with nobody reading the copyleft source. It can be released under a permissive licence, subject to separate patent checks, mainly for erasure-coding acceleration.

## 1. What are Ceph's licences, per component?

### Takeaway
Ceph's default licence, covering CRUSH, msgr, monitors, BlueStore and crimson unless a file says otherwise, is "LGPL-2.1 or LGPL-3". It is weak copyleft and not permissive. A few vendored files are permissive, mainly the Jerasure plugin sources (BSD-style), CRC32C (BSD-3) and some Apache-2.0, MIT and Boost headers. A few scripts are GPL-3 or AGPL-3.

### Cited Findings
- Ceph's COPYING (Debian-style machine-readable format) gives the default `Files: *` (Copyright Sage Weil 2004-2010) as "LGPL-2.1 or LGPL-3 (see COPYING-LGPL2.1 and COPYING-LGPL3)". The packaging block is also LGPL-2.1 or LGPL-3.0. — [ceph/ceph COPYING](https://raw.githubusercontent.com/ceph/ceph/main/COPYING)
- Exceptions listed in COPYING. — [ceph/ceph COPYING](https://raw.githubusercontent.com/ceph/ceph/main/COPYING)
  - `src/osd/ErasureCodePluginJerasure/*.{c,h}`: BSD-style 3-clause text (James S. Plank, University of Tennessee), with no SPDX label.
  - `src/common/crc32c_intel*`: BSD-3 (Intel). `src/common/sctp_crc32.c`: BSD-style (Cisco, Intel).
  - `src/common/deleter.h`, `src/common/sstring.hh`, `src/include/cpp-btree`, `src/common/*s390x*`: Apache-2.0.
  - `src/json_spirit`, `src/test/perf_local.cc`: MIT.
  - `bloom_filter.hpp`, `timegm.h`, `function2.hpp`, `uses_allocator.h` and some async helpers: Boost-1.0. `expected.hpp`: CC0.
  - `src/include/ceph_hash.cc` and `ceph_stable_mod()` in `rados.h`: public domain. The rest of `rados.h` is LGPL-2.1/3.
  - `src/mount/canonicalize.c`: LGPL-2+.
  - `src/ceph-volume/plugin/zfs/*`: BSD-3.
  - `src/rgw/madler/*`: zlib-style.
  - `doc/*`: CC-BY-SA-3.0.
  - `bin/git-archive-all.sh`: GPL-3.
  - `src/script/backport-create-issue` and `backport-resolve-issue`: AGPL-3.
- COPYING has no separate stanza for CRUSH, msgr2, monitors, BlueStore, crimson, gf-complete, ISA, SHEC, Clay or LRC, so those fall under the LGPL-2.1/LGPL-3 default (or their own file headers). — [ceph/ceph COPYING](https://raw.githubusercontent.com/ceph/ceph/main/COPYING)
- The header of `src/crush/mapper.c` on main reads "Copyright (C) 2015 Intel Corporation ... GNU Lesser General Public License version 2.1 ... See file COPYING." — [ceph/ceph src/crush/mapper.c](https://raw.githubusercontent.com/ceph/ceph/main/src/crush/mapper.c)
- The `libcrush` crate on crates.io says its CRUSH sources "were extracted from Ceph on January 2017 and are licensed under LGPL-2.1". — [docs.rs libcrush README](https://docs.rs/crate/libcrush/latest/source/README.md)

### Inferences
- All the core components the question names (CRUSH, msgr2, monitors, BlueStore, crimson, and the LRC, SHEC and Clay plugin code written by Ceph) are LGPL-2.1 or LGPL-3. A Rust port of them would be a modified version of an LGPL library, so it must stay under LGPL. That breaks the "permissive only" rule.
- The Jerasure plugin glue is BSD-style, so on copyright grounds it could be ported and kept under BSD-3, with its notice retained. The patent history in section 4 is a separate risk.
- In 2026, gf-complete, jerasure and ISA-L reach Ceph through git submodules. Their licences are set by their own repos, not by Ceph's COPYING. See Gaps.

### Gaps
- I did not open the gf-complete, jerasure or isa-l submodule LICENSE files. ISA-L is generally reported as BSD-3-Clause, but this run did not verify that.
- I did not read per-file headers for msgr2, mon, BlueStore or crimson. Some files may have their own copyright lines (Red Hat, SUSE, Intel and others) but still LGPL.
- The Linux kernel's copy of CRUSH (`net/ceph/crush/`) probably carries a GPL-2.0 SPDX tag. That is unverified, and the copy is not permissive either way.

## 2. What is OpenZFS's licence, and is CDDL file-level copyleft? Would ported CDDL files be allowed?

### Takeaway
OpenZFS is CDDL-1.0, apart from a few third-party crypto pieces and the GPLv2 SPL on Linux. CDDL is a per-file weak copyleft, much like MPL. Its definition of "Modifications" covers any new file that contains any part of the original. A Rust translation of a CDDL file would therefore be a CDDL-covered modified file. The policy only allows MPL-style licences for unmodified dependencies, so that fails.

### Cited Findings
- From OpenZFS COPYRIGHT: "Unless otherwise noted, all files in this distribution are released under the Common Development and Distribution License (CDDL)". The code comes from OpenSolaris, with OpenZFS and Illumos additions. — [openzfs/zfs COPYRIGHT](https://raw.githubusercontent.com/openzfs/zfs/master/COPYRIGHT)
- Exceptions listed in COPYRIGHT. — [openzfs/zfs COPYRIGHT](https://raw.githubusercontent.com/openzfs/zfs/master/COPYRIGHT)
  - Skein (`module/icp/algs/skein/THIRDPARTYLICENSE`).
  - AES (`THIRDPARTYLICENSE.gladman` and `.openssl`).
  - PBKDF2 (OpenSSL).
  - GCM and GHASH asm (CRYPTOGAMS and OpenSSL).
  - SPL, `module/os/linux/spl/THIRDPARTYLICENSE.gplv2` (GPLv2).
- CDDL-1.0 text, from the OpenZFS LICENSE file. — [openzfs/zfs LICENSE](https://raw.githubusercontent.com/openzfs/zfs/master/LICENSE)
  - §1.9 "Modifications" means "A. Any file that results from an addition to, deletion from or modification of the contents of a file containing Original Software or previous Modifications; B. Any new file that contains any part of the Original Software or previous Modifications; or C. Any new file that is contributed or otherwise made available under the terms of this License."
  - §3.1: Covered Software distributed in Executable form "must also be made available in Source Code form and that Source Code form must be distributed only under the terms of this License."
  - §3.2: "The Modifications that You create or to which You contribute are governed by the terms of this License."
  - §3.6: You may create a Larger Work combining Covered Software with other code "and distribute the Larger Work as a single product", provided the CDDL requirements are met for the Covered Software.
- Secondary sources say the FSF calls CDDL-1.0 a free licence that is incompatible with the GPL. They say this is because it "has a weak per-file copyleft (like version 1 of the Mozilla Public License)". — [Wikipedia archive (UNT), CDDL](https://webarchive.library.unt.edu/web/20160706033538/https://en.wikipedia.org/wiki/Common_Development_and_Distribution_License) (I did not read gnu.org's license list directly.)
- The FSF GPLv3 wiki lists CDDL and MPL (1.x) among licences incompatible with GPLv3. — [gplv3.fsf.org Compatible licenses](https://gplv3.fsf.org/wiki/index.php/Compatible_licenses)
- On 2016-02-25, Software Freedom Conservancy (Kuhn and Sandler) concluded that Canonical shipping a binary `zfs.ko` with Ubuntu 16.04 violated the GPL, on the grounds that CDDLv1 is GPLv2-incompatible. Note: this was SFC, not SFLC. LWN commenters disputed the analysis. — [SFC blog](https://sfconservancy.org/blog/2016/feb/25/zfs-and-linux/); [LWN discussion](https://lwn.net/Articles/677383)

### Inferences
- CDDL works per file, like MPL. Under §1.9.B, a Rust file that "contains any part of" a CDDL file is itself a Modification and stays under CDDL (§3.1/§3.2), even if it sits in a crate that is otherwise MIT. Whether a translation "contains any part of" the original is a copyright question (section 3). Our view is that a faithful line-by-line translation very likely does.
- Under the policy, ported CDDL files would be (a) CDDL rather than MPL-2.0, which is a licence the policy does not name at all, and (b) modified rather than unmodified dependencies. They fail on both counts. Even an unmodified CDDL dependency is not covered by the MPL-only exception; allowing one would need an explicit policy change.
- The GPL/Linux incompatibility only matters if the project ships with Linux kernel code. It is a reason why CDDL code cannot later be merged into GPL projects, and why some downstream users avoid it.

### Gaps
- I did not retrieve the SFLC's own 2016 opinion ("Linux kernel and ZFS: Is it legal?" by Eben Moglen and Mark Wilson), which reportedly concluded that distributing the combination can be acceptable. That conclusion is unverified here, so it is not cited.
- No court has ruled on CDDL's file-level boundary in a translation case (none found).

## 3. Is translating code into another language a derivative work (US/UK/EU)?

### Takeaway
Yes, by statute in all three places. US law lists "translation" first among derivative works. UK law expressly treats converting a program "into a different computer language or code" as a translation, and so as an adaptation. The EU Software Directive reserves "translation, adaptation, arrangement" to the rightsholder. A line-by-line Rust port of Ceph or OpenZFS therefore needs a licence from the copyright holders, and the LGPL or CDDL is that licence, with its copyleft terms. Functionality, programming languages and data or file formats are not protected in themselves (CJEU, SAS v WPL), which is what makes clean-room reimplementation possible.

### Cited Findings
- **US:** 17 U.S.C. §101 defines a "derivative work" as "a work based upon one or more preexisting works, such as a translation, musical arrangement, ... or any other form in which a work may be recast, transformed, or adapted". — [Cornell LII 17 U.S.C. §101](https://www.law.cornell.edu/uscode/text/17/101); [copyright.gov Title 17 ch.1](https://www.copyright.gov/title17/92chap1.pdf) (text taken from search excerpts)
- **UK:** CDPA 1988 s.21 makes making an adaptation a restricted act. For computer programs, a "translation" "includes a version of the program in which it is converted into or out of a computer language or code or into a different computer language or code". The Copyright (Computer Programs) Regulations 1992 (in force 1993-01-01) added a program-specific "adaptation" definition. — [legislation.gov.uk CDPA s.21](https://www.legislation.gov.uk/ukpga/1988/48/section/21); [SI 1992/3233](https://www.legislation.gov.uk/uksi/1992/3233/data.pdf) (I did not check the current consolidated subsection numbering.)
- **EU:** Directive 2009/24/EC Art. 4(1)(b) gives the rightsholder the exclusive right to "the translation, adaptation, arrangement and any other alteration of a computer program and the reproduction of the results thereof". Art. 6 permits decompilation and translation only where they are indispensable to obtain interoperability information. — [legislation.gov.uk copy of Dir. 2009/24 Art. 4](https://www.legislation.gov.uk/eudr/2009/24/article/4/data.html)
- **EU, SAS Institute v World Programming (C-406/10, Grand Chamber, 2 May 2012):** a program's functionality, its programming language and the format of its data files are not forms of expression and so are not protected by copyright under the Software Directive. Copying source code, or parts of it, would still be reproduction. WPL did not infringe partly because it never had access to SAS's source code. — [Wikipedia summary](https://en.wikipedia.org/wiki/SAS_Institute_Inc_v_World_Programming_Ltd); [SCL report](https://www.scl.org/2451-sas-institute-inc-v-world-programming-ltd-report-and-analysis/)
- No court decision squarely addresses source-to-source language translation of software. Commentary treats it as unsettled at the margins: for example, whether a purely mechanical conversion adds new authorship, so that the output is "the same work" rather than a new derivative. — [Kentucky Law Journal blog (2023)](https://kentuckylawjournal.org/blog/should-we-take-a-closer-look-at-the-copyright-implications-of-translating-computer-code-); [debian-legal 2004](https://lists.debian.org/debian-legal/2004/12/msg00104.html); [LKML 2007](https://www.lkml.org/lkml/2007/2/17/201) (forum opinions, not authority)
- **Practice, uutils coreutils (MIT):** its CONTRIBUTING file says the project is original code. It must not contain code from GNU or other Unix-like utilities. Contributors must not read the source of implementations under reciprocal licences (GPL, LGPL), but are encouraged to use the GNU Coreutils manual as a behavioural reference. Acceptable dependency licences are MIT and Apache-2.0. — [uutils CONTRIBUTING.md (mirror)](https://git.joshthomas.dev/mirrors/coreutils/src/commit/3382623bbcead96feb9311fd22d3d61ba76a4d7e/CONTRIBUTING.md); [Apertis mirror](https://gitlab.apertis.org/pkg/rust-coreutils/-/blob/7f891e9d9899ac30cdb3441aceed1babc97b44c8/CONTRIBUTING.md) (I read mirrors, not github.com/uutils directly.)

### Inferences
- Both a mechanical translation (c2rust-style) and a careful hand translation are covered. Either the output is the "same work" in a new form (a reproduction) or it is a "translation"/"adaptation". Either way it needs the copyright holder's licence. Neither reading lets the output be relicensed as MIT or Apache.
- LGPL-2.1/3 allows modifying and redistributing the library only if the modified library stays under LGPL (our understanding of LGPL-2.1 §2 and LGPL-3 via GPL-3; this run did not fetch the licence text). A Rust port of Ceph code is therefore LGPL. Linking such a crate as a separate library would be allowed by LGPL, but the policy bans LGPL outright.
- The uutils "do not read the source" rule is the accepted community way of keeping a reimplementation clearly independent. It fits the SAS v WPL reasoning, where lack of access to the source code helped the defendant.

### Gaps
- I did not fetch the FSF GPL FAQ entries (for example on translations and "work based on the Program") or any Rust-for-Linux mailing-list discussion on porting GPL C to Rust. Rust-for-Linux code goes into the GPL-2.0 kernel, so the derivative question did not come up there. No sourced statement found.
- No UK or US case directly on C-to-Rust or other source-to-source software translation was found.

## 4. Clean-room reimplementation: what it allows, and patent issues

### Takeaway
Implementing CRUSH from Weil et al.'s published SC'06 paper, or ZFS from Sun's published on-disk format specification and observed behaviour, can be released under MIT or Apache-2.0. This holds if implementers do not copy expression from the LGPL or CDDL source. Ideally they should not read it at all (the uutils model, supported by SAS v WPL). Copyright does not protect algorithms, functionality or file formats. Patents are a separate and real risk: erasure-code acceleration has a documented patent-assertion history (StreamScale against GF-Complete and Jerasure 2.0).

### Cited Findings
- Functionality, programming language and data-file format are not protected by copyright (EU). Access to and copying of source code is what turns reimplementation into reproduction. — [SCL report on SAS v WPL](https://www.scl.org/2451-sas-institute-inc-v-world-programming-ltd-report-and-analysis/)
- EU Directive 2009/24 Art. 6 permits translation of a program's form without authorisation where indispensable for interoperability information. — [legislation.gov.uk Dir. 2009/24](https://www.legislation.gov.uk/eudr/2009/24/article/4/data.html) (summary excerpt)
- StreamScale asserted that using GF-Complete (especially in Jerasure 2.0+) "or any similar software, method or code for erasure coding" infringes US Patent 8,683,296. James Plank then withdrew both libraries. — [Plank, UT-EECS-14-721](https://library.eecs.utk.edu/files/ut-eecs-14-721.pdf); [Techdirt, 2014-11-15](https://www.techdirt.com/articles/20141115/07113529155/patent-troll-kills-open-source-project-speeding-up-computation-erasure-codes.shtml)
- In April 2015 StreamScale's counsel wrote to the Ubuntu Technical Board asking it to drop packages, naming Ceph's `src/erasure-code/jerasure`. The letter describes earlier confidential trade-secret litigation with Plank. — [Ubuntu technical-board list, 2015-04](https://lists.ubuntu.com/archives/technical-board/2015-April/002100.html)
- USENIX removed two FAST '13 papers after a StreamScale demand. — [USENIX FAST'13 memo](https://db.usenix.org/system/files/conference/fast13/fast13_memo_021715.pdf)
- Local Reconstruction Codes came from Microsoft (USENIX ATC 2012 best paper) and were deployed in Azure Storage from 2012. Ceph documents its `lrc` plugin as layering erasure codes. — [Microsoft Research](https://www.microsoft.com/research/publication/erasure-coding-in-windows-azure-storage/); [Ceph LRC docs](https://docs.ceph.com/en/reef/rados/operations/erasure-code-lrc/)

### Inferences
- **CRUSH.** A permissive clean-room implementation from the paper is legally viable for copyright. Bit-exact placement compatibility with real Ceph clusters needs exact constants and details: the hash function, the `straw2` ln table, and tunables. These are mostly found in the LGPL source, though `ceph_hash.cc` is public domain per COPYING. Getting them by reading LGPL code weakens the clean-room claim. Safer routes are a two-team "spec writer / implementer" process, or deriving them from public-domain files, docs and black-box testing.
- **ZFS.** Implementing from the published ZFS On-Disk Specification and black-box testing is the clean path, consistent with SAS v WPL on data-file formats. Interoperability-driven decompilation (Art. 6) does not apply to source that is already available. It also does not permit using copied code as the product.
- **Patents.**
  - US 8,683,296 (StreamScale, SIMD Galois-field multiplication) is the known risk for any fast GF(2^8) Reed-Solomon. It should be checked: claims scope and expiry or term, which this run did not verify. Using an existing permissive library such as ISA-L, whose licensor is Intel, may lower but does not remove that risk. Copyright licences like BSD-3 grant no patent licence. Apache-2.0 does include one.
  - I found no sourced patent on CRUSH itself. LRC and Clay patent status was not found, so freedom-to-operate checks are needed.

### Gaps
- No search results on whether Weil, UC Santa Cruz or Inktank/Red Hat hold patents on CRUSH.
- No sourced information on Microsoft LRC patents or on Clay code (Vajha et al., FAST'18) patents.
- The current status (expiry, litigation) of US 8,683,296 was not checked.
- I did not confirm the ISA-L licence text or whether Intel grants any patent licence.

## 5. Existing Rust projects in these areas and how they handled licensing

### Takeaway
Known Rust CRUSH code is extracted from Ceph and stays LGPL-2.1. Rust Ceph bindings that only link to librados use permissive or MPL licences for their own code and note the LGPL dependency. Rust ZFS readers are a mix: some are dual GPL-2.0/MIT, and at least one from-scratch reader is Apache-2.0. Nobody we found claims a permissive licence on a translated Ceph or ZFS source.

### Cited Findings
- `libcrush` / `crush-sys` (crates.io, v1.0.1, 2025-10): CRUSH sources extracted from Ceph in January 2017, under LGPL-2.1. — [docs.rs libcrush](https://docs.rs/crate/libcrush/latest/source/README.md); [crush-sys](https://rust-digger.code-maven.com/crates/crush-sys)
- `crushtool` crate: decompiles Ceph crushmaps. It is not a placement implementation. — [docs.rs crushtool](https://docs.rs/crushtool)
- `librados` (datdenkikniet, async Rust bindings): MIT for its own code. It notes it links librados, which is LGPLv2.1, and that this places requirements on compiled software. — [docs.rs librados 0.2.0](https://docs.rs/crate/librados/0.2.0/source/README.md)
- `rad` (rad-rs, built on ceph-rust's raw bindings): MPL-2.0. Last push June 2021. — [ecosyste.ms rad-rs](https://awesome.ecosyste.ms/projects/github.com%2Fsdleffler%2Frad-rs)
- `rzfs` / `rzfs_lib` (cybojanek): ZFS data-structure parsing library, licensed "GPL 2.0 or later or MIT, at your option". — [rzfs_lib](https://rust-digger.code-maven.com/crates/rzfs_lib)
- `lamzfs`: `no_std` read-only ZFS reader for UEFI. It vendors rzfs decoders "electing the MIT half". Its README licence statements are inconsistent across versions (GPL-2.0 OR MIT versus MIT OR Apache-2.0). — [docs.rs lamzfs 0.1.1](https://docs.rs/crate/lamzfs/0.1.1/source/README.md)
- `zfs-forensic-core`: described as a from-scratch on-disk reader (labels, uberblocks, MOS, ZPL) under Apache-2.0 with `#![forbid(unsafe_code)]`. RAIDZ reconstruction is deferred. — [docs.rs zfs-forensic](https://docs.rs/crate/zfs-forensic/0.1.2)
- `razor-rs`: Rust OpenZFS bindings and tools. Its licence was not shown in the excerpt. — [GitHub razor-rs](https://github.com/razor-zfs/razor-rs)

### Inferences
- The pattern matches the analysis above. Code lifted from Ceph keeps LGPL. Bindings can be permissive because they only link (though LGPL applies to the linked library, which the policy may still ban as a runtime dependency). Independent ZFS readers built from the format can be permissive.
- These crates' clean-room process was not documented in what we read. Their permissive licences are self-declared, not an audit result. The project should not vendor them without checking provenance.

### Gaps
- I could not confirm the licence of the `ceph-rust` crate itself.
- No permissively licensed, independent Rust CRUSH implementation was found.
- I did not verify whether `zfs-forensic-core` documents a clean-room process.
