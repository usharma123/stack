# Leases

Limit how long a checkout's services remain running.

## Sub-features

Expire and renew TTLs, reap expired sessions, and tie service lifetime to an owner process.

## How to get to it (user POV)

`stack up --ttl`, `stack renew`, `stack up --owner-pid`, `stack gc`, and `stack status`.

## Driving it with the native harness

Use the prerequisites in [the skill](../SKILL.md). Run `.cursor/skills/verify-stack-cli/drive.sh 3-leases`.

JSON reports lease and reap outcomes, and process checks confirm stopped services exit.

## Gotchas

Use an idle host when investigating timing failures.
