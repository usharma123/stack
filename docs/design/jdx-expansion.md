# Stack and the jdx tool ecosystem: selective integration

Status: design, not implemented. Drafted by Claude Fable 5.1, critiqued over two rounds by
GPT-6-Astra; disagreements and resolutions are in [Fable vs Astra](#fable-vs-astra). Facts
about upstream tools were checked against mise 2026.10.3, fnox 1.39.0, packslip 1.6.0, mbx
1.22.0 and pitchfork 2.29.0 on macOS arm64 unless a line says otherwise. "Observed" means run
here; "documented" means read in upstream docs or release notes but not run; "assumed" is
neither.

Stack borrows tool installation from mise and service supervision from Pitchfork
([DESIGN.md](../DESIGN.md)). A dogfooding pass on Stack 0.1.7 (three parallel agents on a sample
project) concluded that the rest of the jdx ecosystem should be adopted selectively. This
document designs the four routes that passed that bar and records what was decided against.

In scope:

1. Artifact checksums in `stack.lock` (mise.lock, packslip signer commitments).
2. Mr Boxington (mbx) as an optional Rust bundle.
3. fnox per-task secret grants.
4. Version-matched agent skills.

Out of scope, decided: one umbrella MCP proxying the mise, Pitchfork and fnox MCP servers.
Stack keeps one narrow MCP surface with its own error contract; proxying would import three
foreign contracts and their failure modes. Deferred: task output caching for service-dependent
tasks (results depend on live database state; see [Deferred](#deferred-task-caching)); hk and
usage (later).

A separate thread is shipping Stack 0.1.18 (`stack restart`, `.stack/` gitignore and
`session_conflict`, exec timeout results, log markers, inspect drift and service listing fixes).
Nothing here depends on those changes. Routes 1 and 4 add fields to `inspect` output and should
be rebased on them.

## Summary

| Route | What changes for users | Lock/schema | Effort |
|---|---|---|---|
| 1. Checksums | `stack.lock` v3 embeds mise's artifact lock; `up`/`install` verify downloads through `mise install --locked`; coverage is reported per tool and platform | stack.lock v3; generated `.config/mise/mise.lock` | 7-10 days |
| 2. mbx bundle | `rust-mbx` example bundle; tool entries accept an allowlist of typed mise options, which also reach version resolution | `options` recorded on lock entries | 2-3 days |
| 3. Secrets | `[tasks.x] secrets = [...]`, `stack exec --secret`, MCP `secrets`; values redacted from captured output | none | 5-8 days |
| 4. Skills | `inspect --json` lists the stack's skills with status; MCP `stack_skill`; opt-in sync later | none | 1-2 days, plus 2-3 for sync |

Order: 2, 4 (discovery), 1, 3, then 4 (sync). See [Ordering](#ordering-and-dependencies).

## Route 1: artifact checksums in stack.lock

### Goal

Close the gap [guarantees.md](../user/guarantees.md) names: "An exact version names a release; it
is not a checksum of the downloaded artifact." After this route, a machine that downloads a
locked tool gets the bytes `stack.lock` recorded for its platform, or installation fails naming
the tool. Stack never hashes artifacts itself: mise computes checksums when locking and checks
them when downloading. Stack's job is to make the lock mise checks against a function of
committed state, to keep committed checksums from changing without an explicit update, and to
say exactly which tools are covered on which platforms.

### What mise provides

Observed:

- `mise lock --platform a,b` writes `mise.lock` (`lockfile_version = 3`) beside the config root.
  Under Stack's isolation (`MISE_OVERRIDE_CONFIG_FILENAMES=.config/mise/conf.d/stack.toml` and
  the rest of `config_env`) the file is `<root>/.config/mise/mise.lock`.
- Tools declared by `[daemons.<name>] preset = ...` are locked under the preset's tool name
  (`[[tools.postgres]]`), and `mise install` installs them, so services are covered like tools.
- Each tool gets `[[tools.<name>]]` with `version`, `backend`, `specifiers`, optional `options`,
  and `[tools.<name>."platforms.<os>-<arch>"]` with `checksum`, `url`, sometimes `url_api`, and
  for the packslip backend `signer` and `repository_ids`. Conda-backed tools (postgres, redis)
  add `conda_deps` naming entries of a top-level `[conda-packages.<platform>.<pkg>]` table. npm
  tools get `aube = { path = ".mise/locks/npm-prettier/3.6.2", digest = "sha256:…" }` and a
  sidecar directory beside the lock (`locks/` next to `mise.lock`); documented: Python tools get
  a `uv` sidecar the same way, and locked installation fails when a referenced sidecar is
  missing or its digest differs.
- Downloads are checked: with a tampered checksum `mise install` fails "Checksum mismatch ...
  Expected/Actual" and installs nothing. A version that is already installed is not checked:
  `mise install --locked` with a wrong checksum reports "already installed" and succeeds.
- `MISE_LOCKED=1` (or `--locked`) refuses a tool with no entry for the current platform
  ("jq@1.7.1 is not in the lockfile"). Documented: backends that cannot record a download URL
  (asdf, cargo, gem, go, npm, pypi/pipx, ubi, core:dotnet, core:rust, core:swift, vfox plugins)
  are exempt from that check.
- `mise lock` refreshes metadata for every locked version it processes: a tampered checksum was
  silently rewritten to the upstream value on the next run, and `mise lock --dry-run --json`
  printed `[]` before and after (it reports version changes only). Entries for tools not named
  on the command line and platforms not requested are left alone.
- Coverage on four platforms (macos-arm64, macos-x64, linux-x64, linux-arm64): python, node, uv,
  jq, fnox, packslip, mbx, postgres, cockroach, nats-server and spicedb all lock. pitchfork locks
  on three (no macos-x64 artifact; mise says "1 skipped", exit 0). redis (`conda:redis-server`)
  fails to lock on every platform ("failed to solve redis-server") although it installs; mise
  then exits nonzero but still writes the other tools' entries.
- A tool can have several entries for one version when its artifact identity depends on options
  (documented, Swift's `swift_platform`); entries match on options exactly. The `options` mise
  writes are its own, not the project's: `rust = { mr_boxington = true }` and a fnox entry with
  identity options produced entries without `options`, while a postgres preset with no declared
  options got `options = { channel = "conda-forge" }`.
- `mise lock <tool>...` processes only the named tools and leaves the others' entries alone.
  When metadata cannot be fetched (offline), `mise lock` exits 0, reports the tool as "skipped",
  and keeps whatever entry was already in the file; exit status and the presence of an entry
  therefore prove nothing about whether a refresh happened.
- A stripped sidecar reference is fatal under `--locked`: a cold `mise install --locked
  npm:prettier` fails with "has no embedded-aube dependency graph in the revision 2 lockfile".
  Plain `mise install` succeeds, regenerates the sidecar directory and writes the reference back
  into the lock.
- `mise install --locked --force` re-downloads and rejects a wrong checksum (observed for an
  aqua tool; not every backend was tried).
- Any mise command that loads a config evaluates its `[env]` templates: `mise ls --json` and
  `mise skills ls --json` both ran an `exec()` template that created a file. A config containing
  only `[tools]` executed nothing.

Documented: mise refuses a packslip signer change at install; `locked_verify_provenance`
re-verifies provenance on every install; mise.lock v3 needs mise 2026.9.16 or newer. Stack
does not pin mise.

### Design

One committed lock. `stack.lock` stays the single file users commit. It embeds mise's lock
verbatim, re-nested under one table, and Stack renders `.config/mise/mise.lock` from it the way
it renders `conf.d/stack.toml`: generated, gitignored, rewritten before every provider call.
Verbatim embedding is lossless for tables and fields Stack does not know (unknown platform
keys, extra entries per version, `conda-packages`, future fields), so a newer mise's additions
survive a round trip without Stack learning them. The alternatives (a Stack-shaped schema, or
committing mise.lock with only its digest in stack.lock) are discussed under
[Fable vs Astra](#fable-vs-astra).

Lock format, version 3:

```toml
version = 3

[[bundle]]
# unchanged

[[tool]]
name = "fnox"
requested = "1.39.0"
resolved = "1.39.0"
resolved_on = "macos-arm64"          # canonical mise platform name from v3 on

[[tool]]
name = "rust"
requested = "1.93"
resolved = "1.93.1"
resolved_on = "macos-arm64"
options = { mr_boxington = true }    # route 2; part of the pin's identity

[[service]]
name = "db"
tool = "postgres"
requested = "17"
resolved = "17.6"
resolved_on = "macos-arm64"

[provider_lock]
provider = "mise"
lockfile_version = 3                 # mise's own version field, carried as written

[[provider_lock.tools.fnox]]
version = "1.39.0"
backend = "packslip:github.com/jdx/fnox"
specifiers = ["1.39.0"]

[provider_lock.tools.fnox."platforms.macos-arm64"]
checksum = "sha256:5604196e…"
url = "https://github.com/jdx/fnox/releases/download/v1.39.0/fnox-aarch64-apple-darwin.tar.gz"
signer = "sigstore-oidc:https://github.com/jdx/fnox/.github/workflows/release.yml"
repository_ids = { repository = "1078762196" }

[[provider_lock.tools.postgres]]
version = "17.6"
backend = "conda:postgresql"
specifiers = ["17.6"]

[provider_lock.tools.postgres.options]
channel = "conda-forge"

[provider_lock.tools.postgres."platforms.macos-arm64"]
checksum = "sha256:37d520f8…"
url = "https://conda.anaconda.org/conda-forge/osx-arm64/postgresql-17.6-hb74d643_2.conda"
conda_deps = ["libpq-17.6-h31f7a3a_2", "…"]

[provider_lock.conda-packages.macos-arm64."libpq-17.6-h31f7a3a_2"]
url = "…"
checksum = "sha256:f1098a2f…"
```

Rules:

- Rendering mise.lock from `[provider_lock]` is a prefix strip: every key under
  `provider_lock` except `provider` becomes a top-level key. Capturing is the inverse. The
  capture test compares parsed TOML documents, not `mise lock --dry-run --json`, which cannot see
  checksum changes.
- Stack's pins and mise's entries are matched by tool name as rendered in the generated config
  (tool name for tools, preset tool name for services) and `resolved` version. The `options`
  inside an embedded entry are mise's own (a conda channel, a Swift platform) and are carried
  verbatim, never compared with the options a manifest declares; those live on Stack's `[[tool]]`
  entry (route 2) and are a separate identity. When mise writes several entries for one version,
  all are kept. An entry whose (name, version) matches no pin is dropped at capture; a pin with
  no entry has no coverage. An embedded entry whose `version` is not that of a pin with the same
  name is `lock_invalid` (internal inconsistency), never silently kept.
- Sidecars (`aube`, `uv`) are not carried in v3. Coverage `unsupported` is derived from the
  backend (`npm:`, `pypi:`, `pipx:` and any backend whose entry carries a sidecar reference at
  capture), so it survives serialisation. At capture Stack removes the sidecar reference from
  the entry, so the rendered mise.lock never points at a file Stack does not ship. These tools
  are never installed under `--locked` (mise would refuse the missing graph); plain install
  regenerates the sidecar under `.config/mise/locks/`, which is gitignored with the rendered
  lock, and Stack re-renders the lock before every provider call anyway.
- `[lock] platforms` in `stack.toml` (project only; a bundle cannot set it) lists the mise
  platform names to lock, default `["macos-arm64", "macos-x64", "linux-x64", "linux-arm64"]`,
  Stack's own release matrix. `"current"` is accepted as a name and expands to the compiling
  machine's platform. Entries for platforms no longer listed are dropped at capture.
- `[lock] artifacts = "best-effort" | "required"`, default `best-effort`. Required: every pin
  must be `verified` or `exempt` on every listed platform, else compile fails; locked operations
  refuse a runtime platform that is not listed; v2 locks are refused.
- Platform names are mise's (`<os>-<arch>` with `arm64`, `x64`). Stack maps
  `std::env::consts` (`aarch64` to `arm64`, `x86_64` to `x64`); v3 writes canonical names in
  `resolved_on` and reads the older `macos-aarch64` form from v2.

Coverage is derived, never stored. For each pin and listed platform:

| State | Meaning |
|---|---|
| `verified` | the entry has `checksum` and `url` for the platform (plus `signer` for packslip) |
| `exempt` | the backend records neither a URL nor a dependency graph (core:rust, cargo, go, gem, ubi, asdf, vfox); mise's `--locked` accepts it as is |
| `unsupported` | the backend locks a dependency graph in a sidecar Stack does not carry (npm, pypi, pipx); not eligible for `--locked` |
| `missing` | no entry, or no entry for the platform |

`inspect --json` and `compile --json` add to each `versions[]` entry `backend`, and
`artifacts: { "<platform>": { state, checksum?, signer?, reason? } }`; `reason` is mise's
text for a `missing` platform when it was captured. `status --json` and the `install` step of
`up` report `artifacts: { platform, verified: [...], exempt: [...], unsupported: [...], missing:
[...] }` for the current platform.

Compile (`stack compile`, ordinary and `--update`):

1. Resolve and pin versions as today. Nothing below runs before every pin resolved.
2. Decide whether locking is needed: `--update`, or any pin with `missing` coverage on a listed
   platform, or a changed pin. Otherwise skip to step 6 (no network for artifacts).
3. Create a unique scratch config root under the cache (`<cache>/lock/<random>/`, removed when
   the command ends, success or not). Write a minimal lock config there: `[tools]` with every
   pin at its exact version (service pins translated to their preset tool, `postgres = "17.6"`)
   and the allowlisted options route 2 renders; no `[env]`, `[tasks]` or `[daemons]`, so
   nothing of the project's is evaluated or executed. Write a mise.lock seeded with the
   committed entries of the pins that are not being locked this time; the pins being locked
   are seeded with nothing, so an entry for them after the run can only be freshly produced.
   Nothing in the project directory is touched.
4. Run `mise lock --platform <list> <tool>...` there, naming only the pins that need entries
   (every pin under `--update`), under `configure_command` pointed at the scratch root, with a
   deadline (default 10 minutes; locking fetches metadata and for some backends artifacts) and
   a bounded output tail.
5. Read the scratch lock back and merge against the committed entries. Merge keys are
   (name, version, platform) for tool entries and (platform, package) for `conda-packages`
   records; other top-level tables mise adds are merged by their natural key when Stack knows
   it and replaced whole otherwise.
   - a key with no committed value takes the scratch value;
   - a committed value is kept unchanged unless `--update`, in which case the scratch value
     replaces it and the difference is reported as `artifact_changed` in `versions[].artifacts`
     (`checksum_was`, `url_was`, `signer_was`; for a dependency record, `dep` and the package);
   - without `--update`, a committed value whose scratch value differs is kept and reported as a
     warning (`artifacts.<name>.<platform> differs upstream; run compile --update to accept`),
     because mise refreshes everything it touches and a silent replacement would defeat the
     commitment;
   - a pin named for locking that has no scratch entry on a listed platform is `missing`; its
     `reason` is mise's stderr line naming the tool and platform when one exists. Under
     `--update` a committed entry is retained in that case and reported as `retained` with the
     reason, never as refreshed: an offline `mise lock` exits 0 and would otherwise pass for a
     successful update;
   - a `conda-packages` record that no retained entry references is pruned, and every
     `conda_deps` name must resolve to a record on its platform, else `lock_invalid`.
6. Write stack.lock and re-render the project's mise.lock from it. Resolution failures and
   `artifact_lock_failed` leave stack.lock, the provider config and the rendered lock as they
   were, as [DESIGN.md](../DESIGN.md) promises for `resolve_failed`.

`mise lock` exiting nonzero is not by itself `artifact_lock_failed`: mise reports a tool it
cannot lock that way and still writes the others (redis today). `artifact_lock_failed` is for
mise not running, the deadline passing, or the scratch lock being unreadable or malformed.
Under `artifacts = "required"` a `missing` state after merge is `artifact_unlocked`.

Locked operations (`compile --locked`, `inspect` with a lock, `install`, `up`, `exec`, `status`):

- Never run `mise lock`. Render the project's mise.lock from stack.lock before any provider
  call that installs (`install`, `up`); `exec` and `status` do not install and do not render it.
- A v3 lock whose embedded entries disagree with its pins is `lock_invalid`. A valid lock that
  lacks what current configuration or policy requires (`required` with a `missing` state,
  `required` on a v2 lock, a runtime platform not listed under `required`) is `lock_outdated`
  or `artifact_unlocked` as the table below says.
- `install` and `up` split the install into two provider calls: `mise install --locked
  <pins whose coverage on this platform is verified or exempt>` and plain `mise install <pins
  that are unsupported or missing>`; an empty partition is skipped. The step detail records both
  lists. Under `required`, an `unsupported` or `missing` pin on the runtime platform is
  `artifact_unlocked` before either call. Nothing is installed under `--locked` that mise would
  refuse, and nothing with a checked entry is installed without the check.
- mise's checksum and signer refusals are surfaced as `artifact_mismatch` with `details:
  [{ kind: "checksum" | "signer", name, platform, expected, actual, url }]` parsed from mise's
  message; when parsing fails the error is `install_failed` with mise's output tail as today.

Guarantee boundary: mise checks checksums for artifacts it downloads. A release that is
already installed on the machine is reported as installed and not re-checked. Stack's
`install` and `up` results therefore say `verified` for the policy applied, not for bytes on
disk; a fresh machine or a fresh tool store is where the guarantee bites. A forced re-download
(`mise install --locked --force`) does re-check (observed for one backend), so a later
`stack install --reinstall` could offer it; not part of this route.

Migration:

- v2 locks stay valid for locked operations under `best-effort`: they carry the exact versions
  v2 was introduced for, and every pin is `missing`. The next `stack compile` writes v3, which
  needs network access for `mise lock`. Under `required` a v2 lock is `lock_outdated`. (v1 is
  still refused outright: it lacks exact versions.)
- Older Stack releases reading v3 fail with `lock_invalid` "written by a newer stack", as today.
- `.gitignore` gains `**/.config/mise/mise.lock` and `**/.config/mise/locks/` next to the
  conf.d entry.
- A session's `config_digest` hashes stack.lock. Migrating to v3 changes it and the next `up`
  restarts services once. Kept for this scope rather than redesigning the digest.

Provider version: v3 entries need mise 2026.9.16 or newer. Commands that are about to render a
mise.lock (`compile`, `install`, `up`) run `mise version` once and fail with `provider_outdated`
(message names the minimum) before writing anything; `doctor` reports the same.

Packslip: Stack records `signer` and `repository_ids` as mise wrote them, so a different signer
is refused at install. For packslip projects not on github.com or gitlab.com, mise needs `pubkey`
or identity options on the tool; those are typed tool options (route 2).

### Guarantees after this route

- A tool downloaded on a platform where its coverage is `verified` has the checksum stack.lock
  records, or `install`/`up` fail naming the tool. Packslip-backed tools also have the recorded
  signer identity.
- Committed checksums change only through `compile --update`, and every change is reported.
- Coverage is explicit per tool and platform in every version report; nothing is called
  verified that mise did not check for.
- Not covered: tools mise cannot lock (redis today), URL-exempt backends (core:rust), dependency
  graphs of npm and Python tools, and releases already installed on the machine.

### Failure modes and codes

| Code | When | Hint |
|---|---|---|
| `artifact_lock_failed` | `mise lock` cannot run, times out, or leaves no readable scratch lock | check mise and network; stack.lock unchanged |
| `artifact_unlocked` | `required` and a pin is `missing` on a listed platform, or the runtime platform is unlisted | lock the platform, relax the policy, or change platforms |
| `artifact_mismatch` | mise refuses a download (checksum or signer) | verify upstream, `compile --update`, review the diff |
| `lock_invalid` | embedded entries disagree with pins; malformed `[provider_lock]`; v3 read by an older Stack | run `stack compile --update` / upgrade stack |
| `lock_outdated` | valid lock lacking what configuration or `required` demands (including v2 under `required`) | run `stack compile` |
| `provider_outdated` | mise older than the lock format needs | upgrade mise |
| `install_failed` | unchanged: provider failure Stack could not classify | |

Details schemas: `artifact_unlocked` lists `[{ name, platform, state, reason? }]`;
`artifact_mismatch` as above; `lock_invalid` names the entry.

### Tests

- Unit (lock.rs): v3 round trip; v2 and v1 parse; `[provider_lock]` with an entry whose version
  matches no pin is `lock_invalid`; unknown keys inside `provider_lock` survive parse and write.
- Unit (provider/mise.rs): render and capture are inverses on fixtures copied from real output
  (packslip with `repository_ids`, aqua with `url_api`, core:python, conda with deps, an entry
  with an unknown field, an npm entry with a sidecar reference, two entries for one version
  with different options); platform name mapping.
- Compile (fake mise that writes a lock into the scratch root): ordinary compile locks only
  pins that need entries, and the scratch config it writes has only `[tools]` (a fixture `[env]`
  `exec()` template must not run); the scratch root is unique per invocation and gone
  afterwards; keeps a committed checksum when the fake returns a different one and warns;
  `--update` replaces it and reports `artifact_changed`; a fake that keeps the seeded entry and
  reports "skipped" under `--update` yields `retained`, not a refresh; a changed
  `conda-packages` checksum is kept without `--update` and reported with it; unreferenced
  dependency records are pruned and a dangling `conda_deps` name is `lock_invalid`; drops
  entries for removed pins and unlisted platforms; `missing` coverage with a reason; `required`
  fails; a fake that exits nonzero after writing partial output yields `missing`, not an error;
  a fake that writes garbage yields `artifact_lock_failed` and leaves stack.lock, the config and
  the rendered lock byte-identical; a timeout likewise; offline compile with full coverage never
  calls `mise lock`; locked mode never calls it.
- Runtime (fake mise): `install` renders the lock, then calls `install --locked <covered>` and
  `install <rest>` with the right partition (a core:rust pin in the locked call as exempt, an
  npm pin in the plain call as unsupported, an empty partition skipped); a fake "Checksum
  mismatch" and a fake signer refusal give `artifact_mismatch` with parsed details; v2 lock runs
  with every pin `missing`; `required` refuses v2, an unlisted runtime platform and an
  unsupported pin before any install.
- E2E (real mise, `tests/e2e`): compile a project with fnox and postgres; remove the tool store
  entry for fnox, tamper its checksum in stack.lock, expect `artifact_mismatch` from `stack
  install`; restore, expect success; compile twice and assert the second makes no `mise lock`
  call and no diff; assert `mise install --locked` is used for the covered set.

### Open questions

- Whether to expose `stack install --reinstall` (`mise install --locked --force`) for users
  who want installed releases re-checked.
- Carrying npm and Python sidecars in a later lock version (embedding the sidecar directory's
  files as content, or a digest plus regeneration).
- Redis: track mise's conda solver for `redis-server`; until it locks, the e2e redis scenarios
  run `missing`.
- Whether `inspect` without a lock should list the platforms that would be locked.

## Route 2: Mr Boxington as an optional Rust bundle

### Goal

A Rust project using Stack gets shared, self-pruning Cargo build caching across worktrees by
adding one bundle, with no Stack runtime code. Parallel agents in worktrees are mbx's case:
each checkout's `target/` becomes a link into mbx's managed store and compiled work is shared.

### What mise and mbx provide

Observed:

- `rust = { version = "1.93.1", mr_boxington = true }` plus `mbx = "1.22.0"` in `[tools]` makes
  mise publish a Cargo command wrapper: `mise env --json` lists
  `~/.local/share/mise/command-wrappers/bin` first on `PATH`, where `cargo` is a link to mise.
  `stack exec` resolves the program on that `PATH` (`which_in`) and runs it directly, and `stack
  run` goes through `mise run`, so both reach mbx with nothing added to Stack. A `cargo build`
  in that environment created `target -> ~/Library/Caches/mbx/targets/v1/<hash>` and `mbx
  stats` counted the build.
- mbx keeps its settings in a global file (`mbx settings ls`: cache dir, GC budgets, remote
  cache); it needs no environment from the project. `mbx doctor` still warns "Cargo shim is not
  installed" in this mode; documented: doctor does not detect mise's native option.

Documented: the option arrived in mise 2026.9.2 ("Rust tools accept `mr_boxington = true`");
it applies to the first platform-supported Rust entry; mbx must be in the active tools,
"merely having mbx on PATH is not sufficient"; an explicit `[wrappers.cargo]` takes precedence.

### Design

The bundle, under `examples/bundles/rust-mbx` and publishable to OCI like any other:

```toml
[bundle]
name = "rust-mbx"
version = "1.0.0"
description = "Rust toolchain with shared Cargo build caching through Mr Boxington"

[tools]
rust = { version = "1.93", mr_boxington = true }
mbx = "1.22.0"
```

Stack support: tool entries that carry mise tool options, restricted to an allowlist of typed
options so that Stack's configuration contract does not absorb every installation and trust
knob mise has.

- Manifest: `tools.<name> = "<request>"` or `tools.<name> = { version = "<request>", <option> =
  <value>, ... }`. Both forms canonicalise to the same value: `"1.93"` and `{ version = "1.93" }`
  are equal for composition, locking and rendering.
- Allowlist, v1: `mr_boxington: bool` on `rust`; `pubkey`, `identity`, `identity_prefix`,
  `issuer: string` on tools whose registry backend is `packslip:` (route 1's trust options for
  projects off github.com and gitlab.com; documented as the backend's options; `pin` is a
  packslip CLI flag, not a mise option, and is not accepted). `pubkey` must be the literal
  minisign-format public-key line: mise also accepts a path to a `.pub` file, but a path
  resolved against an unspecified directory does not pin the key's contents and would behave
  differently in the scratch roots Stack uses, so a path is `invalid_tool` in v1. Any other
  option, a wrong type, a missing `version`, or a table or array value is `invalid_tool`, with a
  hint naming the allowed options for that tool. Extending the list is a code change with a
  test, documented in commands.md.
- Resolution: trust options must reach version resolution too, since mise cannot list a
  domain-backed packslip project's releases without a signer policy (observed: resolution with
  no config fails for such a tool). `MiseResolver` stops relying on `MISE_NO_CONFIG=1` and
  instead writes a one-tool request config in its scratch directory (`[tools] <name> =
  { version = "<request>", <options> }`) with the same isolation env as every other provider
  call; the request still cannot be reinterpreted by project or global configuration.
- Compose: the canonical value is the unit of agreement. Layers that differ in any option
  conflict and only `[override.tools]` resolves it, replacing the whole value; the override
  record shows what it replaced.
- Lock: `[[tool]]` entries record `options` (omitted when empty) as part of the pin's identity.
  Locked mode treats an option change like a request change (`lock_outdated`). These are
  Stack's declared options; the `options` inside route 1's embedded provider entries are mise's
  own and are never compared with them.
- Render: `[tools] rust = { version = "<exact>", mr_boxington = true }`.
- Validation: `mr_boxington = true` without `mbx` or `mr-boxington` in the composed tools is
  `invalid_tool` ("mise requires mr-boxington in the active tools"). `install`, `up` and
  `doctor` check `mise version` against 2026.9.2 when the option is present and fail with
  `provider_outdated`; an older mise would ignore the option and build without caching, which
  the user asked for and must not lose silently.

No `MISE_*` environment is needed, so the existing rejection stays. Build caching changes
nothing Stack verifies: services, ports, sessions and endpoints are untouched. mbx's cache is per
user and shared across projects, which is its purpose; a bundle cannot point it elsewhere.

### Failure modes and codes

| Code | When |
|---|---|
| `invalid_tool` | option not allowlisted for the tool, wrong type, missing `version`, nested value, or `mr_boxington` without mbx |
| `conflict` | layers disagree on any option |
| `provider_outdated` | mise older than 2026.9.2 with `mr_boxington` set |

### Tests

- Manifest and compose: string and table forms parse and canonicalise equal; unknown option
  (including `pin`), wrong type, missing `version`, nested value, and a `pubkey` path fail
  `invalid_tool` with the hint; bundle and project disagreeing on an option is a `conflict`
  listing both origins; an override replaces the whole table and is recorded; `mr_boxington`
  without mbx fails.
- Resolver: the request config written for `mise latest` carries the tool's options; a fake
  mise asserts the file's content; a real-mise e2e resolves and installs a packslip tool with
  an explicit `pubkey`.
- Compile (fake mise): generated config renders the table with the exact version; stack.lock
  records `requested`, `resolved` and `options`; changing an option in locked mode is
  `lock_outdated`; `inspect --json` shows options.
- Runtime (fake mise): `exec` resolves `cargo` to a wrapper directory the fake puts first on
  `PATH` (the `which_in` path); `run` passes through `mise run` unchanged. A fake `mise version`
  below 2026.9.2 makes `install` fail `provider_outdated`.
- Manual, recorded in `docs/eval`: with real mise, `stack exec -- cargo build` in two worktrees
  of the same crate shows cache hits in `mbx stats`, and `stack run build` does too.

### Open questions

- rust-analyzer: `mbx setup` writes an editor override; the mise option does not. Document that
  editors must run Cargo through `mise exec` or the wrapper, as mise's docs say.
- Whether to let bundles set `[wrappers.cargo]` explicitly for Rust managed outside mise
  (not now: Stack renders no `[wrappers]`).

## Route 3: fnox per-task secret grants

### Goal

Tasks and commands receive the secrets they are declared to need, from the project's fnox
configuration, with values never entering bundles, locks, session records, provider config, or
captured output.

### What exists today, and what integration adds

`stack exec -- fnox exec -- cmd` works now with zero integration: Stack hands fnox the stack's
environment and fnox injects every secret of the active profile. It stays the documented path
for what the design below does not support (file secrets, leases, interactive logins). What it
lacks:

1. Declaration. Nothing in `stack.toml` says a task needs a secret, so `stack inspect` and an
   agent cannot know it, and `stack run test` either works by accident (ambient env) or fails
   late. The grant belongs with the task.
2. Least privilege at the source. `fnox exec` injects the whole profile; a grant resolves only
   the listed keys.
3. Redaction. Stack captures output for `--json` and MCP and can redact only values it knows.
4. Endpoint integrity. fnox can set or remove variables; a grant is checked against the
   endpoint variables Stack verified or poisoned, before anything is applied.
5. Non-interactive. Agents must not hang on a browser login.

mise 2026.10.4 (released 2026-10-07) adds experimental task-scoped fnox secrets:
`[secrets.fnox]`, `[tasks.x] secrets = [...]`, `mise x --secrets KEY`, redaction of task
output. Stack does not use it in this design. The decisive reason is Stack's execution
contract: `exec` computes the environment with `mise env --json` and runs the program itself
(that is how withholding works), so a mise-side grant would not reach `stack exec`, and
documented: `mise x` does not redact. A second reason is compatibility: mise 2026.10.3 rejects
`secrets` in a task as "unknown field", a hard parse error that breaks every Stack command, so
any emission would have to be version-gated. Delegating task grants to mise later, keeping
Stack's compile-time checks, remains possible; see [Fable vs Astra](#fable-vs-astra).

### fnox interface (observed)

- `fnox env --json --keys A,B` prints one JSON line: `{ schema: 1, fnox_version, scope, profile,
  set: {A: "…"}, files: {}, remove: [...], missing: [], leases: [] }`. Help: "for tools that start
  processes themselves, such as mise"; callers apply `remove`, then `set`, then `files`.
- `remove` names every variable the fnox configuration declares with `env = false`, requested
  or not, including `DATABASE_URL` when a project's fnox.toml defines one. Unrequested keys
  declared with `env = true` appear in neither `set` nor `remove`.
- An unknown key is a structured error on stdout, `{ schema: 1, error: { kind: "invalid_keys",
  message, unknown: [...] } }`, exit 1. A key whose provider fails lands in `missing` with a
  warning on stderr. A malformed fnox.toml gives `{ error: { kind: "config", message } }` on
  stdout and a diagnostic on stderr that quotes the offending line of the file, which can contain
  a secret value.
- `fnox env --json --describe` is value-free: `keys: [{ key, kind, env, as_file, injectable: {
  exec, shell } }]`, `dynamic_leases`. `--non-interactive` is a global flag.

### Design

Configuration:

```toml
[tools]
fnox = "1.39.0"

[tasks.deploy]
run = "./deploy.sh"
services = ["postgres"]
secrets = ["DEPLOY_KEY", "SENTRY_DSN"]
```

- `secrets` is a list of key names. Bundles may declare it (names are not secrets). Two layers
  that define the task must agree on the whole task, as today.
- The project must list `fnox` in `[tools]` when any task declares secrets; compile fails with
  `invalid_secret` otherwise. Stack adds nothing implicitly.
- Protected variables: every variable of every service (the per-service withheld sets Stack
  already computes: `DATABASE_URL`, `REDIS_URL`, `<NAME>_URL`, `<NAME>_PORT`, `PG*`), `STACK_*`,
  `MISE_*`, `__MISE*`, `PATH`, and keys defined in `[env]`. Compile rejects a declared key that
  is protected (`invalid_secret`). Keys must match `[A-Z_][A-Z0-9_]*`.
- `stack exec --secret KEY` (repeatable) and MCP `stack_exec.secrets: [...]`, validated the same
  way at plan time. `stack run` and `stack_run` use the task's list and accept no additions.

Resolution, in `plan_exec` after service verification and endpoint withholding, only when the
plan has at least one key:

1. Locate `fnox` on the planned `PATH` and require it to be the pinned release: the path, before
   resolving links, must start with the install directory mise reports for `fnox` at the locked
   version (`install_path` from `mise ls --json` under isolation, which is
   `<installs>/fnox/<resolved>/` today and remains the version's directory under mise's
   identity install layout), and must be a regular file after resolving links. Another version,
   another tool's executable, or a link elsewhere is `secret_unavailable` ("fnox on PATH is not
   the stack's pinned release").
2. Run `fnox env --json --describe --non-interactive` (value-free), working directory the
   project root, planned environment, 30-second deadline, 64 KiB cap, process group killed at
   the deadline. Require `schema == 1`. Reject a requested key that is absent (`secret_missing`,
   `reason: "unknown"`), has `as_file = true` or `injectable.exec = false`
   (`secret_unsupported`), or appears in `dynamic_leases` (`secret_unsupported`).
3. Run `fnox env --json --keys <list> --non-interactive` the same way. Parse stdout as the
   structured protocol: an `error` object maps by `kind` (`invalid_keys` to `secret_missing`
   with its `unknown` list; `config` and anything else to `secret_unavailable` with
   `details: [{ kind, exit_code }]`); `missing` non-empty is `secret_missing`; `leases` or
   `files` non-empty is `secret_unsupported` (defensive; `--describe` should have caught it).
   Stdout that is not the protocol, a nonzero exit without a protocol error, or a timeout is
   `secret_unavailable` with exit status and timing only.
4. Apply `remove`: a protected variable in `remove` is not removed and is listed in the result's
   `warnings` ("fnox asked to remove DATABASE_URL; kept"); other names are removed. Apply `set`:
   a key not in the request is dropped (fnox may return dependencies); a key that is protected
   is `invalid_secret` and nothing is applied. Then the values are set.
5. Record key names, never values, in the result: `secrets: ["DEPLOY_KEY"]`.

fnox's stdout and stderr are never forwarded to any Stack output, error message, detail, hint,
or log. Stack reports its own messages with key names, fnox's version from the protocol, the
error kind, exit status and whether the deadline passed. The fnox diagnostic quoting a config
line is the concrete reason.

Guarantee stated narrowly: Stack resolves and injects only declared keys, and refuses grants
that would touch protected variables. Variables the caller's shell already had are inherited by
the command as before (Stack preserves inherited env byte for byte), and a command's children
inherit what it received; this mechanism does not confine them.

Redaction, in captured mode (`exec --json`, `run --json`, MCP `stack_exec`, `stack_run`):

- Every granted value is replaced by `[redacted:KEY]` in stdout and stderr before output is
  bounded, by a streaming matcher in `process::capture` that keeps a carry of (longest value
  minus one) bytes across chunks, so a value split across reads or across the retained tail is
  still caught. The 64 KiB limit applies to the redacted stream. Error messages and details
  derived from the command's output pass through the same matcher.
- Values shorter than 8 bytes are refused in captured mode (`secret_unsupported`, hint: run
  without `--json`); replacing every short substring would mangle output, and leaving them is a
  leak. Terminal mode has no minimum. The threshold is a usability restriction, not a
  confidentiality property: 8 bytes makes the matcher's replacements legible, it does not make
  a longer value safer. Astra would rather redact every nonempty value; see
  [Fable vs Astra](#fable-vs-astra).
- No replacement may write a granted value back: a key is named in its marker only when no value
  could be read across `[redacted:KEY]` (otherwise `[redacted]`), and a value that could still
  be read across a marker, the `…[truncated]…` notice or U+FFFD is refused in captured mode
  (`secret_unsupported`). A conflicting dependency value is left out of redaction.
- Without `--json`, the command owns the terminal: nothing is captured and nothing is redacted.
  The result of a terminal run cannot say otherwise because there is no result; the docs and the
  `--secret` help text say so.
- Transformed values (base64, URL-encoded, split by the program itself) are not caught.
  Documented limit.

Never: values in `stack.lock`, `conf.d/stack.toml`, the rendered mise.lock, `session.json`, the
machine index, timing output, `inspect`, `doctor`, or any log Stack writes. `inspect --json`
lists task `secrets` names. `doctor` reports whether `fnox` is in the tools and installed, and
runs `fnox env --json --describe` for its own value-free check.

### Guarantees and failure modes

- A task or command receives exactly its declared keys, resolved at start from the stack's
  pinned fnox; protected variables are never set or removed by a grant.
- Captured output never contains a granted value in literal form (values of at least 8 bytes;
  shorter ones are refused in captured mode).
- Nothing fnox prints reaches an agent through Stack.

| Code | When | Details |
|---|---|---|
| `invalid_secret` | bad key name, protected key declared or returned in `set`, `fnox` missing from tools | `[{ key, operation: "declare" / "set", reason }]` |
| `secret_missing` | fnox does not know a key, or could not resolve it | `[{ key, reason: "unknown" / "unresolved" }]` |
| `secret_unavailable` | fnox not the pinned tool, not installed, protocol violation, nonzero exit, timeout | `[{ kind?, exit_code?, timed_out }]` |
| `secret_unsupported` | file secret, lease, non-exec-injectable key, or (in captured mode) a value under 8 bytes or one stack's markers could spell out | `[{ key, reason }]` |

### Tests

- Compose and compile: `secrets` parses on bundle and project tasks; protected and malformed
  names fail `invalid_secret`; missing `fnox` tool fails; `inspect --json` shows names only.
- Runtime (fake fnox on the fixture PATH inside a fake mise install dir, like the fake mise):
  the planned command sees exactly the granted keys; `remove` applied except for protected names
  (`DATABASE_URL`, `PGHOST`, `MISE_*`, `__MISE*`, `STACK_*`, `PATH`), which produce warnings;
  undeclared keys in `set` dropped; a protected key in `set` is `invalid_secret` and nothing is
  applied; `missing`, `invalid_keys`, `config` error, invalid JSON, oversized output, nonzero
  exit, timeout each map to their code and none of the fake's sentinel strings (placed in stdout
  and stderr) appear anywhere in the result; a fnox at another version's directory, a different
  executable under the pinned directory's parent, and a link from the pinned directory to a
  file elsewhere are each refused; `--describe` with `as_file` or a lease is refused before
  values are requested.
- Redaction: values appearing twice, overlapping, on stderr, split across read chunks, at the
  truncation boundary, in multi-byte UTF-8 context, and in a timeout result are all replaced;
  a 7-byte value is refused in captured mode and accepted in terminal mode.
- After every run, grep the fixture's `.stack/`, state dir, generated config, rendered lock and
  stack.lock for the sentinel values: none.
- E2E (real fnox with a `plain` provider): `stack run` with a grant, `stack exec --secret`, MCP
  `stack_exec` with `secrets`, and a malformed fnox.toml whose quoted line never appears in the
  JSON error.

### Open questions

- File secrets: writing to a 0600 file under `.stack/tmp` removed after the command. Deferred;
  `fnox exec` covers it.
- Profile selection: fnox honours `FNOX_PROFILE` from the inherited environment and a `[secrets]`
  per-project profile could be added to `stack.toml` later.
- Delegating task grants to mise's native feature once a minimum mise can be required, keeping
  Stack's compile-time checks and `exec` path.

## Route 4: version-matched agent skills

### Goal

An agent working in a Stack checkout can find the skill for exactly the tool versions the stack
pins, through the surfaces it already uses (`stack inspect --json`, MCP), without Stack writing
into the project tree unless asked.

### What mise provides (observed)

- Packslip-installed tools may ship a skill; mise stores it at
  `<installs>/<tool>/<version>/.mise-packslip/repo/skills/<name>/` with a `SKILL.md` inside
  (fnox, mbx, packslip and pitchfork do; 4 to 12 KB each).
- `mise skills ls --json` lists `[{ name, tool, version, path }]` where `path` is the skill
  directory, for tools that are installed, active in the config mise sees, and ship a skill. A
  tool without a skill (jq) is absent; so is every tool when the config is absent.
- `mise ls --json` reports per tool `{ version, installed, active, install_path }`.
- `mise skills sync [--dir] [--prune]` links skills into `<project root>/.claude/skills`
  (`skills.dir`), records them in `.mise-skills.json`, and replaces only links it made.

### Design, phase A: discovery

- `inspect --json`, `compile --json` and MCP `stack_inspect` gain `skills: [{ tool, origin,
  version, status, name?, directory?, entrypoint? }]`, one entry per pinned tool and service
  tool, with `status` one of `available`, `no_skill`, `not_installed`, `unavailable`.
- Source of truth is the lock, not the project's generated config, which may be stale or absent
  in a fresh worktree and which `inspect` must not write. Stack writes a minimal discovery
  config into a unique scratch root under the cache (removed afterwards; the same mechanism
  and the same shape as route 1's lock config: `[tools]` with every pin at its exact version,
  service pins translated to their preset tool, allowlisted options, and nothing else, so no
  project `[env]` template or task can run) and runs `mise ls --json` and `mise skills ls
  --json` there with `configure_command`, each under a 10-second deadline. `mise ls` lists
  every installed tool and version on the machine; Stack keeps only rows whose name and version
  match a pin. A pinned tool absent from `mise skills ls` but installed is `no_skill`; not
  installed is `not_installed`. `inspect` installs nothing and writes nothing into the project;
  it does call the provider read-only and leaves only cache temporaries, which commands.md
  will say. Provider absence, a timeout or an unparseable answer makes every entry
  `unavailable` and never fails the command.
- `entrypoint` is `<directory>/SKILL.md`; `directory` is mise's `path`. If the entrypoint does
  not exist the status is `no_skill` with `reason`.
- Skills of tools Stack adds for its provider (`origin: provider`, Pitchfork today) are excluded
  by default from listing, retrieval and linking. Pitchfork's skill teaches an agent to start and
  stop daemons directly, bypassing Stack's ownership, identity checks and leases. `inspect
  --all-skills` (CLI) and `all_skills: true` (MCP) list them under `provider_skills` for humans.
- When mise lacks the `skills` command, every entry is `unavailable` and `warnings` carries
  `skills_unavailable`. Never an error.

MCP:

- `initialize.instructions` adds one sentence: "Tools in this stack may ship agent skills;
  `stack_inspect` lists them under `skills` and `stack_skill` returns one." Instructions are
  computed before any project directory is known, so content is not inlined.
- `stack_skill { dir?, tool, name }` returns the `SKILL.md` text of an `available`, non-provider
  skill enumerated by the same discovery, bounded at 64 KiB. Errors: `skill_not_found` (not
  enumerated, provider, or `not_installed`), `skill_unreadable`, `skill_too_large`. The text is a
  tool's own documentation at the pinned version; Stack neither executes nor interprets it.

### Design, phase B: opt-in sync (later)

```toml
[skills]
dir = ".claude/skills"
```

- Project-only. When set, `install` and `up` add a `skills` step after `install` that links each
  `available` non-provider skill into `<dir>`. Stack links itself rather than calling `mise
  skills sync`, which has no filter and would link the provider's skill; preferred resolution
  is an upstream filter, after which Stack's linking is dropped.
- Ownership: `<dir>/.stack-skills.json` records each link's name and target. A link is replaced
  or pruned only when it is a symlink whose current target equals the recorded one; a real
  directory, a foreign link or a retargeted link is left alone and reported as `kept` with a
  reason. `dir` must be relative and stay inside the project (`invalid_path`); skill names must
  match `[a-z0-9][a-z0-9_-]*`. Duplicate names across tools are reported and neither is linked.
- Failures in this step are warnings on `up` and `install`, not failures.
- Docs recommend gitignoring the directory (links into one user's mise installs).

### Guarantees and failure modes

- A listed skill belongs to the exact release stack.lock pins for that tool, because mise lists
  skills per active version and Stack's scratch config activates the pinned release.
- Stack never surfaces a provider tool's skill by default.
- `skills_unavailable` (warning), `skill_not_found`, `skill_unreadable`, `skill_too_large`
  (MCP), `invalid_path` and `skills_failed` (phase B warning detail).

### Tests

- Fake mise answers `ls --json` and `skills ls --json` when invoked against the scratch root:
  statuses `available`, `no_skill`, `not_installed` each appear; rows for tools and versions
  not pinned are ignored; pitchfork excluded, included with `--all-skills`; a fresh checkout
  without generated config and a checkout with a stale generated config give the same answer;
  the scratch config contains only `[tools]` and a fixture `[env]` `exec()` template never runs;
  two concurrent `inspect` calls get distinct scratch roots; `inspect` writes nothing into the
  project; a hanging or garbage-printing fake yields `unavailable`, exit 0.
- Fake mise without a `skills` subcommand yields `unavailable` plus the warning.
- MCP: `initialize` instructions contain the sentence; `stack_skill` returns bounded text;
  `skill_not_found` for a provider skill and an uninstalled tool; `skill_too_large` on a fake
  65 KiB file.
- Phase B: links created; real directory, foreign link and retargeted link kept and reported;
  stale Stack links pruned; provider skill never linked; traversal in `dir` refused; duplicate
  names reported.

### Open questions

- Agent directory conventions differ (`.claude/skills`, `.agents/skills`); one `dir` for phase
  B, a list later if asked.
- Upstreaming a filter to `mise skills sync`.

## Ordering and dependencies

1. Route 2 (2-3 days). Tool options with an allowlist and `options` on lock entries settle how
   options take part in lock identity, which route 1 depends on to match provider entries.
2. Route 4 phase A (1-2 days). Independent and small; it also introduces the scratch provider
   root that route 1 reuses, and surfaces the fnox and mbx skills agents need for routes 2 and 3.
3. Route 1 (7-10 days). The guarantee gap and the only lock format change, so it should land
   before anything else that would want a v4. The merge of shared dependency records and the
   retained-versus-refreshed reporting are the parts most likely to run long.
4. Route 3 (5-8 days). Independent in code; benefits from route 1 (fnox installed verified at
   its pin) and route 4 (agents can read fnox's skill before configuring it).
5. Route 4 phase B (2-3 days) when asked for, or dropped if an upstream filter lands first.

Total 17-26 days of focused work. Route 1's range is the widest (lock format edge cases,
sidecars, failure paths).

## Deferred: task caching

mise tasks have an experimental `cache` option (a hit restores declared outputs and replays
logs) and `sources`/`outputs`. Stack's `Task` schema has none of these, and `stack run` requires
every service to verify, so most tasks are service-dependent and must not be cached: a cached
`test` would replay results from a different database state. The one defensible subset is tasks
with `services = []` (lint, format, codegen). If pursued: add `sources`, `outputs` and `cache` to
`Task`, render them only when `services` is empty, and report hits in `run --json`. Not designed
further here. Note that documented: mise does not artifact-cache tasks that list secrets.

## Fable vs Astra

Two review rounds. Each round was a fresh delegation to GPT-6-Astra with the brief, the current
draft, prior findings and the open objections. Astra ran its own experiments against the
installed tools; every claim it made that changed the design was reproduced by Fable before
being adopted (warm installs not re-checked; `mise lock` overwriting committed checksums;
fnox's `remove` naming `DATABASE_URL`; fnox stderr quoting a config line; stripped sidecars
failing cold locked installs; `mise ls` running `[env]` templates; `pin` not being a mise
option).

Round 1 raised fifteen findings, round 2 nine more. Findings accepted outright are folded into
the routes above: F1 warm installs; F2 silent checksum refresh; F3 lossy mapping and sidecars;
F4 locking in place; F5 v2 under `required`, platform naming; F6 mise 2026.9.2 and mbx required;
F9 resolver diagnostics never forwarded; F10 narrow secrets guarantee and describe-first; F12
skills statuses; F13 `stack_skill` qualifier and link ownership; F14 `lock_invalid` versus
`lock_outdated`; F15 estimates; G1 native options are mise's, not Stack's; G2 unsupported
coverage derived from the backend; G3 merge of shared dependency records; G4 retained versus
refreshed; G5 options reach resolution; G6 `pin` removed and `pubkey` literal only; G7 minimal
discovery and lock configs; G8 unique scratch roots; G9 fnox bound to the pinned release
directory.

Disagreements, with both positions and the resolution:

1. **Lock representation (round 1, position a).** Fable first proposed a Stack-shaped schema
   mirroring mise's fields, with unknown platform keys preserved. Astra: that loses entry-level
   and top-level tables mise owns (several entries per version, `conda-packages`, sidecars);
   embed the native payload. Resolved for Astra: `[provider_lock]` is mise's lock re-nested
   verbatim. The other alternative, committing `.config/mise/mise.lock` and recording only its
   digest in stack.lock, was rejected by both: Stack's single committed lock is the contract,
   and a generated directory that is half committed and half ignored is a trap.

2. **Default platforms (D1).** Astra (round 1): lock the current platform only and let projects
   declare more. Fable: default to Stack's four-platform release matrix, because a lock that
   covers only the compiling machine gives a fresh Linux CI runner nothing, which is the case the
   guarantee exists for; the cost is visible in coverage reporting. Resolved for Fable in round
   2; Astra withdrew, noting `required` projects can declare their real subset.

3. **Protected variables in fnox's `remove` (D2).** Astra (F8): error on any protected name in
   `set` or `remove`. Fable: error on `set`; for `remove`, keep the variable and warn, since fnox
   lists every `env = false` key whether requested or not, and a project whose fnox.toml declares
   an `env = false` `DATABASE_URL` for some other purpose would otherwise lose every grant.
   Resolved for Fable in round 2, with Astra's conditions kept: protected variables are never
   touched, and the whole response is validated before anything is applied.

4. **Short secret values in captured mode (D3).** Fable: refuse values under 8 bytes
   (`secret_unsupported`) rather than replace every short substring. Astra: redact every
   nonempty value; the threshold has no confidentiality meaning and refusal blocks usable
   workflows. Unresolved on preference, resolved on safety: Astra agreed refusal is safe and
   did not insist; the design keeps the refusal and documents it as a usability restriction,
   not a security property. Revisit if real projects hit it.

5. **Tool-option allowlist scope (D4).** Astra (F7): typed allowlist starting with
   `mr_boxington` only; defer trust knobs. Fable: include packslip trust options now because
   route 1 cannot install a packslip tool off github.com or gitlab.com without them. Resolved
   for Fable's scope with Astra's conditions (G5, G6): supported options only, `pin` dropped,
   `pubkey` literal only, and options carried into version resolution.

6. **Install partition eligibility (D5).** Fable: everything URL-exempt is `--locked`
   eligible, including npm with its sidecar reference stripped. Astra: observed a cold locked
   npm install failing on the stripped graph; URL exemption does not imply eligibility.
   Resolved for Astra: `unsupported` is a distinct, backend-derived state installed only through
   the plain call, and `required` rejects it before either call.

7. **`inspect` calling the provider (D6).** Fable: discovery runs `mise ls` and `mise skills
   ls` against a scratch root rendered from the lock; a read-only command may read from the
   provider. Astra: the objection was observable execution, not reads; the rendered config ran
   project `[env]` templates, and a shared scratch root races with `compile`. Resolved for
   Astra's conditions: a `[tools]`-only discovery config, a unique scratch root per call, and
   graceful `unavailable`; discovery stays in `inspect` rather than a new command, which Astra
   did not insist on.

8. **Secrets through mise or through Stack (position c).** Both agreed on Stack-resolved
   grants. Astra's note stands: the installed mise version is not by itself the argument, since
   features can be version-gated; the decisive reason is Stack's execution contract (`exec`
   does not run through `mise x`, and `mise x` does not redact).

9. **Skills sync in the first delivery (position d).** Fable had sync in the same route;
   Astra: ship discovery first, sync later with verified link ownership, preferably through an
   upstream filter. Resolved for Astra: phase B.

10. **Effort (F15).** Fable's first total was about two weeks; Astra's 6-9, 2-3, 5-8 and 1-2
    plus 2-3 days were adopted and route 1 widened again after round 2 (7-10). Total 17-26 days.

Astra's standing conditions, all met in the text above: protected variables are never removed
or set by a grant; raw resolver output never reaches an agent; `mise lock --dry-run --json` is
not a round-trip check and ordinary compile never replaces a commitment; `required` fails
closed and warm installs are not called checksum-verified; options and skills discovery are
bound to the locked configuration with fresh-checkout and stale-config tests.
