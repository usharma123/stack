# MCP

Drive Stack tools over a stdio MCP connection.

## Sub-features

Discover tools, start and stop services, run verified tasks, and enforce request timeouts.

## How to get to it (user POV)

`stack mcp` with JSON-RPC initialize, tools/list, and tools/call requests.

## Driving it with the native harness

Use the prerequisites in [the skill](../SKILL.md). Run `.cursor/skills/verify-stack-cli/drive.sh 4-mcp`.

Responses show verified service identity and confirmed shutdown. A timeout is followed by a successful ping.

## Gotchas

Use the fixture script to preserve request ordering and one JSON response per request.
