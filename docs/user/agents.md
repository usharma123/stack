# Coding agents

Stack gives each checkout its own services, so agents working in parallel worktrees never
share a database or fight over a port. Agents can drive it two ways: the CLI with `--json`, or
`stack mcp`, a stdio MCP server with the same operations and the same results.

## Connect over MCP

```sh
claude mcp add stack -- stack mcp     # Claude Code
codex mcp add stack -- stack mcp      # Codex
```

Any other MCP client: launch the command `stack` with the argument `mcp`.

**Pass `dir` on every call.** One registered server is shared by every agent on the machine,
whatever checkout it works in, so the server never acts on its own working directory. Each tool
takes `dir`, the absolute path of the checkout. It may be left out only when the client
supports MCP roots and exposes exactly one: the server asks for `roots/list` after
`initialize` (and again on `notifications/roots/list_changed`) and uses that root. Otherwise a
call without `dir`, or with a relative one, fails with a `usage` error. Every result carries
the checkout it acted on as `project`, an absolute path. `stack_gc`, `stack_ps` and `stack_mv`
act machine-wide and need no `dir`.

| Tool | Does |
|---|---|
| `stack_status` | Checks every service now and renews the lease. Start here |
| `stack_up` | Compiles if needed, installs, starts and verifies services |
| `stack_run` | Runs a `[tasks.<name>]` once every service checks out |
| `stack_exec` | Runs any other command (an argv list) with the stack's tools and env |
| `stack_restart` | Restarts services after you edit code they loaded |
| `stack_logs` | Shows a service's recent output |
| `stack_down` | Stops everything and confirms it is gone |
| `stack_ps` | Lists every checkout on the machine, its services and data size (liveness only) |
| `stack_mv` | After a checkout moved, carries its records and data to the new path (`from`, `to`) |
| `stack_inspect`, `stack_compile`, `stack_install`, `stack_renew`, `stack_gc`, `stack_doctor` | Same as the CLI commands |
| `stack_skill` | Returns the [agent skill](skills.md) a pinned tool ships (CLI: `stack skill <tool>`) |

Each tool has a `title`, a short description that says when to use it, a description for
every argument, and the standard MCP hints: `readOnlyHint` for `stack_status`,
`stack_inspect`, `stack_logs`, `stack_skill`, `stack_doctor` and `stack_ps`,
`destructiveHint` for `stack_down` and `stack_gc`, and `idempotentHint` for those that are
safe to repeat (`stack_up`, `stack_down`, `stack_compile` and `stack_install` among them).

The server also offers this page as the resource `stack://guide`, takes `logging/setLevel`
(it warns, for example, when a client's roots cannot be used), negotiates the protocol revision
the client asks for (2025-11-25, 2025-06-18, 2025-03-26 or 2024-11-05, else the newest), and
answers a malformed message or an unknown method with the standard JSON-RPC error. Batches are
accepted only in 2025-03-26, the one revision that has them.

## The loop

```sh
stack up --ttl 30m --timeout 5m          # start and verify; safe to repeat
stack run test                           # a task from stack.toml
stack restart api                        # after editing code a running service loaded
stack restart --changed                  # or: every service whose `watch` files changed
stack exec --require-all -- <command>    # anything else
stack down                               # before you finish or remove the checkout
```

To save a round-trip, `stack exec --up -- <command>` and `stack run --up <task>` start the
stack first when it needs it (no session, a stale or expired one, or a service that does not
verify), and `stack exec --install` installs missing pinned tools instead of failing with
`tools_not_installed`. Over MCP these are `up`, `ttl` and `install` on `stack_exec` and
`stack_run`; what ran is listed under `prepared`.

A few things that are easy to get wrong:

- **Read connection details from the environment.** `DATABASE_URL`, `REDIS_URL`,
  `<NAME>_PORT` and, for your own services, `<NAME>_URL` (`http://127.0.0.1:<port>`) are set
  for every command. Ports differ per checkout, so never hardcode one.
- **Declare `depends_on` for a service that holds connections to another**, such as an API to
  Postgres. Then a restart or recovery of Postgres restarts the API too; without it, the API
  keeps its dead connection while every check still passes.
- **`watch` does not reload anything.** When a file listed in a service's `watch` changes,
  `status`, `exec` and `run` tell you to run `stack restart <service>`. `up` leaves a service
  with unchanged configuration running, so it will not pick up your edit either.
- **Prefer `--ttl` to `--owner-pid N`.** A lease tied to the shell that ran `stack up` ends as
  soon as that shell exits. To tie it to the agent itself, use `--owner-pid ancestor:claude`
  (MCP `owner_ancestor: "claude"`): Stack uses its nearest ancestor process with that command
  name, and fails with `owner_not_found`, listing the ancestors, if there is none.
- **Leases are reported structured.** `status` and `up` return `lease` with `kind`,
  `ttl_secs`, `expires_at` and, for an owner, `owner_alive`. `status`, `exec`, `run` and
  `renew` count as activity; `stack renew --ttl 1h` (MCP `ttl`) changes the TTL.
- **Port conflicts.** `stack up --reassign-ports` (MCP `reassign_ports`) moves this checkout's
  conflicting ports and continues; the other program is left alone.
- **Do not work around `artifact_mismatch`.** It means a download differs from what
  `stack.lock` records. Report it, and run `stack compile --update` only if the change is
  expected upstream.
- **`stack logs <service> --since-start`** shows only the current process's output.

## Reading results

With `--json`, every command prints one object on stdout. MCP returns the same object as
`structuredContent`, and every tool's `outputSchema` describes it (`ok`, then `data` or
`error` with `code`, `message`, `hint` and `details`). `ok` says whether Stack did what you asked, not whether your command
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

