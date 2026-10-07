# Command reference

| Command | Does |
|---|---|
| `stack compile [--update \| --locked] [--reassign-ports]` | Resolve, lock, assign ports, write provider config |
| `stack inspect` | Show the composed stack, origins and ports; writes nothing |
| `stack install` | Install the locked tools and service binaries; start nothing, record nothing |
| `stack up [--ttl 30m] [--owner-pid N]` | Start services, verify them, record a session |
| `stack status` | Verify every service now; session and lease state (exit 1 if unhealthy) |
| `stack run <task> [--json --timeout D] [-- args]` | Run a `[tasks.<name>]` command once every service verifies |
| `stack exec [--require S \| --require-all] [--json --timeout D] -- <cmd>` | Run with tools and env; unverified endpoints poisoned |
| `stack logs <service> [--tail N]` | Last N lines (default 100, at most 10000) the supervisor kept for a service |
| `stack down` | Stop services and confirm they are gone |
| `stack renew` / `stack gc [--watch [--interval 60s]]` | Renew this session's lease / reclaim expired and deleted-project sessions machine-wide |
| `stack publish <dir> oci:<registry>/<repo>:<tag> [--force]` | Publish a bundle as an OCI artifact |
| `stack doctor` | Check mise, git and tar, Pitchfork's socket path, and that the project compiles |
| `stack mcp` | MCP server (stdio) exposing the same operations |

All accept `-C <dir>` and `--json`. `exec -C` runs in the selected project directory.

- `inspect` before the first `compile` previews what compile would lock; afterwards it fails on drift.
- `install` is `up` without the start: compile in locked mode, trust the generated config, check
  the supervisor socket path, install every pinned tool and preset service binary. Use it to warm
  a checkout (CI caches, disposable worktrees) without a session; `exec` then has the tools.
- `up` fails with `port_conflict` when a port assigned to this checkout accepts connections and
  no running daemon of that service is behind it. `details` name the service, the port, whether
  the project pinned it, and the holding process when `lsof` (or `/proc` on Linux) can tell.
  For an assigned port run `stack compile --reassign-ports` and `stack up` again; for a pinned
  port change or remove the pin in `[override.services]`, or free the port. Stack never kills
  the other program and never reassigns ports on its own.
- `down` succeeds when everything stack owns is stopped. A foreign listener on one of this
  checkout's reserved ports is listed under `conflicts` in the result, not waited for. What
  stack owns is where a running daemon actually listens (asked of Pitchfork when the
  configured port changed under it, as after `--reassign-ports`) and the recorded port of a
  live recorded process; when that cannot be established, the port is waited for rather than
  assumed foreign.
- `logs` asks the supervisor (`mise daemons logs`) for one service's stored output, bounded to
  `--tail` lines, 1 MiB and 30 seconds; it never follows. `unknown_service` names the services
  the project defines; `logs_failed` is a failed retrieval, whether the daemon was never
  started here, the provider exited nonzero, or the deadline passed; its message and `details`
  say which.
- `status --json` reports `healthy` (every service verified and the session current); the
  exit code is 1 exactly when it is false, while `ok` stays true because status itself worked.
  In a checkout with no launch record and no generated config yet (a fresh worktree), services
  are reported not launched without querying the supervisor. With a launch record, a missing
  generated config is written again from `stack.lock` and the services are verified as usual.
- `run <task>` runs a task with `mise run --skip-deps`: mise's task semantics (templates,
  shebangs, how arguments after `--` are passed) apply unchanged, but mise does not try to start
  the services itself. Every service must verify first, whatever the task's `services` list:
  mise hands a task every service's endpoint, including any stack would withhold. For commands
  that should run with services down, use `stack exec`. Stack's tasks have no dependencies other
  than services to skip. `unknown_task` lists the tasks the project defines.
- `exec --json` captures at most 64 KiB of each stream into the result and exits with the
  command's code (124 when `--timeout` expires). Without `--json` the command keeps the terminal.
- `gc` fails with `gc_incomplete` if a session it reclaims could not be confirmed stopped;
  ownership records are kept so it can be retried. For a deleted (or replaced) project directory
  it queries Pitchfork using the recorded daemon IDs but never issues a stop-by-name request.
  Even matching PID/port metadata cannot make that separate request safe from replacement.
  Inspect the intended daemon and stop it explicitly, then retry GC. Unknown, starting,
  stopping and retrying states retain the record. Data directories are kept.
- `gc --watch` runs until terminated (or `--max-passes N`), one pass every `--interval`; with
  `--json` it prints one object per pass. stack installs no background service: run it under
  systemd, launchd or your agent runner if you want unattended expiry.
- `compile` fails with `resolve_failed` (before writing anything) when a version request matches
  no release or names an unknown tool. This happens at compile, earlier than the
  `install_failed` that `up` reports for a release that cannot be installed on this platform.
- `stack.lock` version 1 (stack 0.1.3 and earlier) has no exact versions. `stack compile`
  migrates it, keeping every bundle pin; `compile --locked`, `inspect`, `up`, `exec` and
  `status` refuse it with `lock_outdated` until then. Older stack releases cannot read version 2.
- `up` checks, before any download, that Pitchfork's supervisor socket
  (`$PITCHFORK_STATE_DIR`, else `$XDG_STATE_HOME/pitchfork` on Linux, else
  `$HOME/.local/state/pitchfork`, then `/sock/main.sock`) fits the platform's 104 (macOS) or 108
  (Linux) bytes, and fails with `socket_path_too_long` otherwise. On macOS Pitchfork ignores
  `XDG_STATE_HOME`; set `PITCHFORK_STATE_DIR` to a short directory instead.
- `publish` refuses to move an existing tag to different content (`tag_exists`) unless `--force`.
- Supported presets are postgres, redis, cockroachdb, nats and spicedb. Omitted service
  versions resolve `latest` once and get an exact pin. `prefix:` and `sub-` selectors resolve
  to releases too. Unknown presets fail in locked mode (`unlocked_service`).
- Requests that name no release (`system`, `path:`, `ref:`) remain explicitly nonreproducible
  and produce warnings for both tools and services. Damaged release pins fail with `lock_invalid`.
- Git sources accept only `ref=` and `dir=`; anything else is an error rather than ignored.
  Values are percent-decoded once, so `dir=a%26b` names the directory `a&b`.
- `up` and `exec` mark the generated mise config as trusted, so `run` commands from the bundles
  you use execute without mise's trust prompt. Review bundles as you would any dependency.
- `compile` also warns about service presets mise does not document (it currently documents
  cockroachdb, nats, postgres, redis and spicedb).
- For a custom service, connect with `http://127.0.0.1:$<NAME>_PORT`. mise also sets
  `<NAME>_URL` to a Pitchfork proxy hostname (`https://<name>.<project>.localhost`), which only
  answers when Pitchfork's proxy is running.
- A custom service's `run` should `exec` its server (`run = "exec python3 -m http.server $PORT"`),
  so the supervisor stops the server itself rather than a wrapping shell.

[All docs](../README.md)
