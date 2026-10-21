# Command reference

| Command | Does |
|---|---|
| `stack init [--tool NAME[=VERSION]]... [--service NAME[=PRESET][@VERSION]]...` | Write a starting `stack.toml` (never replaces one) |
| `stack compile [--update \| --locked] [--reassign-ports]` | Resolve bundles and versions, write `stack.lock`, assign ports, write mise config |
| `stack inspect [--all-skills]` | Show the composed stack without writing anything |
| `stack install` | Install the locked tools and service binaries; start nothing |
| `stack up [--ttl 30m] [--owner-pid N] [--timeout D]` | Start services, verify them, record a session |
| `stack status` | Verify every service now (exit 1 if any fails) |
| `stack restart [service... \| --changed] [--timeout D]` | Restart services (all when none named) and verify again |
| `stack run <task> [--timeout D] [-- args]` | Run a `[tasks.<name>]` once every service verifies |
| `stack exec [--require S \| --require-all] [--timeout D] [--secret KEY]... -- <cmd>` | Run a command with the stack's tools and env |
| `stack logs <service> [--tail N] [--since-start]` | Show a service's recent output (default 100 lines) |
| `stack down [--stop-supervisor]` | Stop services and confirm they are gone |
| `stack renew` | Renew this session's lease |
| `stack gc [--reclaim-data] [--watch [--interval 60s]]` | Reclaim expired sessions and what deleted checkouts left, machine-wide |
| `stack publish <dir> oci:<registry>/<repo>:<tag> [--force]` | Publish a bundle to an [OCI registry](registries.md) |
| `stack setup [--force]` | Get mise, unless one is already on `PATH` |
| `stack doctor` | Check mise, git, tar, the supervisor socket path, and that the project compiles |
| `stack mcp` | Serve these operations over MCP (stdio); see [coding agents](agents.md) |

