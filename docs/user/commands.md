# Command reference

| Command | Does |
|---|---|
| `stack compile [--update \| --locked] [--reassign-ports]` | Resolve, lock versions and [artifact checksums](#artifact-checksums), assign ports, write provider config |
| `stack inspect [--all-skills]` | Show the composed stack, origins, ports and the pinned releases' [agent skills](skills.md); writes nothing |
| `stack install` | Install the locked tools and service binaries; start nothing, record nothing |
| `stack up [--ttl 30m] [--owner-pid N] [--timeout D]` | Start services, verify them, record a session |
| `stack status` | Verify every service now; session and lease state (exit 1 if unhealthy) |
| `stack restart [service...] [--timeout D]` | Restart services of the running session (all when none named) and verify again |
| `stack run <task> [--timeout D] [-- args]` | Run a `[tasks.<name>]` command once every service verifies, granted the [secrets](secrets.md) it declares |
| `stack exec [--require S \| --require-all] [--timeout D] [--secret KEY]... -- <cmd>` | Run with tools and env; unverified endpoints poisoned; `--secret` grants a fnox secret |
| `stack logs <service> [--tail N] [--since-start]` | Last N lines (default 100, at most 10000) the supervisor kept for a service |
| `stack down` | Stop services and confirm they are gone |
| `stack renew` / `stack gc [--watch [--interval 60s]]` | Renew this session's lease / reclaim expired and deleted-project sessions machine-wide |
| `stack publish <dir> oci:<registry>/<repo>:<tag> [--force]` | Publish a bundle as an OCI artifact |
| `stack setup [--force]` | Download stack's pinned mise unless one is on `PATH` (`--force`: install it anyway) |
| `stack doctor` | Check mise, git and tar, Pitchfork's socket path, that the project compiles, that mise is new enough for its tool options and stack.lock, and fnox's value-free description when the stack uses fnox |
| `stack mcp` | MCP server (stdio) exposing the same operations |

All accept `-C <dir>` and `--json`. `exec -C` runs in the selected project directory.

- `inspect` before the first `compile` previews what compile would lock; afterwards it fails on drift.
- `inspect` and `compile` list the [agent skills](skills.md) of the releases stack.lock pins
  under `skills`. They install nothing and write nothing into the project. When mise cannot
  answer, every entry is `unavailable` with a `skills_unavailable` warning and the command
  still succeeds. `--all-skills` also lists Pitchfork's under `provider_skills`.
- With `[skills] dir` in stack.toml, `install` and `up` add a `skills` step that links available
  skills into that directory. Its problems are warnings, never failures. See
  [agent skills](skills.md).

- `install` is `up` without the start: compile in locked mode, trust the generated config, check
  the supervisor socket path, install every pinned tool and preset service binary, checked
  against stack.lock's artifact checksums where it has them (see
  [Artifact checksums](#artifact-checksums)). Use it to warm a checkout (CI caches, disposable
  worktrees) without a session; `exec` then has the tools.
- `exec` refuses with `tools_not_installed` (naming each release) when a release stack.lock
  pins is not installed, before anything runs, so neither the command nor anything it starts
  can pick up another release from `PATH`. Run `stack install`. `run` leaves installation to
  `mise run`, which installs what the task's configuration names.
- Errors from `up` that happen once its steps have begun end their `details` with a progress
  record, `{steps, retry_safe, changed}`; error-specific entries (such as each port conflict)
  come before it. Invalid arguments, an unreadable session, and a failed initial GC pass
  have no progress record. Without `--json` both are printed as plain text.
- Postgres and Redis presets get an additional TCP readiness check unless the service sets
  `ready_cmd` or `ready_port`. The supervisor runs the preset's command too and accepts the
  first successful check, so an open port can unblock a hanging preset probe. Stack then
  verifies instance identity through SQL or Redis commands before recording a session.
  Other presets keep their functional readiness checks.
- `up` and `restart` have an overall startup timeout, default `10m`; set `--timeout 30s`
  or another duration of at least 1s. It covers locks, GC, compilation and downloads,
  installation, stopping for restart, readiness and verification. Expiry reports `timed_out`
  and exits 124. Its first detail contains the cut-short error under `cause`; a progress
  record, when available, stays last. The provider request's process group is terminated,
  while the supervisor and services keep running with their ownership records retained.
  Allow up to 10s beyond the deadline to record a partial launch, plus connection-check overhead.
  Inspect `stack status`; retry `up` for a slow service, or run `stack down` before fixing and
  retrying a service that will never become ready. Verification also has its existing 90s
  window after the provider start returns; that window alone reports `not_ready`.
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
  than services to skip. `unknown_task` lists the tasks the project defines. The task runs from
  the configuration as it was when the run was planned, so a `compile` while it starts or runs
  does not change its body, env or tools; the next run sees the change. Its provider lock is
  rendered from the stack.lock that plan validated, never read from a `.config/mise/mise.lock`
  left in the project (missing in a fresh checkout, stale after a pull). Tasks and `exec`
  receive `STACK_SESSION` only while a session exists; an `[env]` that declares it is refused
  with `invalid_env`.
- `exec --json` captures at most 64 KiB of each stream into the result and exits with the
  command's code. When `--timeout` expires the command's process group is killed and stack
  exits 124; with `--json` the result is then `ok: false` with code `timed_out`, and the
  captured output is the error's only detail. Without `--json` the command keeps stdout and
  stderr; with `--timeout` it also runs in its own process group, so its stdin is empty, and
  interrupt, terminate and hangup signals are passed on to it.
- `exec --secret KEY` (repeatable) and a task's `secrets = [...]` grant named fnox secrets,
  resolved through the fnox release `stack.lock` pins once services verify. With `--json`,
  granted values are replaced by `[redacted:KEY]` in the captured output, and values under 8
  bytes are refused; without `--json` output reaches the terminal unredacted. See
  [secret grants](secrets.md).

- `restart [service...]` stops the named services (every service when none are named), waits
  until their recorded processes are gone and ports closed, starts them again and verifies the
  whole stack, like `up`. Other services keep running and the session keeps its id. Use it
  after editing code a running service loaded; `up` leaves an unchanged configuration's
  processes alone. Like `up`, it records an incomplete launch before starting anything and
  records each replacement process as the supervisor reports it, so a failed restart leaves
  the session unverified (run `up`) without losing track of what it started. It fails with
  `no_session` before `up`, `session_stale` when the configuration changed since `up` or the
  last start did not finish verifying (run `up`), and `session_busy` while commands run in
  the session. Over MCP, `services` must be an array of names; omitting it restarts every
  service.
- A service can list `watch = ["app.py", "src"]`: files or directories (walked recursively,
  skipping hidden, `node_modules`, `target` and `__pycache__` entries), relative to the
  project. When one changed after the service started, `status`, `exec` and `run` report it
  in the check's `changed_since_start` (and on stderr) with a `stack restart` hint. It is a
  note, not a verification failure. Each check examines at most 20000 directory entries and
  does not follow links to directories; beyond that it sets `watch_incomplete`. Modification
  times are compared to the millisecond. In a bundle, `{{bundle_dir}}` works in `watch` too.
- `logs` prints, on stderr, when the current process started. The supervisor keeps output
  across restarts; `--since-start` returns only the current process's lines (by the
  supervisor's whole-second timestamps; during a repeated hour when clocks go back it may
  include up to a second more). Processes started before 0.1.18 have no recorded start time
  until `restart`, or `up` starts a new process.
