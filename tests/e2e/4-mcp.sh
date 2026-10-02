# 4. MCP server: initialize, tools/list, exec with withheld endpoints, up, down.
export PATH=/opt/stack:$PATH
cd ~/appA
{
  echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"probe","version":"0"}}}'
  echo '{"jsonrpc":"2.0","method":"notifications/initialized"}'
  echo '{"jsonrpc":"2.0","id":2,"method":"tools/list"}'
  echo '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"stack_exec","arguments":{"command":["bash","-c","echo $DATABASE_URL"]}}}'
  echo '{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"stack_exec","arguments":{"command":["true"],"require":["postgres"]}}}'
  echo '{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"stack_up","arguments":{"ttl":"10m"}}}'
  echo '{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"stack_exec","arguments":{"command":["bash","-c","uv run pytest -q 2>&1 | tail -1"],"require_all":true}}}'
  echo '{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"stack_down","arguments":{}}}'
  echo '{"jsonrpc":"2.0","id":8,"method":"nope"}'
} | stack mcp > /tmp/mcp.out
jq -c 'select(.id==1) | .result | {protocolVersion, server: .serverInfo.name}' /tmp/mcp.out
jq -c 'select(.id==2) | [.result.tools[].name]' /tmp/mcp.out
jq -c 'select(.id==3) | .result.structuredContent.data | {exit_code, stdout, unverified}' /tmp/mcp.out
jq -c 'select(.id==4) | .result | {isError, code: .structuredContent.error.code}' /tmp/mcp.out
jq -c 'select(.id==5) | .result.structuredContent | {ok, checks: [.data.checks[] | {service, identity}]}' /tmp/mcp.out
jq -c 'select(.id==6) | .result.structuredContent.data | {exit_code, stdout}' /tmp/mcp.out
jq -c 'select(.id==7) | .result.structuredContent | {ok, confirmed: .data.confirmed}' /tmp/mcp.out
jq -c 'select(.id==8) | .error' /tmp/mcp.out
