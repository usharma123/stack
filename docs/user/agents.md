# JSON output and MCP

## Agent quickstart

In a checkout with a `stack.toml`:

```sh
stack up --ttl 30m --timeout 5m          # compile from stack.lock if needed, start, verify
stack run test                           # a [tasks.test] command, once every service verifies
stack restart api --timeout 1m            # after editing code a running service loaded
stack exec --require-all -- <command>    # anything else, with the stack's tools and env
stack down                               # before you finish or remove the checkout
```

Tools the stack pins may ship agent skills for exactly that release: `stack --json inspect`
lists them under `skills`, and MCP `stack_skill` returns one's text. See [agent skills](skills.md).

Every checkout, including each Git worktree, gets its own ports and data, so parallel agents
do not share databases. Connection strings arrive in the environment (`DATABASE_URL`,
`REDIS_URL`, `<NAME>_PORT`); do not hardcode ports. Prefer `--ttl` over `--owner-pid` unless
you can name a process that lives as long as your work: a shell that exits after the command
would end the lease at once.

Use `--json` for structured output. Stack emits an object on stdout with `ok`, and errors include a stable `code`, `hint`, and `details`. `stack gc --watch --json` emits one object per pass. `ok` alone does not say whether your command passed: see [reading results](#reading-results).

```sh
stack --json inspect
stack --json up --ttl 30m --timeout 5m
stack --json exec --require-all --timeout 5m -- python --version
stack --json down
```

`exec --json` captures at most 64 KiB from each output stream and exits with the command's exit code. On timeout it exits 124 with `ok: false` and code `timed_out`; the output so far is in `error.details`. Without `--json`, commands keep stdout and stderr, and `--timeout` works too.

`up` and `restart` default to a 10m startup deadline. Their CLI `--timeout` accepts durations
of at least 1s; MCP `timeout_secs` accepts a whole number of seconds of at least 1, default
600. The deadline includes locking, compilation, installation and readiness. Expiry gives
`timed_out`, CLI exit 124 and MCP `isError: true`. The first detail holds `cause`, the error
from the cut-short step; the progress record, if present, stays last. Recording a partial
launch can take up to 10s beyond the deadline, plus connection checks. Inspect `stack status`
and run `stack down` before abandoning a failed launch. Retrying `up` lets a slow service
continue starting; a permanently stuck service needs `down` before a fresh `up`.
Before it asks Pitchfork's supervisor to start or stop services, a startup under a deadline
starts the supervisor in a session of its own, so the deadline never ends the supervisor.
When that fails (`mise x -- pitchfork supervisor start` exits non-zero, or mise cannot be
found or run), the supervisor is not asked and the error is `start_failed`, `stop_failed`
or `provider_unavailable`; one still running at the deadline is left to finish.

`install` and `up` report this platform's artifact coverage in their `install` step
(`artifacts.verified`, `exempt`, `unsupported`, `missing`), and `compile`/`inspect` report it per
`versions[]` entry and platform. `artifact_mismatch` means mise refused a download or signer that
differs from stack.lock: do not work around it; report it, and only run `stack compile --update`
when the change is expected upstream. `artifact_unlocked` means the project requires coverage
that stack.lock lacks. A `verified` release already installed on the machine was not re-checked.

`stack up` does not restart a service whose configuration is unchanged, so after editing code a
service loaded, run `stack restart <service>`. Declare `watch = [...]` on the service and
`status`, `exec` and `run` tell you when that is needed. `stack logs <service> --since-start`
shows only the current process's output.

Start the stdio MCP server with:

```sh
stack mcp
```

Configure your MCP client to launch `stack` with the argument `mcp`. The server exposes Stack operations through the same structured result contract: `stack_inspect`, `stack_compile` (with `reassign_ports` after a `port_conflict`), `stack_install`, `stack_up`, `stack_restart`, `stack_status`, `stack_run`, `stack_exec`, `stack_logs`, `stack_renew`, `stack_down`, `stack_gc`, `stack_doctor`, and `stack_skill` (the `SKILL.md` of one skill `stack_inspect` lists as available, at most 64 KiB).

`stack_exec` takes `secrets: ["KEY", ...]` to grant named fnox secrets to one command;
`stack_run` grants exactly the task's declared `secrets` and refuses additions. Granted values
are replaced by `[redacted:KEY]` in `stdout` and `stderr` (also in a `timed_out` error's
details), the result lists the names under `secrets` and declined fnox removals under
`warnings`, and values under 8 bytes are refused with `secret_unsupported`. Failures are
`invalid_secret`, `secret_missing`, `secret_unavailable` and `secret_unsupported`; nothing fnox
printed is ever included. See [secret grants](secrets.md).

MCP execution is bounded on Unix: at most 64 KiB of each output stream is retained, and the
command's process group is terminated on timeout or completion. Detached children cannot keep
output collection waiting for EOF.

## Reading results

`stack --json` prints the same envelope that an MCP call returns as `structuredContent`. `ok`,
and MCP's `isError` with it, says whether the Stack operation worked, not whether your command
or services did:

| Operation | Succeeded when |
|---|---|
| `exec`, `run` (`stack_exec`, `stack_run`) | `ok` is true and `data.exit_code` is 0 |
| `status` (`stack_status`) | `ok` is true and `data.healthy` is true |
| anything else | `ok` is true |

- A command that ran and exited 7 gives `ok: true`, `isError: false` and `data.exit_code: 7`;
  the CLI exits 7 too. `data.exit_code` is `null` when a signal ended the command.
- `status` gives `ok: true` when it could check, even if a service is down: then
  `data.healthy` is false, `data.checks` says which service and why, and the CLI exits 1.
- `ok: false` (`isError: true`) means Stack did not do what you asked; act on `error.code` and
  `error.hint`. A command killed at its deadline is `timed_out` (CLI exit 124), and
  `error.details[0]` holds its `stdout`, `stderr` and `checks` so far.

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

Use `--require <service>` or `--require-all` when a command must have verified services. See [guarantees and limits](guarantees.md) for endpoint checks, leases, and cleanup behavior.

[All docs](../README.md)
