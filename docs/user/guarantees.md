# Guarantees and limits

- **Pinned by commit.** `stack.lock` records each bundle's commit and content hash. Moving a tag
  upstream changes nothing until `stack compile --update`, which reports what moved.
- **Exact versions.** `stack.lock` also records, for every tool (including Pitchfork, which
  stack adds) and every supported preset service, the requested version and the exact
  release it resolved to (`3.13` → `3.13.16`, `latest` → `0.12.23`). The provider config uses
  the exact release, so a fresh machine installs the same versions after upstream releases.
  `compile` resolves only new or changed requests, `--update` re-resolves all of them and
  reports moves, and `--locked` (and `up`, `exec`, `status`) refuse missing or stale pins.
  An exact version names a release; the artifact checksums below are what pin the bytes.
  A tool's [allowlisted options](bundles.md#tool-options) are part of its pin: stack.lock
  records them, and changing one is a changed request (`lock_outdated` in locked mode).
- **Artifact checksums.** `stack.lock` embeds mise's lock for every pin on each `[lock]
  platforms` entry (and Bun's per-CPU and musl builds mise writes for it). A release downloaded
  on a machine where its coverage, under the key mise looks it up by there, is `verified` has the
  checksum stack.lock records (and for packslip-backed tools the recorded signer), or `install`
  and `up` fail with `artifact_mismatch` naming the tool. Committed checksums change only through
  `compile --update`, which reports every change; ordinary `compile` keeps them and warns when
  upstream differs. Coverage (`verified`, `exempt`, `unsupported`, `missing`) is reported per
  tool and platform, and nothing is called verified that mise did not check. Not covered: tools
  mise cannot lock for a particular release or platform, backends without download URLs (`core:rust`, `cargo`, `go`),
  npm and Python dependency graphs, and releases already installed on the machine, which mise
  does not re-check. `[lock] artifacts = "required"` turns any gap on a listed platform, or under
  the key this machine needs, into `artifact_unlocked`. See [Artifact checksums](commands.md#artifact-checksums).
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
- **Secrets by name, only where granted.** A task or command receives exactly the fnox secrets it
  is granted, resolved at start from the fnox release `stack.lock` pins; a grant never sets or
  removes a service endpoint or other variable stack controls. Values never enter `stack.lock`,
  generated configuration, session records, timings or reports, and captured `stdout` and
  `stderr` (`--json`, MCP; the strings as a JSON parser returns them) never contain a granted
  value literally, including through stack's own markers (values of at least 8 bytes that no
  marker could spell out; others are refused there). Key names are not secret and appear in the
  result. Terminal output is not redacted, transformed values are not caught, and inherited
  variables pass through: see [secret grants](secrets.md#boundary).
- **Honest failures.** `up` reports the steps it completed, whether anything changed, and whether
  retrying is safe. `up` and `restart` have a 10m startup deadline, configurable with
  `--timeout`. Expiry retains launch records and reports `timed_out`; recording the partial
  launch can take up to 10s beyond the deadline, plus connection checks.
- **Skills match the pins.** A listed [agent skill](skills.md) belongs to the exact release
  stack.lock pins (mise lists skills per active release, and stack's scratch configuration
  activates only the pinned ones). The provider's own skill is never surfaced to agents. Skill
  links are opt-in, and stack only ever replaces or removes a link it recorded and that still
  points where it left it.
- **Agent-friendly.** `--json` emits one object on stdout, including for argument errors and
  `exec` (whose output is captured into the object); errors have a stable `code`, a `hint` and
  `details`. A command that could not do its job reports `ok: false`. `stack mcp` serves the same
  contract over MCP. Git never prompts.

## Provider configuration

Stack provider commands use only the generated Stack mise configuration. Project, parent
and global mise aliases cannot reinterpret locked releases. Put application variables and
tasks in Stack bundles or `stack.toml`; `MISE_*` variables in their `[env]` are rejected.
Version resolution runs `mise latest` in a scratch provider root of its own under stack's
cache, whose only configuration is the one tool and its options; nothing in the project's
`[env]` or tasks is evaluated, and the root is removed afterwards. No scratch configuration is
written for a pin with template syntax in its name, version or options, however the pin
arrived (a manifest, a service version, an edited stack.lock). Artifact locking does the
same with one scratch root per `compile` naming every pin at its exact version; values with
template syntax are refused before mise could evaluate them.
Skills discovery (`inspect`, `compile`, the `skills` step) works the same way, in a scratch
root naming only stack.lock's pins; tool versions and options with template syntax are
rejected because mise would evaluate them there.
`stack exec` carries this same boundary into nested mise commands. Direct mise invocations
outside `stack exec` still follow mise's normal configuration rules.

[All docs](../README.md)
