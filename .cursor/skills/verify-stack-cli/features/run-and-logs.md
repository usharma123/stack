# Run and logs

Hand a task its arguments, and read a service's output each line once.

## Sub-features

`stack run` gives the task every argument after its name; `logs --json --cursor` returns new lines once (repeats included) with stamps, caps a read with `--tail` and says `more` only while whole lines are left, reports a `truncated` gap for a log cut by hand, and refuses an invalid cursor; `logs --follow` prints each new line once; `--project=PATH` selects a project's checkouts while `--project PATH` is refused with the spelling that works; a checkout directory named like a subcommand is not taken for one.

## How to get to it (user POV)

`stack run <task> [args]`, `stack logs <service> --json --cursor C [--tail N]`, `stack logs --follow`, `stack logs --project[=PATH]`, `stack -C <dir> exec`.

## Driving it with the native harness

Use the prerequisites in [the skill](../SKILL.md). Run `.cursor/skills/verify-stack-cli/drive.sh 14-run-logs-cursor` (or `pass2`).

A fixture service prints what each request asks for, so every expected line and repeat is known.

## Gotchas

The supervisor writes a line a moment after the service prints it: the scenario reads on from its cursor until the lines arrive, for at most `STACK_E2E_WAIT_SECONDS` (15 by default). Each `run` and `logs` call runs under `within` (`STACK_E2E_CALL_SECONDS`, 30 by default): a call that stalls is stopped and the scenario fails naming it, with the reads so far and the service log's tail. The `--follow` client is tracked and stopped however the scenario ends.
