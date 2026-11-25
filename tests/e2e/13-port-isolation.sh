#!/usr/bin/env bash
# Variables in command strings are expanded by the child.
# shellcheck disable=SC2016
set -Eeuo pipefail
# shellcheck source=tests/e2e/assert.sh
source "${STACK_E2E_ASSERT:-/tmp/stack-e2e-assert.sh}"
# Port reservations across checkouts of their own (earlier and later scenarios' ports untouched):
# concurrent compiles, stable reuse, pins against other checkouts and against the checkout's own
# services, --reassign-ports, a checkout out of reach and a deleted one.
git config --global user.email a@b
git config --global user.name a
git config --global init.defaultBranch main
# mkport DIR [TOML]: a checkout with two light services (and TOML appended), not compiled.
mkport() {
  mkdir -p "$1"
  cd "$1" || exit 1
  git init -q
  cat >stack.toml <<EOF
[tools]
python = "3.13"

[services.web]
run = "exec python3 -m http.server \$PORT --bind 127.0.0.1"
ready_cmd = "curl -fsS http://127.0.0.1:\$PORT/ >/dev/null"
${2:-}

[services.api]
run = "exec python3 -m http.server \$PORT --bind 127.0.0.1"
ready_cmd = "curl -fsS http://127.0.0.1:\$PORT/ >/dev/null"
${3:-}
EOF
}
ports_of() { jq -c '.data.ports' "$1"; }
port_of() { jq -r --arg s "$2" '.data.ports[$s]' "$1"; }

echo 'Two checkouts compiling at once get distinct ports in Stack range'
mkport "$W/portP1"
mkport "$W/portP2"
(cd "$W/portP1" && stack compile --json >"$T/ports-p1.json") &
first=$!
(cd "$W/portP2" && stack compile --json >"$T/ports-p2.json") &
second=$!
wait "$first"
wait "$second"
all=$(jq -s '[.[].data.ports[]]' "$T/ports-p1.json" "$T/ports-p2.json")
jq -e 'length == 4 and (unique | length) == 4 and all(.[]; . >= 20000 and . <= 29999)' <<<"$all" >/dev/null || fail "ports: $all"

echo 'Each keeps its ports across compiles'
for n in 1 2; do
  (cd "$W/portP$n" && stack compile --json >"$T/ports-p$n-again.json")
  [[ "$(ports_of "$T/ports-p$n-again.json")" == "$(ports_of "$T/ports-p$n.json")" ]] || fail "portP$n moved without --reassign-ports"
done

echo "A pin on another checkout's port is refused, and that checkout keeps it"
taken=$(port_of "$T/ports-p1.json" web)
mkport "$W/portP3" "port = $taken"
expect_error port_conflict stack compile --json
(cd "$W/portP1" && stack compile --json >"$T/ports-p1-after-pin.json")
[[ "$(ports_of "$T/ports-p1-after-pin.json")" == "$(ports_of "$T/ports-p1.json")" ]] || fail 'the pin moved the other checkout'

echo "A pin on the port the checkout's own earlier service would get compiles; that service goes elsewhere"
mkport "$W/portP4"
STACK_STATE_DIR="$T/scratch-state" stack compile --json >"$T/ports-p4-scratch.json"
would=$(port_of "$T/ports-p4-scratch.json" web)
mkport "$W/portP4" "" "port = $would"
stack compile --json >"$T/ports-p4.json" || fail "own pin refused: $(cat "$T/ports-p4.json")"
[[ "$(port_of "$T/ports-p4.json" api)" == "$would" && "$(port_of "$T/ports-p4.json" web)" != "$would" ]] || fail "own pin: $(ports_of "$T/ports-p4.json")"

echo 'compile --reassign-ports gives every assigned service a new port, even while the old ones are free'
cd "$W/portP2" || exit 1
stack compile --reassign-ports --json >"$T/ports-p2-reassigned.json"
jq -e --slurpfile old "$T/ports-p2.json" '[.data.ports[]] as $new | ($old[0].data.ports | [.[]]) as $was | all($new[]; . as $p | ($was | index($p)) == null)' "$T/ports-p2-reassigned.json" >/dev/null \
  || fail "reassigned $(ports_of "$T/ports-p2.json") -> $(ports_of "$T/ports-p2-reassigned.json")"

echo 'Both start on their own verified ports at once'
for p in P1 P2; do
  cd "$W/port$p" || exit 1
  stack up --json >"$T/up-port$p.json"
  assert_json '.ok and all(.data.checks[]; .ready)' "$T/up-port$p.json"
done
jq -se '[.[].data.checks[].port] | (unique | length) == 4' "$T/up-portP1.json" "$T/up-portP2.json" >/dev/null || fail 'running services share a port'
[[ "$(checked web port "$T/up-portP2.json")" == "$(port_of "$T/ports-p2-reassigned.json" web)" ]] || fail 'up did not use the reassigned port'

# Root reads through mode 000: only an unprivileged user can make a checkout refuse lookups.
if [[ "$(id -u)" != 0 ]]; then
  echo "A checkout out of reach keeps its ports through other checkouts' compiles and gc"
  mkdir -p "$W/vault"
  touch "$W/vault/sibling"
  mkport "$W/vault/portP5"
  stack compile --json >"$T/ports-p5.json"
  held=$(port_of "$T/ports-p5.json" web)
  chmod 000 "$W/vault"
  trap 'chmod 755 "$W/vault"' EXIT
  (cd "$W/portP1" && stack compile --json >/dev/null)
  stack gc --json >"$T/gc-out-of-reach.json" || true
  mkport "$W/portP6" "port = $held"
  if stack compile --json >"$T/ports-p6.json"; then fail "a pin took the port of a checkout out of reach: $(cat "$T/ports-p6.json")"; fi
  assert_json '.ok == false and .error.code == "port_conflict"' "$T/ports-p6.json"
  chmod 755 "$W/vault"
  trap - EXIT
  (cd "$W/vault/portP5" && stack compile --json >"$T/ports-p5-back.json")
  [[ "$(ports_of "$T/ports-p5-back.json")" == "$(ports_of "$T/ports-p5.json")" ]] || fail 'the checkout came back to other ports'
else
  echo 'note: running as root; the out-of-reach check needs an unprivileged user'
fi

echo "A deleted checkout's ports are released by the next compile anywhere"
cd "$W/portP2" || exit 1
freed=$(port_of "$T/ports-p2-reassigned.json" web)
stack down --json >"$T/down-portP2.json"
assert_json '.ok and .data.confirmed' "$T/down-portP2.json"
cd "$W" && rm -rf "$W/portP2"
(cd "$W/portP1" && stack compile --json >/dev/null)
mkport "$W/portP7" "port = $freed"
stack compile --json >"$T/ports-p7.json" || fail "the deleted checkout's port is still reserved: $(cat "$T/ports-p7.json")"
[[ "$(port_of "$T/ports-p7.json" web)" == "$freed" ]] || fail 'the pin did not take the released port'

cd "$W/portP1" || exit 1
stack down --json >"$T/down-portP1.json"
assert_json '.ok and .data.confirmed' "$T/down-portP1.json"
# shellcheck disable=SC2046
assert_dead $(recorded_pids "$T/up-portP1.json")