Every command accepts `-C <dir>` (the project directory) and `--json` (one result object on
stdout; see [reading results](agents.md#reading-results)).

## init

Writes `stack.toml` in the project directory with the tools and preset services you name, and
commented examples of a service of your own and a task. It never replaces an existing file
(`project_exists`) and writes nothing else: run `stack compile` next. A `stack.toml` link, even
one pointing nowhere, counts as an existing file and is never followed. The file appears whole
or not at all, and of several `init`s at once exactly one writes it; the others get
`project_exists`. A tool without a version is `latest`, pinned to an exact release at the first
compile.

```sh
stack init --tool python=3.13 --tool uv --service postgres@17 --service cache=redis
```

## compile

Resolves bundles and version requests, records exact versions and [artifact
checksums](#artifact-checksums) in `stack.lock`, and assigns this checkout's ports. Writes
nothing if resolution fails (`resolve_failed`).

- A plain `compile` resolves only new or changed requests. `--update` re-resolves everything
  and reports what moved. `--locked` resolves nothing and fails with `lock_outdated` if
  `stack.lock` is missing or stale. `up`, `exec` and `status` behave like `--locked`.
- `--reassign-ports` gives this checkout new ports, after a `port_conflict`.
- Requests that name no release (`system`, `path:`, `ref:`) work but are warned about:
  they are not reproducible.
- Git bundle sources accept only `ref=` and `dir=`.
- With `python` pinned, the environment sets `UV_PYTHON` and `UV_PYTHON_PREFERENCE=only-system`,
  so `uv` uses the locked Python. Set either in `[env]` to choose otherwise.

## inspect

Shows the composed stack: tools, services, ports, tasks, where each value came from, and the
[agent skills](skills.md) of the pinned releases. Before the first `compile` it previews what
would be locked; afterwards it fails if `stack.toml` has drifted from `stack.lock`.

## install

`up` without starting anything: it installs every pinned tool and service binary, checking
downloads against `stack.lock`. Use it to warm a checkout or a CI cache. `exec` needs the
pinned tools installed and fails with `tools_not_installed` otherwise, rather than run
whatever release `PATH` holds.

## up

Installs what is missing, starts every service, checks each one and records a session.
Running it again is safe: a service whose configuration has not changed keeps running, so to
load code you edited use `restart`.

- **Leases.** `--ttl 30m` expires the session after 30 minutes without use (`exec` and `renew`
  extend it). `--owner-pid N` ties it to a process that lives as long as your work.
- **Timeout.** The default is 10 minutes, covering downloads, installs and readiness. On
  expiry you get `timed_out` (exit 124). Services may still be starting: check `status`, retry
  `up`, or run `down` before fixing a service that will never become ready. A service whose
  `ready_cmd` does not pass is named in the hint with how it answers (`web's ready_cmd exits 7
  (curl: (7) Failed to connect …)`), and `details` keep its exit status and last output lines
  under `readiness`; the same goes for a failed `restart`.
- **Port conflicts.** If another program is listening on one of this checkout's ports, `up`
  fails with `port_conflict`, naming the port and, where it can, the program. Run
  `stack compile --reassign-ports` and `up` again. Stack never kills the other program and never
  reassigns ports on its own.
- **Postgres encoding.** A new Postgres cluster is created `UTF8` with locale `C.UTF-8`
  (code-point collation), whatever locale the shell running `up` has, so CI, `env -i` and
  containers get the same database as a laptop. A locale set in the project's `[env]` reaches
  Postgres too and takes precedence. An existing cluster keeps the encoding it was created
  with; when it is not `UTF8` (one created under `LC_ALL=C` by Stack 0.1.22 or earlier), `up`
  says so in `warnings`, with the commands to dump it and recreate it. Its data is never
  reinitialized. Redis likewise runs with `LC_ALL=C`, so a caller's locale the machine lacks
  (`LANG=en_US.UTF-8` in a minimal Linux image) cannot keep it from starting.
- **Recreated checkouts** start with empty data. If a checkout at the same path was deleted
  without `down`, its databases are moved to `<data dir>.replaced-<time>` (never deleted) and
  a warning names them. A service's data directory is mise's, under its state directory
  (`$MISE_STATE_DIR/daemons/<hash>/data/<service>`), not under `STACK_DATA_DIR`; `.stack/session.json`
  names it, and [`gc`](#gc) reports and reclaims a deleted checkout's. The same checkout keeps its data across `down` and `up`.
- **Result.** `--json` gives one check per service (port, readiness, identity) and the
  session's id and lease. The full record, with PIDs and data directories, is
  `.stack/session.json`.
- **Failures** after the steps begin end with a progress record, `{steps, retry_safe,
  changed}`, saying what was done and whether a retry is safe. Service output in an error is
  cut to each service's last lines; `stack logs <service>` has the rest.
- `up` marks the generated mise config as trusted, so commands from the bundles you use run
  without mise's trust prompt. Review bundles like any dependency.

## status

Checks every service now, not from a saved record: the process is alive, its port answers,
and, for Postgres, Redis and services with an [identity probe](identity-probes.md), that it is
this checkout's instance. `ok` stays true; `healthy` says whether everything passed, and the
exit code is 1 when it did not. A session whose lease expired is not healthy either, though
its services still answer: `lease_expired` says why, and that the next `gc` or `up` stops them
(`stack renew` or any `exec` renews a TTL lease).

## restart

Stops the named services (all when none are named), waits until they are gone, starts them
again and verifies the whole stack. Other services keep running. Fails with `no_session`
before `up`, `session_stale` when the configuration changed since `up` (run `up`), and
`session_busy` while commands are running in the session. `session_busy` (from `up` too)
lists each Stack process running a command (`details[].pid` and `command`). `down` is not
refused while commands run: it is how you stop everything, and those commands lose their
services.

`restart` also restarts every service that `depends_on` one it restarts, and starts nothing
else. A service it would restart that depends on one outside that set needs that service
running, verified and passing its readiness first: otherwise `restart` fails with
`dependency_not_ready`, naming it and why, before anything is stopped, set up or started.
`stack restart <that service>` restarts it with what depends on it; `stack up` starts every
service that is not running.

A service can list `watch = ["app.py", "src"]`. When one of those files changes after the
service started, `status`, `exec` and `run` report it with a `stack restart` hint. Nothing
restarts automatically: `stack restart --changed` restarts exactly the services whose watched
files changed (none, when nothing did), so an agent need not pick the names out itself.

## exec

Runs a command with the stack's tools and environment, in the project directory.

- Services that do not verify keep their variables, but with the host replaced by
  `unverified.stack.invalid`, so an app that falls back to `localhost` fails loudly instead of
  reaching another server. `--require <service>` or `--require-all` refuses to run instead
  (`service_unavailable`). Repeat `--require` for several services (`--require postgres
  --require redis`); `--require postgres,redis` names one service called `postgres,redis`.
- `--timeout` counts from the call. When it expires the command's process group is killed and
  Stack exits 124 (`timed_out`).
- With `--json`, at most 64 KiB of each output stream is captured into the result, and Stack
  exits with the command's exit code. A command a signal ended has `exit_code: null` and
  `signal: <number>`, and Stack exits 128 plus the signal, with or without `--json`.
- **Cancelling.** SIGINT, SIGTERM or SIGHUP sent to Stack is passed on to the command, and Stack
  waits for it rather than exit and leave it running. A command still running 10 seconds later,
  or when a second signal comes, is killed with its process group. Without a terminal (an agent
  or CI runner) the command has its own process group, so each signal reaches everything it
  started, and whatever is left of the group once it ends is killed. On a terminal the command
  stays in the terminal's foreground group (so it can read the terminal and gets Ctrl-C
  directly), and a SIGTERM sent to Stack alone is passed on to the command itself. `run` behaves
  the same. A SIGKILL cannot be passed on.
- `--secret KEY` (repeatable) grants a named fnox secret. See [secrets](secrets.md).

## run

Runs a `[tasks.<name>]` through `mise run`, so mise's task rules apply (templates, shebangs,
arguments after `--`). Every service must verify first, whatever the task's `services` list
says; for a command that should run with services down, use `exec`. A task gets the secrets it
declares, and installs any missing pinned tool before it starts. `unknown_task` lists the
tasks the project defines.

## logs

Prints up to `--tail` lines (default 100, at most 10000) that the supervisor kept for one
service. It never follows. `--since-start` keeps only the current process's output, which
helps after a restart. `unknown_service` lists the project's services; `logs_failed` means the
supervisor could not return them.

## down

Stops every service and returns once the processes are gone and their ports closed. It works
in a checkout that never ran `up`, such as a fresh clone with only `stack.toml` and `stack.lock`:
it writes the generated configuration from `stack.lock` first, then confirms nothing runs. Another
program listening on one of this checkout's ports is reported under `conflicts` and left
alone. A service process the supervisor started for this checkout but lost track of is
stopped too, listed with `"unrecorded": true`. Run `down` before deleting a checkout: Stack will not stop a deleted project's services
by name.

The Pitchfork supervisor that ran the services (and its `log-sink` helpers) keeps running
after `down`: every project using its state directory shares it. `--stop-supervisor` stops it
too, for a runner that gives each agent a root of its own, but only once this checkout's
services are confirmed gone, no other checkout's session on the machine records the same
supervisor, and no daemon it tracks (Stack's or anyone's) is running, waiting or stopping.
Otherwise it is kept, and `supervisor.reason` says why.

## gc

Stops sessions whose lease expired. For a checkout that was deleted, it drops the session
once its services are confirmed stopped. `--watch` repeats every `--interval` until stopped (with `--json`, one object per
pass). Stack installs no background service, so run `gc --watch` under systemd, launchd or
your agent runner if you want unattended cleanup.

A deleted checkout that has no session left (it ran `down` before it was removed, or `gc`
confirmed its services stopped) gets its port reservations released at once. Its services'
data, which mise keeps under its state directory (`$MISE_STATE_DIR/daemons/<hash>/data`, by
default `~/.local/state/mise`), is reported with its size and kept; `--reclaim-data` removes
it. Data is removed only when all of these hold, otherwise it is kept and the result says why:

- `up` recorded that directory for that checkout, and its `.stack-checkout` marker still names
  it (data a later checkout at the path took over, or data Stack never recorded, is never touched),
- it is the very directory `up` saw while its service ran: found where it resolved then
  (aliases such as macOS's `/tmp` and `/var` resolved), with no link on the way and the same
  device and inode. A directory above it replaced by a link, even to the same place, or a copy
  put in its place marker and all, is left alone; it is removed through the directory checked,
  never by looking its path up again. Data recorded before its directory existed is kept until
  an `up` sees it,
- the supervisor recorded with it confirms the service is not running, and
- no process carrying the checkout's token runs.

A path that holds a directory again keeps everything: `up` moves the old data aside there (see
[recreated checkouts](#up)). mise's own small state files for the project stay; `mise daemons
prune` removes them, along with the data of any other deleted project mise knows about.

When it cannot confirm that a deleted checkout's service stopped, `gc` fails with
`gc_incomplete` and keeps the record. Each such service comes with `recovery.inspect`, a
command that shows the recorded process, and sometimes `recovery.stop`. Run inspect first,
stop only if the PID still matches, then run `gc` again. The two steps are not atomic, which
is why Stack does not do this for you.

## doctor and setup

`doctor` checks mise, git and tar, that Pitchfork's socket path fits the platform limit, that
mise is new enough for the project's options and lock, and that the project compiles. Its
`stack` check names this release and binary, and warns when another `stack` comes first on
`PATH` (which hooks, agents and `stack mcp` registrations run) or when `stack.lock` was last
written by another release. `stack.lock` records that release as `written_by`; it is not part
of what the lock pins, so it never makes the lock outdated, and `compile` changes it only when
it rewrites the lock anyway (and otherwise warns when it differs). When
the stack uses fnox it also checks the declared secret names, without reading any values.

Pitchfork's socket lives under its state directory. On macOS, if `up` reports
`socket_path_too_long`, set `PITCHFORK_STATE_DIR` to a short directory.

`setup` downloads the mise release Stack is tested against, unless one is on `PATH`.
`--force` installs it anyway.

## Errors

Malformed `stack.toml` and `bundle.toml` errors name the file, line and column, and show the
line with a caret. Errors from mise keep its last diagnostic lines. A `stack.lock` written by a
newer Stack fails with `lock_invalid`; upgrade Stack.

## Artifact checksums

`stack.lock` embeds mise's own lock under `[provider_lock]`, with a checksum for every pinned
download on every listed platform. A machine that downloads a locked release gets exactly
those bytes, or the install fails naming the tool. Stack does not hash anything itself: mise
records checksums (and a signer, for packslip tools) when it locks, and checks them when it
downloads.

```toml
# stack.toml, project only (a bundle cannot set it)
[lock]
platforms = ["macos-arm64", "linux-x64", "linux-arm64"]   # the default
artifacts = "best-effort"                                 # or "required"
```

`platforms` uses mise's names, including qualifiers such as `linux-x64-musl`; `"current"` means
the compiling machine. `macos-x64` is left out by default because services cannot run there; a
tools-only project can add it.

`compile`, `inspect`, `install` and `up` report coverage for each pin:

| State | Meaning |
|---|---|
| `verified` | a checksum and URL are recorded for the platform |
| `exempt` | the backend records no download URL, and mise accepts that (`core:rust`, `cargo`, `go`, ...) |
| `unsupported` | the backend locks a dependency graph Stack does not carry (`npm`, `pypi`, `pipx`) |
| `missing` | nothing recorded for the platform, usually because upstream publishes no build |

A committed checksum only changes under `compile --update`, which reports every change. A
release already installed on the machine is not checked again.

| Code | When |
|---|---|
| `artifact_mismatch` | A download differs from `stack.lock`. Check upstream; if the change is expected, run `compile --update` and review the diff. `details` give `expected`, `actual` and `url` |
| `artifact_unlocked` | `artifacts = "required"` and a pin is `missing` or `unsupported` on a listed platform. Drop the platform, pin another release, or use `best-effort` |
| `artifact_lock_failed` | `mise lock` could not run or returned something unreadable; nothing changed |
| `lock_invalid` | The embedded lock disagrees with the pins; `compile --update` replaces it |
| `provider_outdated` | mise is older than 2026.9.16, which the embedded lock needs |
| `install_failed` | Any other install failure, with mise's output |

Postgres comes from conda, and every conda package it pulls in gets its own checksum per
platform (about 25 for Postgres 17). Listing fewer platforms keeps that section smaller.

[All docs](../README.md)
