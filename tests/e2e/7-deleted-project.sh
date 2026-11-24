#!/usr/bin/env bash
# Variables in command strings are expanded by the child.
# shellcheck disable=SC2016
set -Eeuo pipefail
# shellcheck source=tests/e2e/assert.sh
source "${STACK_E2E_ASSERT:-/tmp/stack-e2e-assert.sh}"
echo 'Gone-project GC stops Stack-supervised services, releases ports, and keeps data until reclaimed'
cp -r "$EX/app" "$W/appE"
cd "$W/appE" || exit 1
rm -f stack.lock
edit "s|path:../bundles/pybase|git+file://$SRV/pybase?ref=v1|; s|path:../bundles/obs|git+file://$SRV/obs?ref=v1|" stack.toml
stack compile >/dev/null
stack up --json >"$T/up-appE.json"
assert_json '.ok' "$T/up-appE.json"
assert_json '.provider.supervisor == "stack" and (.provider.state_dir | length > 0)' .stack/session.json
pids=$(recorded_pids "$T/up-appE.json")
[[ -n "$pids" ]] || fail 'no service PIDs recorded'
cd "$W" || exit 1
rm -rf "$W/appE"
# Native GC stops the deleted checkout's recorded services, releases its ports, and keeps data
# until --reclaim-data asks to delete it.
stack -C "$W" gc --json >"$T/gc-deleted.json"
assert_json '.ok and (.data | length >= 1) and all(.data[]; .reason == "project directory deleted" and .stopped == true) and any(.data[].services[]; .outcome | test("^port [0-9]+ released$")) and any(.data[].services[]; .outcome | startswith("data kept in"))' "$T/gc-deleted.json"
# shellcheck disable=SC2086
assert_dead $pids
if jq -e --arg p "$W/appE" 'any(.reservations[]; .project == $p)' "${STACK_STATE_DIR:-$HOME/.local/state/stack}/ports.json" >/dev/null; then fail 'ports still reserved'; fi
[[ ! -e "$W/appE" ]] || fail 'cleanup recreated the deleted project'
data=$(jq -r '.data[].services[] | select(.service == "postgres" and (.outcome | startswith("data kept"))) | .outcome | capture("data kept in (?<d>[^ ]+) ").d' "$T/gc-deleted.json")
[[ -f "$data/PG_VERSION" ]] || fail "postgres data not kept at $data"
stack -C "$W" gc --reclaim-data --json >"$T/gc-reclaim.json"
assert_json '.ok and all(.data[0].services[]; .outcome | test("removed"))' "$T/gc-reclaim.json"
[[ ! -e "$data" ]] || fail "reclaimed data still at $data"
stack -C "$W" gc --json >"$T/gc-last.json"
assert_json '.ok and (.data | length == 0)' "$T/gc-last.json"
