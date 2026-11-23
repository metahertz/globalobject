# Status

Prototype. **Don't store real data on it.** It has been tested on loopback only, and has had no independent review.

Storage, crypto and transport are now existing open-source projects, rather than custom code. See `dependencies.md`.

## Plan phases

| Phase | State |
|---|---|
| 1. Design docs | Done, and updated for the migration |
| 2. Simulator | Done. See `sim-report.md`, including its limits |
| 3. Prototype | Done for its scope, and migrated onto `rustic_core`, `iroh`, `iroh-blobs` and `pasetors` |

## Install, setup and operations (2026-11-23)

The aim was one command for a user to join, one for an operator to run a server, and an obvious way for each to see
whether things are healthy. The user guide is in `user-guide.md` and the operator guide in `operations.md`.

**Users**
- `install.sh` downloads a release, checks its SHA-256, and runs `globalobject setup`.
- `setup` takes a join link and handles:
  - registering the account
  - a 12-word BIP39 phrase, which it asks you to confirm
  - the slice size (a quarter of free disk by default)
  - which folders to back up, and how often
  - installing the background service (a systemd user unit or a macOS LaunchAgent)
- `globalobject agent` runs the storage slice in-process, scheduled backups with retention, and a dashboard on
  127.0.0.1.
  - The dashboard needs a token cookie, accepts only a local Host header, and requires a custom header on POST.
- There's a readable `status` command, and `slice`, `paths` and `service` commands.
- `setup --restore` finds the account from the phrase alone, through a new `GET /v1/accounts/whoami` signed with a
  `k<public key>` id.
- Settings live in `config.toml`. The phrase lives in a `credentials` file readable only by its owner, which
  unattended backups need.

**Operators**
- `deploy/install-server.sh` starts the Compose stack and creates the first pool. The stack is:
  - MongoDB
  - the control plane
  - Caddy, for HTTPS
  - nightly `mongodump`
  - Prometheus with alert rules (optional)
- `globalobject-control-plane admin` covers `status`, `pools`, `create-pool`, `invite`, `nodes` and
  `remove-member`.
- The control plane serves:
  - `/healthz`, `/readyz` and `/metrics`
  - an operator dashboard at `/admin`
  - join pages at `/join/<invite>`
  - `/install.sh`
- Operators can invite to, and remove members from, any pool.
- Releases come from `.github/workflows/release.yml`.

**Behaviour that changed**
- A node registering with no site gets one derived from its address: the IPv4 /24 or IPv6 /48. `X-Forwarded-For`
  counts only from `--trusted-proxy` addresses. Users no longer choose their own failure domain.
- `globalobject-node` takes sizes like `500GB`. Its `--site` and `--asn` flags are now optional.

**Not done**
- The standalone node has no config file. Environment variables cover it.
- There's no OS keyring for the phrase.
- There's no self-update command; re-running `install.sh` upgrades.
- Windows isn't supported.

**Verified** in the sandbox container on 2026-11-23:
- clippy (with `-D warnings`), fmt and shellcheck are clean, and the licence check passes.
- All 46 end-to-end tests pass, including two new ones:
  - `u01` covers the whole user path: join link, `setup`, the agent's slice, a backup and a restore from the
    dashboard, settings, and recovering the account from the phrase alone. It also checks the dashboard's token,
    Host and CSRF rules.
  - `u02` covers the operator side: health, operator authentication, summary counts, invites to a private pool,
    metrics and member removal.
- `scripts/grid.sh` runs the real binaries the new way, with five owners joining through `setup` and running
  `agent`. With a 300 MB file it passes: the killed owner's node held 308 MB, and repair moved its 16 pieces
  elsewhere.
- That demo caught two problems in this work, both fixed:
  - The script killed a shell wrapper rather than the agent, so nothing was really lost.
  - Backups run from the command line didn't show in `status`.

**Not run here**
- macOS: launchd, and the macOS release builds.
- systemd user units. The container has no systemd user session, so the unit text is checked only by a golden test,
  and `setup` was tested with `--no-service`.
- Docker Compose, Caddy, Prometheus and its alert rules.
- The release workflow, and publishing the image.
- `install.sh` was tested against a local directory laid out like a release. That covers download, checksum and
  install, and it refuses a tampered archive. It was not tested against GitHub. It needs a repository: there's no
  remote yet.

## Partial reads, reclaim and local parity (2026-11-08)

Built from the Ceph and OpenZFS research, as designed in `plans/reads-reclaim-repair.md`:

| PR | Change | Evidence |
|---|---|---|
| 1 | **Security fix.** Hosts serve only single-blob requests. Before this, one read grant for a crafted piece fetched other accounts' pieces through a hash-sequence request | `v04_hash_sequence_requests_cannot_reach_other_blobs`. Mutation check: with the fix disabled, the test fetched all 18 of another account's pieces on that host (3.15 MB) with one grant |
| 2 | Range grants and partial reads. A small read from a large pack fetches and is charged for about that range. Degraded reads fall back to a full decode | `v04_range_grants_limit_what_is_served`, `t_partial_read_*`. Reading a 20 KB file from a 24 MiB pack charged 37–39 KB under both policies |
| 3 | `globalobject forget` and `globalobject prune`, using `rustic_core`'s own retention and prune | `t_forget_and_prune_reclaim_space`: 2 packs (8.4 MB) deleted, quota back at once, hosts freed the pieces, the newer snapshot restores |
| 4 | Optional local parity (pyramid codes), with repair from the group | `t_local_parity_repairs_from_the_group`: 6 lost pieces rebuilt by fetching 2× their size, against 4× for plain RS. Readable after `r + 1` losses. The unit test checks every loss pattern on four layouts |

After these changes (2026-11-08): `scripts/test.sh` ran twice, giving 27 unit tests passing both times and 44 end-to-end tests
passing 44/44 once and 43/44 once. fmt, clippy and the licence check (638 packages) are clean. `scripts/grid.sh` passes at 300 MB with
real processes.

**One unexplained failure.** `t_false_pledge_surfaces_and_loses_standing` failed once in the full suite, with two audit
failures on an honest host. It then passed 18 times on its own and once more in a full suite. The audit path now logs why an
audit fails, and `scripts/test.sh` saves a failing test's full output when `FAIL_LOG_DIR` is set. The cause isn't known, so treat
it as open: false audit failures lead to disqualification.

Nothing was needed upstream to build these. Two upstream contributions to `iroh-blobs` are drafted in `upstream/` but not sent.
Designed but not built: a delete grace window (PR 5), which matters now that `forget` and `prune` exist.

## Control plane on MongoDB (2026-11-08)

The control plane moved from Postgres to MongoDB, at the user's request, using the MongoDB plugin's schema-design and
connection skills. The fifteen tables became the collections described in `architecture.md`. The `sqlx` dependency is
gone. MongoDB's server licence (SSPL) was accepted for running unmodified as a backend: see `dependencies.md`.

Behaviour that changed on the way:

- Crediting read receipts now raises a grant's credited total with one conditional update, so two receipts for the
  same grant arriving together can't both credit the same bytes. Before, that was a read followed by a write.
- The read token bucket is a compare-and-set on the account document, retried, instead of a row lock.
- Placement refusals from an unknown host id are ignored, where the foreign key used to turn them into a 500.
- Uploads by one account are serialised by a deliberate write conflict on the account document, in place of a row
  lock. The losing transaction retries after a random, growing delay, up to 10 attempts, and then returns an error.
  Many parallel uploads from one account could still exhaust that. A row lock would have queued them instead.

Tested on MongoDB 8.0.34 as a single-node replica set, in the sandbox container on 2026-11-08: unit tests and all 44
end-to-end tests pass. Each test gets its own database, which `scripts/test.sh` drops afterwards: each holds about 30
files, and mongod's default limit of 1024 open files ran out partway through the first attempt. The MongoDB setup in
CI and Compose hasn't been run.

