# GlobalObject

Reciprocal, end-to-end encrypted cloud storage. You contribute a slice of your own disk and get redundant storage on
other people's disks in return. Your data is encrypted before it leaves your machine, your space is capped by what you
contribute, and your download speed by what your link contributes.

**Status: prototype. Not safe for real data.** Tested on loopback only. See [docs/status.md](docs/status.md).

The storage, crypto and transport are existing open-source projects: restic's repository format through `rustic_core`,
`iroh` and `iroh-blobs` for peers, and PASETO tokens through `pasetors`. See [docs/dependencies.md](docs/dependencies.md).

## Join a pool

Someone running a pool sends you a join link. Opening it shows one command for Linux or macOS:

```sh
curl -fsSL https://cp.example.org/install.sh | sh -s -- --join https://cp.example.org/join/<invite>
```

It installs `globalobject`, then asks how much disk to share and which folders to back up, gives you a 12-word
recovery phrase, and starts a background service. After that:

```sh
globalobject status       # is everything healthy?
globalobject dashboard    # the same, in your browser, plus restores and settings
globalobject backup       # back up now, rather than waiting for the schedule
```

Coming back on a new machine: `globalobject setup --restore --server https://cp.example.org`, then type your phrase.
See [docs/user-guide.md](docs/user-guide.md).

## Run a pool

On a server with Docker and a DNS name pointing at it:

```sh
git clone <this repository> && cd globalobject
./deploy/install-server.sh --domain cp.example.org --monitoring
```

That starts MongoDB, the control plane, HTTPS through Caddy, nightly database backups and Prometheus with alerts, then
creates a first pool and prints its join link. Manage it from the dashboard at `https://cp.example.org/admin` or with
`globalobject-control-plane admin` (`status`, `pools`, `create-pool`, `invite`, `nodes`, `remove-member`). See
[docs/operations.md](docs/operations.md).

## Documents

| | |
|---|---|
| [docs/user-guide.md](docs/user-guide.md) | Joining, the dashboard, restoring, changing what you share |
| [docs/operations.md](docs/operations.md) | Running a control plane: deploy, upgrade, back up, monitor, alerts |
| [docs/status.md](docs/status.md) | What's built, what was verified, and what isn't done |
| [docs/architecture.md](docs/architecture.md) | How it works, and why |
| [docs/threat-model.md](docs/threat-model.md) | Who could attack it, and what remains exposed |
| [docs/dependencies.md](docs/dependencies.md) | Which project does what, licences, and what remains custom |
| [docs/protocol.md](docs/protocol.md) | Grants and APIs |
| [docs/sim-report.md](docs/sim-report.md) | Durability, availability and quota simulations |

## Layout

| Crate | Purpose |
|---|---|
| `crates/crypto` | Signing identities and PASETO tokens |
| `crates/segment` | `(k,n)` coding of encrypted data |
| `crates/policy` | Redundancy, placement, quota and bandwidth rules, shared with the simulator |
| `crates/proto` | Wire types |
| `crates/net` | Peer connections and verified transfers |
| `crates/client` | The `globalobject` CLI: a restic-format repository stored on the grid |
| `crates/node` | The `globalobject-node` storage node |
| `crates/control-plane` | The `globalobject-control-plane` service |
| `crates/sim` | The simulator |
| `crates/e2e` | End-to-end tests |

## Build and test from source

You need Rust and MongoDB running as a replica set, because the control plane uses transactions. One node is enough:

```sh
mongod --replSet rs0 --dbpath ./mongo-data --fork --logpath ./mongod.log
mongosh --eval 'rs.initiate({_id: "rs0", members: [{_id: 0, host: "127.0.0.1:27017"}]})'
export MONGODB_URI='mongodb://127.0.0.1:27017/?replicaSet=rs0&directConnection=true'   # the scripts' default

scripts/test.sh                      # each end-to-end test in its own process
python3 scripts/check-licences.py    # AGPL and GPL stay out

cargo run --release -p globalobject-sim > report.md    # full simulation

cargo build --release -p globalobject-control-plane -p globalobject-node -p globalobject-client
scripts/grid.sh    # real processes: an operator, five owners joining with setup, backup, kill -9, repair, restore
LARGE_MB=2000 scripts/grid.sh    # the same, with a 2 GB file
```

Releases come from pushing a `v*` tag: `.github/workflows/release.yml` builds Linux and macOS binaries, publishes them
with checksums, and pushes the control-plane image to GitHub's container registry.

## Licence

MIT or Apache-2.0. Dependencies are limited to permissive licences, plus MPL-2.0 for unmodified files. See
[docs/dependencies.md](docs/dependencies.md).