`exec` and `run` take `--timeout`, counted from the call, so time spent compiling or
installing comes out of the command's share. On the CLI they have no default; over MCP they
default to 10 minutes, so an unattended call is always bounded. `up` and `restart` default to
10 minutes everywhere. Over MCP, give either `timeout`, a duration like the CLI's (`"90s"`,
`"5m"`), or `timeout_secs`, a whole number of seconds, but not both. When a deadline passes, Stack kills the command's process group and returns
`timed_out` (CLI exit 124), with the output so far in `error.details`.

A runner that cancels Stack with SIGTERM (or SIGINT, SIGHUP) cancels the command: Stack passes
the signal on to the command's process group, waits up to 10 seconds, then kills the group; a
second signal kills it at once. With `--json` the result is still printed (`exit_code: null`
and `signal` for a command the signal ended). `stack mcp` does the same for the commands its calls are running,
answers those calls, starts nothing more, then exits. Send SIGTERM, not SIGKILL: Stack cannot pass a SIGKILL on.
A signal that comes while `up` or `restart` runs a service's `setup` cancels the start, even
when the setup handles it and exits 0: no further setup runs and nothing more is started. The
result is `cancelled` (CLI exit 128 plus the signal), and anything launched before stays
recorded for `stack status` and `stack down`. At any other point of `up` or `restart` the
signal ends Stack at once, as before; that launch record is kept too.

If `up` times out, services may still be starting. Check `stack status`. Retrying `up` lets
a slow service finish; a service that will never become ready needs `stack down` before you
try again.

## Long calls over MCP

The server reads requests while a call runs, so `ping`, `tools/list` and calls on other
checkouts are answered meanwhile. Calls on the same checkout run one at a time, in the order
they arrived.

- **Cancelling.** `notifications/cancelled` for a running call stops it the way its timeout
  would: its commands are killed with their process groups and no response is sent. A
  cancelled `stack_up` or `stack_restart` leaves what a timed-out one leaves: anything it
  launched stays recorded, so `stack_status` shows it, `stack_down` stops it and a retried
  `stack_up` keeps waiting for it. A call cancelled while it waits for its turn never runs.
- **Progress.** A call whose request carries `_meta.progressToken` gets
  `notifications/progress` as each lifecycle step starts and ends (`compile`, `install`,
  `start`, `verify` and the rest of `steps`), and a heartbeat every 10 seconds while a step
  such as an install or a readiness wait takes long.

## MCP arguments

Each call is checked against the tool's `inputSchema` before anything runs. An unknown
argument (`secret` instead of `secrets`, `required` instead of `require`), a wrong type, a
missing required argument or a conflicting pair (`timeout` with `timeout_secs`) is a `usage` error, and the hint lists the
accepted names. `command` must be an argv list such as `["sh", "-c", "echo hi"]`; a string is
refused, not split. `null` counts as omitted.

`stack_down` takes `purge: true`, like the CLI's `--purge`. `stack_ps` takes `no_size`, like
the CLI's `--no-size`. `stack_mv` takes `from` and `to`: absolute paths, or paths relative to
`dir` when it is given.

`stack_exec` takes `secrets: ["KEY"]` to grant named fnox secrets to one command. A name that
is not an upper-case variable name is `invalid_secret`, as `--secret` reports it on the CLI. `stack_run`
grants the task's declared `secrets`. Granted values show up as `[redacted:KEY]` in captured
output. A grant controls what Stack injects. It does not limit what the command can read on
its own. See [secrets](secrets.md).

See [what Stack guarantees](guarantees.md) for how services are verified and cleaned up.

[All docs](../README.md)