- `gc` fails with `gc_incomplete` if a session it reclaims could not be confirmed stopped;
  ownership records are kept so it can be retried. For a deleted (or replaced) project directory
  it queries Pitchfork using the recorded daemon IDs but never issues a stop-by-name request.
  Even matching PID/port metadata cannot make that separate request safe from replacement.
  Each uncertain service can include `recovery.inspect`, a copyable POSIX shell command for
  the recorded supervisor, or `ps` when only a PID was recorded. `recovery.stop` is offered
  only when the supervisor still reports the recorded PID and port. Run inspect first and
  stop only if the PID still matches; these two operations are not atomic. Then retry GC.
  GC holds the project's lock, so no stack command can start another generation meanwhile,
  but `pitchfork stop <id>` (Pitchfork 2.29.0) signals whatever process group runs under the
  id when it arrives: one started by hand with `pitchfork` or `mise daemons`, or a supervisor
  restart, would be stopped instead. Automatic cleanup needs a stop that takes the expected
  PID and refuses otherwise, which Pitchfork does not offer.
  Unknown, starting, stopping and retrying states retain the record. Data directories are kept.
- Malformed `stack.toml` and `bundle.toml` errors name the file, line and character column,
  with the offending line and a caret. JSON details repeat the location and the bundle source.
  Errors about a missing whole-document table have no specific location. Captured provider
  errors keep the final diagnostic lines with terminal escapes and spinner frames removed.
