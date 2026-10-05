#!/usr/bin/env bash
# Variables in command strings are expanded by the child.
# shellcheck disable=SC2016
set -Eeuo pipefail
# shellcheck source=tests/e2e/assert.sh
source "${STACK_E2E_ASSERT:-/tmp/stack-e2e-assert.sh}"
echo 'Services of a deleted project are stopped through the supervisor, without the project'
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
rm -rf "$W/appE"
stack -C "$W" gc --json >"$T/gc-deleted.json"
assert_json '.ok and (.data | map(select(.reason == "project directory deleted")) | length == 1 and .[0].stopped and all(.[0].services[]; .outcome | test("stopped pid")))' "$T/gc-deleted.json"
# shellcheck disable=SC2086
assert_dead $pids
[[ ! -e "$W/appE" ]] || fail 'cleanup recreated the deleted project'
stack -C "$W" gc --json >"$T/gc-again.json"
assert_json '.ok and (.data | length == 0)' "$T/gc-again.json"
