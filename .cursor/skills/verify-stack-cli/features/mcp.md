# MCP

Drive Stack tools over a stdio MCP connection.

## Sub-features

Discover tools, start and stop services, run verified tasks, and enforce request timeouts. Admission: calls on one checkout take turns in arrival order while other checkouts run alongside, a batch is answered once, a call cancelled while waiting never runs, every call accepted before input ends is answered, and a signal drops waiting calls while the running one is answered.

## How to get to it (user POV)

`stack mcp` with JSON-RPC initialize, tools/list, and tools/call requests.

## Driving it with the native harness

Use the prerequisites in [the skill](../SKILL.md). Run `.cursor/skills/verify-stack-cli/drive.sh 4-mcp`, and for admission `drive.sh 15-mcp-eof` (or `pass2`).

Responses show verified service identity and confirmed shutdown. A timeout is followed by a successful ping. In 15, each command appends to an order file, so which calls ran, and in what order, is read from it; the model behind it is `verify-stack-ports-mcp`.

## Gotchas

Use the fixture script to preserve request ordering and one JSON response per request.
