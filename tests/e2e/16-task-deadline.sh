#!/usr/bin/env bash
# Variables in command strings are expanded by the child.
# shellcheck disable=SC2016
set -Eeuo pipefail
# shellcheck source=tests/e2e/assert.sh
source "${STACK_E2E_ASSERT:-/tmp/stack-e2e-assert.sh}"
# A task cut short at its deadline does nothing more. `mise run` gives each task a process
# group of its own, so killing the group Stack started ends mise and misses the task, which
# then finishes its work after Stack reported it killed. Judged by what the task does, not by
# what Stack reports: through `stack run`, `stack run --json` and MCP `stack_run`, a task that
# also ignores SIGTERM is gone when the call returns and never writes its last line.
# STACK_E2E_CALL_SECONDS (default 30) bounds each call and STACK_E2E_SETUP_SECONDS (default 600)
# the compile.
calls=${STACK_E2E_CALL_SECONDS:-30} setup=${STACK_E2E_SETUP_SECONDS:-600}
started="$T/deadline-pids"
: >"$started"
track_pids "$started"
mkdir -p "$W/appD"
cd "$W/appD" || exit 1
git init -q
# The task records its shell (for the checks below and the scenario's cleanup), says it began,
# and four seconds later, well after the one-second deadline, that it finished.
cat >stack.toml <<TOML
[tasks.probe]
run = '''
trap "" TERM
$(record_pid "$started")
echo began >>"$T/deadline-\$PROBE"
sleep 4
echo finished >>"$T/deadline-\$PROBE"
'''
TOML
within "$setup" stack compile >/dev/null
diagnose() {
  local file
  for file in "$T"/deadline-*; do
    [[ -f "$file" ]] && printf -- '--- %s\n%s\n' "$file" "$(tail -c 2000 "$file")"
  done
  return 0
}
# Whether a process a task recorded is still running.
task_runs() {
  local pid when
  while read -r pid when; do
    if alive "$pid" "$when"; then return 0; fi
  done <"$started"
  return 1
}
task_gone() { ! task_runs; }
# cut_short NAME: the task of the call NAME began, and nothing of it is left running. A killed
# process is still listed until it is collected, so allow that two seconds; a task left running
# has more than that to go.
cut_short() {
  grep -qx began "$T/deadline-$1" || fail "$1: the task never began"
  eventually 2 "$1: the task is gone once the call returned" task_gone
}

echo 'stack run --timeout ends the task with the call'
status=0
PROBE=plain within "$calls" stack run --timeout 1s probe >"$T/deadline-plain.out" 2>&1 || status=$?
[[ "$status" == 124 ]] || fail "plain: exit $status: $(cat "$T/deadline-plain.out")"
cut_short plain

echo 'stack run --json --timeout ends the task with the call'
status=0
PROBE=json within "$calls" stack run --json --timeout 1s probe >"$T/deadline-json.out" || status=$?
[[ "$status" == 124 ]] || fail "json: exit $status: $(cat "$T/deadline-json.out")"
assert_json '.error.code == "timed_out" and .error.details[0].timed_out == true' "$T/deadline-json.out"
cut_short json

echo 'MCP stack_run with a timeout ends the task with the call'
{
  echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"probe","version":"0"}}}'
  echo '{"jsonrpc":"2.0","method":"notifications/initialized"}'
  jq -nc --arg dir "$W/appD" '{jsonrpc: "2.0", id: 2, method: "tools/call", params: {name: "stack_run", arguments: {dir: $dir, task: "probe", timeout_secs: 1}}}'
} | PROBE=mcp within "$calls" stack mcp >"$T/deadline-mcp.out"
jq -se 'map(select(.id == 2)) | length == 1 and .[0].result.structuredContent.error.code == "timed_out"' "$T/deadline-mcp.out" >/dev/null \
  || fail "mcp: $(cat "$T/deadline-mcp.out")"
cut_short mcp

echo 'No task finished after its deadline'
# Longer than a task that was left running would need to write its last line.
sleep 5
for name in plain json mcp; do
  ! grep -qx finished "$T/deadline-$name" || fail "$name: the task finished its work after the deadline"
done
