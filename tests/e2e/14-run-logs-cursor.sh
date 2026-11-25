#!/usr/bin/env bash
# Variables in command strings are expanded by the child.
# shellcheck disable=SC2016
set -Eeuo pipefail
# shellcheck source=tests/e2e/assert.sh
source "${STACK_E2E_ASSERT:-/tmp/stack-e2e-assert.sh}"
# `stack run` forwarding every argument after the task's name, `logs --json --cursor` (each line
# once, repeats included, with stamps, `more` and a truncation's gap), `logs --follow`, and
# `--project` spelling, against a service that writes what it is asked to. Every client call is
# bounded: one that stalls is stopped and fails the scenario with the log and reads so far.
# STACK_E2E_CALL_SECONDS (default 30) bounds each call, STACK_E2E_WAIT_SECONDS (default 15) each
# wait for lines to arrive.
calls=${STACK_E2E_CALL_SECONDS:-30} waits=${STACK_E2E_WAIT_SECONDS:-15}
diagnose() {
  local file
  for file in "$T"/logs-*.json "$T"/follow.out "$T"/follow.err; do
    [[ -f "$file" ]] && printf -- '--- %s\n%s\n' "$file" "$(tail -c 2000 "$file")"
  done
  if [[ -n "${log:-}" && -f "$log" ]]; then printf -- '--- %s\n%s\n' "$log" "$(tail -c 2000 "$log")"; fi
  return 0
}
git config --global user.email a@b
git config --global user.name a
git config --global init.defaultBranch main
mkdir -p "$W/appQ"
cd "$W/appQ" || exit 1
git init -q
# GET /say/<word>/<n> prints <word> n times on stdout; GET / answers readiness.
cat >emit.py <<'PY'
import http.server, os
class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        parts = self.path.strip("/").split("/")
        if len(parts) == 3 and parts[0] == "say":
            for _ in range(int(parts[2])):
                print(parts[1], flush=True)
        self.send_response(200)
        self.end_headers()
        self.wfile.write(b"ok")
    def log_message(self, *args):
        pass
http.server.HTTPServer(("127.0.0.1", int(os.environ["PORT"])), Handler).serve_forever()
PY
cat >stack.toml <<EOF
[tools]
python = "3.13"

[services.talk]
run = "exec python3 -u $W/appQ/emit.py"
ready_cmd = "curl -fsS http://127.0.0.1:\$PORT/ >/dev/null"

[tasks.args]
run = "printf '[%s]'"
EOF
stack compile >/dev/null
stack up --json >"$T/up-appQ.json"
assert_json '.ok and all(.data.checks[]; .ready)' "$T/up-appQ.json"
port=$(checked talk port "$T/up-appQ.json")
log="$(jq -r '.provider.state_dir' .stack/session.json)/logs/talk.log"
say() { curl -fsS --max-time "$calls" "http://127.0.0.1:$port/say/$1/$2" >/dev/null; }
logs() { within "$calls" stack logs talk "$@"; }

echo 'run gives the task every argument after its name; options before it are Stack own'
out=$(within "$calls" stack run args --timeout 3 --json -q x)
[[ "$out" == '[--timeout][3][--json][-q][x]' ]] || fail "task arguments: $out"
out=$(within "$calls" stack run --timeout 1m args -- --up)
[[ "$out" == '[--up]' ]] || fail "a -- right after the name: $out"
within "$calls" stack run --json args a >"$T/run-json.json"
assert_json '.ok and .data.exit_code == 0 and .data.stdout == "[a]"' "$T/run-json.json"

