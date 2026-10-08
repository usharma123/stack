# Guarantees and limits

- **Pinned by commit.** `stack.lock` records each bundle's commit and content hash. Moving a tag
  upstream changes nothing until `stack compile --update`, which reports what moved.
- **Exact versions.** `stack.lock` also records, for every tool (including Pitchfork, which
  stack adds) and every supported preset service, the requested version and the exact
  release it resolved to (`3.13` → `3.13.16`, `latest` → `0.12.23`). The provider config uses
  the exact release, so a fresh machine installs the same versions after upstream releases.
  `compile` resolves only new or changed requests, `--update` re-resolves all of them and
  reports moves, and `--locked` (and `up`, `exec`, `status`) refuse missing or stale pins.
  An exact version names a release; it is not a checksum of the downloaded artifact.
- **No silent conflicts.** If two layers define the same key differently, compile fails with every
  conflict listed. Only `[override.*]` resolves one, and the output records what it replaced.
- **Bundles carry files.** `{{bundle_dir}}` and `paths.bin` resolve to the bundle's own files.
- **Instance values stay out of bundles.** Bundles cannot pin ports. Each checkout gets its own
  ports from a machine-wide registry (40000-49999, never service defaults), stable across restarts.
- **Never the wrong instance.** Before every `exec`, each service is checked live. Postgres and
  Redis are confirmed over the app's own `DATABASE_URL`/`REDIS_URL` to be this checkout's server
  (by data directory). Other services can opt in to an [identity probe](identity-probes.md);
  without one they are verified for liveness only, and labelled so. Endpoints of unverified services are poisoned (host replaced with
  `unverified.stack.invalid`) so apps with hardcoded fallbacks fail loudly instead of reaching some
  other server. `--require` makes the command refuse to run instead.
- **Verified generations.** Session records fingerprint the complete compiled configuration and
  assigned ports. Changed bundles, project overrides, or ports make service checks unavailable
  until `stack up` restarts and verifies the new generation.
- **Owned lifetimes.** Sessions can lease on a TTL or a runner's PID. Active commands protect
  TTL sessions until completion; `stack gc` (and every `stack up`) reclaims expired idle ones,
  and `stack gc --watch` does so periodically in the foreground for a supervisor you choose.
  For deleted or replaced projects, GC retains live or uncertain services and reports
  `gc_incomplete`: Pitchfork cannot atomically validate and stop a recorded generation.
  Stop the original checkout with `stack down` before deleting it. GC releases gone-project
  records only after shutdown is confirmed; it never signals a replacement service. Its
  recovery commands let you inspect the recorded supervisor and explicitly stop a service
  only after checking its recorded PID still matches.
- **Honest failures.** `up` reports the steps it completed, whether anything changed, and whether
  retrying is safe. `up` and `restart` have a 10m startup deadline, configurable with
  `--timeout`. Expiry retains launch records and reports `timed_out`; recording the partial
  launch can take up to 10s beyond the deadline, plus connection checks.
- **Agent-friendly.** `--json` emits one object on stdout, including for argument errors and
  `exec` (whose output is captured into the object); errors have a stable `code`, a `hint` and
  `details`. A command that could not do its job reports `ok: false`. `stack mcp` serves the same
  contract over MCP. Git never prompts.

## Provider configuration

Stack provider commands use only the generated Stack mise configuration. Project, parent
and global mise aliases cannot reinterpret locked releases. Put application variables and
tasks in Stack bundles or `stack.toml`; `MISE_*` variables in their `[env]` are rejected.
`stack exec` carries this same boundary into nested mise commands. Direct mise invocations
outside `stack exec` still follow mise's normal configuration rules.

[All docs](../README.md)