- `gc --watch` runs until terminated (or `--max-passes N`), one pass every `--interval`; with
  `--json` it prints one object per pass. stack installs no background service: run it under
  systemd, launchd or your agent runner if you want unattended expiry.
- `compile` fails with `resolve_failed` (before writing anything) when a version request matches
  no release or names an unknown tool. This happens at compile, earlier than the
  `install_failed` that `up` reports for a release that cannot be installed on this platform.
- `stack.lock` version 1 (stack 0.1.3 and earlier) has no exact versions. `stack compile`
  migrates it, keeping every bundle pin; `compile --locked`, `inspect`, `up`, `exec` and
  `status` refuse it with `lock_outdated` until then. Version 2 locks (exact versions, no
  artifact checksums) remain usable; see [Artifact checksums](#artifact-checksums). A stack release older than the
  lock it reads fails with `lock_invalid` ("written by a newer stack").
- `up` checks, before any download, that Pitchfork's supervisor socket
  (`$PITCHFORK_STATE_DIR`, else `$XDG_STATE_HOME/pitchfork` on Linux, else
  `$HOME/.local/state/pitchfork`, then `/sock/main.sock`) fits the platform's 104 (macOS) or 108
  (Linux) bytes, and fails with `socket_path_too_long` otherwise. On macOS Pitchfork ignores
  `XDG_STATE_HOME`; set `PITCHFORK_STATE_DIR` to a short directory instead.
- `publish` refuses to move an existing tag to different content (`tag_exists`) unless `--force`.
- Supported presets are postgres, redis, cockroachdb, nats and spicedb. Omitted service
  versions resolve `latest` once and get an exact pin. `prefix:` and `sub-` selectors resolve
  to releases too. Unknown presets fail in locked mode (`unlocked_service`). A service `version`
  or `preset` with template syntax (`{{`, `{%`, `{#`) is `invalid_service`, in any layer.
- `tools.<name>` accepts a table with `version` and [allowlisted options](bundles.md#tool-options)
  (`mr_boxington` on `rust`; `pubkey`, `identity`, `identity_prefix`, `issuer` on packslip-backed
  tools). Anything else is `invalid_tool`. `install`, `up` and `doctor` fail with
  `provider_outdated` when an option needs a newer mise (2026.9.2).
- Requests that name no release (`system`, `path:`, `ref:`) remain explicitly nonreproducible
  and produce warnings for both tools and services. Damaged release pins fail with `lock_invalid`.
- Git sources accept only `ref=` and `dir=`; anything else is an error rather than ignored.
  Values are percent-decoded once, so `dir=a%26b` names the directory `a&b`.
- `up` and `exec` mark the generated mise config as trusted, so `run` commands from the bundles
  you use execute without mise's trust prompt. Review bundles as you would any dependency.
- `compile` also warns about service presets mise does not document (it currently documents
  cockroachdb, nats, postgres, redis and spicedb).
- When a project pins `python`, the generated environment sets `UV_PYTHON` to the locked
  release and `UV_PYTHON_PREFERENCE=only-system`, so `uv` selects the locked Python release,
  not another one from an active Conda environment or a build it manages itself; a virtualenv
  built on the locked release is still used. Set either variable in the project's `[env]` to
  choose otherwise.
- For a custom service, connect with `http://127.0.0.1:$<NAME>_PORT`. mise also sets
  `<NAME>_URL` to a Pitchfork proxy hostname (`https://<name>.<project>.localhost`), which only
  answers when Pitchfork's proxy is running.
- A custom service's `run` should `exec` its server (`run = "exec python3 -m http.server $PORT"`),
  so the supervisor stops the server itself rather than a wrapping shell.

## Artifact checksums

`stack.lock` version 3 embeds mise's own lock (`mise.lock`) under `[provider_lock]`. A machine
that downloads a locked release gets the bytes stack.lock records for its platform, or the
install fails naming the tool. Stack never hashes artifacts itself: mise records a checksum, and
for packslip-backed tools a signer, when it locks, and checks them when it downloads.

```toml
# stack.toml, project only (a bundle cannot set it)
[lock]
platforms = ["macos-arm64", "linux-x64", "linux-arm64"]   # the default
artifacts = "best-effort"                                             # or "required"
```

`platforms` uses mise's names, including qualifiers such as `linux-x64-musl`; `"current"` means
the compiling machine. The default leaves out `macos-x64`: Pitchfork publishes no Intel macOS
build, so stacks with services cannot run there. A tools-only project can list `macos-x64` (or
`"current"` on an Intel Mac) itself. mise looks a release up under one key per machine and backend: Node on
Alpine needs `linux-x64-musl` listed, while Bun's per-CPU and musl builds are locked with their
unqualified platform.

Coverage is reported for every pin and listed platform:

| State | Meaning |
|---|---|
| `verified` | the entry has a checksum and URL for the platform, plus a signer for packslip |
| `exempt` | the backend records no download URL and mise accepts it as is (`core:rust`, `cargo`, `go`, `gem`, `ubi`, ...) |
| `unsupported` | the backend locks a dependency graph stack does not carry (`npm`, `pypi`, `pipx`) |
| `missing` | no entry for the platform: mise publishes no artifact, could not lock it, or nothing was locked yet |

`compile --json` and `inspect --json` add `backend` and `artifacts` to each `versions[]` entry.
`status --json` and the `install` step of `install` and `up` report this machine's coverage.

`compile` locks only pins that need it (all of them under `--update`), in a scratch directory
under stack's cache, without evaluating anything else from the project. A committed checksum is
kept unless you run `compile --update`: an upstream difference is a warning, and under
`--update` each change is reported with the old value. When `--update` gets no fresh answer
(offline, for example) the committed value is kept and reported `retained`. With full coverage,
`compile` makes no `mise lock` call.

Locked operations never run `mise lock`. `install` and `up` render `.config/mise/mise.lock` from
stack.lock, then run `mise install --locked` for pins that are `verified` or `exempt` here and a
plain `mise install` for the rest. mise checks what it downloads: a release already installed on
the machine is not checked again.

| Code | When |
|---|---|
| `artifact_mismatch` | mise refused a download, signer or repository identity that differs from stack.lock. Verify upstream, then `compile --update` and review the diff if the change is expected. `details` give `expected`, `actual` and `url`, and `upstream` when mise compared the asset with GitHub's digest; mise's own advice to edit `mise.lock` is left out, since stack renders that file from stack.lock. A task whose tools `mise run` fails to install this way keeps mise's output and gets the same remedy as an `artifact_mismatch` warning (captured runs only) |
| `artifact_unlocked` | `artifacts = "required"` and a pin is `missing` or `unsupported` on a listed platform or on this machine, or this machine's platform is not listed. Nothing is written or installed. A pin `compile` just failed to lock usually has no artifact for that platform upstream: drop the platform, pin another release, or use `best-effort` |
| `artifact_lock_failed` | `mise lock` could not run, timed out, or left a lock stack cannot read; nothing was changed |
| `lock_invalid` | the embedded lock disagrees with the pins or is malformed; `compile --update` replaces it |
| `lock_outdated` | `artifacts = "required"` with a version 2 lock |
| `provider_outdated` | mise is older than 2026.9.16, which the embedded lock needs |
| `install_failed` | any other install failure, with mise's output |

Conda-backed releases, such as the Postgres preset (`conda:postgresql`), record every
dependency package mise downloads under `[provider_lock.conda-packages]`, one checksum per
package and platform: some 25 per platform for Postgres 17. They are what mise checks those
downloads against, so stack keeps them; listing fewer `[lock] platforms` is what makes the
section smaller.

Migration: version 2 locks stay valid under `best-effort`, with every pin `missing`. The next
`compile` writes version 3 and needs network access for `mise lock`. Because a session's
configuration includes stack.lock, the first `up` after migrating restarts services once.
`.config/mise/mise.lock` and `.config/mise/locks/` are generated; in a git checkout stack
excludes them (see [Install](install.md#first-run)).

[All docs](../README.md)