echo 'A cursor read returns each new line once, repeats included, with its stamp'
logs --json --cursor "" >"$T/logs-begin.json"
assert_json '.ok and (.data.cursor | startswith("v2;"))' "$T/logs-begin.json"
cursor=$(jq -r '.data.cursor' "$T/logs-begin.json")
say same 3
# read_on WANT: read on from $cursor until WANT lines came (the supervisor writes them a moment
# later), failing if they have not within $waits seconds; sets $got (every line, in order),
# $gaps (every gap reported) and $cursor.
read_on() {
  local want=$1 stamps='[]' deadline=$(($(date +%s) + waits))
  got='[]' gaps='[]'
  while :; do
    logs --json --cursor "$cursor" >"$T/logs-read.json"
    got=$(jq -c --argjson got "$got" '$got + .data.lines' "$T/logs-read.json")
    stamps=$(jq -c --argjson s "$stamps" '$s + .data.timestamps' "$T/logs-read.json")
    gaps=$(jq -c --argjson g "$gaps" '$g + [.data.gap // empty]' "$T/logs-read.json")
    cursor=$(jq -r '.data.cursor' "$T/logs-read.json")
    [[ "$(jq length <<<"$got")" -ge "$want" ]] && break
    if (($(date +%s) >= deadline)); then
      diagnose >&2
      fail "read $got (gaps $gaps), not $want lines, within ${waits}s"
    fi
    sleep 0.1
  done
  jq -e --argjson s "$stamps" '($s | length) == length and $s == ($s | sort) and all($s[]; . > 1000000000000)' <<<"$got" >/dev/null || fail "stamps $stamps for $got"
}
read_on 3
[[ "$got" == '["same","same","same"]' && "$gaps" == '[]' ]] || fail "first read: $got, gaps $gaps"
logs --json --cursor "$cursor" >"$T/logs-idle.json"
jq -e --arg c "$cursor" '(.data.lines | length) == 0 and (.data.more // false) == false and .data.cursor == $c' "$T/logs-idle.json" >/dev/null \
  || fail "nothing new: $(cat "$T/logs-idle.json")"

echo '--tail caps a read and says more exactly while whole lines are left'
say twice 2
both_written() {
  logs --json --cursor "$cursor" >"$T/logs-peek.json"
  [[ "$(jq '.data.lines | length' "$T/logs-peek.json")" == 2 ]]
}
eventually "$waits" 'both lines written' both_written
logs --json --cursor "$cursor" --tail 1 >"$T/logs-tail1.json"
assert_json '.data.lines == ["twice"] and .data.more == true' "$T/logs-tail1.json"
logs --json --cursor "$(jq -r '.data.cursor' "$T/logs-tail1.json")" --tail 1 >"$T/logs-tail2.json"
assert_json '.data.lines == ["twice"] and (.data.more // false) == false' "$T/logs-tail2.json"
cursor=$(jq -r '.data.cursor' "$T/logs-tail2.json")

echo 'A log cut by hand under the cursor is read again from its start, with a truncated gap'
[[ -f "$log" ]] || fail "no log at $log"
: >"$log"
say after 1
read_on 1
[[ "$got" == '["after"]' && "$gaps" == '["truncated"]' ]] || fail "after truncation: $got, gaps $gaps"

echo 'An invalid cursor is a usage error'
expect_error usage within "$calls" stack logs talk --json --cursor 'v2;nope'

echo '--follow prints every new line once, repeats included'
stack logs talk --follow --tail 1 </dev/null >"$T/follow.out" 2>"$T/follow.err" &
follower=$!
track "$follower"
eventually "$waits" '--follow printed the last line' test -s "$T/follow.out"
sleep 1.5
say same 3
say end 1
eventually "$waits" '--follow printed the new lines' grep -qx end "$T/follow.out"
kill -0 "$follower" 2>/dev/null || fail "--follow ended by itself: $(cat "$T/follow.err")"
kill "$follower" 2>/dev/null || true
wait "$follower" 2>/dev/null || true
[[ "$(tail -n +2 "$T/follow.out" | tr '\n' ' ')" == 'same same same end ' ]] || fail "follow printed: $(cat "$T/follow.out")"

echo '--project takes a path only as --project=PATH; a word after a space is a service'
within "$calls" stack logs --project="$W/appQ" talk --json >"$T/logs-project.json"
assert_json '.ok and (.data.checkouts | length) == 1' "$T/logs-project.json"
within "$calls" stack logs --project talk --json >"$T/logs-project-service.json"
assert_json '.ok' "$T/logs-project-service.json"
if within "$calls" stack logs --project "$W/appQ" --json >"$T/logs-project-space.json"; then fail '--project PATH with a space was accepted'; fi
assert_json '.ok == false and .error.code == "usage" and (.error.hint | contains("--project="))' "$T/logs-project-space.json"

echo 'A checkout directory named like a subcommand is never taken for one'
mkdir -p "$W/logs"
cd "$W/logs" || exit 1
git init -q
printf '[tools]\npython = "3.13"\n' >stack.toml
stack compile >/dev/null
cd "$W" || exit 1
out=$(within "$calls" stack -C logs exec printf '%s|' --project ./x)
[[ "$out" == '--project|./x|' ]] || fail "exec in a checkout named logs: $out"

cd "$W/appQ" || exit 1
stack down --json >"$T/down-appQ.json"
assert_json '.ok and .data.confirmed' "$T/down-appQ.json"
