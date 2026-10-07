#!/usr/bin/env bash
# Variables in command strings are expanded by the child.
# shellcheck disable=SC2016
set -Eeuo pipefail
# shellcheck source=tests/e2e/assert.sh
source "${STACK_E2E_ASSERT:-/tmp/stack-e2e-assert.sh}"
echo 'A foreign listener on an assigned port is refused as a port conflict, never "stopped"'
# Own bundle sources: earlier scenarios may have rewritten the shared ones (scenario 8 turns
# pybase into a probed custom service), and this one needs the Postgres and Redis presets.
git config --global user.email a@b
git config --global user.name a
git config --global init.defaultBranch main
for b in pybase obs; do
  rm -rf "$SRV/$b-9"
  cp -r "$EX/bundles/$b" "$SRV/$b-9"
  (cd "$SRV/$b-9" && git init -q && git add -A && git commit -qm v1 && git tag v1)
done
cp -r "$EX/app" "$W/appH"
cd "$W/appH" || exit 1
rm -f stack.lock
edit "s|path:../bundles/pybase|git+file://$SRV/pybase-9?ref=v1|; s|path:../bundles/obs|git+file://$SRV/obs-9?ref=v1|" stack.toml
stack compile --json >"$T/compile-appH.json"
assert_json '.data.ports | has("postgres") and has("redis")' "$T/compile-appH.json"
port=$(jq -r '.data.ports.postgres' "$T/compile-appH.json")
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
stack compile --reassign-ports --json >"$T/reassign-appH.json"
moved=$(jq -r '.data.ports.postgres' "$T/reassign-appH.json")
[[ "$moved" != "$port" ]] || fail 'reassign kept the occupied port'
stack up --json >"$T/up-appH.json"
assert_json '.ok and all(.data.checks[]; .ready and .identity == "instance")' "$T/up-appH.json"
jq -e --argjson p "$moved" '.data.session.services.postgres.port == $p' "$T/up-appH.json" >/dev/null || fail 'postgres did not move'
stack logs postgres --tail 20 --json >"$T/logs-appH.json"
assert_json '.ok and (.data.lines | length > 0) and .data.truncated == false' "$T/logs-appH.json"

echo 'A generation change whose new port is taken: the old services stop, the squatter is a conflict'
# While the services run on `moved`, reassign again; mise now reports the daemons with their
# new configured ports although they still listen on the old ones. Squat the new Postgres port.
stack compile --reassign-ports --json >"$T/reassign2-appH.json"
next=$(jq -r '.data.ports.postgres' "$T/reassign2-appH.json")
[[ "$next" != "$moved" ]] || fail 'second reassign kept the port'
python3 -c 'import socket,sys,time; s=socket.socket(); s.bind(("127.0.0.1", int(sys.argv[1]))); s.listen(); time.sleep(600)' "$next" &
squatter2=$!
trap 'kill "$squatter" "$squatter2" 2>/dev/null || true' EXIT
for _ in $(seq 50); do (exec 3<>/dev/tcp/127.0.0.1/"$next") 2>/dev/null && break; sleep 0.1; done
started=$(date +%s)
if stack up --json >"$T/up-generation.json"; then fail 'up started over a foreign listener after a generation change'; fi
(( $(date +%s) - started < 15 )) || fail 'up waited on the foreign listener'
assert_json '.ok == false and .error.code == "port_conflict"' "$T/up-generation.json"
jq -e --argjson p "$next" '.error.details[0].service == "postgres" and .error.details[0].port == $p' "$T/up-generation.json" >/dev/null || fail "generation conflict: $(cat "$T/up-generation.json")"
jq -e '[.error.details[-1].steps[] | select(.step == "stop_previous" and .status == "ok")] | length == 1' "$T/up-generation.json" >/dev/null || fail 'the previous generation was not stopped first'
# shellcheck disable=SC2046
assert_dead $(recorded_pids "$T/up-appH.json")
if (exec 3<>/dev/tcp/127.0.0.1/"$moved") 2>/dev/null; then fail "old Postgres port $moved still accepts"; fi
kill -0 "$squatter2" || fail 'the second foreign listener was killed'
stack down --json >"$T/down-generation.json"
jq -e --argjson p "$next" '.ok and .data.confirmed and .data.conflicts[0].port == $p' "$T/down-generation.json" >/dev/null || fail "down after generation change: $(cat "$T/down-generation.json")"
kill "$squatter2"; wait "$squatter2" 2>/dev/null || true
stack up --json >"$T/up-appH2.json"
assert_json '.ok and all(.data.checks[]; .ready and .identity == "instance")' "$T/up-appH2.json"
jq -e --argjson p "$next" '.data.session.services.postgres.port == $p' "$T/up-appH2.json" >/dev/null || fail 'postgres did not move to the free port'
stack down --json >"$T/down-appH.json"
assert_json '.ok and .data.confirmed and (.data.conflicts == null)' "$T/down-appH.json"
# shellcheck disable=SC2046
assert_dead $(recorded_pids "$T/up-appH2.json")

