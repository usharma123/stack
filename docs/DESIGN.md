# Design

Evidence for these decisions is in [eval/REPORT.md](../eval/REPORT.md).

## What stack owns, and what it borrows

| Layer | Owner |
|---|---|
| Installing tools | mise (provider) |
| Supervising service processes | Pitchfork via `mise daemons` (provider) |
| Bundle format, composition, conflict rules, files, distribution, locking | **stack** |
| Sessions: one record of a running instance, verified endpoints, ownership | **stack** |
| Agent interface: JSON, structured errors, refusing on stale state | **stack** |

Providers sit behind `src/provider/`. A bundle never names a provider, so replacing one must not
change what a bundle means.

## Objects

- **Bundle**: a versioned, shareable definition in a git repo (`bundle.toml` plus files): tools,
  env, services, tasks, bin paths. Contains no machine-specific values.
- **Project**: `stack.toml` lists bundles, adds its own definitions, and resolves conflicts in
  `[override.*]`. `stack.lock` pins each bundle by commit and content hash, and each tool and
  resolvable preset service by requested and exact version.
- **Session**: one running instance of a project on this machine (`.stack/session.json`, indexed
  in the machine state dir): assigned ports, supervisor PIDs, data directories, verification
  results with timestamps, and an optional lease. It records what stack started; it is never
  treated as proof of what is running.

## Composition rule

Layers apply in order: bundles (in `[[use]]` order), then the project. A key defined by two
layers must have equal values, otherwise it is a conflict. `[override.*]` is the only way to
resolve a conflict or replace a value, and every override is reported with what it replaced.

## Versions

