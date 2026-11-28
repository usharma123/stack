# Command reference

| Command | Does |
|---|---|
| `stack init [--tool NAME[=VERSION]]... [--service NAME[=PRESET][@VERSION]]... [--detect]` | Write a starting `stack.toml` (never replaces one) |
| `stack compile [--update \| --locked] [--reassign-ports]` | Resolve bundles and versions, write `stack.lock`, assign ports, write mise config |
| `stack inspect [--all-skills]` | Show the composed stack without writing anything |
| `stack skill <tool> [--name N]` | Print a pinned tool's [agent skill](skills.md) |
| `stack install` | Install the locked tools and service binaries; start nothing |
| `stack up [--ttl 30m] [--owner-pid N \| ancestor:NAME] [--timeout D] [--reassign-ports] [--no-init \| --reinit S] [--from-snapshot NAME] [--label K=V]...` | Start services, verify them, run pending [init tasks](data.md), record a session |
| `stack status [--project[=PATH] \| --all] [--label K=V]...` | Verify every service now (exit 1 if any fails in a started checkout) and renew the lease; with a [fleet selection](#fleet-selections), of every matching checkout |
| `stack restart [service... \| --changed] [--timeout D]` | Restart services (all when none named) and verify again |
| `stack run [--timeout D] [--up [--ttl D]] <task> [args]` | Run a `[tasks.<name>]` once every service verifies |
| `stack exec [--require S \| --require-all] [--timeout D] [--secret KEY]... [--up [--ttl D]] [--install] -- <cmd>` | Run a command with the stack's tools and env |
| `stack logs [service...] [--tail N] [--since-start] [--since D] [--grep TEXT [-i]] [--follow \| --project[=PATH] \| --label K=V]` | Show services' recent output (default 100 lines each) |
| `stack env [--shell bash\|zsh\|fish]` | Print the environment `exec` would give a command, for `eval` |
| `stack hook direnv` | Print an `.envrc` snippet that loads `stack env` |
| `stack down [--keep-supervisor] [--purge] [--project[=PATH]] [--label K=V]...` | Stop services and confirm they are gone; the checkout's supervisor exits with them (for services an older Stack started under Pitchfork, its supervisor is stopped if Stack started it and nothing else uses it); `--purge` also removes their data |
| `stack renew [--ttl D]` | Renew this session's lease, or set its TTL |
| `stack snapshot save\|restore\|list\|rm [NAME]` | Save and restore service data, shared by every checkout of the project ([data](data.md)) |
| `stack gc [--reclaim-data] [--stop-orphans] [--dry-run \| --watch [--interval 60s]]` | Reclaim expired sessions and what deleted checkouts left, machine-wide |
| `stack ps [--no-size] [--project[=PATH]] [--state S] [--label K=V]... [--worktrees]` | List every checkout Stack has records for on this machine (read-only), or those selected |
| `stack mv <old> <new>` | After moving a checkout, carry its session, ports, tokens and data to the new path |
| `stack publish <dir> oci:<registry>/<repo>:<tag> [--force]` | Publish a bundle to an [OCI registry](registries.md) |
| `stack setup [--force]` | Get mise, unless a working one is already on `PATH`; one it installs comes first on `PATH` for Stack |
| `stack setup --gc-agent [--print \| --remove] [--enable]` | Write a launchd agent or systemd user unit that runs `gc --watch` |
| `stack completions <bash\|zsh\|fish\|elvish\|powershell>` | Print a shell completion script |
| `stack doctor` | Check mise, git, tar, and that the project compiles |
| `stack errors [--markdown]` | List every [error code](errors.md) with its meaning, default hint and whether a retry can succeed |
| `stack mcp [--profile core] [--allow-host-purge]` | Serve these operations over MCP (stdio); `--profile core` offers only the daily loop; `--allow-host-purge` lets `stack_down` purge checkouts selected by label. See [coding agents](agents.md) |

Every command accepts `-C <dir>` (the project directory), `--json` (one result object on
stdout; see [reading results](agents.md#reading-results)) and `--quiet` (`-q`).

**Finding the project.** Without `-C`, Stack uses the nearest `stack.toml` at or above the
working directory. It searches no higher than the Git worktree root (the directory holding
`.git`), or `$HOME` and then the filesystem root outside Git, so a parent checkout's
`stack.toml` is never picked up by mistake. If none is found, `manifest_missing` says where it
looked and suggests `stack init`, which then writes one in the working directory. With `-C`,
Stack uses exactly that directory and searches nothing.

**Progress.** On a terminal, `compile`, `install`, `up` and `restart` print each step to stderr
as it starts and ends (`stack: install ...`, `stack: install ok (41.2s)`). Nothing is printed
with `--json`, `--quiet`, or when stderr is not a terminal. `NO_COLOR` turns off the coloured
`ok`/`FAIL` markers.

## Exit codes

| Code | Means |
|---|---|
| 0 | Success |
| 1 | `status` found a service unhealthy in a started checkout (`ok: true, passed: false`) |
| 2 | Usage error: bad arguments or flag values (`usage`) |
| 70 | Any other Stack error (`ok: false`); `error.code` says which |
| 124 | Stack's own deadline passed (`timed_out`) |
| 128 + N | Signal N cancelled Stack, or ended the command `exec` or `run` ran |
| any | `exec` and `run` exit with the command's own code |

A command run by `exec` or `run` can itself exit 1, 2, 70 or 124, so the exit code alone
cannot say who failed. With `--json`, `ok` and `passed` can: `ok: true, passed: false` is the
command's failure, `ok: false` is Stack's. Through 0.1.22, Stack's own errors exited 1.

## Which command does what

`compile`, `install` and `up` overlap: each does everything the one before it does, then more.

| Command | Resolves versions | Installs | Starts | Verifies | Needs first |
|---|---|---|---|---|---|
| `compile` | New or changed requests (`--update`: all; `--locked`: none). Writes `stack.lock`, ports and the mise config | No | No | No | `stack.toml` |
| `install` | No: uses `stack.lock` (`lock_outdated` if stale) | Pinned tools and service binaries | No | No | Current `stack.lock` |
| `up` | No, like `install` | Whatever is missing | Every service (keeps unchanged ones running) | Every service, waiting until ready | Current `stack.lock` |
| `restart` | No | No | Named services (all when none named) | Every service | A session from `up` with unchanged configuration |
| `run` | No | The task's missing pins | Only with `--up` | Every service; all must pass | Current `stack.lock` |
| `exec` | No | Only with `--install` (else `tools_not_installed`) | Only with `--up` | Every service; unverified ones withheld, required ones refused | `install` or `up`, or `--install`/`--up` |
| `env` | No | No | No | Like `exec` | `install` or `up` |
| `status` | No | No | No | Every service, now | Nothing (reports `not started`, exit 0) |
| `down` | No | No | Stops every service | Confirms they are gone | Nothing |

In practice: `compile` after editing `stack.toml`, `up` to work, `install` to warm a cache or CI
image without starting anything.

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

`--detect` also adds what the directory's own files suggest, and lists each under `detected`
(`kind` `tool` or `service`, `name`, `value`, `source`) in the `--json` result:

| Files | Adds |
|---|---|
| `.python-version`, `pyproject.toml` (`requires-python`), `requirements.txt` | `python`: the version a version file names, or the `major.minor` a lower bound such as `>=3.12` names, else `latest` |
| `uv.lock`, or `[tool.uv]` in `pyproject.toml` | `uv` |
| `.nvmrc`, `.node-version`, `package.json` | `node`, at the version file's number when it is one (`20`, `22.11`) |
| `pnpm-lock.yaml`; `bun.lock` or `bun.lockb` | `pnpm`; `bun` |
| `rust-toolchain.toml` (a numeric `channel`), `rust-toolchain`, `Cargo.toml` | `rust` |
| `go.mod` | `go`, at its `go` directive |
| `.mise.toml` (or `mise.toml`) `[tools]`, `.tool-versions` | each plain pin (not `system`, a template or a backend such as `npm:`) |
| `compose.yaml`, `compose.yml`, `docker-compose.yaml`, `docker-compose.yml` | a preset service for each `image:` of Postgres (and pgvector, PostGIS), Redis (and Valkey), CockroachDB, NATS or SpiceDB, at the image tag's major version when it starts with one |
| `.env.example`, `.env.sample` | `postgres`, `redis` or `nats` for a `DATABASE_URL`, `REDIS_URL` or `NATS_URL` with that scheme |

A version file wins over a manifest, and a found version over none. `--tool` and `--service`
win over a detected entry of the same name, which is then not listed. Detection only reads
files at the top of the directory: it never runs project code, reads at most 256 KiB of a file
(a larger one counts as present, unread), and ignores a link that leads out of the directory.
Without `--detect` nothing is read and the result has no `detected` key.

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

## skill

`stack skill <tool> [--name N]` prints the `SKILL.md` of one `available` skill that `inspect`
lists under `skills`, at the release `stack.lock` pins; `--json` returns the same object as MCP
`stack_skill`. See [reading a skill](skills.md#reading-a-skill).

## install

`up` without starting anything: it installs every pinned tool and service binary, checking
downloads against `stack.lock`. Use it to warm a checkout or a CI cache. `exec` needs the
pinned tools installed and fails with `tools_not_installed` otherwise, rather than run
whatever release `PATH` holds.

## up

Installs what is missing, starts every service, checks each one and records a session.
Running it again is safe: a service whose configuration has not changed keeps running, so to
load code you edited use `restart`. When every pinned release is already installed, the
install step is skipped and says so (`skipped` in its detail); a pin that is missing installs
as usual.

- **Leases.** `--ttl 30m` expires the session after 30 minutes without use (`exec`, `run`,
  `status` and `renew` extend it). `--owner-pid N` ties it to a process that lives as long as
  your work: that very process, by PID, start time and boot, so a later process given its PID
  does not keep the session. `--owner-pid ancestor:<name>` finds that process for you: Stack walks up from its
  own parent and uses the nearest ancestor whose command name (or `argv[0]`) is `<name>`, for
  example `ancestor:claude` from a hook Claude Code runs. With no match it fails with
  `owner_not_found`, listing the ancestors it saw, and starts nothing. With neither flag, a
  project's `[session] default_ttl` (in `stack.toml`, e.g. `default_ttl = "30m"`) is the TTL;
  without that, the session lives until `stack down`. See [leases](#leases).
- **Timeout.** The default is 10 minutes, covering downloads, installs and readiness. On
  expiry you get `timed_out` (exit 124). Services may still be starting: check `status`, retry
  `up`, or run `down` before fixing a service that will never become ready. A service whose
  `ready_cmd` does not pass is named in the hint with how it answers (`web's ready_cmd exits 7
  (curl: (7) Failed to connect …)`), and `details` keep its exit status and last output lines
  under `readiness`; the same goes for a failed `restart`.
- **Port conflicts.** If another program is listening on one of this checkout's ports, `up`
  fails with `port_conflict`, naming the port and, where it can, the program. Pass
  `up --reassign-ports`, which gives only the conflicting services new ports and carries on;
  the result lists each move under `reassigned_ports` (`service`, `old`, `new`). Or run
  `stack compile --reassign-ports` (new ports for every service) and `up` again. A port the
  project pins in `[override.services]` is never moved. Stack never kills the other program,
  and without the flag never reassigns ports on its own.
- **Other checkouts.** `up` first reclaims expired sessions and those of deleted checkouts,
  machine-wide, but never waits for another checkout: a session another command holds locked
  (a long `up` in another worktree, say) is skipped and listed under the `gc` step's
  `skipped`. A problem reclaiming someone else's session never fails `up`: the `gc` step
  becomes a `warning` and the message is added to `warnings`.
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
- **Init tasks.** Once every service verifies, `up` runs the `init` tasks of each service whose
  data directory has not completed them, and reports them in its steps and under `init`. A
  failing one is `init_failed`, with services left running; the next `up` resumes from it.
  `--no-init` skips them; `--reinit <service>` runs a service's list again. See
  [service data](data.md).
- **Snapshots.** `--from-snapshot <name>` fills each data directory that does not exist yet from
  a snapshot before the first start. See [service data](data.md#snapshots).
- `up` marks the generated mise config as trusted, so commands from the bundles you use run
  without mise's trust prompt. Review bundles like any dependency.

## status

Checks every service now, not from a saved record: the process is alive, its port answers,
and, for Postgres, Redis and services with an [identity probe](identity-probes.md), that it is
this checkout's instance. `ok` stays true; `healthy` says whether everything passed, and the
exit code is 1 when it did not, unless nothing of the checkout is started (below). A session whose lease expired is not healthy either, though
its services still answer: `lease_expired` says why, and that the next `gc` or `up` stops them
(`stack renew` or any `exec` renews a TTL lease). When a service declares init tasks, `init`
gives each service's state (`done`, `pending`, `failed`, `stale` or `none`; see
[service data](data.md)).

**Not started.** A checkout with nothing of it started is not a failed check. `status` says so
only when it has looked, with the same evidence `down` confirms a stop with: no session, no
service process the supervisor reports, no command Stack started for it still running, a
command registry that can be read, and then (only in that case, so a checkout with a session
never pays for it) its supervisor's record read in full, whatever the manifest calls its
services now, and every process searched for the checkout's service token and command mark. In
practice that is a checkout that never ran `up`, or one stopped by a confirmed `down` (`down`
removes the session, so the two look the same). `status` then prints ``not started; run `stack
up` ``, exits 0, and reports `started: false` and `passed: true`. `healthy` is false (no
service was verified), except in a checkout that declares no services, where it is true as it
always was. The plain output still shows what is more than "not started": a service whose port
another program holds (with how to reassign it), init state, which outlives a session, and
warnings.

When any of that is there, `started` is true and a service that does not verify exits 1 as
before, with or without a session. Registered commands appear under `commands`; what the second
look turns up appears under `found`, and the plain output prints each as a `found` line: that
the last `stack down` here was not confirmed (see [below](#commands-still-running)), a process
carrying the checkout's environment (such as a helper a command left running), a service still running under
the checkout's supervisor, or why it could not look (a record that cannot be read).

The second look is best effort, like `down`'s own search. A process whose environment the
system hides from the start (on macOS, Apple's binaries such as `/bin/sleep`) and that `down`
never reported is seen by neither. The search reads each process once, so a handoff can slip
through it: a detached parent that exits after the process list was taken, leaving a new child
that was not in the list.

A supervisor record at this path may belong to another checkout: one that was moved away, or a
directory that was here before this one (another token). It does not make this checkout
started: `status` lists what it still runs under `other_checkout` (plain output: `other` lines)
and stays not started, exit 0. `stack up` here stops those services before starting this
checkout's own (its `stop_previous` step), and `stack down` here stops them too.

So the exit code of `status` alone does not say the services are up: to assert that, check
`data.started and data.healthy` with `--json`, or let the command that needs them say so with
`stack exec --require-all` (refused with `service_unavailable` otherwise), or start them first
with `--up`.

`commands` lists the commands Stack registered for this checkout (`exec`, `run`, MCP calls)
that are still running, also once the session is gone, from the [command
registry](#commands-still-running) alone. What a command left running after it ended is found
by `down`, and by `status` only in a checkout with nothing else started (see `found` above).
Each has the fields `down` reports. They do not make the checkout unhealthy, but one running makes it `started` (so not
"not started"); `down` is not confirmed while one runs.

Checking counts as using the session: `status` renews a lease that has not expired yet, like
`exec`. An expired lease is reported, not revived, and so is the lease of a session that is
stale or did not finish starting. The result carries a [`lease`](#leases) object.

### Endpoints

A service that verifies lists, under `endpoints`, the connection variables commands get for it,
with the values they get (in `up`'s and `restart`'s checks too): Postgres's `DATABASE_URL`,
`PGHOST`, `PGPORT`, `PGUSER` and `PGDATABASE`; Redis's `REDIS_URL`; CockroachDB's
`COCKROACH_URL`, `COCKROACH_HOST` and `CRDB_HTTP_PORT`; NATS's `NATS_URL`, `NATS_MONITOR_PORT`
and `NATS_MONITORING_URL`; SpiceDB's `SPICEDB_ENDPOINT`, `SPICEDB_HTTP_PORT` and
`SPICEDB_METRICS_PORT`; and `<NAME>_URL` and `<NAME>_PORT` of a service of your own. A project's
`[env]` value for one of them is what is listed. Only addresses appear: SpiceDB's preshared key
is left out, and a password inside a URL is shown as `***`. A service that does not verify has
no `endpoints`; what it would have had is under `withheld`.

Without `--json`, `up`, `restart` and `status` print a verified service's endpoints under its
line, the same masked values: its URLs, or every variable when it has none. A service without an
assigned port shows `port -`. A service that does not verify shows its reason and the variables
withheld from commands, never an address.

### Reason codes

A service that is not ready has a `reason` for people and a `reason_code` to branch on:

| `reason_code` | Means | Do |
|---|---|---|
| `not_launched` | Nothing in this checkout started it | `up` |
| `launch_incomplete` | The last `up` did not finish verifying it | `up` |
| `config_changed` | The compiled configuration changed since `up` | `up` |
| `process_changed` | Its process differs from the one `up` verified | `up` |
| `no_port` | No port assigned | `compile`, then `up` |
| `no_token` | No instance token assigned for its identity probe | `compile`, then `up` |
| `not_started` | The supervisor does not know it | `up` |
| `not_running` | The supervisor reports it stopped, failed or still starting | `logs`, then `restart` |
| `process_dead` | Its process is gone | `restart` |
| `wrong_port` | It runs on a port other than this checkout's | `down`, then `up` |
| `port_closed` | Its port does not accept connections | `logs`, then `restart` |
| `env_missing` | A connection variable it needs is not set | `compile`, then `up` |
| `probe_failed` | The identity check could not run or gave no answer | `logs`; fix the probe or the service |
| `wrong_instance` | The app's settings reach a different instance | `down`, then `up` |

## snapshot

`save <name>` copies this checkout's postgres and redis data, `restore <name>` replaces it,
`list` and `rm <name>` manage them. Only the data directory `up` recorded for this checkout is
copied or replaced. Running services, and those that depend on them, are stopped for the copy,
then started and verified again. A restore replaces every snapshotted service's data or none;
a failure lists what each service holds (`unchanged`, `rolled_back`, or `manual_recovery`
with where its old data is, `restore_incomplete`). Snapshots are shared by every checkout of the same
repository. See [service data](data.md#snapshots).

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
`dependency_not_ready`, naming it and why, before anything is stopped, set up or started. A
`ready_cmd` that cannot be run (no `sh` on the stack's `PATH`, say) has not passed: the error
says why it could not run.
`stack restart <that service>` restarts it with what depends on it; `stack up` starts every
service that is not running.

A service can list `watch = ["app.py", "src"]`. When one of those files changes after the
service started, `status`, `exec` and `run` report it with a `stack restart` hint. Nothing
restarts automatically: `stack restart --changed` restarts exactly the services whose watched
files changed (none, when nothing did), so an agent need not pick the names out itself.

## exec

Runs a command with the stack's tools and environment. It runs in the directory you called
Stack from when the project was found by searching upward, and in the project directory when
you pass `-C`. `STACK_PROJECT` always names the project directory, and `STACK_COMMAND_MARK`
marks the command as this checkout's (see [`down`](#commands-still-running)); `stack env` sets
neither mark, and `[env]` cannot set it.

- Services that do not verify keep their variables, but with the host replaced by
  `unverified.stack.invalid`, so an app that falls back to `localhost` fails loudly instead of
  reaching another server. `--require <service>` or `--require-all` refuses to run instead
  (`service_unavailable`). Repeat `--require` for several services (`--require postgres
  --require redis`); `--require postgres,redis` names one service called `postgres,redis`.
- `--timeout` counts from the call. When it expires the command is killed with everything it
  started that is still in its session, whatever process group that moved to (mise gives each
  task its own), and Stack exits 124 (`timed_out`). What called `setsid` to outlive the command
  (a daemon, a `stack up` service) is left running.
- Several `exec` calls in one checkout plan at the same time, so a test runner that fans out
  does not queue behind itself. `up`, `restart`, `down`, `compile` and `gc` still run alone:
  they wait for commands that are being planned, and new ones wait for them. `run`, and `exec`
  with `--up` or `--install` while it starts or installs, run alone too.
- With `--json`, at most 64 KiB of each output stream is captured into the result (the end is
  kept; `stdout_truncated`, `stderr_truncated`, `stdout_bytes` and `stderr_bytes` say what was
  dropped), and Stack exits with the command's exit code. A command a signal ended has `exit_code: null` and
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
- `--up` runs `stack up` first when there is no session, or it is stale (the configuration
  changed), expired, incomplete or has a service that does not verify. Otherwise it does
  nothing. The session keeps its lease (a TTL, or an owner still alive) unless `--ttl` gives
  it a new one. A session it starts without a lease to keep gets the project's `[session]
  default_ttl`, else 30 minutes, so a one-shot call never leaves services running forever. `up` gets what is left of `--timeout`, at most its usual 10 minutes. What it
  did is listed under `prepared` in the `--json` result (`step` `up`, with the `reason`), and
  on stderr otherwise; nothing appears when it did not run.
- `--install` installs pinned tools that are missing, as `stack install` would, instead of
  failing with `tools_not_installed`. It is listed under `prepared` too (`step` `install`,
  with the `missing` pins), only when it ran.

## run

Runs a `[tasks.<name>]` through `mise run`, so mise's task rules apply (templates, shebangs,
arguments after `--`). Every service must verify first, whatever the task's `services` list
says; for a command that should run with services down, use `exec`. A task gets the secrets it
declares, and installs any missing pinned tool before it starts. `unknown_task` lists the
tasks the project defines. `--up` and `--ttl` work as for [`exec`](#exec).

Stack's options, global ones such as `--json` included, go before the task's name. Every
argument after the name is the task's: `stack run test --timeout 3` gives the task `--timeout 3`,
while `stack run --timeout 5m test` bounds the run. A `--` right after the name only separates
(`stack run test -- -x` gives the task `-x`); any later `--` is passed on.

## logs

Prints up to `--tail` lines (default 100, at most 10000) that the supervisor kept for a
service. With no service named, or several, it shows each one with lines prefixed
`<service> | `; with `--json` the result is `{services: {<name>: {lines, ...}}, errors}`, where
`errors` lists services whose output could not be read. One named service keeps the
single-service result.

- `--since-start` keeps only the current process's output, which helps after a restart. The
  supervisor stamps each line with the millisecond it was read, so the cut is exact.
- `--since 10m` keeps only output from the last 10 minutes.
- `--grep TEXT` keeps lines containing `TEXT` (a plain substring, not a regex); `-i` ignores
  case. `--tail` then counts matching lines, out of the last 10000.
- `--follow` (`-f`) keeps printing new lines until Ctrl-C ends it, as it ends `tail -f`. Every
  second Stack reads what each log gained since the last read (and nothing else: no window is
  read again and nothing is compiled), so every line is printed once, repeated lines included;
  output a rotation or truncation took before it was read is said on stderr. Not with `--json`;
  use `--cursor` there.
- `--cursor CURSOR` (with `--json`; MCP `stack_logs` takes `cursor`) reads only what was written
  after the position a previous result gave. Every result carries `cursor` (one per service, and
  one for all of them in a several-service result); pass it back, or `""` to begin. A cursor read
  returns each line once, repeated lines included, adds `timestamps` (when the supervisor read
  each line, Unix milliseconds, 0 for a line it did not stamp), caps the lines per read at
  `--tail` (`more: true` says to read again), and reports `gap: "rotated"` or `gap: "truncated"`
  when output was lost between the cursor and what is kept. It compiles nothing and reads only
  the new bytes (at most 1 MiB per read). The cursor names the log file (device, inode, and the
  generation in the header line each file the supervisor makes begins with) and the byte offset
  after the last whole line read, with a check of that line, so a rotation is followed into the
  new file, and a new file that reuses the inode of one rotated away is never read from the old
  file's offset: the read reports `gap: "rotated"` and returns the oldest file kept from its
  start. Files written by an older Stack's supervisor have no header; there the check of the
  line before the offset tells a new file from the old one, except one holding the very same
  line at that offset, and a file shorter than the cursor is taken for one truncated. A cursor
  an older Stack gave (it begins `v1`) is still accepted, but cannot show its file is still
  kept: its first read returns the oldest file kept from its start with `gap: "rotated"`
  (lines already read may come again), and the cursor it gives back is a new one (`v2`).
  A line still being written is left for the next read, and so is one that the rest of a read's
  1 MiB cannot hold after a rotation (it comes whole, with its timestamp, on the next read). A
  cursor naming a service no project can have (a name with `/` or NUL, `.` or `..`) is a
  `usage` error. A cursor of several services reads those
  services, a service with no log yet included (its output appears once it has some); a service
  added to the project since needs a new cursor. Services an older Stack started under Pitchfork
  have no cursor reads until the next `up`.

The output is read from `logs/<service>.log` in the checkout's supervisor directory (see
[worktrees](worktrees.md#services-and-the-supervisor)); no process is started for it. Each log is
rotated at 16 MiB, one older file kept; at most the last 1 MiB is read. Each line is one record,
stamped with the millisecond the supervisor read it; a line longer than 64 KiB is stored as
several records, and output that ends without a newline is completed with one. The supervisor
holds at most 1 MiB of each service's output it has not written yet: a service's output waits
for room for up to a second (its writes block meanwhile, as on a slow terminal), then is
dropped, and the log says so (`[stack] N line(s) of output (B bytes) dropped`); the supervisor's
own lines never wait. A write that fails part way is cut back off the file where the filesystem
allows; where it does not (or a crash cut a record), the cut record's line is ended before
anything else is written, so the dropped-output note is always a record of its own, and that cut
record is the only one not as it was read. Logs are read and written only as regular files of
the checkout's own `logs` directory: a symbolic link in a log's place, or in the directory's, is
an error, never followed. For services an older
Stack started under Pitchfork, `logs` asks Pitchfork (`pitchfork logs`) until the next `up`.
`unknown_service` lists the project's services; `logs_failed` means the output could not be read
(or the service never started in this checkout).

## env

Prints the environment `stack exec` would give a command right now, as `export` lines for
`eval "$(stack env)"` (`--shell fish` for fish; the default follows `$SHELL`). It is planned
exactly like `exec`: pinned tools must be installed, every service is checked, and an
unverified service's connection variables are poisoned or unset just as `exec` does. It renews
a TTL lease as `exec` does, but records no command as running. A
comment, and a line on stderr, names each unverified service and what was withheld.
`--json` returns `{env, unset, withheld, skipped}`.

Stack's own mise selection variables (`MISE_*`) are left out (`skipped`), so the mise in your
shell keeps reading its usual configuration.

The output is a snapshot. Unlike `exec`, which checks services every time, a shell keeps these
values after a service stops or another checkout takes its port. Re-run `stack env` after
`up`, `restart` or `down`. See [worktrees](worktrees.md#your-shell-and-direnv) for shell and
direnv setups.

`stack hook direnv` prints an `.envrc` snippet that runs `stack env` and reloads when
`stack.toml`, `stack.lock` or the session changes.

## down

Stops every service and returns once the processes are gone and their ports closed, and, to
confirm, that no command Stack started for the checkout still runs (see [commands still
running](#commands-still-running)). It works
in a checkout that never ran `up`, such as a fresh clone with only `stack.toml` and `stack.lock`,
and confirms nothing runs there. Another program listening on one of this checkout's ports is
reported under `conflicts` and left alone. A service process the supervisor started for this
checkout but lost track of (its environment carries the checkout's token) is stopped too,
listed with `"unrecorded": true`.

A session record that cannot be read (left empty by a machine crash, say) makes every other
command in the checkout fail `state_invalid`. `down` then stops what the checkout's supervisor
recorded, with the same checks, and removes the record once that is confirmed
(`removed_unreadable`). With no supervisor directory there is nothing to stop, and the record
is cleared when there is no supervisor record, no port reserved for this checkout, and no
process seen carrying its token (a process whose environment cannot be read is not seen); commands
Stack started are looked for as in every `down` (see below). `down` reports exactly that (`recovery_checked`, and a `checked:` line). Without the supervisor's own record of this checkout, or with one that does not
account for every service, it stops nothing, keeps everything and says so. A record that
changes while `down` runs is kept and named.

The checkout's supervisor (see [worktrees](worktrees.md#services-and-the-supervisor)) stops each
service's process group (Postgres with `SIGINT`, the rest with `SIGTERM`, `SIGKILL` 10 seconds
later) and exits; `supervisor` is then `{"stopped": true, "state_dir": "<its directory>"}`. A
supervisor that was killed is not needed: `down` stops what it left by the recorded pid, start
time and process group. A checkout without services gets no `supervisor` field.

For services an older Stack started under Pitchfork, `down` asks Pitchfork to stop each one by its
recorded id, and once they are confirmed gone stops the Pitchfork supervisor that ran them, and
with it its `log-sink` helpers, when all of these hold:

- That Stack started it: the supervisor carries the mark of this Stack state Stack 0.1.23 and
  earlier kept in `<state dir>/supervisor-owner`. One without it, or whose environment cannot be
  read, is kept. So is every supervisor if that file holds anything but a mark Stack wrote.
- No other checkout's session in this Stack state records the same supervisor (its state
  directory compared after resolving links), or records services without naming their
  supervisor, and every session record can be read.
- No daemon it tracks, Stack's or anyone's, is anything but stopped, failed or errored, has a
  process still alive, or has a schedule (`cron` in its Pitchfork configuration), or one
  `pitchfork status --json` does not report in a way Stack can read.
- No other command holds its lock (`stack-supervisor.lock` in its state directory), which
  `down` waits up to 5 seconds to take alone.

Otherwise that supervisor is kept and `supervisor.reason` says why; that never fails `down`.
`--keep-supervisor` (MCP: `"stop_supervisor": false`) keeps it in every case; Stack's own
supervisor always exits with the checkout's services. `--stop-supervisor`, which turned this on
before it was the default, is still accepted and changes nothing.

### Commands still running

A command Stack started for this checkout (`exec`, `run`, an MCP `stack_exec` or `stack_run`
call) holds its environment: `DATABASE_URL`, the ports, and so on. `down` stops the services
whatever runs, but it does not report the checkout confirmed gone while such a command still
runs, and it never signals one. It fails `stop_unconfirmed` (exit 70, as when a service did not
stop), with the services stopped and the session removed. Each command is a detail with
`"kind": "command"`, so you can tell "wait for your command" from a service process that would
not stop (those details have no `kind`): its `pid`, `program` (the executable's name, never its
arguments), `started_by` (`exec`, `run <task>`, `mcp stack_exec`, `mcp stack_run <task>`, or
what Stack could tell of a process found by its environment), `running_secs` (that process's
own age) and `found_by` (`registry`, `mark`, `token` or `session`, below). A process of another
checkout holding this checkout's service token has `"another_checkout": true`. The hint names
the PIDs to wait for or stop, never an MCP server's or another checkout's (for those it says to
look at that checkout); then run `down` again. Every later `down` says the same
until they have exited, and `--purge` removes nothing meanwhile.

**What `status` learns from it.** A `down` that ends without confirming, for any reason, leaves
a marker for the checkout (`<state dir>/unconfirmed/`, keyed like the registry: when, and the
error); a `down` that confirms removes it. `status` reads it: in a checkout without a session,
the marker (or one that cannot be read) makes the checkout `started`, with a `found` line saying
the last `down` was not confirmed and to run `stack down` again, also when a registered command
already makes it started. So `status` never
says "not started" after an unconfirmed `down` until a `down` confirms, and it agrees with
`down`: once a later `down` confirms (what it reported exited, or that `down` can no longer
see it, a limit listed below), `status` is not started either. If the marker cannot be written,
the failing `down` says so in its message and in a detail with `"kind": "marker"`. If a `down`
that confirms cannot remove it, its result is still the confirmed one (with any purge), plus a
warning naming the marker's path in `warnings`: `status` keeps reporting the last `down` as
unconfirmed until the file is deleted. `up` reads it only to warn that what that `down` reported
may still run beside the services it starts; it does not clear it: a session then exists, which
`status` goes by, and the next `down` decides.

How Stack knows:

- **The command registry.** Every command is registered before it starts and removed when it
  ends, in `<state dir>/commands/`, keyed by the checkout directory itself (device, inode and
  creation time), not its path: it exists without a session, outlives `down`, follows a
  checkout moved with [`stack mv`](#mv), and a directory recreated at the same path does not
  inherit it. An entry names the command's own process by PID and start time, so a PID that is
  later given to another process never counts. Commands register while they plan, under the
  lock `down` holds, so none can slip in between `down`'s look and its answer. `status` lists
  exactly these, under `commands`. The registry's lock file stays after its last command ended, so
  its checkout's mark stays known (nothing removes it: it records no path for `gc` to check). A
  registry that cannot be read makes `down` unconfirmed (a detail with `"kind": "registry"`),
  and that `down` then stops only the services' own recorded processes and groups, nothing
  else; `status` reports `commands_error`. One that cannot be written stops the command from
  running. If the command's process could not be recorded once it started (a full disk, say)
  and the Stack process that started it is gone, whether it runs cannot be known: `down` stays
  unconfirmed (`"kind": "registry"`, naming how and when it was started) and `status` lists the
  other commands beside a `commands_error`, until you check it no longer runs and run `stack
  down --forget-unrecorded` (MCP `forget_unrecorded`), which removes such entries alone, under
  the registry's lock.
- **The command mark.** Each command gets `STACK_COMMAND_MARK`, and what it starts inherits it,
  also a child that daemonizes into a process group or session of its own and outlives the
  command. `down` (not `status`) also searches every process's environment for it, repeating
  until two searches agree so a process that forks and exits meanwhile does not hide its child.
  Only an environment Stack can read is seen: not another user's process, nor, on macOS, an
  Apple binary's such as `/bin/sh` or `/bin/sleep`. So a `/bin/sleep` that `sh -c 'sleep 60 &'`
  leaves behind after the shell exits is not found, and neither is a process that cleared its
  environment.
- **The service token.** A command a service of this checkout started (a service that runs
  `stack exec`) carries the checkout's service token too. `down` never signals a process the
  registry shows to be a command, or the Stack process running one, of any checkout, nor one
  carrying this checkout's mark or the mark of a checkout whose registry exists here (any other
  mark value counts for nothing), and it reports every such process holding this checkout's
  token: `down` is not confirmed while one lives. Each `down` decides once per process and keeps
  to it, for stopping and reporting alike. A leftover service process whose process group holds
  such a process is not signalled as a group: every other member is signalled one by one, while
  still in that group, and `down` is not confirmed while any of them survives. A worker that joins
  the group meanwhile is signalled only while it is in it and while a process seen in the group
  at the start (or found in it since) still holds the group's number; once none does, the number
  may name another group, so what it lists is reported, never signalled. The first listing of a
  leftover's group follows the same rule: it counts only while that leftover is seen leading the
  group right before and right after it. Such a process, and a worker that left the group, is a detail with `"unproven": true` and a `reason`: Stack could not
  show it belongs to this checkout, so check what it is before stopping it yourself.
- **The session (older Stack).** A command an older Stack started is not registered: Stack knows
  only the PID of the Stack process running it, from the session. While that process runs, the
  first `down` reports it (`started_by: "older stack"`, `found_by: "session"`) and nothing it
  started, and its hint names no PID to stop (it may be an older MCP server). That `down`
  removes the session, so a later one no longer knows of it: wait for that call to finish, or
  restart that session under this Stack.

Only the `down` process itself is left out, when it is the command (`stack exec -- stack
down`, also before the Stack process running it has recorded its PID). A shell that runs `stack down` and carries on (`stack exec -- sh -c 'stack down
--purge; more'`) counts: that `down` is not confirmed and purges nothing. Another checkout's
commands are never counted. To end the commands yourself, signal the PIDs the hint names; ending
a `stack exec` with SIGTERM passes the signal on to its command (see [cancelling](#exec)).

What `down` cannot rule out, so a confirmed `down` does not promise:

- A process a command left running whose environment Stack cannot read (another user's; on
  macOS, Apple's own binaries such as `/bin/sh` and `/bin/sleep`) or that cleared its
  environment, once the command that started it has ended.
- A process forked after `down`'s last search of process environments (two agreeing searches
  make it unlikely, not impossible).
- What a command an older Stack started left running, and that command itself once the first
  `down` removed the session.
- A command a running service starts inside that service's own process group: it ends with the
  service when the supervisor stops the group. (Stack's own `exec` and `run`, run without a
  terminal, put their command in a group of its own.)
- On a filesystem without creation times, a directory recreated at the same path that is given
  the same inode is the same checkout to Stack, and inherits its registry.
- While the registry cannot be read, a leftover service process that no supervisor record
  names is left running (that `down` is unconfirmed anyway).

Service data is kept, so the next `up` finds the same databases. `down --purge` (MCP:
`"purge": true`) also removes it once every service is confirmed stopped, and releases the
checkout's port reservations and identity tokens (the next `up` assigns new ones). It removes
only the data directories `up` recorded for this very checkout, and only as [`gc
--reclaim-data`](#gc) removes a deleted checkout's: the directory `up` saw, reached with no
link on the way, its `.stack-checkout` marker naming this checkout, the recorded supervisor
confirming the service is not running and no process carrying the checkout's token. Anything
else is kept, `purge.data[].kept` is true with the reason in `outcome`, and `purge.complete` is
false; `down` itself still succeeds. Data Stack never recorded, and data of an earlier
directory at the same path (moved aside by `up` as `.replaced-<time>`), is never removed.

## renew

Renews the session's lease. `--ttl 30m` sets its TTL (giving a session without a lease one,
and keeping an owner process if it has one); without it, a session that has no lease fails
with `no_lease`, whose hint says how to give it one.

## Leases

`status` and `up` report the lease as a `lease` object, the same one `ps` lists per checkout:

| Field | Meaning |
|---|---|
| `kind` | `ttl`, `owner_pid`, `ttl+owner_pid`, or `none` (no lease, or no session) |
| `ttl_secs` | The TTL, when there is one |
| `renewed_at` | When it was last renewed (Unix seconds; 0 without a lease) |
| `expires_at` | When a TTL lease runs out unless something renews it first (Unix seconds) |
| `owner_pid`, `owner_alive` | The owner process, and whether it is alive now (absent when that cannot be told) |
| `owner_identity` | `pid+start_time+boot`, or `pid` for a lease an earlier Stack recorded |
| `owner_unknown` | Why whether the owner runs cannot be told; the lease is kept meanwhile |
| `expired`, `expired_reason` | The lease ran out, and why; `gc` or the next `up` anywhere reclaims the session |

`status` keeps `lease_expired`, the reason with what to do about it, beside it.

An owner is the process the PID named when the lease was taken: `up` records its start time,
the boot it runs in and, on Linux, the PID namespace `up` saw it from. The lease runs out once
that process exits: no process has the PID, the PID now names a process with another start
time (a reused PID), or the machine restarted. When that cannot be told, the lease is kept, as
for a live owner (a TTL beside it still runs out): on Linux, when the PID namespace it was
recorded from is not this one or cannot be read (a container sharing `STACK_STATE_DIR` with
the host, say, where the PID names another process), and when a process has the PID but its
start time cannot be read, now or when the lease was taken. Whether any process has the PID is
asked of the kernel itself (`kill(pid, 0)`): another user's process counts as running though it
may not be signalled, and only no process at all, or a zombie, counts as exited. A probe that
fails otherwise (refused, an I/O error, a status that is hidden) is never taken for an exit: it
cannot be told, for a bare-PID lease too. A command that keeps the session's
lease (`exec --up`, `run --up`, `renew`) keeps its owner as recorded. A lease Stack 0.1.24 or
earlier recorded names its owner by PID alone (`owner_identity` `pid`): it stays alive while
any process has that PID, as before, until a command names an owner again.

Which lease a session gets, when the command starting it names none:

```toml
# stack.toml (project only; a bundle that sets it is bundle_invalid)
[session]
default_ttl = "30m"
```

| Started by | `--ttl` or `--owner-pid` given | Otherwise |
|---|---|---|
| `up` | That lease | `default_ttl`, else none (lives until `stack down`) |
| `exec --up`, `run --up` | `--ttl` replaces the lease | The session's own lease when it has one (a TTL, or an owner still alive), else `default_ttl`, else 30 minutes |

`default_ttl` takes a duration as `--ttl` does (`90s`, `30m`, `2h`, `1d`); anything else, or
zero, makes every command that reads `stack.toml` fail with `manifest_invalid`.

## gc

Stops sessions whose lease expired. For a checkout that was deleted, it drops the session
once its services are confirmed stopped. `--watch` repeats every `--interval` until stopped (with `--json`, one object per
pass; after `--max-passes`, the exit code is 70 if the last pass failed). Stack runs nothing in the background on its own. For unattended cleanup, run
[`stack setup --gc-agent`](#doctor-and-setup), or run `gc --watch` under your own supervisor or
agent runner.

`gc` decides from each session's record which ones look expired or belong to a deleted
checkout, and locks only those. If another command holds one of them, `gc` waits for it. A
record it cannot read, the index's or the checkout's own copy, is reported (`gc_incomplete`,
reason `session record unreadable`, naming the checkout where Stack can tell) and nothing of
that checkout is stopped, reclaimed or released; the other checkouts are handled as usual.
`--dry-run` lists it under `unreadable` and plans nothing for it; `ps` shows it as
`unreadable`. Run `stack down` in it.

`--dry-run` reports what `gc` would do and changes nothing: no supervisor query, no record
touched, no data directory opened, and no lock waited for (it takes and releases each Postgres
template's lock without waiting, to list only what a pass would try to remove). Each entry under `would_reclaim` has the
`session`, `project`, `reason`, `action` (`stop` for an expired lease, `release_if_stopped`
for a deleted checkout) and the recorded `services` (`port`, `pid`, whether that pid is alive,
supervisor id). `busy` means a command was running in it, so `gc` would leave it alone.
`deleted_checkouts` lists what deleted checkouts left: the `ports` that would be released and
the recorded `data` directories, each with `action` `report` (kept and reported), or
`remove_if_safe` with `--reclaim-data` (removed only if the checks below pass).
`after_session` marks a checkout whose session record is still kept: the rest goes only once
its services are confirmed stopped. `templates` lists the Postgres templates (see
[service data](data.md#postgres-templates)) a pass would try to remove (a pass can still leave
one in place, if it cannot be renamed): `dir`, `reason` and `bytes`;
one a build holds is left out, as a pass would skip it.
`gc` removes them without listing them in its result.

A checkout counts as deleted only when that can be shown: its path is not found and the
deepest directory above it that is there is on the very mount the checkout was on. With its
session, port reservations, identity tokens, data records and supervisor, Stack records the
filesystem's device and the mount: that mount's root (the highest directory above the checkout
on it), which directory the root is (device, inode, and birth time where the filesystem keeps
one) and, on Linux, the kernel's mount id with the boot and mount namespace it was read in
(Linux mount ids are reused once a mount is gone; the unique id of Linux 6.8 and later is
compared where both sides have it). The directory above the missing path must be on a mount
rooted at the same place, at the same directory; on Linux it must also be the same mount, by
id in the same boot and mount namespace, and by the root's birth time otherwise (with no birth
time to go by, a checkout deleted across a restart stays `unavailable`). So a lookup refused for
permission or failing for I/O, a volume that is not mounted right now (its directory under
`/Volumes` gone, or its mount point left empty), a bind mount that is not mounted (on Linux,
even one of a directory of the same filesystem, whose mount point has the same device), or a
directory at the path on another filesystem, shows nothing: such a checkout is `unavailable`,
and nothing of it is stopped, reclaimed, released or removed, not even an expired lease's
services. The same holds for a checkout whose session recorded it as the root of its own mount
when another directory is at its path on another mount: that is what the mount point looks like
while it is not mounted, so it is `unavailable` rather than `replaced`. `gc` lists it
(`left_alone`, with `stopped` false since nothing was stopped, the reason after `checkout
unavailable:`) without failing (Stack 0.1.24 and earlier said `stopped: true` there),
`--dry-run` under `unavailable`, `ps` with directory `unavailable`, and `up`'s pass under
`skipped` (`"skipped": "unavailable"`). After a reboot gives a filesystem another device number
(removable or network volumes, btrfs subvolumes), a checkout deleted from it stays
`unavailable` rather than being taken for deleted; one still there is still recognised by its
inode and the birth time its session recorded (with none recorded, the same inode under another
device may be another volume mounted there: `unavailable`, and `up` does not adopt its session).
A root birth time recorded on one side only (recorded but unreadable now, or readable now but
never recorded) is not taken for the same mount either.

Two things update where a record says its checkout is. A command run in the checkout (`up`, any
compile of it, the supervisor started for it) adopts the path's records for the checkout there
now and records its mount; where the mount cannot be learned it keeps what was recorded, unless
the checkout is now on another filesystem. Everything else, `gc` and other checkouts' commands
included, only observes: it brings a record up to date only where what is at the path is shown
to be on the very mount recorded (the same mount seen again after a restart gets its new ids),
so the empty mount point a volume or bind mount that is not mounted leaves at a checkout's path,
or another volume mounted there, never replaces the mount recorded, and once that mount point is
gone too the checkout is still `unavailable`, never deleted.

Records an earlier Stack kept (0.1.24 and earlier) name no mount. While their checkout is there,
a command run in it records its mount; `gc` completes them by observation only where that cannot
pass a mount point off as the checkout: data records whose checkout token is the directory's at
the path, and, where every mount has a device of its own (macOS), records whose device is the one
there. Once the checkout is gone they decide nothing on their own: one with only a device is
deleted only where every mount has a device of its own (macOS), never on Linux, where an unmounted
bind mount of the same filesystem leaves the same behind; one with no device at all is never
shown deleted, whatever its parent directory holds. On Linux an earlier Stack's session whose
path holds another directory on the same filesystem is `unavailable` rather than `replaced`, for
the same reason. Such a checkout stays `unavailable`, its reason saying so. To let Stack reclaim
it, first make sure it is not a volume or bind mount that is merely not mounted (mounting it
again is all that takes: it is found as it was). If it was deleted, recreate the checkout at that
path (its `stack.toml` with it) and run `stack up` there: that adopts the path's records for the
new checkout and records its mount if there is no conflicting session. `stack down --purge`
there then releases its ports and identities, or deleting the directory lets `gc` reclaim them.
If `up` reports `session_conflict` and `gc` still reports `unavailable`, the old session needs
manual recovery. Confirm its services are stopped before changing its records. A directory
recreated at the path and left alone does not give `gc` evidence about the original checkout.

A deleted checkout that has no session left (it ran `down` before it was removed, or `gc`
confirmed its services stopped) gets its port reservations released at once, and its
supervisor directory (records and logs) removed. Its services' data, which Stack keeps under
its state directory (`$STACK_STATE_DIR/data/<checkout key>/<service>`; data mise's presets kept
for an older Stack stays where it was, under `$MISE_STATE_DIR/daemons/<hash>/data`), is
reported with its size and kept; `--reclaim-data` removes it. Data is removed only when all of these hold, otherwise it is kept and the result says why:

- `up` recorded that directory for that checkout, and its `.stack-checkout` marker still names
  it (data a later checkout at the path took over, or data Stack never recorded, is never touched),
- it is the very directory `up` saw while its service ran: found where it resolved then
  (aliases such as macOS's `/tmp` and `/var` resolved), with no link on the way and the same
  device and inode. A directory above it replaced by a link, even to the same place, or a copy
  put in its place marker and all, is left alone; it is removed through the directory checked,
  never by looking its path up again. Data recorded before its directory existed is kept until
  an `up` sees it,
- nothing of it is on another mount: the directory is no mount point itself and nothing under
  it is one, a bind mount of the same filesystem included, whether of a directory or of a
  single file (Linux tells those apart by the kernel's mount id, from `statx`, else
  `/proc/self/fdinfo`; a kernel that gives neither keeps the data). Each directory is checked
  as it was opened and each other entry as named in the directory held open, not as a path
  names it. A mount found keeps all of the data, and one put there while the data is being
  removed stops the removal before it; either way the result says where, and the record is
  kept. Everything is checked before anything is removed: a directory that cannot be listed
  whole (`readdir` failing), or opened, keeps all of the data. Links inside are removed
  themselves, never followed,
- the supervisor recorded with it confirms the service is not running, and
- no process carrying the checkout's token runs.

A path that holds a directory again keeps everything: `up` moves the old data aside there (see
[recreated checkouts](#up)). mise's own small state files for the project stay; `mise daemons
prune` removes them, along with the data of any other deleted project mise knows about.

A deleted checkout's services that Stack's own supervisor runs are stopped by `gc` through
their recorded pid, process start time and group (never another process that took the pid).
For services an older Stack started under Pitchfork, when it cannot confirm that a deleted
checkout's service stopped, `gc` fails with `gc_incomplete` and keeps the record. Each such service comes with `recovery.inspect`, a
command that shows the recorded process, and sometimes `recovery.stop`. Run inspect first,
stop only if the PID still matches, then run `gc` again. The two steps are not atomic, which
is why Stack does not do this for you by default.

### Stopping orphans

`gc --stop-orphans` opts in to stopping such a service itself. It acts only where `gc` would
offer `recovery.stop`, and only when every recorded fact still matches:

1. the supervisor (asked by its recorded binary and state directory) reports the recorded
   daemon id as `running`, with the recorded pid and port;
2. the live process with that pid started when the record says: no earlier than 2 seconds
   before the recorded start (`started_at_ms`, taken just before the supervisor's start call)
   and no later than 10 minutes after it. Records without a start time (from before Stack
   0.1.18), and processes whose start time cannot be read, are refused;
3. a second supervisor query, made after the start-time check, still reports the same id,
   pid and port.

Then it asks the supervisor to stop that id and waits until the pid is gone and the port
closed. Anything else keeps the record and reports why, as without the flag. It cannot be
combined with `--dry-run`.

The remaining race: Pitchfork cannot compare and stop in one step, so it stops whatever runs
under the id when the stop arrives. If, in the moment between the last check and the stop,
the daemon is restarted under the same id (for example by a new checkout created at the same
path running `stack up`), that new process is the one stopped. Do not use `--stop-orphans`
while something may be starting services at a deleted checkout's path.

## ps

Lists every checkout this machine's Stack state has records of: a session, port reservations,
identity tokens or a data directory `up` recorded. For each: its path, whether the directory
still exists (`dir` is `present`, `missing`, `replaced`, or `unavailable` when that cannot be
shown: see [gc](#gc)), the session id and `state`, the
lease (see [leases](#leases)), and for each service its port, pid and
data directory, with the directory's size on disk. `state` is `running` (every service live),
`partial`, `stopped`, `launching`, `recorded` (a session without services) or `reserved`
(records but no session). `reclaimable` marks a checkout [`gc`](#gc) reclaims: its directory is
shown to be gone and no session of it is left, so gc releases its ports and reports its data, which
`--reclaim-data` removes where it is safe. `totals` adds it all up, with the data of reclaimable
checkouts in `reclaimable_bytes`.

The checks are liveness only, and the field names say so: `pid_alive` (a process with the
recorded pid exists) and `port_accepting` (something accepts connections on the port, which may
be another program). `ps` never checks identity; `stack status` in the checkout does. It takes
no lock, so it never waits behind a running `up`, and it needs no `stack.toml`. Measuring data
directories walks them, which can be slow for large databases: `--no-size` skips it
(`data_bytes` is then `null`).

Each checkout also says, where it can be read cheaply from files (no `git` is run per
checkout): `project_id` (one id for every worktree of a repository at the same subdirectory,
the identity [snapshots](data.md) share), `branch` and `head` (the 7-character commit) from its
Git directory, its session's `labels` (`up --label`), `commands` (commands Stack started there
still running, from the command registry), and `watch_changed` (live services whose `watch`
files in its `stack.toml` changed since they started: `stack restart --changed` restarts them).
A live service of a session gets an `endpoint`: its preset's connection URL
(`postgresql://postgres@127.0.0.1:<port>/postgres`, `redis://127.0.0.1:<port>`, ...) or
`http://127.0.0.1:<port>` for a custom service. Fields with nothing to say are left out.

Selections narrow the list (all must hold): `--project[=PATH]` lists the checkouts of the
project of the current checkout (or of `PATH`), `--state S` those in state `S`, and
`--label K=V` (repeatable) those whose session has every label given. `--worktrees` also lists
the repository's worktrees that have a `stack.toml` and nothing of Stack's recorded yet, as
state `unstarted` (one `git worktree list` for the repository). `totals` count what is listed.

## Fleet selections

`status`, `logs` and `down` act on one checkout unless given a selection: `--project[=PATH]`
(every checkout of the current checkout's project, or of `PATH`'s, with Stack records),
`--label K=V` (repeatable: checkouts whose session has every label), and for `status` also
`--all` (every checkout with records). Only checkouts whose directory is still there are acted
on, one after another, each under its own lock. Labels are set by `stack up --label K=V`, kept
while the session lives (a later `up --label` adds or replaces keys) and gone after `down`.
A path is given only as `--project=PATH`: `--project PATH` (with a space) is refused as a usage
error naming that spelling, in `ps` too, rather than taking the path for a service of `logs`.
`stack logs --project web` is still the `web` service of every checkout: for `logs`, only a word
with a `/`, or `.` or `..`, is taken for a misplaced path.

- `stack status --project` returns `{checkouts: [{path, branch, labels, status | error}],
  total, running, healthy, passed}` and exits 0 only when every started checkout is healthy and
  every status could be told (1 otherwise).
- `stack logs --label agent=claude` returns `{checkouts: [{path, branch, labels, logs | error}]}`;
  for people each line is prefixed `[<branch or directory>:<service>]`. Not with `--follow`.
- `stack down --project [--purge]` returns `{checkouts: [{path, branch, labels, down | error}],
  confirmed, failed}` and exits 1 when any checkout's stop failed or was not confirmed; one
  failing does not keep the others from stopping. Not with `--forget-unrecorded`.

## mv

When a checkout was moved or renamed (`git worktree move`, `mv`), its records still name the
old path, and Stack reports `session_conflict` in it. `stack mv <old> <new>` carries them over:
the session record, port reservations and identity tokens, so the checkout keeps its ports.
Relative paths resolve against the working directory. Service data is kept per checkout path,
so `mv` also moves each data directory `up` recorded for the checkout to where Stack keeps data
for the new path (`$STACK_STATE_DIR/data/<key of the new path>/<service>`). The
[checkout token](../DESIGN.md#runtime-contract) follows from the path, so each moved directory
gets the new token in its `.stack-checkout` marker and in Stack's data record, as `up` would
write them. The old path's supervisor directory (records and logs) is removed.

Data is checked as [`gc --reclaim-data`](#gc) checks it before removing anything: it must be
the very directory `up` recorded (where it resolved, no link on the way, the same device and
inode) and its marker must name the checkout. It is moved by renaming that directory, never
copied, so a new path on another file system is refused. Data recorded for an earlier
directory at the old path, or whose marker names another checkout, is left where it is (`gc`
reports it); data that is already gone is dropped from the record.

`mv` changes nothing unless everything checks out:

| Code | When |
|---|---|
| `mv_services_running` | Something recorded for the old path may still run: a live pid, an open port, a supervisor that does not report a service stopped, an interrupted start, or a process carrying the checkout's token. Running services were started with the old path in their configuration, so only that path can stop them: move the directory back, run `stack -C <old> down`, move it again and rerun `stack mv` |
| `mv_target_has_state` | The new path has a session of its own: run `stack -C <new> down`, or move elsewhere |
| `mv_source_exists` | The old path still exists, so nothing was moved. A copy needs no `mv`; it gets its own ports on `up` |
| `mv_nothing_recorded` | Nothing is recorded for the old path; `stack ps` lists what is |
| `mv_target_missing` | The new path is not an existing directory; move the checkout first |
| `mv_data_unmovable` | A recorded data directory cannot be confirmed to be the checkout's (replaced, linked, never seen by `up`, or named only by a session older than the data record), or its place at the new path already holds data |

Ports and tokens that a compile at the new path assigned (a worktree hook usually compiles
first) are replaced by the old ones, and the result's `notes` say so. A compile anywhere
releases the reservations of a path that no longer exists, so run `stack mv` before other
Stack commands to keep the old ports; the data moves either way.

## completions

`stack completions <shell>` prints a completion script for bash, zsh, fish, elvish or
PowerShell. For example:

```sh
stack completions zsh > "${fpath[1]}/_stack"
stack completions bash > ~/.local/share/bash-completion/completions/stack
stack completions fish > ~/.config/fish/completions/stack.fish
```

## doctor and setup

`doctor` checks mise, git and tar, that
mise is new enough for the project's options and lock, and that the project compiles. Its
`stack` check names this release and binary, and warns when another `stack` comes first on
`PATH` (which hooks, agents and `stack mcp` registrations run) or when `stack.lock` was last
written by another release. `stack.lock` records that release as `written_by`; it is not part
of what the lock pins, so it never makes the lock outdated, and `compile` changes it only when
it rewrites the lock anyway (and otherwise warns when it differs). When
the stack uses fnox it also checks the declared secret names, without reading any values.

`setup` downloads the mise release Stack is tested against, unless one is on `PATH`.
`--force` installs it anyway.

`setup --gc-agent` writes a per-user service that runs `stack gc --watch`, and prints the
command that loads it. It loads nothing unless you add `--enable`.

| Platform | File | Load it with |
|---|---|---|
| macOS | `~/Library/LaunchAgents/dev.stack.gc.plist` (log: `~/Library/Logs/stack-gc.log`) | `launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/dev.stack.gc.plist` |
| Linux | `~/.config/systemd/user/stack-gc.service` (`$XDG_CONFIG_HOME` if set; log: the journal) | `systemctl --user daemon-reload && systemctl --user enable --now stack-gc.service` |

The service runs the `stack` on your `PATH` (else this executable) with your current `PATH`
and any `STACK_*`, `PITCHFORK_STATE_DIR`, `MISE_*_DIR` and `XDG_*` store variables, so it sees
the same sessions you do. Run it again after changing those. `--print` shows the file without
writing it. `--remove` deletes it and prints the unload command (`launchctl bootout
gui/$(id -u)/dev.stack.gc`, or `systemctl --user disable --now stack-gc.service`); with
`--enable` it runs that command first.

## Errors

Every code, with its meaning, default hint and whether a retry can succeed, is in [error
codes](errors.md); `stack errors` prints the same list. Malformed `stack.toml` and `bundle.toml` errors name the file, line and column, and show the
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
