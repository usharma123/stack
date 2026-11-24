# Design

Evidence for these decisions is in [eval/REPORT.md](../eval/REPORT.md).

## What stack owns, and what it borrows

| Layer | Owner |
|---|---|
| Installing tools | mise (provider) |
| Supervising service processes | **stack** (one supervisor process per checkout) |
| Bundle format, composition, conflict rules, files, distribution, locking | **stack** |
| Sessions: one record of a running instance, verified endpoints, ownership | **stack** |
| Agent interface: JSON, structured errors, refusing on stale state | **stack** |

Providers sit behind `src/provider/`. A bundle never names a provider, so replacing one must not
change what a bundle means. Stack 0.1.23 and earlier ran services under Pitchfork through
`mise daemons`; Stack now runs them itself (`src/supervisor.rs`), and keeps only what stopping
services such a release started under Pitchfork needs.

## Objects

- **Bundle**: a versioned, shareable definition in a git repo (`bundle.toml` plus files): tools,
  env, services, tasks, bin paths. Contains no machine-specific values.
- **Project**: `stack.toml` lists bundles, adds its own definitions, and resolves conflicts in
  `[override.*]`. `stack.lock` pins each bundle by commit and content hash, and each tool and
  resolvable preset service by requested and exact version.
- **Session**: one running instance of a project on this machine (`.stack/session.json`, indexed
  in the machine state dir): assigned ports, supervisor PIDs, data directories, verification
  results with timestamps, each service's start time, and an optional lease. It records what
  stack started; it is never treated as proof of what is running. The project copy only
  mirrors the machine index for its own path. One naming another directory from a different
  directory (by device and inode) was copied in (committed, cloned, duplicated) and is
  ignored. So is one whose original path's machine index still holds it, or whose services
  the supervisor confirms are gone (the same test `stack gc` uses before dropping a record).
  Otherwise (this directory was moved) it may be the last record of running services: it is kept and reported as
  `session_conflict` naming the original path. Stack keeps `.stack/` out of Git with its own `.gitignore`.

## Composition rule

Layers apply in order: bundles (in `[[use]]` order), then the project. A key defined by two
layers must have equal values, otherwise it is a conflict. `[override.*]` is the only way to
resolve a conflict or replace a value, and every override is reported with what it replaced.

## Versions

