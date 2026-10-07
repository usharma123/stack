#!/usr/bin/env bash
# Variables in command strings are expanded by the child.
# shellcheck disable=SC2016
set -Eeuo pipefail
# shellcheck source=tests/e2e/assert.sh
source "${STACK_E2E_ASSERT:-/tmp/stack-e2e-assert.sh}"
echo 'A foreign listener on an assigned port is refused as a port conflict, never "stopped"'
# Self-contained: publish the example bundles if an earlier scenario has not.
if [[ ! -d "$SRV/pybase" ]]; then
  git config --global user.email a@b
  git config --global user.name a
  git config --global init.defaultBranch main
  for b in pybase obs; do
    cp -r "$EX/bundles/$b" "$SRV/$b"
    (cd "$SRV/$b" && git init -q && git add -A && git commit -qm v1 && git tag v1)
  done
fi
cp -r "$EX/app" "$W/appF"
cd "$W/appF" || exit 1
rm -f stack.lock
edit "s|path:../bundles/pybase|git+file://$SRV/pybase?ref=v1|; s|path:../bundles/obs|git+file://$SRV/obs?ref=v1|" stack.toml
stack compile --json >"$T/compile-appF.json"
port=$(jq -r '.data.ports.postgres' "$T/compile-appF.json")
python3 -c 'import socket,sys,time; s=socket.socket(); s.bind(("127.0.0.1", int(sys.argv[1]))); s.listen(); time.sleep(600)' "$port" &
squatter=$!
trap 'kill "$squatter" 2>/dev/null || true' EXIT
for _ in $(seq 50); do (exec 3<>/dev/tcp/127.0.0.1/"$port") 2>/dev/null && break; sleep 0.1; done
if stack up --json >"$T/up-conflict.json"; then fail 'up started over a foreign listener'; fi
assert_json '.ok == false and .error.code == "port_conflict"' "$T/up-conflict.json"
jq -e --argjson p "$port" '.error.details[0].service == "postgres" and .error.details[0].port == $p and .error.details[0].pinned == false' "$T/up-conflict.json" >/dev/null || fail "conflict details: $(cat "$T/up-conflict.json")"
jq -e '.error.hint | test("--reassign-ports")' "$T/up-conflict.json" >/dev/null || fail 'hint does not name --reassign-ports'
jq -e --argjson pid "$squatter" '.error.details[0].holder.pid == $pid' "$T/up-conflict.json" >/dev/null || echo "note: holder not attributed: $(jq -c '.error.details[0].holder' "$T/up-conflict.json")"
jq -e '[.error.details[-1].steps[] | select(.step == "start")] | length == 0' "$T/up-conflict.json" >/dev/null || fail 'start ran despite the conflict'
[[ ! -e .stack/session.json ]] || fail 'ownership recorded for a refused start'
kill -0 "$squatter" || fail 'the foreign listener was killed'

echo 'down owns nothing here and reports the foreign listener instead of failing'
stack down --json >"$T/down-conflict.json"
jq -e --argjson p "$port" '.ok and .data.confirmed and .data.conflicts[0].port == $p' "$T/down-conflict.json" >/dev/null || fail "down: $(cat "$T/down-conflict.json")"
kill -0 "$squatter" || fail 'down killed the foreign listener'

echo 'Reassigning ports recovers; the moved checkout starts and verifies normally'
stack compile --reassign-ports --json >"$T/reassign-appF.json"
moved=$(jq -r '.data.ports.postgres' "$T/reassign-appF.json")
[[ "$moved" != "$port" ]] || fail 'reassign kept the occupied port'
stack up --json >"$T/up-appF.json"
assert_json '.ok and all(.data.checks[]; .ready and .identity == "instance")' "$T/up-appF.json"
jq -e --argjson p "$moved" '.data.session.services.postgres.port == $p' "$T/up-appF.json" >/dev/null || fail 'postgres did not move'
stack logs postgres --tail 20 --json >"$T/logs-appF.json"
assert_json '.ok and (.data.lines | length > 0) and .data.truncated == false' "$T/logs-appF.json"
stack down --json >"$T/down-appF.json"
assert_json '.ok and .data.confirmed and (.data.conflicts == null)' "$T/down-appF.json"
# shellcheck disable=SC2046
assert_dead $(recorded_pids "$T/up-appF.json")

echo 'install puts the locked binaries in place without a session'
rm -rf "$W/appG" && cp -r "$W/appF" "$W/appG" && rm -rf "$W/appG/.stack"
cd "$W/appG" || exit 1
stack install --json >"$T/install-appG.json"
assert_json '.ok and ([.data.steps[].step] == ["compile", "preflight", "install"])' "$T/install-appG.json"
[[ ! -e .stack/session.json ]] || fail 'install recorded a session'
if stack status --json >"$T/status-appG.json"; then fail 'status reported ready services after a plain install'; fi
assert_json '.ok and .data.session == null and all(.data.checks[]; .ready == false)' "$T/status-appG.json"
stack exec -- bash -c 'command -v psql >/dev/null && command -v redis-server >/dev/null' || fail 'installed tools are not on PATH in exec'
