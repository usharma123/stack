# Design

Evidence for these decisions is in [eval/REPORT.md](../eval/REPORT.md).

## What stack owns, and what it borrows

| Layer | Owner |
|---|---|
| Installing tools | mise (provider) |
| Supervising service processes | Pitchfork via `mise daemons` (provider) |
| Bundle format, composition, conflict rules, files, distribution, locking | **stack** |
| Sessions: one record of a running instance, verified endpoints, ownership | **stack** (next) |
| Agent interface: JSON, structured errors, refusing on stale state | **stack** |

Providers sit behind `src/provider/`. A bundle never names a provider, so replacing one must not
change what a bundle means.

## Objects

- **Bundle**: a versioned, shareable definition in a git repo (`bundle.toml` plus files): tools,
  env, services, tasks, bin paths. Contains no machine-specific values.
- **Project**: `stack.toml` lists bundles, adds its own definitions, and resolves conflicts in
  `[override.*]`. `stack.lock` pins each bundle by commit and content hash.
- **Session**: one running instance of a project on this machine (`.stack/session.json`, indexed
  in the machine state dir): assigned ports, supervisor PIDs, data directories, verification
  results with timestamps, and an optional lease. It records what stack started; it is never
  treated as proof of what is running.

## Composition rule

Layers apply in order: bundles (in `[[use]]` order), then the project. A key defined by two
layers must have equal values, otherwise it is a conflict. `[override.*]` is the only way to
resolve a conflict or replace a value, and every override is reported with what it replaced.

## Runtime contract

- **Ports.** Assigned per checkout from 40000-49999 via a locked machine-wide registry; reused
  across restarts; pruned when a project directory disappears; `--reassign-ports` after a foreign
  program takes one. Projects may pin ports in `[override.services]`; pins are checked against
  other projects' reservations. Service defaults (5432, 6379) are never used.
- **Verification** happens on every `exec` and `status`, not from the record: supervisor state,
  PID alive, supervisor port equals assigned port, TCP accept, then identity. Postgres and Redis
  connect with the app's own URL and compare the server's data directory to the supervisor's.
  Other services get liveness only, and are labelled `liveness` rather than `instance`.
- **Withholding.** Endpoints of unverified services are poisoned (`unverified.stack.invalid`)
  rather than unset, because apps commonly fall back to `localhost:<default>`; values with no
  host are removed. `STACK_UNVERIFIED` lists affected services. `--require` turns this into a
  refusal to run.
- **Leases.** `--ttl` (renewed by `exec`/`renew`) or `--owner-pid` (a long-lived runner, not the
  short-lived shell that ran `stack up`). Reclaimed by `stack gc` and at the start of every
  `stack up`; there is no background daemon, so expiry takes effect at the next of those.
- **Stopping.** `down` succeeds only once recorded PIDs are dead and assigned ports closed.
- **Partial failure.** `up` returns completed steps, whether services may have been started
  (`changed`), and `retry_safe`. Arbitrary setup is not rolled back.

## Distribution

`git+<url>?ref=` pins a commit; `oci:<registry>/<repo>:<tag>` pins a manifest digest; `path:` pins
a content hash. A bundle's content hash is identical across transports (deterministic archives).
OCI bundles are artifacts (`application/vnd.stack.bundle.v1`) with one deterministic tar.gz layer.

## Not covered yet

- macOS: compile/inspect are tested; services and sessions are only tested on Linux.
- Identity checks exist for Postgres and Redis presets only; other services are liveness-only.
- OCI auth is env credentials or anonymous tokens; no Docker credential helpers.
- Expired leases are reclaimed lazily (next `gc`/`up`), not by a background process.
- Sessions for deleted project directories can't be stopped by stack (see `mise daemons prune`).