`stack.lock` version 3 records `[[tool]]` and `[[service]]` entries (as version 2 did) and
embeds mise's artifact lock under `[provider_lock]` (see below). The entries are: `name`, `requested` (as
composed), `resolved` (an exact release), `resolved_on` (the platform that resolved it) and, for
services, the provider `tool` the preset installs. Resolution is `mise latest <tool>@<request>`,
run outside any project so project configuration cannot change what a request means; empty
output (mise's answer when no release matches) is an error, not a version.

- Ordinary `compile` keeps a pin while its request is unchanged and resolves only new or changed
  requests; removed requests drop out. `--update` resolves every request and reports `moved_from`.
- Locked mode (`compile --locked`, `inspect` once a lock exists, `up`, `exec`, `status`) never
  resolves: a missing, stale or extra pin is `lock_outdated`. `inspect` and `doctor` without a
  lock report `resolved: null` rather than contacting a release source.
- Resolution failures (`resolve_failed`) leave stack.lock and the provider config untouched.
- One exact version applies to every platform. If a platform lacks that release, installing
  fails there (`install_failed`); stack does not resolve differently per machine.
- Stack adds no provider tool. A lock Stack 0.1.23 or earlier wrote pins Pitchfork for a stack
  with services: locked operations read it without that pin (and its `[provider_lock]`
  entries), and the next `compile` writes the lock without them; a project that names
  `tools.pitchfork` itself keeps it. The session's configuration digest leaves that pin out
  too, so a session started before the lock was rewritten stays current after it. Stack 0.1.23
  rejects a lock rewritten this way (`lock_outdated`): going back to it means `compile` with it
  first, which pins Pitchfork again.
- Preset mappings (mise 2026.9.18) are postgres → postgres, redis → redis,
  cockroachdb → cockroach, nats → nats-server and spicedb → spicedb. An omitted service
  version becomes a `latest` request, resolved and pinned once. Unknown presets fail in
  locked mode. Nonrelease requests remain explicitly warned exceptions.
- Reused release pins are validated offline with provider-specific version shapes. Floating,
  empty and nonrelease values cannot replace an exact pin. `--update` can repair invalid pins.
- Stack isolates mise config selection for resolution, installation, daemons and nested exec.
  Only its generated file is loaded; parent/global/project aliases are excluded.
- Version 1 locks are read for migration only. `compile` rewrites them keeping every
  bundle pin; locked operations refuse them. Moving a running session from a range to its exact
  release is a configuration change, so the next `up` restarts it. Within a major version
  (`17` → `17.11`, `8` → `8.10.2`) the presets reuse existing data; this was checked on macOS
  with mise 2026.9.18.
- An exact version is not an artifact checksum. `[provider_lock]` is mise's `mise.lock`
  embedded verbatim, matched to pins by tool name and version. `compile` locks only the pins
  that need entries, in a scratch root, and merges the result: committed values change only
  under `--update`, every difference is reported, and shared conda records are merged per
  package. Locked operations render `.config/mise/mise.lock` from stack.lock and install with
  `mise install --locked` where coverage allows. Version 2 locks stay valid under
  `best-effort`. See [Artifact checksums](user/commands.md#artifact-checksums).

## Runtime contract

- **Ports.** Assigned per checkout from 20000-29999 via a locked machine-wide registry; reused
  across restarts; never inside the system's ephemeral range, where an outgoing connection can
  hold a stopped service's port (a reservation made there earlier is replaced); pruned when a
  project directory disappears (by the next compile anywhere, and by `gc` once no session of
  it is left); `--reassign-ports` after a foreign program takes one. Projects
  may pin ports in `[override.services]`; pins are checked against other projects'
  reservations. Service defaults (5432, 6379) are never used. Before starting,
  `up` checks every assigned port: one that accepts connections without a running or starting
  supervisor daemon of that service behind it is a `port_conflict`, naming the service, the port
  and, when `lsof` or `/proc` can say, the holding process. The hint is `--reassign-ports` for
  assigned ports and the override for pinned ones. Stack never reassigns on its own: `down`
  removes the session while reservations persist, so "no session" does not mean "never used".
  `up --reassign-ports` is the explicit opt-in: on a `port_conflict` at the `ports` step with no
  pinned port involved, only the conflicting services get new reservations (the old port stays
  excluded while choosing) and `up` runs again once, reporting `reassigned_ports`.
- **Fewer round-trips.** `exec`/`run --up` run a live `status` first and `up` only when it finds
  no session, a launching, stale or expired one, or a service that does not verify; the lease is
  carried over unless `--ttl` is given. `exec --install` runs `install` when a pin is missing.
  Both happen before planning, outside the plan's lock,
  within the call's deadline.
- **Generations.** A session fingerprints the composed configuration except its tasks, the
  bundle lock and the assigned ports. Tasks change what `run` runs, never a service, so editing
  one keeps the session; a fingerprint recorded before tasks were left out, which still matches
  the configuration with its tasks, is accepted too. `status` reports a change as stale; `exec` withholds service endpoints until
  `up` stops the previous owned instance and verifies the new configuration. Provider data-version checks
  remain in force: incompatible version changes fail explicitly and preserve the data, rather
  than silently reusing the old process or resetting the database.
- **Verification** happens on every `exec` and `status`, not from the record: supervisor state,
  PID alive, supervisor port equals assigned port, TCP accept, then identity. Postgres and Redis
  connect with the app's own URL and compare the server's data directory to the supervisor's.
  Stack asks this itself over the wire (`src/wire.rs`): a Postgres protocol 3.0 startup (no
  `client_encoding`, so the text comes as psql on a pipe receives it) and the simple query
  `select current_setting('data_directory')`, or Redis `CONFIG GET dir`, and compares the text
  `psql -Atc` or `redis-cli` would print, exactly as before. A Postgres answer must be what
  psql accepts: one RowDescription of one column, rows of that width, CommandComplete, then a
  well-formed ReadyForQuery; notices, parameter changes and notifications between them must be
  well formed too, and RESP lengths are digits as Redis writes them (no sign, no leading
  zero). Text that is not UTF-8 is left to psql. The client fallback runs
  `psql -X`, so `~/.psqlrc` cannot change what it prints. It does so only
  for a URL naming an IP address and port, a user and database for Postgres, and no password,
  query or second host, when no client variable could change the connection (`PG*` other than
  `PGHOST`, `PGPORT`, `PGUSER`, `PGDATABASE` and `PGCONNECT_TIMEOUT`; any `REDISCLI_*`), and
  only while the server asks for no authentication and, for Postgres, accepts the plain
  connection (a refusal other than starting up or shutting down may want the SSL or GSS
  encryption psql tries first). Otherwise `psql` or `redis-cli` from the stack's PATH is run
  as before. An error, a refusal, a malformed, partial or oversized reply
  (16 KiB per message, 64 KiB in all) or no complete answer within 5 seconds (3 to connect)
  fails verification; it never falls back to the client.
  A service with an `identity` probe is `instance` only when the probe, run with the app's
  environment minus every `STACK_IDENTITY_*` variable, prints exactly the checkout's token for
  it. The token is random per checkout and service, kept in machine state like ports, given to
  the service as `STACK_IDENTITY_<NAME>`, and part of the configuration fingerprint. The probe is
  bounded (deadline, 4 KiB of output, process-group kill). Services without a probe get liveness
  only, labelled `liveness` rather than `instance`. Probes are trusted bundle code, not a sandbox.
- **Withholding.** Endpoints of unverified services are poisoned (`unverified.stack.invalid`)
  rather than unset, because apps commonly fall back to `localhost:<default>`. This covers the
  service's variables whether the provider set them or the command would inherit them from the
  caller, including caller values that are not valid Unicode; every other inherited variable
  reaches the command byte for byte. Whatever host a URL names (IPv4, IPv6, DNS alias) is
  replaced by parsing, keeping credentials, port, path and query. PostgreSQL URIs and keyword
  strings are parsed the way libpq parses them (no fragments, quoted values, several hosts), and
  the `host`, `hostaddr` and `service` parameters that would override the host are dropped. Host
  variables (`PGHOST`, `PGHOSTADDR`, `PGSERVICE`, `*_HOST`) get the invalid host outright, and
  `PGHOST` is set even when absent so libpq cannot use its default. Empty or malformed endpoints
  become a URL that names only the invalid host. Only values that name no host (ports, users,
  databases, passwords) are removed. `STACK_UNVERIFIED` lists affected services. `--require`
  turns this into a refusal to run.
- **Provider environment.** `exec`, `run`, `env`, `status` and `up` need `mise env --json` and
  whether every pin is installed (`mise ls --json` in a scratch root). Stack keeps the two
  answers together in `$STACK_STATE_DIR/projects/<checkout key>.provider-env.json` (mode 0600,
  replaced atomically, the last 8 written, by key; a hit does not move an answer up) and uses
  them instead of starting mise while nothing they depend on changed
  (`src/provider/env_cache.rs`). The key is the mise binary (the first `mise` on PATH, links
  resolved, by path, device, inode, size and modification and change times; a script that runs
  another file, or an empty or relative PATH entry at or before it, which mise started in the
  checkout would resolve there, means no key), mise's whole environment as Stack starts it, the
  checkout's path, the generated configuration and rendered lock by content, whether
  `.stack/provider-config` is absent or a real empty directory (anything in it, or a link,
  means no key: settings there can make mise read files nothing covers), and the pins. Each
  pin's install directory as mise reported it, and the tool directory above it, must be real
  directories (a link's target could change unseen) and are recorded with every entry directly
  inside them (name, kind, device, inode, size, times), which must be unchanged. mise's
  `incomplete` marker (`$MISE_CACHE_DIR/<tool>/<version>/incomplete`) is not recorded: mise
  replaces the install directory whenever it sets one, which the record sees. An answer is
  kept only when every pin was installed; when the configuration has no template syntax and no
  `$` in an `[env]` value; when every configured tool is a pin whose locked backend is not a
  plugin (asdf, vfox, or one Stack does not know); when every variable mise returned is the
  configuration's own value or lies under an install directory and every `PATH` entry is the
  caller's, an install's or a declared `_.path`; when the key was the same after asking; and
  when every recorded entry last changed at least 2 seconds before mise was asked. The
  configuration checks run before mise is asked, so a question whose answer would never be
  kept asks only what the caller needs. The file is used as read: like the session and port
  records, `$STACK_STATE_DIR` is trusted, and whatever can write it can set the PATH of later
  commands. Under
  Stack's isolation mise reads no other configuration file (`mise.toml`, `.mise.toml`,
  `.tool-versions`, other `conf.d` files and the isolated directory's `config.toml` or
  `conf.d` were checked against mise 2026.10.3). Anything else asks mise, as before. An install
  that runs, `down --purge` and `gc` of a deleted checkout drop the file. Any change to mise's
  environment is another key, so a caller that changes a variable on every call (hyperfine's
  `HYPERFINE_RANDOMIZED_ENVIRONMENT_OFFSET`) always asks mise. The release `mise version`
  printed is recorded the same way for the binary it came from (`mise-version.json`), and `up`,
  `install` and `restart` trust the generated configuration once per content, as `exec`
  already did (trusting it again before an install that does run).
- **Secret grants.** Stack resolves grants itself rather than using mise's native task
  secrets, because `exec` computes the environment with `mise env --json` and runs the program
  itself, so a mise-side grant would not reach it. Names are checked at compile and plan time
  against service endpoints, `[env]` and reserved variables. After verification, stack runs the
  pinned `fnox` twice (`--describe`, then `--keys`), bounded in time and output, validates the
  whole answer before applying it, and never forwards what fnox printed. Captured output passes
  through a streaming matcher before it is bounded. See [secret grants](user/secrets.md).
- **Leases.** `--ttl` (renewed by `exec`/`run`/`renew`, and by `status` while not yet
  expired; `renew --ttl` changes it) or `--owner-pid` (a long-lived runner, not the
  short-lived shell that ran `stack up`). Owner PIDs must be 1 to 2147483647 (a positive
  `pid_t`); other values are rejected as `usage` before any lifecycle work, never truncated.
  `--owner-pid ancestor:<name>` walks the parent chain of the stack process (kernel process
  info through `procinfo`, no subprocess) and takes the nearest process whose `comm` or
  `argv[0]` basename is `<name>`; none is `owner_not_found` listing the chain.
  `status`, `up` and `ps` report the lease as one structured `lease` object (`kind`,
  `ttl_secs`, `renewed_at`, `expires_at`, `owner_alive`, `expired`).
  Reclaimed by `stack gc`, at the start of every `stack up`, and by each pass of
  `stack gc --watch`, an opt-in foreground loop meant to run under a supervisor the user
  chooses. stack installs no background service.
- **GC passes.** A pass reads each machine index entry without its lock and locks only
  candidates (an expired lease, or a gone project); everything is checked again under the
  lock. The pass at the start of `up` try-locks: a busy candidate is skipped and reported, and
  any failure becomes a warning of the `gc` step, so one checkout's `up` never waits on or
  fails because of another's. Standalone `gc` waits for busy candidates. `gc --dry-run`
  reads without locks and writes nothing; it lists the sessions a pass would reclaim and the
  ports and data a deleted checkout would release (or, with `--reclaim-data`, have removed
  where the checks pass).
  Commands register active executions under the lifecycle lock and renew on completion. A
  command that cannot retake the lock within a second leaves an atomic completion record
  (session, token, PID) instead; the next lifecycle operation applies it, but only once it has
  seen that command's process exit, so a running command is never forgotten. GC ignores TTL
  expiry while an execution it saw running is recorded. An explicit runner-death policy still
  takes precedence over a surviving command.
- **Commands and `down`.** `down` stops the services whatever runs, but is not confirmed
  while a command Stack started for the checkout still runs (`src/commands.rs`). It fails
  `stop_unconfirmed`, one `kind: "command"` detail per command (`pid`, `program` name,
  `started_by`, `running_secs`, `found_by`, `another_checkout`; never arguments); `--purge`
  then removes nothing. Sources:
  - *Registry*: `<state>/commands/<identity>.json` (identity: a hash of the checkout
    directory's device, inode and creation time), under `<identity>.lock`. `plan` registers
    every registered command (not `stack env`) while it holds the project lock `down` takes
    exclusively, before it spawns: coordinator PID, start time and boot, how it was started,
    the program's name; the first spawn on that thread records the command's PID, start time
    and boot (`process::on_next_spawn`); the plan removes it when finished or dropped. An
    entry is live while its command (before that is recorded, its coordinator) is the process
    observed in the same boot. An entry with no command recorded whose coordinator is gone is
    *unrecorded*: never pruned, it makes `down` unconfirmed (`kind: "registry"`) and `status`
    report `commands_error` beside the live entries, until `down --forget-unrecorded` removes
    unrecorded entries alone under the registry lock. A registry file is removed when its last entry ends, but its lock file is never
    removed, and keeps its identity's mark recognised (it records no path for `gc` to
    check). A registry that cannot be read is an error the same way, and that `down` stops only
    the services' recorded processes and groups (no leftover, stray or token-carrier stop, in
    `down` or a supervisor's recovery); one that cannot be written refuses the command.
    `status` lists the live entries and reads no environment.
  - *Mark*: `STACK_COMMAND_MARK`, a hash of the identity, set on every registered command,
    inherited by what it starts, rejected in `[env]`, and stripped from the provider's
    environment wherever Stack reads it (`provider_env`, so services, `exec`, `env`, `status`),
    and from every service environment. `down` searches environments for it until two passes
    agree (at most 5).
  - *Token*: a process carrying the checkout's service token that is exempt (below) is always
    reported: another checkout's with `another_checkout`, a coordinator without its PID in the
    hint.
  - *Older Stack*: each `active_executions` PID still running that no registry names as a
    coordinator is reported once, as `older stack`, by the `down` that removes the session; its
    PID is never in the hint.
  Exempt (`commands::Exempt`): a process some registry proves to be a live command or
  coordinator (PID, start time, boot), or carrying the checkout's own mark (computed, no
  registry needed) or the mark of an identity some registry lock file is for. One snapshot per
  `down`, each process decided once, serves stopping and reporting. Exempt processes are never
  signalled by `down`'s leftover search, `up`'s stray stop, or a supervisor's recovery; a
  leftover group with any exempt member is signalled member by member (every non-exempt member,
  each only while still in the group observed), and a recorded group the same way after its
  leader. Only the `down` process itself is left out of what
  counts: when it is a registered command, or the direct child of the coordinator of an entry
  whose command is not recorded yet. Not found or not promised: listed under "commands still
  running" in docs/user/commands.md, among them a command a live service starts in its own
  process group, which the supervisor stops with that group.
- **Data.** Stack (as mise did) keeps a preset's data under a directory keyed by the project's path alone,
  so a checkout recreated at a path would find its predecessor's databases. Before starting,
  `up` writes the checkout token (see *Checkout token*) to `<data dir>.stack-checkout`. Data
  marked for another checkout is moved, with its marker, to `<data dir>.replaced-<unix time>`
  (never deleted) and named in `warnings`; the new checkout starts empty. Unmarked data, from
  before markers or with an unreadable marker, is adopted. A running service's data is not
  touched. `up` also records each data directory, with the token and the service's supervisor
  identity, in the machine state (`data.json`). Once a checkout is deleted and no session of
  it is left, `gc` reports that data and `gc --reclaim-data` removes it, only while its marker
  still names that checkout, the recorded supervisor confirms the service is not running, and
  no process carrying the token runs. A path names whatever is there when it is looked up, so
  `up` also records where the directory resolved and, once a verified service uses it, its
  device and inode. `gc` reaches it from `/` down that resolved path with `O_NOFOLLOW` at each
  step, requires the same device and inode, reads the marker and removes the tree through the
  descriptors it opened (`openat`/`unlinkat`), and removes the directory's own name only if it
  is still that directory: a replaced ancestor, a link, a copy in its place or a swap after
  the check never redirects the removal. Quarantined `.replaced-` data and data Stack never
  recorded are never removed.
- **Dependencies.** `depends_on` orders starts in waves: a service is requested only once the
  services it depends on verified and passed their declared readiness. `restart` adds every
  service that depends on one it restarts, and `up` stops a running dependent of a service it
  must start again before starting both, so no dependent keeps a connection to a dead
  instance. A dependency outside what `restart` starts is never started by it: it must
  verify and pass its readiness before anything is stopped, and again before each wave's
  setups, or the restart fails `dependency_not_ready` without side effects of its own. A
  `ready_cmd` passes only when it ran and exited 0 in time; one Stack could not run or read
  answered nothing and fails, with the reason in the error. Identity stays per service: a dependent is verified on its own. A service's
  `setup` runs just before it is requested (never for one left running), with the environment
  `exec` would get, so migrations and seeding need not be chained into `run`.
- **Starting.** Each checkout's services run under a supervisor of its own: the `stack` binary
  re-executed as `stack __supervise <dir>`, forked twice into a session of its own, with no
  socket and nothing shared with another checkout. `<dir>` is
  `$STACK_STATE_DIR/supervisors/<checkout key>`. Commands talk to it through files: under the
  project lock they replace `spec.json` (each service's argv, its whole environment, port,
  readiness check, stop signal, data directory, whether it is wanted, and a start counter;
  `shutdown` to stop everything and exit), with a revision one higher, then send `SIGHUP` to the
  pid `state.json` records while that pid still has the recorded process start time, or start a
  supervisor when none runs, and wait until `state.json` reports the revision applied. The
  supervisor holds `<dir>/lock` (`flock`) for life, so two never run for one checkout; a second
  one finds it held and exits. It writes `state.json` atomically: the checkout's path and
  token (from the spec), its own pid and start time, and
  for each service its status (`starting`, `running`, `stopping`, `stopped`, `failed`), pid,
  process start time, process group, port, data directory and how it last ended. Each service is
  spawned directly (a `run` command through `/bin/sh -c`) in a session and process group of its
  own, with signal dispositions reset, its environment exactly the spec's: what the command that
  started it inherited (without `STACK_*` and mise's own selection variables), the preset's
  locale, `STACK_CHECKOUT_TOKEN`, `$PORT`, then the provider's environment (`mise env --json`),
  which wins. Its output goes through a pipe the supervisor drains into `logs/<service>.log`,
  each line stamped with the millisecond it was read; logs rotate at 16 MiB. A service is
  `starting` until its readiness passes (its `ready_cmd`, run through `sh` from its PATH and
  bounded at 5 seconds per try, else its `ready_port`, else for Postgres and Redis the assigned
  port, for other presets the check mise's preset ran; a service without any is running once
  spawned), then `running`. `depends_on` orders starts in waves: each wave is one spec change
  that starts its services together, and the next wave is written only once Stack verified the
  wave and its declared readiness passed. A service that exits on its own is `failed` and stays
  so (no restart policy is configurable yet; the supervisor supports a bounded retry count with
  a growing delay, which Stack sets to zero): it is started again only when a command raises its
  start counter. A service that exits while starting fails the start at once with
  `start_failed`, its exit and last lines in the details. Listing what runs (`status`, `exec`,
  `ps` through records, every check of `up`) is a read of `state.json` and a start-time check
  of each pid: no process is started for it.
- **Presets.** Stack runs the presets mise 2026.10.3 defined, as it ran them, so data they
  initialised keeps working: `stack __preset <preset> <data dir> <port> <pinned version>
  [<role>=<port>...] [templates=<dir>]` initialises the data directory when it is empty or missing and then
  becomes the server. Each preset service runs its own pinned release: its PATH starts with
  mise's install directory for that tool and version (`installs/<tool>/<version>/bin`), so two
  services pinned to different releases of one tool each get theirs, and the server's full
  version, not only its major, must be the pinned one before anything is initialised.
  Postgres: a clone of the template for its server and environment (see *Postgres
  templates*), or else `initdb -D <tmp> -U postgres --auth=trust`, into a sibling
  `.stack-init-*` directory renamed into place, then `postgres -D <dir> -p <port> -h 127.0.0.1
  -k ""`, stopped with `SIGINT`. Redis: `redis-server --bind 127.0.0.1 --port <port> --dir <dir> --appendonly yes`.
  CockroachDB, NATS and SpiceDB: mise's arguments, except that their secondary ports (mise's
  fixed 8080, 8222, 8443 and 9090) are reserved per checkout in the port registry, as
  `<service>.http` (CockroachDB, SpiceDB), `<service>.monitor` (NATS) and `<service>.metrics`
  (SpiceDB), exported in their variables (`NATS_MONITOR_PORT`, ...) and used by their readiness
  checks, so two checkouts run them at once and each check reaches its own server. Each
  directory records `<preset>:<major>` in `.mise-daemon-version`;
  existing data whose record differs is refused (`incompatible ... data`), one without a record
  is refused as not initialised completely, and a server on PATH whose major version is not the
  pinned one initialises nothing. The connection variables mise exported for a preset
  (`DATABASE_URL`, `PG*`, `REDIS_URL`, `COCKROACH_*`, `NATS_*`, `SPICEDB_*`) and a custom service's
  `<NAME>_URL` and `<NAME>_PORT` are rendered into the generated `[env]`; a project's own value
  wins. Each preset's server is a `[tools]` entry at its pinned release.
- **Postgres templates.** `initdb` costs about half a second of CPU and disk per new data
  directory, and checkouts started together contend for both. Stack runs it once per recipe
  and clones the result (`src/pgtemplate.rs`). A template is
  `$STACK_STATE_DIR/templates/postgres/<key>/` (`data/`, the cluster with its
  `.mise-daemon-version`, and `template.json`), about 40 MB.
  - *Key.* A digest of: the `initdb` found on the service's PATH and the `postgres` beside it
    (resolved path, device, inode, size, times; the `postgres` first on PATH must be that same
    file); the install's inputs by metadata, one level each: the install directory, `bin`,
    `lib`, `lib/postgresql` when present, each share directory holding `postgres.bki` (`share`,
    `share/postgresql`, or both when both hold one), its `extension`, `timezonesets` and
    `tsearch_data`, and the `plpgsql` module, each entry with its identity and times, and a
    symbolic link with its text and what it leads to; the arguments; `LC_*`, `LANG` and `TZ` as
    the service gets them; `/etc/localtime` (link and file); the operating system release
    (`uname`); on Linux every `libc.so.6`, `ld-musl-*` loader and `libc.musl-*` in `/lib`,
    `/lib64`, `/usr/lib`, `/usr/lib64` and their `*-linux-*` multiarch directories, and
    `/usr/lib/locale` with its `locale-archive`, by resolved path and identity, read from disk
    because Stack's own Linux binaries are static musl; the user; the platform; a format version
    (`stack-pg-template-3`: a template recorded under another is rebuilt, and removed by `stack
    gc`). No key is made, and `initdb` runs, when the share directory or the `plpgsql` module is
    not found, a link dangles or loops, a PATH entry before `initdb` is empty or relative, an
    install entry changed in the last 2 seconds, or (Linux) no system C library is found or one
    of those library directories exists but cannot be listed. Not keyed: a C library a server
    finds through its own `RPATH`, file contents (an edit in place shows only through the times
    it changes), and the IPv6 lines `initdb` writes into `pg_hba.conf` depending on whether
    `::1` resolved, which do not matter while the preset listens on 127.0.0.1 only. The key
    includes device numbers, so a volume renumbered across boots orphans its templates until
    `stack gc`.
  - *Build.* A missing template is built by the first start that needs it under `<key>.lock`:
    `initdb` into `.build-<key>-*`, the marker written and flushed, a digest of every entry of
    the cluster (path, kind, mode, size, inode, link count, modification and change time), the
    recipe checked again, the record written and flushed, then on macOS one `F_FULLFSYNC` (the
    one flush of the drive's cache per template, off the path of later starts; on Linux `fsync`
    already is that), then one rename to `<key>`. A template that could not be fully flushed
    records the boot it was built in (`kern.bootsessionuuid`, `/proc/sys/kernel/random/boot_id`)
    and is built again after a restart; with no boot id it is not kept. No template is built for
    a major release whose layout is not verified (below); its data directories are made by
    `initdb`. A build the identity edit would refuse (an unverified `pg_control_version`, a
    checksum that does not match, a WAL segment without the layout's magic, long header or
    identifier; checked before anything else) is discarded, never published and never used: it
    may be damaged, so `initdb` runs at the data directory. Only a build that passes and whose
    `postgresql.conf` has the values `initdb` picks on an unconstrained machine
    (`max_connections = 100`, `shared_buffers = 128MB`, checked on 17 and 18) is published; a
    build that passes with other values is renamed to the data directory being made and used for
    it alone, and a later start tries again. Other starts wait for the lock (up to 60 seconds,
    then run `initdb` themselves) and clone the result. Only a complete build is renamed into
    place; the next builder removes what one that died left, and a template whose record names
    another recipe is replaced. If the build's `initdb` fails (the template's volume is full, or
    `templates/` was removed), the failed build is discarded and `initdb` runs once at the data
    directory; if that fails too, both errors are reported.
  - *Clone.* `clonefile` of the directory on macOS, and nothing else: on another volume, or a
    filesystem that cannot clone (HFS+), `initdb` runs at the data directory, since a copy saves
    little and would need every file flushed. On Linux `FICLONE` per file (Btrfs, XFS), else a
    plain copy, then `syncfs`; the Linux path has been type-checked, not run. Stack never starts
    a server on a template. After each clone the template's digest is taken again and must be
    the recorded one: any write, chmod, link, rename or new entry since the build changes some
    change time, which cannot be set back, so the clone is of what was built; otherwise the
    clone is discarded, Stack tries to retire the template (see *Retiring*) and `initdb` runs.
    The record a start clones by and the directory it clones are one generation: a start without the lock
    takes the directory's identity before and after reading the record and, if a rebuild
    replaced it meanwhile, reads it again under the lock. On a filesystem with whole-second
    times a template is cloned only once 2 seconds old (its builder excepted). Clones refresh
    `used` without the lock.
  - *Identity.* `initdb` gives each cluster a random system identifier (in `pg_control` and in
    the long header of each WAL segment) and a mock authentication nonce; clones would share
    them. Physical replication, `pg_rewind`, WAL archives and backup tools such as pgBackRest
    tell clusters apart by that identifier, and `pg_resetwal` cannot set it. So each clone gets
    a new identifier made as `initdb` makes it (seconds, microseconds, process id) and a new
    nonce. The edit is made only for a `pg_control_version` whose layout was verified with
    `offsetof` against the installed server headers: 1700 (17.6, 17.11, 17.12: nonce at 256,
    checksum at 288, WAL page magic `0xD116`) and 1800 (18.6, 18.7: nonce at 257, three bytes of
    padding, checksum at 292, magic `0xD118`); the identifier is at 0 in `pg_control` and at 24
    in a long WAL page header in both. The stored CRC-32C must match at the layout's offset, and
    every WAL segment must carry the layout's magic, the long-header flag and the old
    identifier, before anything is written; exactly the identifier, the nonce and the checksum
    change, and the two files are flushed before the rename. Any other version, a mismatch, or a
    big-endian target (Postgres stores these in native order; only little-endian is verified)
    runs `initdb`. When the edit fails on a clone for any reason (contents it refuses, damage to
    the clone since it was made, a full disk), the clone is discarded; if the
    template's lock can be taken, the template itself is then checked as the edit checks a copy
    (read-only, under that lock), kept if it passes and, if it is refused, Stack tries to
    retire it (see *Retiring*); then `initdb` runs. Nothing else in a never-started cluster
    names it or its path: `-D` gives the data directory, `postmaster.pid` and `postmaster.opts`
    appear only once a server runs, and there are no tablespaces or logs. What still comes from
    the template: file times and the time of its checkpoint in `pg_control`.
  - *Retiring.* A template that changed since its build, or that the edit refuses, is never
    used for a data directory: every start that clones it checks it and runs `initdb`. Before
    that `initdb`, the start tries to remove the template: it waits up to 5 seconds for the
    template's lock (so a held lock delays the start's `initdb` by up to that long), and under
    the lock removes it only while its record is still the one the clone was made by (a rebuilt
    template's record differs), by renaming it aside in one step. The service's log says what
    happened, also when that `initdb` fails: removed; kept because the template itself passes;
    the lock could not be taken within the wait, so nothing was checked or removed; already
    replaced or gone, so nothing was removed; or left in place because the lock could not be
    opened, or the record could not be read, or the template could not be checked or renamed,
    with the error. A later start that clones a template still there tries again.
  - *Collection.* `stack gc` removes a template whose install's inputs changed or are gone, one
    with an unreadable record or another format, one not fully flushed from an earlier boot, one
    no clone was made from in 14 days (`used`), and builds or removals a process left. Each is
    judged under its recipe's lock, taken without waiting, and only while the directory is still
    the one listed; one locked by a build is skipped. `gc --dry-run` takes and releases the same
    locks and lists, under `templates`, what a pass would try to remove; a pass can still leave
    one in place (a rename that fails), and does not list templates in its result. A clone that
    refreshes `used` after `gc` judged a template may still see it removed: its own copy is
    complete and checked by then, and the next start builds again. Lock files stay. Removing the
    templates directory by hand is safe only while no build runs (a lock file unlinked under a
    builder lets a second builder in).
  - *Leftovers.* A `stack __preset` killed while making a data directory can leave a
    `.stack-init-*` staging directory beside it, possibly with the identity half edited. It is
    never moved into place (Postgres would refuse it), and nothing removes it; this was already
    true with `initdb`.
- **Stopping.** A stop is a spec change: the supervisor sends each unwanted service's group its
  stop signal (`SIGINT` for Postgres, `SIGTERM` otherwise), `SIGKILL`s the group after 10
  seconds, and kills what a leader left in its group as soon as the leader is reaped. `down`
  writes `shutdown` and waits until every recorded process is gone and the supervisor exited;
  then it reconciles recorded PIDs and ports as before, including ports from an older generation.
  Query and stop failures preserve ownership records. Success requires those processes dead and
  ports closed. A reserved port is Stack's to wait for only when a running or starting service,
  or a live recorded process, is behind it. A foreign listener on a reserved port is reported in
  `conflicts`, not stopped and not a failure.
- **Records.** Every process in `state.json` is recorded by pid, process start time and boot
  (Linux `/proc/sys/kernel/random/boot_id`, macOS `kern.bootsessionuuid`; not `kern.boottime`,
  which macOS rewrites when the clock is stepped); a record without a boot, or
  from another boot, names nothing alive and is never signalled. A request gets a revision above
  both the spec's and the last one the supervisor acknowledged, and a `spec.json` or
  `state.json` that exists but cannot be read is `state_invalid`, not a fresh start, so an
  acknowledgment only ever comes from a supervisor that read the request. A start asked of a
  service already running is consumed by it: it is not started again when it later exits. A
  start asked while it is stopping is kept, and starts it once the stop completed. The
  supervisor retries a `state.json` it could not write every pass; if services run without a
  published record for 5 seconds it stops them all, and the waiting command sees no
  acknowledgment. Its readiness checks run every 10 ms at first and back off to once a second
  for a service still starting after 2 seconds; `up` checks every 5-25 ms for 2 seconds, then
  less often up to every half second, and checks a service only while the supervisor no longer
  reports it starting, keeping a check that passed. At the deadline a service never checked is
  reported "not verified before the deadline", apart from one whose check failed. A readiness
  probe runs in its own group, killed with its service; the supervisor does not finish a stop
  or exit while one runs. `supervisor.log` is cut to its last 256 KiB, from a line, once it
  passes 1 MiB: at start, and by the running supervisor itself, which checks its size every
  pass.
- **A supervisor that dies.** When the supervisor is killed its services keep running (on Linux
  each service's main process gets its stop signal through `PR_SET_PDEATHSIG`, checked against
  its parent after it is set). They are listed `orphaned` and are not verified. Every command
  that starts a supervisor first stops what the dead one left, and a new supervisor does the
  same before it starts anything; `down` does it without starting a supervisor. What is stopped:
  the processes of each recorded group and every process carrying the checkout's token that
  started at or after the service's recorded start (2 s slack), in this boot. A group is
  signalled while its recorded leader is still the recorded process and leads it. Its members
  are looked up between two checks of that leader: while the same leader lives before and
  after, the group number was held throughout and only processes of the leader's session can be
  in it; if the second check fails the whole look is discarded. Members of an accepted look are
  remembered and stopped by pid even after the leader exits, but are not signalled one by one
  while the leader handles its stop signal (a Postgres postmaster stops its backends itself):
  only once the leader exited, with the service's stop signal, and at the `SIGKILL`. A leader
  pid now held by another process means the group is gone. While waiting, the processes
  carrying the token are searched again every 250 ms (each search reads every process's
  environment), and once more before anything is reported gone. A process that is in a leaderless group
  but cannot be shown to be the checkout's (its environment unreadable) is never signalled: the
  result is `stop_unconfirmed`, and no replacement starts and no data operation runs while it
  lives. A descendant that left the group and does not carry the token is not found. Signals go
  through `src/stopping.rs`, the path `up` and `down` use for strays: on Linux through a pidfd
  opened before the process is checked again (without pidfd support nothing is signalled and
  the stop is unconfirmed); on macOS by pid right after the check, which leaves the gap
  docs/user/guarantees.md describes. A supervisor whose directory is removed stops its services
  and exits; one still running for a deleted checkout with no session left is shut down by `gc`
  under the checkout's lock, then its directory is removed.
- **Sessions started under Pitchfork.** A session Stack 0.1.23 or earlier recorded names
  Pitchfork's binary and state directory (`provider.pitchfork`); Stack's own sessions record
  `provider.supervisor = "stack"` and its directory (with `pitchfork` empty, which older releases
  require). For such a session `status` and `exec` ask `pitchfork status --json <id>` per
  recorded service, `logs` asks `pitchfork logs`, `restart` and snapshots refuse with
  `session_stale`, and `down`, lease expiry and `up` (which then starts the services under Stack's
  own supervisor with the same data) stop them with `pitchfork stop -- <id>` through the recorded
  binary, holding that supervisor's `stack-supervisor.lock` shared. Once they are confirmed gone,
  `down` (and `up`, reported in its `stop_previous` step) stops the Pitchfork supervisor when it
  carries the owner mark Stack 0.1.23 wrote to `<state>/supervisor-owner`, no other session
  records it or records services without a supervisor, every session record is readable, no
  daemon it tracks is anything but stopped, failed or errored, has a live process or has a
  schedule, and its lock can be taken alone within 5 seconds (`--keep-supervisor` opts out).
  Stopped means `supervisor status` no longer answers and no process of its group or child of it
  carries the mark. Every other outcome keeps it and gives the reason; when the migrating `up`
  keeps it, the reason is a warning and the supervisor is recorded in
  `<state>/pitchfork-kept/<checkout>.json`, so the next `down` tries again under the same rules.
- **Checkout token.** Every service gets `STACK_CHECKOUT_TOKEN` in its own daemon environment
  (never in `[env]`, so no task, `exec` command or `mise x` has it): a hash of the checkout's
  path, device, inode and creation time, so stable for one directory and different for one
  recreated at its path. A listener on a reserved port whose environment carries it was
  launched by a supervisor for this checkout even when no record accounts for it (a service's
  child left behind, or a process Pitchfork 2.29.0 lost track of for an older Stack). `up` stops
  it, its token-carrying ancestors and the token carriers of its process group; `down`, once
  every recorded process is gone, stops every process still carrying the token, listening or
  not, including ones started while it stops, and does not confirm while one lives. On macOS
  the environment is read from `KERN_PROCARGS2` past exactly `argc` arguments; a token passed
  as an argument is not one. Each process is observed before and after its
  environment is read, and each signal is bound to the PID and start time observed (through a
  pidfd on Linux; on macOS by a check right before the signal, with the gap described under
  a supervisor that dies). A listener
  whose environment cannot be read (another user's, or a macOS platform binary's) is foreign.
- **Installing.** `install` runs the compile, trust and install steps of `up`
  and nothing after: no session, no stop, no start. It is locked like `up`.
- **Logs.** A service's stdout and stderr are pipes to its supervisor; one thread per service
  writes its log and rotates it past 16 MiB to one older file, so rotation never races a writer.
  The pipes end with the supervisor: a service that writes after its supervisor died gets
  `SIGPIPE` (or `EPIPE`) and may exit. `logs <service> --tail N` reads the supervisor's
  `logs/<service>.log` (and the one older rotated file), at most its last 1 MiB, never following. `--since-start` keeps lines
  stamped at or after the start Stack recorded, to the millisecond. For a session started under
  Pitchfork, `pitchfork logs <id>` through the recorded binary, bounded by a deadline and a 1 MiB
  cap.
- **Deleted projects.** At `up` the machine index records the supervisor (Stack's directory, or
  for an older session Pitchfork's binary, state directory and each daemon's qualified id) and
  the project directory's device and inode, plus its creation time when the filesystem exposes
  it. Creation time detects inode reuse after deletion; older records and filesystems without
  creation times retain device/inode checks. When the directory is gone, or a different
  directory now has its path, GC has Stack's supervisor stop what it recorded for the checkout
  (each process only while its pid and process start time match, so a reused pid is never
  signalled) and drops the record once nothing recorded runs; its supervisor directory goes once
  no session of it is left. For a session started under Pitchfork, GC asks Pitchfork
  (`pitchfork status --json <id>`, which reads its state without starting a supervisor).
  Pitchfork does not provide atomic compare-and-stop, so by default GC never issues a separate
  stop by daemon name for such a gone project, even when PID and port match. `gc --stop-orphans`
  is the explicit opt-in: where PID and port match it also requires the live process's start
  time to fall within [recorded start − 2 s, recorded start + 10 min] (unreadable or unrecorded
  is a refusal), queries the supervisor again, then stops the id and waits for the PID to exit
  and the port to close. The window between that last query and the stop remains. Live,
  unknown, transitional, retrying and incomplete-launch states retain the record and return
  `gc_incomplete`. A confirmed stopped daemon with no live recorded PID, or a missing daemon
  after a completed launch and dead recorded PID, permits release. Unrelated listeners remain
  untouched. Nothing is recreated, and data is kept. While ownership remains unresolved, a new
  directory at the old path gets `session_conflict`.
- **Machine-wide view.** `ps` reads the session index, `ports.json`, `identities.json` and the
  data record (`data.json`) without locks (each is replaced atomically) and checks liveness
  only: pid existence and TCP accept. It never runs identity verification. Sizes count the
  blocks of what is inside each data directory without following links.
- **Moving.** Stack keys each checkout's supervisor directory and new data directories by the
  project path, the checkout token includes the path, and running services carry both. So `mv`
  refuses while anything recorded for the old path may run (live pid, open port, supervisor not
  reporting stopped, launch in progress, a process carrying the token the directory had at the
  old path, which a move keeps computable since device, inode and creation time stay). It holds
  both paths' project locks, checks each data directory recorded under that token as `gc`
  does (`hold`: resolved location, `O_NOFOLLOW`, same device and inode, marker), and only then
  rekeys reservations and tokens, renames each held directory (`renameat` from the held
  parent, then confirmed by device and inode, renamed back otherwise) to Stack's data directory
  for the new path (`$STACK_STATE_DIR/data/<key>/<service>`), writes the new token into its marker and the data record, and
  moves the session (re-stamped with the directory) and completion records. Data moves and
  registry changes are undone if a later step fails.
- **Purging.** `down --purge` removes an existing checkout's own data after `down` confirmed
  the stop, under the same project lock and before the supervisor may stop: each data-record
  entry of this path whose token is the directory's own goes through the same reclaim as `gc
  --reclaim-data` (resolved location, `O_NOFOLLOW`, device and inode, marker, supervisor
  status, token carriers, removal through the held descriptors). Settled entries leave the
  record; the rest are kept and reported. It then removes the checkout's supervisor directory
  (records and logs) and releases the path's reservations and tokens.
- **Concurrent access.** Compile, renew, execution registration and lifecycle operations share
  a per-project advisory lock released by the kernel on process death. Each JSON write uses a
  unique temporary file and atomic replacement, flushed with `fsync` first. On macOS that is
  not `F_FULLFSYNC` (what `File::sync_all` asks for): that flushes the drive's whole cache,
  checkouts starting together queued behind one another for it, and it only adds durability
  across power loss, which Stack does not claim (Postgres too uses `fsync` there by default).
  After a power loss a replaced record can therefore come back unreadable, not only older. A
  session record has the recovery rule below; the port and identity registries, `data.json`,
  init records and data markers do not: an unreadable one fails `state_invalid` until it is
  repaired or removed by hand (removing the identity registry issues new checkout tokens, after
  which existing markers name no checkout and their data is refused, never reused).
  `up` records all of a checkout's data directories in `data.json` with one write, binds them
  with one, and writes its launch record once with what the supervisor already shows. A session record that changes ownership or what
  was launched is waited for on disk (`up` when not finding the same verified launch running,
  `restart`, `down`). One where all that changed is executions in progress, a lease renewal or
  when services were last verified (`exec`, `status`, `env`, and `up` finding the same verified
  launch running) is not, and a machine crash may leave such a record empty or partial on
  filesystems that do not flush a file's data before replacing it. A session record (the
  machine index's or the project copy) that exists but cannot be read or parsed is a state of
  its own, never no session, an expired or a stale one, and the other copy does not stand in
  for it: `status`, `exec`, `run`, `env`, `up`, `restart` and the commands that use the record
  fail `state_invalid`, naming `stack down`, and start nothing; `gc` (and its pass in another
  checkout's `up`) reports it and stops, reclaims, releases and removes nothing of that
  checkout (whichever copy is damaged, expired or not), going on with the others; `gc
  --dry-run` lists it the same way and plans nothing for it; `ps` marks it `unreadable`. Each
  names the checkout where the supervisor's `spec.json` or the port registry does, else the
  record. Only `stack down`, under the checkout's lock, recovers it. It reads both copies once
  (what decides one is unreadable is what any removal is compared with). With the supervisor's
  own records of exactly that checkout (`spec.json` naming its path and its directory's token,
  and a `state.json` that acknowledged a request (`applied` at least 1, at most the spec's
  revision), records every service the spec ever asked to run, and names every process with
  its boot), it stops what they show as `down` stops anything, each process only while its
  pid, start time and boot match (a record of an earlier boot names nothing alive, which is
  how a checkout is recovered after a reboot), and only while the state stays that evidence
  throughout the wait. Every read of `state.json` in this path must itself name that
  checkout's path and token (a `state.json` written by Stack 0.1.23's supervisor names none and
  is no evidence); the supervisor signalled is the one named by a state read that passed that
  check, and every write of `spec.json` or `state.json` is staged completely in a temporary file
  (its mode set on that file, never on the path after publishing), then re-reads the file it
  replaces (the spec must still be the very file checked, the state must name the checkout),
  with nothing but the rename after that check. A write cannot be made atomic with its check
  against a program that replaces files in Stack's own state directory; the window left is
  exactly the instant between that check and the rename, and nothing is signalled on
  unidentified state. With no supervisor
  directory at all there is nothing to stop, and the record is cleared when what can be
  checked holds: no supervisor record, no port reserved for this checkout, no process seen
  carrying its token (a process whose environment cannot be read is not seen). Commands
  Stack started are looked for as in every `down` (registry and mark). `down` reports exactly
  that (`recovery_checked`), not that nothing runs. A port reserved, a process seen carrying
  the token, no token, or a supervisor directory without usable evidence refuses. Either way
  each copy is moved to a private name beside it that is never read as a record, checked to
  be the same file with the same bytes, and only then unlinked; one that changed is put back
  without replacing anything at its path (both kept and named if something took it), and one
  that appeared is kept and named. Without that evidence, or on a stop it
  cannot confirm, it fails `state_invalid` and keeps every record, reservation and data
  directory. Every Stack command writes session records under the checkout's lock, so none
  races this; another program, or a Stack with another `STACK_STATE_DIR` (whose lock is
  another file) writing the project copy, is what the comparison guards against. Nothing here
  claims durability across power loss.
  The machine session index is authoritative if a crash interrupts updating the project
  mirror.
- **MCP execution.** Unix process groups bound command descendants. Output is drained through
  nonblocking pipes with fixed-size tails; neither a full pipe nor a detached descendant can
  extend collection beyond command completion or the deadline.
- **Cancellation.** One signal handler, installed when the first command starts, passes
  SIGINT, SIGTERM and SIGHUP on to the process group of every command running then. For a
  user's command (`exec`, `run`, MCP calls) Stack then goes on waiting, kills the group 10 seconds
  after the first signal or at the second, and reports the command once it is done; the MCP
  server then answers the call and exits with 128 + the signal. A service's `setup` is run the
  same way, and the signal cancels the start whatever the setup's exit status: `up` and
  `restart` run no later setup, request nothing more, and fail `cancelled`. A signal that comes
  as the command exits, too late to reach it, is still owed. The handler only records a
  signal; a separate thread acts on it, and may run late on a busy machine. A command's run
  therefore returns only once every signal that arrived while it was running was acted on,
  and no command starts while a signal that already arrived is still unhandled, so a signal is
  never judged after the step it cancelled. A provider command gets the
  signal and Stack ends at once, as before. A command that exits on its own leaves what it
  started running, as under a shell, unless a signal was its, including one that arrived as it
  exited and is acted on only after: its leader is then reaped only once that signal was
  handled, and its group gets the signal and, for a user's command, is killed first. Groups are
  named and unnamed under one lock with the spawn and the reap, so a signal never reaches a
  group ID that may have been reused. A
  command attached to a terminal stays in Stack's foreground group and is passed only SIGTERM,
  as its own PID: the terminal's own interrupt and hangup already reach it.
- **Projects without services** never start a supervisor: `down`, `status` and `exec` have
  nothing to reconcile.
- **Service data.** A preset's data directory is the one the supervisor last ran it with, else
  the one `data.json` records for the path, else the one an older session recorded, else the one
  mise's preset made for the path (`$MISE_STATE_DIR/daemons/<hash>/data/<service>`, found by the
  `root` its `state.json` names), else a new `$STACK_STATE_DIR/data/<checkout key>/<service>`,
  created on first start (see *Presets*). Stack knows it before the first start. Init tasks run
  after verification, through the same planning as `stack run`, once per data directory; a
  service's `setup` is the hook that runs before every start. A marker in Stack's state records
  the directory as the data record does (resolved location, device and inode) and a digest of
  the init list and task definitions, so a recreated directory re-runs init and a changed list
  is reported `stale`, never re-run silently. `init` is composed apart from the service
  definition and excluded from the session fingerprint. Snapshots (postgres and redis only)
  copy and replace only the data directory recorded for the checkout (held as `gc
  --reclaim-data` holds it), only while its service and its dependents are confirmed stopped,
  keyed by the Git common directory, and restore by exchanging a complete sibling copy into
  place, then bind the record to the new directory. The instance check compares the server's
  reported directory (from `-D`/`--dir`, not stored in the data) with the supervisor's, so
  restored data verifies as the restoring checkout's. See [service data](user/data.md).
- **Partial failure.** `up` returns completed steps, whether services may have been started
  (`changed`), and `retry_safe`. Arbitrary setup is not rolled back.

## Distribution

`git+<url>?ref=[&dir=<subdir>]` pins a commit (other parameters are rejected); `oci:<registry>/<repo>:<tag>` pins a manifest digest; `path:` pins
a content hash. A bundle's content hash is identical across transports (deterministic archives).
Every transport holds a bundle to one rule: regular files and directories with UTF-8 names, `.git`
and `.stack` excluded. Hashing refuses a symbolic link or special file anywhere in the tree
(`bundle_invalid`) instead of skipping it, so nothing is readable through a bundle that its hash
does not cover; `bundle.toml` is read only as a regular file, OCI layers carrying excluded
directories are refused at extraction, and each `paths.bin` entry must name a real directory
inside the bundle (no `..`, absolute path, link or excluded directory), checked when the bundle
is loaded and again when it is composed.
OCI bundles are artifacts (`application/vnd.stack.bundle.v1`) with one deterministic tar.gz layer.

OCI credentials (`STACK_OCI_USERNAME` / `STACK_OCI_PASSWORD`) are bound to the one origin the
operator names in `STACK_OCI_REGISTRY` (scheme, host and effective port). They are offered only
when that origin challenges a request for a reference naming it; references, redirects and
challenges cannot widen the scope, other registries are reached anonymously, and credentials
without a valid binding fail closed (`oci_credentials_unbound`). Authorization is scoped to the
original registry origin. Bearer token realms must share that origin or appear explicitly, as
origins, in `STACK_OCI_AUTH_REALMS`. HTTP is allowed for exact loopback
hosts or the explicit development opt-in `STACK_OCI_PLAIN_HTTP=1` (exactly `1`; empty, `0` or
`false` keep HTTPS); an HTTPS origin can never downgrade to HTTP. Token
requests do not follow redirects, and upload/registry redirects do not receive another origin's
authorization header.

The OCI bundle cache is shared by every project on the machine. Installing a digest takes a
lock beside its cache directory, rechecks it, extracts into a uniquely named staging directory
and renames only a complete extraction into place; failures remove the staging directory.
Extraction accepts only regular files and directories and is bounded at 256 MiB of file
content and 10,000 entries (`bundle_too_large`), since a digest bounds the download, not
what it expands to. The whole gzip stream is read through that budget before publication, so
its length and CRC are checked; only zero padding may follow the tar end marker, and nothing
may follow the single gzip member. Directories are created with default permissions rather
than their archived modes, and cleanup grants the owner access first, so no mode can leave a
staging directory behind.

## Decisions

Choices that are easy to revisit by accident, and why they were made.

- **One MCP surface.** Stack does not proxy the mise, Pitchfork or fnox MCP servers. Each has
  its own error contract and failure modes; agents get one contract from Stack.
- **`stack.lock` embeds mise's lock verbatim** under `[provider_lock]`. A Stack-shaped schema
  would lose tables mise owns (several entries per version, conda packages, sidecars).
  Committing `.config/mise/mise.lock` beside `stack.lock` was rejected: one committed lock is
  the contract, and a generated directory that is half committed is a trap.
- **Default lock platforms are the release matrix**, not the compiling machine. A lock that
  covers only a laptop gives a fresh Linux CI runner nothing. `macos-x64` is excluded: it was
  excluded while Pitchfork published no build for it, and adding it would change every
  existing lock's coverage. A project may list it.
- **`unsupported` is its own coverage state.** A backend that records no download URL is not
  automatically safe for `mise install --locked`: a cold locked npm install failed on the
  stripped dependency graph. Such pins install through the plain call, and `required` rejects
  them.
- **Discovery runs in a scratch root per call**, with only `[tools]` rendered. Rendering the full
  config ran project `[env]` templates, and a shared root raced with `compile`.
- **Stack resolves secret grants itself.** `exec` does not run through `mise x`, and `mise x`
  does not redact. fnox asking to remove a protected variable is a warning rather than an
  error, because fnox lists every `env = false` key whether requested or not.
- **Secret values under 8 bytes are refused in captured output.** Replacing every occurrence
  of a short string mangles output. This is a usability limit, not a security property;
  revisit it if real projects hit it.
- **Skills are linked by Stack, not `mise skills sync`**, which would also link Pitchfork's
  skill and teach agents to manage daemons around Stack.
- **`status` of a checkout with nothing started exits 0.** Agents run `status` as their first
  look at a worktree; exiting 1 there read as a failed command and cost a round trip. "Not
  started" is proven with the evidence `down` confirms a stop with, never inferred from a missing
  session: only when no session, supervised service process, registered command or unreadable
  registry says otherwise, `status` reads the supervisor's record strictly (an unreadable one is
  no proof) and searches every process once for the checkout's token and command mark. What it
  finds is reported as `found` and makes the checkout `started`, so an unverified service exits 1
  as before. The hot path (a session exists) does not search. A `down` that did not
  confirm leaves a marker (time and error, no process identities) that only `status` reads: it
  keeps a session-less checkout started until a `down` confirms, so `status` never contradicts
  the last `down`. Identities were tried and dropped: they had `down` reason about processes it
  can no longer show are the checkout's. A supervisor record at this path written for another token
  is another checkout's: listed as `other_checkout`, never counted as this one's (`up` here stops
  it first). The search is one pass and best effort, as `down`'s is. A confirmed `down` also leaves no
  session, so it reads as never started: nothing of it runs, and reservations and data are
  reported elsewhere (`ps`, `down --purge`). `healthy` stays false when services were declared
  (nothing was verified); `started: false` and `passed: true` carry the difference, so `passed`
  keeps matching the exit code. Asserting that services are up is `data.started and
  data.healthy`, or `exec --require-all`/`--up`, not the bare exit code.
- **Task caching is deferred.** Most tasks depend on services, and a cached `test` would replay
  results from a different database state. Only tasks with `services = []` could be cached.

## Not covered yet

- The end-to-end suite runs natively on macOS and Linux (`tests/e2e/native.sh`) and in Docker;
  CI defines a job for each OS, but a workflow definition is not evidence that it has passed.
  x64 macOS is not exercised by the service scenarios.
- Identity probes are opt-in; services without one, and presets other than Postgres and Redis,
  are liveness-only.
- Artifact checksums cover what mise can lock and downloads; releases already installed are not
  re-checked, and npm/Python dependency graphs, URL-less backends and tools mise cannot lock
  are reported but not covered.
- OCI auth is env credentials or anonymous tokens; no Docker credential helpers.
- Unattended expiry needs `stack gc --watch` running under a supervisor of your choice.
- Deleted-project cleanup depends on what was recorded at launch; it refuses rather than guesses.
- Agent productivity has not been measured beyond single observations; see
  [pilot-protocol.md](pilot-protocol.md).
