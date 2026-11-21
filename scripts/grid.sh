#!/usr/bin/env bash
# Runs the real binaries as separate processes: a control plane, several owners with nodes,
# then upload, kill a host, repair, and download. Needs MONGODB_URI pointing at a replica set
# (one node is enough), MONGODB_DATABASE naming a database that's empty or absent (default
# globalobject_grid), and binaries built with `cargo build --release`.
set -euo pipefail

BIN="${BIN:-target/release}"
export MONGODB_URI="${MONGODB_URI:-mongodb://127.0.0.1:27017/?replicaSet=rs0&directConnection=true}"
export MONGODB_DATABASE="${MONGODB_DATABASE:-globalobject_grid}"
OWNERS="${OWNERS:-5}"
LARGE_MB="${LARGE_MB:-300}"
WORK="$(mktemp -d)"
PORT="${PORT:-7400}"
CP="http://127.0.0.1:$PORT"
export GLOBALOBJECT_OPERATOR_TOKEN="$(head -c 16 /dev/urandom | od -An -tx1 | tr -d ' \n')"
export GLOBALOBJECT_CONTROL_PLANE="$CP"
PIDS=()
NODE_PIDS=()
cleanup() { kill "${PIDS[@]}" "${NODE_PIDS[@]}" 2>/dev/null || true; rm -rf "$WORK"; }
trap cleanup EXIT

# Short grace, so the demo shows repair in about a minute.
"$BIN/globalobject-control-plane" --listen "127.0.0.1:$PORT" --key-file "$WORK/cp.key" --grace-secs 10 --local >"$WORK/cp.log" 2>&1 &
PIDS+=($!)
for _ in $(seq 50); do curl -s -o /dev/null --noproxy '*' "$CP/v1/accounts/me" && break; sleep 0.2; done

POOL=$(curl -sS --noproxy '*' -X POST "$CP/v1/pools" -H "x-globalobject-operator: $GLOBALOBJECT_OPERATOR_TOKEN" -H 'content-type: application/json' -d '{
  "name": "demo", "private": true, "distinct_sites": false,
  "policy": {"k":1,"n":3,"repair_at":3,"upload_extra":0,"min_failure_domains":3,"domain_level":"owner"},
  "quota": {"reserve":0.2,"headroom":1.5,"probation_allowance":34359738368}}')
INVITE=$(python3 -c 'import json,sys; print(json.loads(sys.argv[1])["admin_invite"])' "$POOL")

cli_as() { local i=$1; shift; GLOBALOBJECT_LOCAL=true GLOBALOBJECT_PHRASE="demo phrase $i" GLOBALOBJECT_STATE="$WORK/owner$i" "$BIN/globalobject" "$@"; }

for i in $(seq 0 $((OWNERS - 1))); do
  if [ "$i" = 0 ]; then inv="$INVITE"; else inv=$(cli_as 0 invite); fi
  cli_as "$i" register "$inv"
  read -r id seed < <(cli_as "$i" account)
  "$BIN/globalobject-node" --account "$id" --account-seed "$seed" --store "$WORK/node$i" --pledged 34359738368 --site "site$i" --asn "asn$i" --heartbeat-secs 1 --local >"$WORK/node$i.log" 2>&1 &
  NODE_PIDS+=($!)
done
sleep 2

mkdir -p "$WORK/src"
head -c 8000000 /dev/urandom > "$WORK/src/original"
echo "canary-this-must-not-reach-any-disk" >> "$WORK/src/original"
echo "--- backup"
SNAP=$(cli_as 0 backup "$WORK/src" | tee /dev/stderr | awk '{print $2}' | tr -d ':')
cli_as 0 snapshots
if grep -rl "canary-this-must-not-reach-any-disk" "$WORK"/node* >/dev/null 2>&1; then
  echo "FAIL: plaintext found on a node's disk"; exit 1
fi
echo "ok: no plaintext on any node's disk"

mkdir -p "$WORK/large"
head -c "${LARGE_MB}000000" /dev/urandom > "$WORK/large/large.bin"
echo "--- $LARGE_MB MB, for throughput on this machine"
LARGE=$(cli_as 0 backup "$WORK/large" | tee /dev/stderr | awk '{print $2}' | tr -d ':')
time cli_as 0 restore "$LARGE" "$WORK/large.out"
cmp "$WORK/large/large.bin" "$WORK/large.out$WORK/large/large.bin" && echo "ok: large file identical"

echo "--- killing a node process, then reading"
kill -9 "${NODE_PIDS[1]}"
cli_as 0 restore "$SNAP" "$WORK/back1"
cmp "$WORK/src/original" "$WORK/back1$WORK/src/original" && echo "ok: identical after losing a host"

echo "--- waiting for repair (grace is 10 seconds, and rounds run every 30)"
cli_as 0 status | grep -E '"(state|used_raw|quota_raw)"'
# Reading alone proves little, since other copies survive. Check the pieces really moved.
if command -v mongosh >/dev/null; then
  left=unknown
  # Pieces still recorded on the killed node, which belongs to account 2.
  count='const n = db.nodes.distinct("_id", {account: 2});
    print(db.segments.aggregate([{$unwind: "$pieces"}, {$match: {"pieces.host": {$in: n}}}, {$count: "c"}]).toArray()[0]?.c ?? 0)'
  for _ in $(seq 60); do
    left=$(mongosh --quiet "$MONGODB_URI" --eval "db = db.getSiblingDB('$MONGODB_DATABASE'); $count")
    [ "$left" = 0 ] && break
    sleep 5
  done
  if [ "$left" != 0 ]; then echo "FAIL: $left pieces still recorded on the killed node"; exit 1; fi
  echo "ok: no pieces left on the killed node"
else
  sleep 45
  echo "note: mongosh not found, so repair itself wasn't checked"
fi
cli_as 0 restore "$SNAP" "$WORK/back2"
cmp "$WORK/src/original" "$WORK/back2$WORK/src/original" && echo "ok: identical after repair"
echo "grid demo passed"
