# Service identity

Keep commands from using a service owned by another checkout.

## Sub-features

Reject a foreign PostgreSQL instance and withhold stale service endpoints.

## How to get to it (user POV)

`stack up`, `stack status --json`, and `stack exec --require postgres -- <command>`.

## Driving it with the native harness

Use the prerequisites in [the skill](../SKILL.md). Run `.cursor/skills/verify-stack-cli/drive.sh 2-wrong-instance`.

The unrelated server is rejected and the required command is refused. After changes to the endpoint, status reports stale and execution withholds the foreign address.

## Gotchas

The scenario starts a foreign fixture server on port 5432.
