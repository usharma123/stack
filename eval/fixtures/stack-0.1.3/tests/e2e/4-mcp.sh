#!/usr/bin/env bash
# Variables in command strings are expanded by the child.
# shellcheck disable=SC2016
set -Eeuo pipefail
# shellcheck source=tests/e2e/assert.sh
source /tmp/stack-e2e-assert.sh
export PATH=/opt/stack:$PATH
cd ~/appA || exit 1
{
  echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"probe","version":"0"}}}'
  echo '{"jsonrpc":"2.0","method":"notifications/initialized"}'
  echo '{"jsonrpc":"2.0","id":2,"method":"tools/list"}'
  echo '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"stack_exec","arguments":{"command":["bash","-c","echo $DATABASE_URL"]}}}'
  echo '{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"stack_exec","arguments":{"command":["true"],"require":["postgres"]}}}'
  echo '{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"stack_up","arguments":{"ttl":"10m"}}}'
  echo '{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"stack_exec","arguments":{"command":["bash","-c","set -euo pipefail; uv run pytest -q"],"require_all":true}}}'
  echo '{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"stack_down","arguments":{}}}'
  echo '{"jsonrpc":"2.0","id":8,"method":"nope"}'
} | stack mcp >/tmp/mcp.out
jq -se '
  length == 8 and
  (map(select(.id == 1)) | length == 1 and .[0].result.protocolVersion == "2025-06-18") and
  (map(select(.id == 2)) | length == 1 and (.[0].result.tools | map(.name) | sort) == (["stack_inspect", "stack_compile", "stack_up", "stack_status", "stack_exec", "stack_renew", "stack_down", "stack_gc", "stack_doctor"] | sort)) and
  (map(select(.id == 3)) | .[0].result.structuredContent.data | .exit_code == 0 and (.stdout | contains("unverified.stack.invalid"))) and
  (map(select(.id == 4)) | .[0].result | .isError and .structuredContent.error.code == "service_unavailable") and
  (map(select(.id == 5)) | .[0].result.structuredContent | .ok and all(.data.checks[]; .ready and .identity == "instance")) and
  (map(select(.id == 6)) | .[0].result.structuredContent.data | .exit_code == 0 and (.stdout | contains("3 passed"))) and
  (map(select(.id == 7)) | .[0].result.structuredContent | .ok and .data.confirmed) and
  (map(select(.id == 8)) | .[0].error.code == -32601)
' /tmp/mcp.out >/dev/null

echo 'MCP timeout kills descendants and permits the next request'
start=$(date +%s)
{
  echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"stack_exec","arguments":{"command":["sh","-c","sleep 20 & wait"],"timeout_secs":1}}}'
  echo '{"jsonrpc":"2.0","id":2,"method":"ping"}'
} | stack mcp >/tmp/mcp-timeout.out
(( $(date +%s) - start < 6 )) || fail 'MCP response exceeded the timeout'
jq -se 'length == 2 and .[0].result.structuredContent.data.timed_out and .[1].result == {}' /tmp/mcp-timeout.out >/dev/null