echo 'install puts the locked binaries in place without a session'
rm -rf "$W/appI" && cp -r "$W/appH" "$W/appI" && rm -rf "$W/appI/.stack"
cd "$W/appI" || exit 1
stack install --json >"$T/install-appI.json"
assert_json '.ok and ([.data.steps[].step] == ["compile", "preflight", "install"])' "$T/install-appI.json"
[[ ! -e .stack/session.json ]] || fail 'install recorded a session'
if stack status --json >"$T/status-appI.json"; then fail 'status reported ready services after a plain install'; fi
assert_json '.ok and .data.session == null and all(.data.checks[]; .ready == false)' "$T/status-appI.json"
stack exec -- bash -c 'command -v psql >/dev/null && command -v redis-server >/dev/null' || fail 'installed tools are not on PATH in exec'

echo 'Redis with a hanging preset readiness command still starts and verifies'
# The supervisor races the preset's `redis-cli ... ping` with its TCP check. Once TCP
# succeeds, stack verifies instance identity through the real client before recording a session.
mkdir -p "$W/hang-bundle/bin" "$W/appJ"
cat >"$W/hang-bundle/bin/redis-cli" <<'S'
#!/bin/sh
case " $* " in *" ping "*) exec sleep 100000 ;; esac
self=$(cd "$(dirname "$0")" && pwd)
IFS=:; for dir in $PATH; do
  [ "$dir" = "$self" ] && continue
  [ -x "$dir/redis-cli" ] && exec "$dir/redis-cli" "$@"
done
exit 127
S
chmod +x "$W/hang-bundle/bin/redis-cli"
printf '[bundle]\nname = "hang"\n[paths]\nbin = ["bin"]\n' >"$W/hang-bundle/bundle.toml"
printf '[[use]]\nbundle = "path:%s"\n[services.redis]\npreset = "redis"\nversion = "7"\n' "$W/hang-bundle" >"$W/appJ/stack.toml"
cd "$W/appJ" || exit 1
git init -q
stack compile >/dev/null
# macOS does not ship GNU timeout. Give this test request its own process group so a
# failed regression also terminates its waiting clients before the runner cleans up services.
if ! python3 - >"$T/up-appJ.json" <<'PY'
import os
import signal
import subprocess
import sys

with subprocess.Popen(["stack", "up", "--json"], start_new_session=True) as child:
    try:
        sys.exit(child.wait(timeout=120))
    except subprocess.TimeoutExpired:
        try:
            os.killpg(child.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        child.wait()
        sys.exit(124)
PY
then
  cat "$T/up-appJ.json"
  fail 'up did not verify a preset whose readiness command hangs'
fi
assert_json '.ok and all(.data.checks[]; .ready)' "$T/up-appJ.json"
stack down >/dev/null
