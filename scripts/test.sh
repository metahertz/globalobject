#!/usr/bin/env bash
# Runs the workspace's tests, with each end-to-end test in its own process, so one test's memory
# or crash can't affect another. A grid test hosts all its nodes in one process, and peaks near
# 1 GiB, so this also suits small machines.
#
# Needs MongoDB as a replica set (one node is enough, since transactions need one). MONGODB_URI
# defaults to a local one. Each test creates its own database, named t_<random>. They're dropped
# before the run and after each end-to-end test: each holds about 30 collections and indexes, one
# file each, and a few dozen left behind exhaust mongod's default limit of 1024 open files.
set -uo pipefail
export MONGODB_URI="${MONGODB_URI:-mongodb://127.0.0.1:27017/?replicaSet=rs0&directConnection=true}"
drop_test_dbs() {
  command -v mongosh >/dev/null || return 0
  mongosh --quiet "$MONGODB_URI" --eval \
    'db.adminCommand({listDatabases: 1, nameOnly: true}).databases.filter(d => /^t_[0-9a-f]{16}$/.test(d.name)).forEach(d => db.getSiblingDB(d.name).dropDatabase())'
}
drop_test_dbs || { echo "can't reach MongoDB at $MONGODB_URI"; exit 1; }

cargo test --workspace --exclude globalobject-e2e || exit 1
BIN=$(cargo test -p globalobject-e2e --test grid --no-run --message-format=json 2>/dev/null |
  python3 -c 'import json,sys
for line in sys.stdin:
    m=json.loads(line)
    if m.get("executable") and m.get("target",{}).get("name")=="grid": print(m["executable"])')
[ -n "$BIN" ] || { echo "couldn't build the tests"; exit 1; }

pass=0; failed=()
for t in $("$BIN" --list --format=terse | sed -n 's/: test$//p'); do
  if out=$("$BIN" --exact "$t" --test-threads=1 2>&1); then
    pass=$((pass + 1)); echo "ok    $t"
  else
    rc=$?
    failed+=("$t"); echo "FAIL  $t (exit $rc)"
    # Full output of failures, for flaky ones. Set RUST_LOG too, to get the logs in it.
    [ -n "${FAIL_LOG_DIR:-}" ] && mkdir -p "$FAIL_LOG_DIR" && echo "$out" > "$FAIL_LOG_DIR/$t.log"
    echo "$out" | grep -E 'panicked|assert|left:|right:' -A1 | cut -c1-300 | head -12
  fi
  drop_test_dbs >/dev/null || echo "can't drop test databases"
done
echo "end-to-end: $pass passed, ${#failed[@]} failed"
[ "${#failed[@]}" = 0 ]
