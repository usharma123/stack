# Independent checkouts

Bring up two checkouts without sharing service ports or data.

## Sub-features

Compile, start, run a declared task, and connect using exported endpoints.

## How to get to it (user POV)

`stack compile`, `stack up`, `stack run test`, and `stack exec --require-all -- <command>`.

## Driving it with the native harness

Use the prerequisites in [the skill](../SKILL.md). Run `.cursor/skills/verify-stack-cli/drive.sh 1-independent-checkouts`.

Both checkouts run their task, use distinct ports and PostgreSQL data directories, and report the locked server version.

## Gotchas

The app fixture requires mise-managed Python, uv, PostgreSQL, and Redis.
