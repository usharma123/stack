#!/usr/bin/env bash
# Variables in command strings are expanded by the child.
# shellcheck disable=SC2016
set -Eeuo pipefail
# shellcheck source=tests/e2e/assert.sh
source "${STACK_E2E_ASSERT:-/tmp/stack-e2e-assert.sh}"
# MCP admission end to end: calls on two checkouts take turns per checkout and run alongside
# across them, a batch is answered once, a call cancelled while waiting never runs, and every
# other call accepted before input ended is answered; a signal drops what waits and answers
# what runs. Every wait is bounded: a server that never finishes is stopped and fails the
# scenario with what it answered and which commands ran, and every command a call started
# records its PID so none outlives the scenario. STACK_E2E_MCP_SECONDS (default 60) bounds each
# server, STACK_E2E_WAIT_SECONDS (default 15) each wait for a command or the signal's effect.
limit=${STACK_E2E_MCP_SECONDS:-60} waits=${STACK_E2E_WAIT_SECONDS:-15}
for app in appM appN; do
  mkdir -p "$W/$app"
  cd "$W/$app" || exit 1
  git init -q
  printf '[tools]\npython = "3.13"\n' >stack.toml
  stack compile >/dev/null
  stack install >/dev/null
done
order="$T/mcp-order" started="$T/mcp-pids"
: >"$order"
: >"$started"
track_pids "$started"
diagnose() {
  local file
  for file in "$T"/mcp-eof.out "$T"/mcp-signal.out "$order"; do
    [[ -f "$file" ]] && printf -- '--- %s\n%s\n' "$file" "$(tail -c 4000 "$file")"
  done
  return 0
}
# call ID APP SCRIPT: a stack_exec request running SCRIPT in APP, which first records its PID.
call() {
  jq -nc --argjson id "$1" --arg dir "$W/$2" --arg script "echo \$\$ >>$started; $3" \
    '{jsonrpc: "2.0", id: $id, method: "tools/call", params: {name: "stack_exec", arguments: {dir: $dir, command: ["sh", "-c", $script]}}}'
}
note() { printf 'echo %s >>%s' "$1" "$order"; }
# nap SECONDS: a sleep that records its PID too, so it is stopped even if its shell is not.
nap() { printf 'sleep %s & echo $! >>%s; wait $!' "$1" "$started"; }

echo 'Calls accepted before input ends are each answered once, in per-checkout order'
{
  echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"probe","version":"0"}}}'
  echo '{"jsonrpc":"2.0","method":"notifications/initialized"}'
  call 2 appM "$(nap 3); $(note a)"
  call 3 appM "$(note b)"
  call 4 appN "$(note c)"
  jq -nc --argjson five "$(call 5 appN "$(note d)")" --argjson six "$(call 6 appM "$(note e)")" \
    '[$five, {jsonrpc: "2.0", id: "p", method: "ping"}, $six]'
  call 7 appM "$(note z)"
  echo '{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":7,"reason":"probe"}}'
} | within "$limit" stack mcp >"$T/mcp-eof.out"
jq -se '
  (map(select(type == "object") | .id) | sort) == [1, 2, 3, 4] and
  (map(select(type == "array")) | length == 1 and (.[0] | map(.id | tostring) | sort) == ["5", "6", "p"]) and
  all(.[] | if type == "array" then .[] else . end | select(.id != 1 and .id != "p"); .result.structuredContent.data.exit_code == 0)
' "$T/mcp-eof.out" >/dev/null || fail "responses: $(cat "$T/mcp-eof.out")"
ran=$(tr '\n' ' ' <"$order")
! grep -qx z "$order" || fail "the call cancelled while waiting ran: $ran"
[[ "$(grep -xE 'a|b|e' "$order" | tr '\n' ' ')" == 'a b e ' ]] || fail "one checkout's calls out of order: $ran"
[[ "$(head -1 "$order")" == c || "$(head -1 "$order")" == d ]] || fail "the other checkout waited behind the busy one: $ran"

echo 'A signal drops the waiting call unanswered and answers the running one'
: >"$order"
fifo="$T/mcp-in"
rm -f "$fifo"
mkfifo "$fifo"
stack mcp <"$fifo" >"$T/mcp-signal.out" &
server=$!
track "$server"
exec 3>"$fifo"
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"probe","version":"0"}}}' >&3
call 2 appM "$(note started); $(nap 30); $(note finished)" >&3
call 3 appM "$(note waiter)" >&3
eventually "$waits" 'the first call started' grep -qx started "$order"
kill -TERM "$server"
server_ended() { ! kill -0 "$server" 2>/dev/null; }
eventually "$waits" 'the server ended after the signal' server_ended
wait "$server" || true
exec 3>&-
jq -se 'map(.id) | sort == [1, 2]' "$T/mcp-signal.out" >/dev/null || fail "after the signal: $(cat "$T/mcp-signal.out")"
! grep -qx waiter "$order" || fail 'a waiting call ran after the signal'
! grep -qx finished "$order" || fail 'the running command was not passed the signal'
