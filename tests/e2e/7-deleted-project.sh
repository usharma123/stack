#!/usr/bin/env bash
# Variables in command strings are expanded by the child.
# shellcheck disable=SC2016
set -Eeuo pipefail
# shellcheck source=tests/e2e/assert.sh
source "${STACK_E2E_ASSERT:-/tmp/stack-e2e-assert.sh}"
echo 'Gone-project GC preserves live services until explicit cleanup confirms shutdown'
cp -r "$EX/app" "$W/appE"
cd "$W/appE" || exit 1
rm -f stack.lock
edit "s|path:../bundles/pybase|git+file://$SRV/pybase?ref=v1|; s|path:../bundles/obs|git+file://$SRV/obs?ref=v1|" stack.toml
stack compile >/dev/null
stack up --json >"$T/up-appE.json"
assert_json '.ok' "$T/up-appE.json"
assert_json '(.provider.state_dir | length > 0) and all(.services[]; .provider_id | test("/"))' .stack/session.json
pids=$(recorded_pids "$T/up-appE.json")
[[ -n "$pids" ]] || fail 'no service PIDs recorded'
cd "$W" || exit 1
# Make the checkout unavailable without losing its identity, so the test can restore it
# for an explicit down. Unit tests also cover actual directory deletion and replacement.
mv "$W/appE" "$W/appE-held"
restore_checkout() { [[ ! -e "$W/appE-held" ]] || mv "$W/appE-held" "$W/appE"; }
trap restore_checkout EXIT
if stack -C "$W" gc --json >"$T/gc-deleted.json"; then fail 'GC accepted an unsafe stop-by-name'; fi
assert_json '.ok == false and .error.code == "gc_incomplete" and any(.error.details[]; .reason == "project directory deleted" and .stopped == false)' "$T/gc-deleted.json"
for pid in $pids; do kill -0 "$pid" || fail "GC signalled live pid $pid"; done
# Its session record is kept, so its ports stay reserved.
jq -e --arg p "$W/appE" 'any(.reservations[]; .project == $p)' "${STACK_STATE_DIR:-$HOME/.local/state/stack}/ports.json" >/dev/null || fail 'GC released the ports of a checkout it could not confirm stopped'
[[ ! -e "$W/appE" ]] || fail 'cleanup recreated the unavailable project'
restore_checkout
trap - EXIT
stack -C "$W/appE" down --json >"$T/down-appE.json"
assert_json '.ok and .data.confirmed' "$T/down-appE.json"
# shellcheck disable=SC2086
assert_dead $pids
rm -rf "$W/appE"
# Removed after a confirmed down: its ports come back, and its data is reported, not removed.
stack -C "$W" gc --json >"$T/gc-again.json"
assert_json '.ok and (.data | length == 1) and .data[0].reason == "project directory deleted" and any(.data[0].services[]; .outcome | test("^port [0-9]+ released$")) and any(.data[0].services[]; .outcome | startswith("data kept in"))' "$T/gc-again.json"
if jq -e --arg p "$W/appE" 'any(.reservations[]; .project == $p)' "${STACK_STATE_DIR:-$HOME/.local/state/stack}/ports.json" >/dev/null; then fail 'ports still reserved'; fi
data=$(jq -r '.data[0].services[] | select(.service == "postgres" and (.outcome | startswith("data kept"))) | .outcome | capture("data kept in (?<d>[^ ]+) ").d' "$T/gc-again.json")
[[ -f "$data/PG_VERSION" ]] || fail "postgres data not kept at $data"
stack -C "$W" gc --reclaim-data --json >"$T/gc-reclaim.json"
assert_json '.ok and all(.data[0].services[]; .outcome | test("removed"))' "$T/gc-reclaim.json"
[[ ! -e "$data" ]] || fail "reclaimed data still at $data"
stack -C "$W" gc --json >"$T/gc-last.json"
assert_json '.ok and (.data | length == 0)' "$T/gc-last.json"
