# Running a GlobalObject control plane

The control plane holds pools, accounts, node records, placement and the ledger. It never holds keys or file
contents, and no data passes through it. If it goes down, no new backups or restores start, but stored data stays
safe. If it **loses its database**, every pool loses its metadata. So the database backups below matter more than
anything else here.

## Deploying

You need:

- a Linux host with Docker and Compose v2
- a DNS name pointing at the host
- ports 80 and 443 open

Then:

```sh
git clone <repository> && cd globalobject
./deploy/install-server.sh --domain cp.example.org --monitoring
```

Without a checkout, you can run the script directly. Set `GLOBALOBJECT_REPO=owner/name`, and it fetches the Compose
files and uses the published image:

```sh
curl -fsSL https://raw.githubusercontent.com/<owner>/<name>/main/deploy/install-server.sh | GLOBALOBJECT_REPO=<owner>/<name> sh -s -- --domain cp.example.org
```

The script:

1. writes `deploy/compose/.env`, which holds the operator token
2. starts the services
3. waits for `/readyz`
4. creates a first pool, named "Main", and prints its join link

The pool is private with 3 copies. Its first member becomes its admin.

| Service | Role |
|---|---|
| `mongo` | MongoDB 8 as a single-node replica set. No published port. |
| `control-plane` | The API, on `127.0.0.1:7400`. Caddy fronts it for everyone else. |
| `caddy` | HTTPS with automatic certificates (profile `tls`). Blocks `/metrics` from outside. |
| `mongo-backup` | A `mongodump` every 24 hours into the `mongobackup` volume, keeping 7. |
| `prometheus` | Scrapes `/metrics` and evaluates `alerts.yml`, on `127.0.0.1:9090` (profile `monitoring`). |

`--no-tls` skips Caddy and serves plain HTTP on port 7400. Use it only on a LAN or for testing: request signatures
protect integrity, but nothing on the wire is encrypted.

**Upgrading:** run `install-server.sh` again. It keeps `.env` and all the data volumes.

## Managing

```sh
cd deploy/compose
alias goadmin='docker compose exec control-plane globalobject-control-plane admin'
goadmin status                    # health at a glance
goadmin pools
goadmin create-pool "Family"      # prints the join link for its first member, who becomes admin
goadmin create-pool "Club" --public --erasure
goadmin invite 1                  # a one-use join link for pool 1
goadmin nodes --pool 1
goadmin remove-member 1 42        # read-only at once; its nodes drain first
```

`admin` talks to the API, so it also works from another machine. Set `GLOBALOBJECT_SERVER=https://cp.example.org`
and `GLOBALOBJECT_OPERATOR_TOKEN`.

The operator dashboard at `https://cp.example.org/admin` shows the same information. It also lets you create pools
and invite links. Sign in with the operator token; it stays in that browser tab only.

**Rotating the operator token:**

1. Edit `GLOBALOBJECT_OPERATOR_TOKEN` in `.env`.
2. Run `docker compose up -d control-plane`.

Users aren't affected, because they authenticate with their own keys.

**The grant-signing key** is `/data/control-plane.key` in the `cpdata` volume. Back it up with the database. Nodes and
clients learn its public half when they register, so a new key would break grant checks until they re-register. That
recovery path hasn't been tested.

## Backups of the database

`mongo-backup` writes `globalobject-<time>.archive.gz` files to the `mongobackup` volume. **Copy them off the host.**
For example, run this nightly from cron:

```sh
docker run --rm -v globalobject_mongobackup:/b -v /srv/offsite:/o alpine sh -c 'cp -n /b/*.gz /o/'
```

To restore:

```sh
docker compose stop control-plane
docker compose exec -T mongo mongorestore --drop --gzip --archive < globalobject-<time>.archive.gz
docker compose start control-plane
```

Anything that changed after that dump is lost. Hosts holding pieces committed after the dump keep them as orphans,
which their garbage collection removes.

## Monitoring

| Endpoint | Meaning |
|---|---|
| `/healthz` | The process is up |
| `/readyz` | MongoDB answers and has a writable primary |
| `/metrics` | Prometheus format, reachable only inside the Compose network or on `127.0.0.1:7400` |

Main series:

| Series | Meaning |
|---|---|
| `globalobject_nodes_online` | Active nodes seen within the grace period |
| `globalobject_segments_unreadable` | Segments below k healthy pieces: readable only when hosts return |
| `globalobject_segments_below_repair` | Segments due for repair |
| `globalobject_segments_degraded` | Segments below n healthy pieces |
| `globalobject_repairs_in_flight`, `globalobject_repairs_completed_total` | Repair activity |
| `globalobject_audits_total{result}` | Audit passes and failures |
| `globalobject_stored_bytes`, `globalobject_pledged_bytes` | Space |
| `globalobject_http_requests_total{route,method,status}`, `globalobject_http_request_seconds` | API traffic and latency |
| `globalobject_transaction_retries_total` | Database write conflicts retried (uploads by one account serialise this way) |
| `globalobject_summary_up` | 0 when the summary query behind these gauges is failing |

The gauges are refreshed every 30 seconds, so they can trail `admin status`, which is computed when asked.

Grafana isn't bundled, because it's AGPL. If you already run it, add Prometheus as a data source.

## Alerts

Alerts are defined in `deploy/compose/alerts.yml`. Add an Alertmanager to `prometheus.yml` to be notified.

| Alert | What to do |
|---|---|
| **ControlPlaneDown** | `docker compose ps` and `docker compose logs control-plane`. Users can't back up or restore until it's back. |
| **DatabaseSummaryFailing** | Check `docker compose logs mongo` and `curl 127.0.0.1:7400/readyz`. Check disk space on the host. |
| **SegmentsUnreadable** | Some data can't be read right now. Usually many hosts are offline at once, for example a regional outage. Check `goadmin nodes`. It clears as hosts return. If hosts are gone for good, repair can't rebuild below k. |
| **RepairBacklog** / **RepairNotKeepingUp** | Repair is slower than losses. Check that enough nodes are online with free space (`goadmin nodes`). A pool near full has nowhere to repair into. |
| **NodesDropped** | More than 20% of nodes went offline within an hour. Often the network, or a release that crashes nodes. The 72-hour grace period means no repair storm yet. |
| **AuditFailures** | A host returned wrong data for an audit. Repeated failures disqualify it automatically, and its pieces are repaired elsewhere. Look for a pattern with `goadmin nodes`. |
