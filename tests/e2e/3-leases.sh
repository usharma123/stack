#!/usr/bin/env bash
# Variables in command strings are expanded by the child.
# shellcheck disable=SC2016
set -Eeuo pipefail
# shellcheck source=tests/e2e/assert.sh
source "${STACK_E2E_ASSERT:-/tmp/stack-e2e-assert.sh}"
cd "$W/appB" || exit 1
stack up --ttl 4s --json >"$T/lease.json"
assert_json '.ok and .data.session.lease.ttl_secs == 4' "$T/lease.json"
sleep 3
stack renew --json >"$T/renew.json"
assert_json '.ok' "$T/renew.json"
sleep 2
stack gc --json >"$T/gc.json"
assert_json '.ok and (.data | length == 0)' "$T/gc.json"
sleep 4
stack gc --json >"$T/gc.json"
assert_json '.ok and (.data | length == 1) and .data[0].stopped' "$T/gc.json"
[[ ! -e .stack/session.json ]] || fail 'expired record survived'
# shellcheck disable=SC2046
assert_dead $(recorded_pids "$T/lease.json")

echo 'An active command protects its short TTL, including its real database'
stack up --ttl 2s --json >"$T/active.json"
stack exec --require-all -- bash -c 'sleep 7; psql "$DATABASE_URL" -Atc "select 1"' >"$T/active-exec.log" &
EXEC_PID=$!
sleep 4
stack gc --json >"$T/gc.json"
assert_json '.ok and (.data | length == 0)' "$T/gc.json"
kill -0 "$EXEC_PID"
[[ -e .stack/session.json ]] || fail 'GC removed an active session'
wait "$EXEC_PID"
[[ $(cat "$T/active-exec.log") == 1 ]] || fail 'active command lost its database'
sleep 3
stack gc --json >"$T/gc.json"
assert_json '.ok and (.data | length == 1) and .data[0].stopped' "$T/gc.json"
# shellcheck disable=SC2046
assert_dead $(recorded_pids "$T/active.json")

echo 'A foreground watcher reclaims an idle lease without another up'
stack up --ttl 2s --json >"$T/watched.json"
stack gc --watch --interval 1s --max-passes 5 --json >"$T/watch.jsonl"
jq -se '[.[].data.reclaimed[]] | length == 1 and .[0].stopped' "$T/watch.jsonl" >/dev/null || fail 'watcher did not reclaim the lease'
# shellcheck disable=SC2046
assert_dead $(recorded_pids "$T/watched.json")

sleep 300 & RUNNER=$!
cd "$W/appA" || exit 1
stack up --owner-pid "$RUNNER" --json >"$T/owner.json"
assert_json '.ok and (.data.session.lease.owner_pid > 0)' "$T/owner.json"
stack exec --require-all -- true
kill "$RUNNER"
if wait "$RUNNER"; then fail 'owner unexpectedly exited successfully'; fi
stack status --json >"$T/owner-status.json"
assert_json '.data.lease_expired | contains("owner process")' "$T/owner-status.json"
cd "$W/appB" || exit 1
stack up --json >"$T/reap.json"
assert_json '.ok and (.data.reaped | length == 1) and .data.reaped[0].stopped' "$T/reap.json"
# shellcheck disable=SC2046
assert_dead $(recorded_pids "$T/owner.json")
stack down --json >"$T/down.json"
assert_json '.ok and .data.confirmed' "$T/down.json"
# shellcheck disable=SC2046
assert_dead $(recorded_pids "$T/reap.json")
