#!/usr/bin/env bash
# Variables in command strings are expanded by the child.
# shellcheck disable=SC2016
set -Eeuo pipefail
# shellcheck source=tests/e2e/assert.sh
source /tmp/stack-e2e-assert.sh
export PATH=/opt/stack:$PATH
alive() { ps -eo stat,comm --no-headers | awk '$1 !~ /Z/ && $2 ~ /^(postgres|redis-server)$/' | wc -l; }
cd ~/appB || exit 1
stack up --ttl 4s --json >/tmp/lease.json
assert_json '.ok and .data.session.lease.ttl_secs == 4' /tmp/lease.json
sleep 3
stack renew --json >/tmp/renew.json
assert_json '.ok' /tmp/renew.json
sleep 2
stack gc --json >/tmp/gc.json
assert_json '.ok and (.data | length == 0)' /tmp/gc.json
sleep 4
stack gc --json >/tmp/gc.json
assert_json '.ok and (.data | length == 1) and .data[0].stopped' /tmp/gc.json
[[ ! -e .stack/session.json && $(alive) -eq 0 ]] || fail 'expired services or record survived'

echo 'An active command protects its short TTL, including its real database'
stack up --ttl 2s >/dev/null
stack exec --require-all -- bash -c 'sleep 7; psql "$DATABASE_URL" -Atc "select 1"' >/tmp/active-exec.log &
EXEC_PID=$!
sleep 4
stack gc --json >/tmp/gc.json
assert_json '.ok and (.data | length == 0)' /tmp/gc.json
kill -0 "$EXEC_PID"
[[ -e .stack/session.json ]] || fail 'GC removed an active session'
wait "$EXEC_PID"
[[ $(cat /tmp/active-exec.log) == 1 ]] || fail 'active command lost its database'
sleep 3
stack gc --json >/tmp/gc.json
assert_json '.ok and (.data | length == 1) and .data[0].stopped' /tmp/gc.json

sleep 300 & RUNNER=$!
cd ~/appA || exit 1
stack up --owner-pid "$RUNNER" --json >/tmp/owner.json
assert_json '.ok and (.data.session.lease.owner_pid > 0)' /tmp/owner.json
stack exec --require-all -- true
kill "$RUNNER"
if wait "$RUNNER"; then fail 'owner unexpectedly exited successfully'; fi
stack status --json >/tmp/owner-status.json
assert_json '.data.lease_expired | contains("owner process")' /tmp/owner-status.json
cd ~/appB || exit 1
stack up --json >/tmp/reap.json
assert_json '.ok and (.data.reaped | length == 1) and .data.reaped[0].stopped' /tmp/reap.json
stack down --json >/tmp/down.json
assert_json '.ok and .data.confirmed' /tmp/down.json
[[ $(alive) -eq 0 ]] || fail 'owned service processes survived cleanup'
