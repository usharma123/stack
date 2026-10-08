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
  under `skills`. To find them they run `mise ls --json` and `mise skills ls --json` (10s
  each) in a scratch directory under stack's cache that names only the pins, and remove it;
  they install nothing and write nothing into the project. When mise cannot answer, every
  entry is `unavailable` with a `skills_unavailable` warning and the command still succeeds.
  `--all-skills` also lists the provider's (Pitchfork's) under `provider_skills`.
- With `[skills] dir` in stack.toml, `install` and `up` add a `skills` step after installing
  that links available skills into that directory. Its problems are warnings (step status
  `warning`, top-level `warnings`), never failures. See [agent skills](skills.md).
- `install` is `up` without the start: compile in locked mode, trust the generated config, check
  the supervisor socket path, install every pinned tool and preset service binary, checked
  against stack.lock's artifact checksums where it has them (see
  [Artifact checksums](#artifact-checksums)). Use it to warm a checkout (CI caches, disposable
  worktrees) without a session; `exec` then has the tools.
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
  than services to skip. `unknown_task` lists the tasks the project defines.
- `exec --json` captures at most 64 KiB of each stream into the result and exits with the
  command's code. When `--timeout` expires the command's process group is killed and stack
  exits 124; with `--json` the result is then `ok: false` with code `timed_out`, and the
  captured output is the error's only detail. Without `--json` the command keeps stdout and
  stderr; with `--timeout` it also runs in its own process group, so its stdin is empty, and
  interrupt, terminate and hangup signals are passed on to it.
- `exec --secret KEY` (repeatable) and a task's `secrets = [...]` grant named fnox secrets,
  resolved through the fnox release `stack.lock` pins once services verify. Names are checked
  before any provider call (`invalid_secret`). With `--json` granted values are replaced by
  `[redacted:KEY]` in the captured output before it is bounded, and values under 8 bytes are
  refused (`secret_unsupported`); without `--json` the command's output reaches the terminal
  unredacted. The result lists granted names under `secrets` and declined removals under
  `warnings`. See [secret grants](secrets.md) for the rules, codes and boundary.
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
  to releases too. Unknown presets fail in locked mode (`unlocked_service`).
- `tools.<name>` accepts a table with `version` and [allowlisted options](bundles.md#tool-options)
  (`mr_boxington` on `rust`; `pubkey`, `identity`, `identity_prefix`, `issuer` on packslip-backed
  tools). Anything else is `invalid_tool`, as is template syntax (`{{`, `{%`, `{#`) in a version
  or option string, which mise would evaluate. `compile` asks `mise registry` once per registry name
  that carries packslip options. Version reports (`inspect`, `compile`, `install`) list a pin's
  `options`. `install`, `up` and `doctor` run `mise version` when an option needs a newer mise
  (2026.9.2) and fail with `provider_outdated` before installing anything.
- `compile` resolves each version request in its own scratch directory under stack's cache
  (`resolve/`), configured with that one tool only, and removes it afterwards. mise records
  each such configuration among its tracked configs; the entries point at removed files.
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

[All docs](../README.md)

## Artifact checksums

`stack.lock` version 3 embeds mise's own lock (`mise.lock`) under `[provider_lock]`, so a
machine that downloads a locked release gets the bytes stack.lock records for its platform, or
the install fails naming the tool. Stack never hashes artifacts itself: mise records a checksum
(and for packslip-backed tools a signer and repository identity) when it locks, and checks them
when it downloads.

```toml
# stack.toml, project only (a bundle cannot set it)
[lock]
platforms = ["macos-arm64", "macos-x64", "linux-x64", "linux-arm64"]   # the default
artifacts = "best-effort"                                             # or "required"
```

`platforms` uses mise's names (`linux-x64-musl` and the other qualifiers mise accepts are
allowed); `"current"` means the compiling machine's platform. Entries for platforms no longer
listed are dropped at the next `compile`.

Coverage is derived from stack.lock for every pin and listed platform, never stored:

| State | Meaning |
|---|---|
| `verified` | the entry has `checksum` and `url` for the platform, plus `signer` for packslip |
| `exempt` | the backend records no download URL and mise's `--locked` accepts it as is (`core:rust`, `core:swift`, `core:dotnet`, `cargo`, `go`, `gem`, `ubi`, `spinel`, `asdf`) |
| `unsupported` | the backend locks a dependency graph in a sidecar file stack does not carry (`npm`, `pypi`, `pipx`); never installed with `--locked` |
| `missing` | no entry, or none for the platform (mise publishes no artifact, could not lock it, or nothing was locked yet) |

`compile --json` and `inspect --json` add `backend` and `artifacts` to every `versions[]`
entry: `artifacts.<platform>` has `state`, and `checksum` and `signer` when verified.
`status --json` and the `install` step of `install` and `up` report this platform's coverage as
`artifacts: { platform, verified, exempt, unsupported, missing }` (entries are `tool@version`).

