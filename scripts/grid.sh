#!/usr/bin/env bash
# Runs the real binaries as separate processes, the way people use them: an operator starts a
# control plane and makes a pool; owners join with `globalobject setup` and run the agent;
# then back up, kill a host, repair, and restore. Needs MONGODB_URI pointing at a replica set
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
GLOBALOBJECT_OPERATOR_TOKEN="$(head -c 16 /dev/urandom | od -An -tx1 | tr -d ' \n')"
export GLOBALOBJECT_OPERATOR_TOKEN
export GLOBALOBJECT_SERVER="$CP"
PIDS=()
AGENT_PIDS=()
# On failure the logs stay, for a look at cp.log and agent*.log.
cleanup() {
  rc=$?
  kill "${PIDS[@]}" "${AGENT_PIDS[@]}" 2>/dev/null || true
  if [ "$rc" = 0 ]; then rm -rf "$WORK"; else echo "logs kept in $WORK" >&2; fi
}
trap cleanup EXIT

# Each owner is a separate "machine": its own config, credentials, state and slice.
as_owner() { local i=$1; shift; GLOBALOBJECT_HOME="$WORK/owner$i" "$BIN/globalobject" "$@"; }
admin() { "$BIN/globalobject-control-plane" admin "$@"; }

echo "--- operator: start the control plane"
# A grace period shorter than production's 3 days, so the demo shows repair in about two minutes.
"$BIN/globalobject-control-plane" --listen "127.0.0.1:$PORT" --key-file "$WORK/cp.key" --grace-secs 40 --local >"$WORK/cp.log" 2>&1 &
PIDS+=($!)
for _ in $(seq 100); do curl -fs -o /dev/null --noproxy '*' "$CP/readyz" && break; sleep 0.2; done
curl -fsS --noproxy '*' "$CP/readyz"
LINK=$(admin create-pool demo | tee -a /dev/stderr | grep -o 'http[^ ]*/join/[0-9a-f]*')

echo "--- owners: join with setup, then run the agent"
for i in $(seq 0 $((OWNERS - 1))); do
  mkdir -p "$WORK/files$i"
  if [ "$i" != 0 ]; then LINK=$(as_owner 0 invite); fi
  as_owner "$i" setup --join "$LINK" --yes --no-service --local --slice 34GB --path "$WORK/files$i" --every-hours 0 --dashboard-port 0 >"$WORK/setup$i.log"
  # Started directly, not through as_owner, so $! is the agent itself and kill -9 hits it.
  GLOBALOBJECT_HOME="$WORK/owner$i" "$BIN/globalobject" agent >"$WORK/agent$i.log" 2>&1 &
  AGENT_PIDS+=($!)
done
for i in $(seq 0 $((OWNERS - 1))); do
  for _ in $(seq 100); do [ -f "$WORK/owner$i/data/state/node" ] && break; sleep 0.2; done
done
sleep 2
admin status

head -c 8000000 /dev/urandom > "$WORK/files0/original"
echo "canary-this-must-not-reach-any-disk" >> "$WORK/files0/original"
echo "--- backup of the configured folder"
SNAP=$(as_owner 0 backup | tee -a /dev/stderr | awk '{print $2}' | tr -d ':')
as_owner 0 snapshots
if grep -rl "canary-this-must-not-reach-any-disk" "$WORK"/owner*/data/slice >/dev/null 2>&1; then
  echo "FAIL: plaintext found in a slice"; exit 1
fi
echo "ok: no plaintext in any slice"

echo "--- the dashboard answers with the token, and only with it"
DASH=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["dashboard"])' "$WORK/owner0/data/state/agent.json")
TOKEN=$(sed -n 's/^dashboard_token = "\(.*\)"/\1/p' "$WORK/owner0/config/credentials")
code=$(curl -s -o /dev/null -w '%{http_code}' --noproxy '*' "$DASH/api/status")
[ "$code" = 401 ] || { echo "FAIL: dashboard answered $code without a token"; exit 1; }
curl -fsS --noproxy '*' -H "authorization: Bearer $TOKEN" "$DASH/api/status" | python3 -c 'import json,sys; s=json.load(sys.stdin); print("ok: dashboard says account", s["account"]["account"], s["account"]["state"])'

mkdir -p "$WORK/large"
head -c "${LARGE_MB}000000" /dev/urandom > "$WORK/large/large.bin"
echo "--- $LARGE_MB MB, for throughput on this machine"
LARGE=$(as_owner 0 backup "$WORK/large" | tee -a /dev/stderr | awk '{print $2}' | tr -d ':')
time as_owner 0 restore "$LARGE" "$WORK/large.out"
cmp "$WORK/large/large.bin" "$WORK/large.out$WORK/large/large.bin" && echo "ok: large file identical"

echo "--- killing an owner's agent (and so its slice), then reading"
kill -9 "${AGENT_PIDS[1]}"
sleep 1
if kill -0 "${AGENT_PIDS[1]}" 2>/dev/null; then echo "FAIL: the agent survived kill -9"; exit 1; fi
HELD=$(curl -fsS --noproxy '*' -H "x-globalobject-operator: $GLOBALOBJECT_OPERATOR_TOKEN" "$CP/v1/admin/nodes" |
  python3 -c 'import json,sys; print(sum(n["held"] for n in json.load(sys.stdin) if n["account"] == 2))')
echo "the killed owner's node held $HELD bytes"
[ "$HELD" -gt 0 ] || { echo "FAIL: the killed node held nothing, so repair can't be shown"; exit 1; }
as_owner 0 restore "$SNAP" "$WORK/back1"
cmp "$WORK/files0/original" "$WORK/back1$WORK/files0/original" && echo "ok: identical after losing a host"
as_owner 0 status | tee "$WORK/status.txt"
grep -q "Last backup" "$WORK/status.txt" || { echo "FAIL: status doesn't show the last backup"; exit 1; }

echo "--- waiting for repair (grace is 40 seconds, and rounds run every 30)"
# Reading alone proves little, since other copies survive. Check the pieces really moved.
if command -v mongosh >/dev/null; then
  left=unknown
  # Pieces still recorded on the killed node, which belongs to account 2. (MongoDB shell
  # code, so no shell expansion is wanted.)
  # shellcheck disable=SC2016
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
  sleep 90
  echo "note: mongosh not found, so repair itself wasn't checked"
fi
as_owner 0 restore "$SNAP" "$WORK/back2"
cmp "$WORK/files0/original" "$WORK/back2$WORK/files0/original" && echo "ok: identical after repair"
admin status
curl -fsS --noproxy '*' "$CP/metrics" | grep -E '^globalobject_(nodes_online|segments_below_repair|repairs_completed_total) '
echo "grid demo passed"
