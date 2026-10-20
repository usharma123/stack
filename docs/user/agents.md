# Coding agents

Stack gives each checkout its own services, so agents working in parallel worktrees never
share a database or fight over a port. Agents can drive it two ways: the CLI with `--json`, or
`stack mcp`, a stdio MCP server with the same operations and the same results.

## Connect over MCP

```sh
claude mcp add stack -- stack mcp     # Claude Code
codex mcp add stack -- stack mcp      # Codex
```

Any other MCP client: launch the command `stack` with the argument `mcp`. Every tool takes a
`dir` argument naming the checkout.

| Tool | Does |
|---|---|
| `stack_status` | Checks every service now. Start here |
| `stack_up` | Compiles if needed, installs, starts and verifies services |
| `stack_run` | Runs a `[tasks.<name>]` once every service checks out |
| `stack_exec` | Runs any other command (an argv list) with the stack's tools and env |
| `stack_restart` | Restarts services after you edit code they loaded |
| `stack_logs` | Shows a service's recent output |
| `stack_down` | Stops everything and confirms it is gone |
| `stack_inspect`, `stack_compile`, `stack_install`, `stack_renew`, `stack_gc`, `stack_doctor` | Same as the CLI commands |
| `stack_skill` | Returns the [agent skill](skills.md) a pinned tool ships |

## The loop

```sh
stack up --ttl 30m --timeout 5m          # start and verify; safe to repeat
stack run test                           # a task from stack.toml
stack restart api                        # after editing code a running service loaded
stack exec --require-all -- <command>    # anything else
stack down                               # before you finish or remove the checkout
```

A few things that are easy to get wrong:

- **Read connection details from the environment.** `DATABASE_URL`, `REDIS_URL`,
  `<NAME>_PORT` and, for your own services, `<NAME>_URL` (`http://127.0.0.1:<port>`) are set
  for every command. Ports differ per checkout, so never hardcode one.
- **`watch` does not reload anything.** When a file listed in a service's `watch` changes,
  `status`, `exec` and `run` tell you to run `stack restart <service>`. `up` leaves a service
  with unchanged configuration running, so it will not pick up your edit either.
- **Prefer `--ttl` to `--owner-pid`.** A lease tied to the shell that ran `stack up` ends as
  soon as that shell exits.
- **Do not work around `artifact_mismatch`.** It means a download differs from what
  `stack.lock` records. Report it, and run `stack compile --update` only if the change is
  expected upstream.
- **`stack logs <service> --since-start`** shows only the current process's output.

## Reading results

With `--json`, every command prints one object on stdout. MCP returns the same object as
`structuredContent`. `ok` says whether Stack did what you asked, not whether your command
passed:

| Operation | Succeeded when |
|---|---|
| `exec`, `run` | `ok` is true and `data.exit_code` is 0 |
| `status` | `ok` is true and `data.healthy` is true |
| anything else | `ok` is true |

```python
def succeeded(operation, envelope):
    """operation: a CLI subcommand or MCP tool name; envelope: parsed stdout or structuredContent."""
    if not envelope["ok"]:
        return False  # error.code, error.hint and operation-specific details say what to do
    operation = operation.removeprefix("stack_")
    if operation in ("exec", "run"):
        return envelope["data"]["exit_code"] == 0
    if operation == "status":
        return envelope["data"]["healthy"]
    return True
```

- A command that exits 7 gives `ok: true` and `data.exit_code: 7`, and the CLI exits 7 too.
  `exit_code` is `null` when a signal ended the command; `data.signal` then names it, and the
  CLI exits 128 plus the signal (137 for SIGKILL), as a shell does.
- A `status` that finds a service down still gives `ok: true`, with `data.healthy: false` and
  `data.checks` saying which service and why. The CLI exits 1.
- `ok: false` (MCP `isError: true`) means Stack could not do it. Act on `error.code` and
  `error.hint`. `error.details` carries the specifics.
- A bad argument is `usage` with a hint, whether the parser or Stack itself rejected it (an
  unknown flag, `--ttl banana`, `--timeout 1ms`), and the CLI exits 2 for it.
- `exec` and `run` capture at most 64 KiB of each output stream.

## Timeouts

`exec` and `run` take `--timeout` (MCP `timeout_secs`), counted from the call, so time spent
compiling or installing comes out of the command's share. `up` and `restart` default to 10
minutes. When a deadline passes, Stack kills the command's process group and returns
`timed_out` (CLI exit 124), with the output so far in `error.details`.

A runner that cancels Stack with SIGTERM (or SIGINT, SIGHUP) cancels the command: Stack passes
the signal on to the command's process group, waits up to 10 seconds, then kills the group; a
second signal kills it at once. With `--json` the result is still printed (`exit_code: null`
and `signal` for a command the signal ended). `stack mcp` does the same for the command a call is running,
answers that call, then exits. Send SIGTERM, not SIGKILL: Stack cannot pass a SIGKILL on.

If `up` times out, services may still be starting. Check `stack status`. Retrying `up` lets
a slow service finish; a service that will never become ready needs `stack down` before you
try again.

## MCP arguments

Each call is checked against the tool's `inputSchema` before anything runs. An unknown
argument (`secret` instead of `secrets`, `timeout` instead of `timeout_secs`), a wrong type, a
missing required argument or a conflicting pair is a `usage` error, and the hint lists the
accepted names. `command` must be an argv list such as `["sh", "-c", "echo hi"]`; a string is
refused, not split. `null` counts as omitted.

`stack_exec` takes `secrets: ["KEY"]` to grant named fnox secrets to one command. A name that
is not an upper-case variable name is `invalid_secret`, as `--secret` reports it on the CLI. `stack_run`
grants the task's declared `secrets`. Granted values show up as `[redacted:KEY]` in captured
output. A grant controls what Stack injects. It does not limit what the command can read on
its own. See [secrets](secrets.md).

See [what Stack guarantees](guarantees.md) for how services are verified and cleaned up.

[All docs](../README.md)
