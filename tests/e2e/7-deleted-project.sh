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
assert_json '.ok and (.data.session.provider.state_dir | length > 0) and all(.data.session.services[]; .provider_id | test("/"))' "$T/up-appE.json"
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
[[ ! -e "$W/appE" ]] || fail 'cleanup recreated the unavailable project'
restore_checkout
trap - EXIT
stack -C "$W/appE" down --json >"$T/down-appE.json"
assert_json '.ok and .data.confirmed' "$T/down-appE.json"
# shellcheck disable=SC2086
assert_dead $pids
rm -rf "$W/appE"
stack -C "$W" gc --json >"$T/gc-again.json"
assert_json '.ok and (.data | length == 0)' "$T/gc-again.json"