Not done: schema validation (`$jsonSchema`) on the collections. All writes go through typed Rust structs today; add
validators before anything else writes to the database. The MongoDB MCP server from the plugin failed to start here
(`npx` isn't installed), so no live schema or index checks were run through it.

## Verification

Run in the project's sandbox container on 2026-11-06, after the last functional change. CI is defined in `.github/workflows/ci.yml`
but has never run.

| Check | Result |
|---|---|
| `scripts/test.sh` | 38 end-to-end tests and 21 unit tests. **5 consecutive clean runs**, no out-of-memory kills |
| `scripts/check-licences.py` | 638 packages, none disallowed |
| `cargo fmt --check`, `clippy -D warnings` | Clean |
| `scripts/grid.sh`, 300 MB | Pass, with real processes: `kill -9` a node, restore, confirm repair moved its pieces, restore again |
| `scripts/grid.sh`, 2 GB | Pass, with the same steps |

| # | Plan item | Result | Test |
|---|---|---|---|
| 1 | No plaintext on any disk or the control plane | Pass, under both policies | `v01_*` |
| 2 | Files from 0 bytes to multi-GB round trip | Pass, up to 96 MiB under both policies, and 2 GB under 1-of-3 only | `v02_*`, grid |
| 3 | One owner's disks count once; too few domains refuses writes | Pass | `v03_*` |
| 4 | Patched clients and bad grants are refused | Pass | `v04_*` |
| 5 | Download follows measured upload, and credit is zero-sum | Pass, using application-level shaping | `v05_*` |
| 6 | Survives `n-k` lost hosts; repair with owner absent and no keys | Pass | `v06_*` |
| 7 | Short outages trigger no repair | Pass | `v07_*` |
| 8 | Cheating hosts are caught, leading to disqualification and repair | Pass, but the plan's "at about the predicted rate" was **not measured** | `v08_*` |
| 9 | Replica whitening | **Removed by design.** Tests now check that EC pieces differ, and replicas match | `v09_*` |
| 10 | Shrink drains first; leavers go through grace, read-only, deletion | Pass | `v10_*` |
| 11 | Pools are isolated | Pass | `v11_*` |

The crypto vectors from the first version are gone, along with the custom crypto they covered. Those libraries carry their own tests.

Measured on loopback with release builds, 1-of-3: a 300 MB restore took about 14 s, and 2 GB about 84 s. The client peaked at
about 450 to 530 MiB, and each node at about 100 MiB. These reflect software overhead on this machine, not real networks.

### Differences from the plan's verification

- **Loopback only.** `iroh`'s hole punching and relays haven't been exercised. Neither have real latency, packet loss or real
  constrained links. The container doesn't allow `tc`, so link limits use the node's own shaper.
- **No Docker.** `deploy/compose` has never been run.
- **Serial tests.** A grid test hosts all its nodes in one process, peaking near 1 GiB, and the container has a 2 GiB limit, so
  `scripts/test.sh` runs each in its own process.

### The earlier unexplained SIGKILL

Now explained: this container's cgroup has a 2 GiB limit, and the kills were out-of-memory kills. My earlier "2.8 GB free" reading came
from a host-wide figure, so was misleading.

## Bugs found by this verification

Worth knowing about, because they show which parts need the most scrutiny.

| Found by | Bug |
|---|---|
| Design review | Pool stalls at about 4× total allowances, however much is pledged. Fixed with `headroom` |
| Simulator check | Dead hosts' repair timers cancelled; events ran past the end of the simulation |
| Timing check | Bandwidth rule double-charged, halving every reader's rate. Fixed with up-front charges and debt |
| Timing check | Pacer let lone transfers through instantly; overlapping probes under-measured links |
| Real-process run | New members stuck at 1 Mbps until the first probe, with long-lasting effects on grants already issued |
| Repeat runs | Connection-pool deadlock under parallel uploads |
| Repeat runs | Repairs aimed at a host that had just died sat idle until the timeout |
| Repeat runs | **Data loss:** a retried upload reused its segment ID, so the abort's delayed deletion could destroy the retry's copy |
| Repeat runs | A host going offline mid-audit, or a piece moved mid-audit, counted as a failed proof |
| Migration | Client's blob store never released uploaded pieces. Reads were unbounded and rebuilt parity needlessly |
| Migration | Reference cycle stopped a node's store being freed |
| Test review | Consecutive-failure audit scoring let a host corrupt one piece indefinitely, as long as its others passed |
| Code review | Hosts buffered up to 128 MiB before checking a grant (the code in question has since been replaced) |
| Design review | **Hash-sequence requests bypassed host grants.** One grant for a crafted piece fetched other blobs held by the same host |

## Costs of the migration

Decisions made knowingly, recorded so they can be revisited:

- **Sybil replica dedupe is back at `k=1`.** See the threat model. EC pools aren't affected.
- **Pack sizes are visible** to the control plane and hosts, with size-class padding gone. Any restic backend sees the same.
- **Receipts are coarser than the first version's.** A withheld receipt costs a host up to one grant. Since 2026-11-08 that is a
  range for partial reads, but restores mostly use whole-piece grants.
- **Two pre-1.0 foundations.** `rustic_core` and `iroh-blobs` can change their APIs, and `iroh-blobs`' docs have warned about production
  use. Pin versions and expect upgrade work.
- **Restic's crypto** has an informal public review rather than a formal audit that I could point to.

## Designed but not built

- **Network:** TLS for the control-plane API (needs a proxy for now), and our own relays. Hole punching and relays are in `iroh`, untested here
- **Access:** `backup`, `snapshots`, `restore`, `forget` and `prune`. No delete grace window yet, so anyone with the phrase can
  delete everything at once. `scripts/grid.sh` doesn't exercise `forget` or `prune` yet
- **Local parity defaults:** pyramid codes are available per pool, but the simulator hasn't weighed them against plain RS
- **Automatic policy switching:** the simulator's rule (40 owners and 75% uptime) isn't implemented. Policy is set per pool, and
  changing it applies to new objects only
- **Upload over-provisioning:** "cancel the slowest" (`upload_extra` is ignored). A failing host causes a retry instead
- **Keys:** multiple device identities, and key rotation
- **Audit depth:** availability credit is all-or-nothing, not measured. Failure counts never decay, so long-lived hosts could eventually
  need a time window. Real-transfer receipts don't yet cross-check probes
- **Slander protection:** three colluding accounts can get a host disqualified
- **Identity:** self-declared site and ASN, and no stronger identity for open signup
- **Operator exit:** owner-signed recovery grants

## Decisions needing a person

1. **Legal advice before any public pool.** Hosts hold content they can't inspect. See the risks in `architecture.md`.
2. **Security review** of how the pieces are combined: grants, the host gate and the backend.
3. **Durability position.** The simulator can't support a durability claim. 1-of-3 showed about 0.045 lost segments per TB per
   year under an invented churn model. Decide whether 1-of-3 is the right default, or whether `1-of-4` is worth 200 GB per TB rather than 267.
4. **Demand.** Contributing 1 TB gives 267 to 400 GB, with days to upload and restore. Test with real users before going further.