`compile` (ordinary and `--update`):

- Locks only pins that need it: every pin under `--update`, otherwise pins with a `missing`
  state on a listed platform (new and changed pins have no entry yet). With full coverage it
  makes no `mise lock` call and needs no network for artifacts. A pin mise cannot lock (redis
  today, or Pitchfork on macos-x64, which has no artifact) stays `missing`, so every ordinary
  `compile` of such a project asks mise again; `mise lock` skips it without failing.
- Runs `mise lock --platform <list> <tools>...` in a scratch root of its own under stack's cache
  (`lock/`), removed afterwards. Its configuration is `[tools]` with every pin at its exact
  version (service pins as their preset tool) and nothing else, so no `[env]` template or task
  of the project's is evaluated. The tools being locked start with no entry, so whatever the run
  leaves for them is fresh. Bounded by 10 minutes and any command deadline.
- Merges against what stack.lock commits. A committed value is kept unless `--update`: a
  different upstream value is a warning (`artifacts.<tool>@<version>.<platform> differs
  upstream; run stack compile --update to accept`) and `change: "differs_upstream"`. Under
  `--update` the new value replaces it and is reported as `change: "artifact_changed"` with
  `checksum_was`, `url_was` and `signer_was`. When `--update` gets nothing fresh for a committed
  value (offline, skipped) the value is kept and reported `change: "retained"` with mise's reason
  if it gave one, never as refreshed. New values are `change: "added"`.
- Conda dependency records (`conda-packages`) shared between tools follow the same rule per
  (platform, package) and are reported under `deps` on each pin that references them. Records no
  entry references are pruned; a `conda_deps` name without a record is `lock_invalid`.
- `missing` platforms carry `reason`, mise's own line, when the run gave one. `mise lock`
  exiting nonzero is reported as a warning, not a failure: it still writes what it could.
- `npm` and Python (`pypi`/`pipx`) sidecar references are removed; stack.lock records which
  releases had one (`stack_sidecars`), so they stay `unsupported`.
- Fields and tables mise adds that stack does not know are carried as written.

Locked operations (`compile --locked`, `inspect`, `install`, `up`, `exec`, `status`) never run
`mise lock`. `compile`, `install` and `up` write `.config/mise/mise.lock` from stack.lock
(everything under `[provider_lock]` but `provider` and `stack_sidecars`) before mise reads it;
`exec`, `status` and `inspect` do not. `install` and `up` then run `mise install --locked
<tools whose coverage here is verified or exempt>` and plain `mise install <the rest>`, skipping
an empty call. A tool name pinned at several versions goes in the locked call only when every
version qualifies. The step detail lists both (`locked`, `plain`) and states the boundary below.
mise checks recorded checksums in the plain call too; `--locked` additionally refuses a release
with no URL for the platform.

Boundary: mise checks what it downloads. A release already installed on the machine is
reported installed and not checked again, so `verified` describes the policy applied, not the
bytes on disk; a fresh machine or tool store is where the check applies.

Errors:

| Code | When |
|---|---|
| `artifact_mismatch` | mise refused a download (checksum) or a packslip signer or repository identity that differs from stack.lock. `details`: `[{ kind: "checksum" \| "signer" \| "repository", name, platform?, expected?, actual?, url? }]`, then mise's output. Verify upstream; `compile --update` and review the diff if the change is expected |
| `artifact_unlocked` | `artifacts = "required"` and a pin is `missing` or `unsupported` on a listed platform, or this machine's platform is not listed (`state: "unlisted"`). `details`: `[{ name, platform, state, reason? }]`. Nothing is written or installed |
| `artifact_lock_failed` | `mise lock` could not run, timed out, or left a lock stack cannot read. stack.lock, the provider config and the rendered lock are unchanged |
| `lock_invalid` | embedded entries disagree with the pins (an entry for a version stack.lock does not pin), a malformed `[provider_lock]`, a dangling `conda_deps` name, or a value with template syntax (`{{`, `{%`, `{#`). `compile --update` replaces a bad embedded lock |
| `lock_outdated` | `artifacts = "required"` with a version 2 lock |
| `provider_outdated` | mise older than 2026.9.16, which the embedded lock needs; checked by `compile`, `install`, `up` and `doctor` before anything is written |
| `install_failed` | any other install failure, with mise's output |

Migration: version 2 locks stay valid for locked operations under `best-effort`, with every pin
`missing`; the next `compile` writes version 3 (and needs network access for `mise lock`) and
rewrites `resolved_on` in mise's platform names. Under `required` a version 2 lock is
`lock_outdated`. A session's configuration digest includes stack.lock, so the first `up` after
migrating restarts services once. Keep `.config/mise/mise.lock` and `.config/mise/locks/` out of
version control with the generated config: they are rendered from stack.lock.

mise records each scratch configuration it loads among its tracked configs; the entries point
at removed files.

