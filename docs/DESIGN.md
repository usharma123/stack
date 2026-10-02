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
- **Session** (not built yet): one running instance of a project, with actual ports, data
  locations, resolved connection environment, ownership and observed readiness.

## Composition rule

Layers apply in order: bundles (in `[[use]]` order), then the project. A key defined by two
layers must have equal values, otherwise it is a conflict. `[override.*]` is the only way to
resolve a conflict or replace a value, and every override is reported with what it replaced.

## Next

1. **Sessions.** Allocate per-instance ports for independent checkouts (mise's `port = "auto"`
   only offsets git worktrees), and have `exec` and `inspect` read a single session record.
   `exec` re-checks the required services against the supervisor before launching; readiness for
   databases means connecting with the app's own settings and confirming instance identity.
2. **Leases.** Sessions bound to a runner or SDK lease with an expiry policy; `down` succeeds
   only once the session's processes are confirmed gone.
3. **Partial failure.** Distinguish "rejected before execution" from "failed after changes", and
   report completed steps and whether a retry is safe.
4. **Distribution.** OCI bundle references alongside git.
5. **MCP server** exposing the same contract as `--json`.