`stack.lock` version 2 records `[[tool]]` and `[[service]]` entries: `name`, `requested` (as
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
- Provider tools stack adds (Pitchfork) are locked like any other tool.
- Postgres and Redis presets map to the mise tools `postgres` and `redis` (verified against mise
  2026.9.18: `version = "17"` installs `postgres@17`). Other presets, and presets without a
  `version`, are not locked and compile warns.
- Version 1 locks are read for migration only. `compile` rewrites them as version 2 keeping every
  bundle pin; locked operations refuse them. Moving a running session from a range to its exact
  release is a configuration change, so the next `up` restarts it. Within a major version
  (`17` → `17.11`, `8` → `8.10.2`) mise's presets reuse existing data; this was checked on macOS
  with mise 2026.9.18.
- An exact version is not an artifact checksum: the same version string could in principle be
  served as different bytes. mise's own lockfile checksums are not used yet.

## Runtime contract

- **Ports.** Assigned per checkout from 40000-49999 via a locked machine-wide registry; reused
  across restarts; pruned when a project directory disappears; `--reassign-ports` after a foreign
  program takes one. Projects may pin ports in `[override.services]`; pins are checked against
  other projects' reservations. Service defaults (5432, 6379) are never used.
- **Generations.** A session fingerprints the complete composed configuration, bundle lock and
  assigned ports. `status` reports a change as stale; `exec` withholds service endpoints until
  `up` stops the previous owned instance and verifies the new configuration. Legacy records
  without a configuration fingerprint require the same restart. Provider data-version checks
  remain in force: incompatible version changes fail explicitly and preserve the data, rather
  than silently reusing the old process or resetting the database.
- **Verification** happens on every `exec` and `status`, not from the record: supervisor state,
  PID alive, supervisor port equals assigned port, TCP accept, then identity. Postgres and Redis
  connect with the app's own URL and compare the server's data directory to the supervisor's.
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
- **Leases.** `--ttl` (renewed by `exec`/`renew`) or `--owner-pid` (a long-lived runner, not the
  short-lived shell that ran `stack up`). Owner PIDs must be 1 to 2147483647 (a positive
  `pid_t`); other values are rejected as `usage` before any lifecycle work, never truncated.
  Reclaimed by `stack gc`, at the start of every `stack up`, and by each pass of
  `stack gc --watch`, an opt-in foreground loop meant to run under a supervisor the user
  chooses. stack installs no background service.
  Commands register active executions before releasing the lifecycle lock and renew on
  completion. GC ignores TTL expiry while a coordinator is alive. An explicit runner-death
  policy still takes precedence over a surviving command.
- **Stopping.** `down` reconciles supervisor state with recorded PIDs and ports, including ports
  from an older generation. Query and stop failures preserve ownership records. Success requires
  those processes dead and ports closed.
- **Deleted projects.** At `up` the machine index records each daemon's qualified Pitchfork id,
  the Pitchfork binary, its effective state directory, and the project directory's device and
  inode, plus its creation time when the filesystem exposes it. Creation time detects inode
  reuse after deletion; older records and filesystems without creation times retain device/inode
  checks. When the directory is gone, or a different directory now has its path, GC asks Pitchfork
  (`pitchfork status --json <id>`, which reads its state without starting a supervisor).
  Pitchfork does not provide atomic compare-and-stop, so GC never issues a separate stop by
  daemon name for a gone project, even when PID and port match. Live, unknown, transitional,
  retrying and incomplete-launch states retain the record and return `gc_incomplete`.
  A confirmed stopped daemon with no live recorded PID, or a missing daemon after a completed
  launch and dead recorded PID, permits release. Unrelated listeners remain untouched.
  Use `stack down` before deleting a checkout; otherwise identify and stop the intended
  supervisor daemon explicitly before retrying GC. Nothing is recreated, and data is kept.
  While ownership remains unresolved, a new directory at the old path gets `session_conflict`.
- **Supervisor socket.** Pitchfork's socket is `<state dir>/sock/main.sock`, where the state
  directory is `PITCHFORK_STATE_DIR` (from the provider config's env, which mise passes to
  Pitchfork, else stack's own), else an absolute `XDG_STATE_HOME/pitchfork` on Linux only, else
  `$HOME/.local/state/pitchfork` (Pitchfork 2.29.0 `src/env.rs`). It must fit `sun_path` (104
  bytes on macOS, 108 on Linux). `doctor` reports it and `up` checks it before installing.
- **Concurrent access.** Compile, renew, execution registration and lifecycle operations share
  a per-project advisory lock released by the kernel on process death. Each JSON write uses a
  unique temporary file and atomic replacement. The machine session index is authoritative if
  a crash interrupts updating the project mirror.
- **MCP execution.** Unix process groups bound command descendants. Output is drained through
  nonblocking pipes with fixed-size tails; neither a full pipe nor a detached descendant can
  extend collection beyond command completion or the deadline.
- **Projects without services** never query the supervisor: `down`, `status` and `exec` have
  nothing to reconcile, and `mise daemons` is not configured for them.
- **Partial failure.** `up` returns completed steps, whether services may have been started
  (`changed`), and `retry_safe`. Arbitrary setup is not rolled back.

## Distribution

`git+<url>?ref=[&dir=<subdir>]` pins a commit (other parameters are rejected); `oci:<registry>/<repo>:<tag>` pins a manifest digest; `path:` pins
a content hash. A bundle's content hash is identical across transports (deterministic archives).
OCI bundles are artifacts (`application/vnd.stack.bundle.v1`) with one deterministic tar.gz layer.

OCI authorization is scoped to the original registry origin. Bearer token realms must share
that origin or appear explicitly in `STACK_OCI_AUTH_REALMS`. HTTP is allowed for exact loopback
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

## Not covered yet

- The end-to-end suite runs natively on macOS and Linux (`tests/e2e/native.sh`) and in Docker;
  CI defines a job for each OS, but a workflow definition is not evidence that it has passed.
  x64 macOS is not exercised by the service scenarios.
- Identity probes are opt-in; services without one, and presets other than Postgres and Redis,
  are liveness-only.
- Exact versions are release names, not artifact checksums.
- OCI auth is env credentials or anonymous tokens; no Docker credential helpers.
- Unattended expiry needs `stack gc --watch` running under a supervisor of your choice.
- Deleted-project cleanup depends on what was recorded at launch; it refuses rather than guesses.
- Agent productivity has not been measured; see [pilot-protocol.md](pilot-protocol.md).
