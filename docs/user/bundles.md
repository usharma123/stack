# Bundles and project configuration

A bundle is a git repo with a `bundle.toml` and whatever files it needs:

```toml
# bundle.toml
[bundle]
name = "pybase"
version = "1.0.0"

[tools]
python = "3.13"       # a request; stack.lock records the exact release (e.g. 3.13.16)
uv = "0.12.23"

[services.postgres]
preset = "postgres"
version = "17"

[tasks.seed]
run = "psql \"$DATABASE_URL\" -f {{bundle_dir}}/fixtures/seed.sql"
services = ["postgres"]

[paths]
bin = ["bin"]          # bundle-shipped CLIs go on PATH
```

A project uses bundles from git, an OCI registry, or a local path in `stack.toml` (or defines
everything itself; `[[use]]` is optional):

```toml
[[use]]
bundle = "git+https://github.com/acme/pybase?ref=v1"

[[use]]
bundle = "oci:ghcr.io/acme/obs:2.0.0"

[[use]]
bundle = "git+https://github.com/acme/bundles?ref=v3&dir=node"   # a bundle in a subdirectory

[[use]]
bundle = "path:../bundles/team"   # a local directory, relative to this stack.toml

[tasks.test]
run = "uv sync -q && uv run pytest -q"
services = ["postgres", "redis"]

[override.env]
LOG_LEVEL = "warn"     # both bundles set LOG_LEVEL; the project must choose
```

A local path needs the `path:` prefix; a bare `../bundles/team` fails with `source_invalid`.
`stack.lock` records a local bundle's content hash, so after editing it run `stack compile`:
`compile --locked` reports `lock_outdated` until you do.

A bundle holds only regular files and directories, with UTF-8 names. Every transport (git, OCI,
local path) refuses a symbolic link, socket or other special file anywhere in the bundle with
`bundle_invalid`, rather than leave content readable through the bundle that its content hash
does not cover; replace a link with a copy of what it points to. `.git` and `.stack` are never
bundle content: they are not hashed or published, and an OCI layer containing them is refused.
Each `paths.bin` entry must be a relative path to a real directory inside the bundle: `..`,
absolute paths, symbolic links along the way, `.git` and `.stack` are `bundle_invalid`, and a
directory that does not exist is `missing_path`.

```sh
stack compile                          # resolve, lock, assign ports, write .config/mise/conf.d/stack.toml
stack up --ttl 30m                     # start services, verify each is *this* instance, record a session
stack run test                         # the [tasks.test] command, once the services verify
stack exec --require postgres -- psql  # any other command
stack down                             # succeeds only once the processes are confirmed gone
```

A task can also list `secrets = ["DEPLOY_KEY"]`: names of fnox secrets `stack run` grants it.
Bundles may declare names (never values); the stack must list `fnox` in `[tools]`, and a name
may not be a service endpoint, an `[env]` key or a variable stack reserves. See
[secret grants](secrets.md).

## Services

A service is a preset (`postgres`, `redis`, `cockroachdb`, `nats`, `spicedb`) or a command of
your own. Stack's own supervisor runs both, one per checkout (see
[worktrees](worktrees.md#services-and-the-supervisor)); mise installs their tools.

Use each preset at most once per stack. Every service of a preset exports the same variable
names (`DATABASE_URL` and `PG*` for every Postgres service, `REDIS_URL` for every Redis one),
so with two of them only one service's values could reach the app and the other could never
pass its instance check. A stack with two `postgres` or two `redis` services, from any mix of
bundles, the project and `[override.services]`, therefore fails `stack compile` (and `up`,
`exec` and `run`) with `invalid_service` naming each service and its layer, before any port is
reserved or service started. A service several layers define identically is one service and is
fine. For a second database of the same kind, use a service of your own with `run`; to replace
a bundle's preset service, set `[override.services.<name>]` to such a definition.

```toml
[services.web]
run = "exec python3 -m http.server $PORT --bind 127.0.0.1"   # `exec`: stopping it stops the server
ready_cmd = "curl -fsS http://127.0.0.1:$PORT/ >/dev/null"
port = "auto"                                                  # the default
watch = ["index.html"]
```

| Field | Default | Meaning |
|---|---|---|
| `preset` | none | Run that server the way mise's preset did (same data layout, `initdb -U postgres --auth=trust` for Postgres); `version` picks its release (default `latest`, pinned in stack.lock) |
| `version` | `latest` | Preset services only |
| `run` | the preset's | Shell command (`/bin/sh -c`) the supervisor starts, in a process group of its own. It gets `$PORT`, the port stack assigned; the app gets it as `$<NAME>_PORT`, and a service without a preset also as `$<NAME>_URL` (`http://127.0.0.1:<port>`) |
| `ready_cmd` | none | Command the supervisor runs (`sh -c`, `sh` from the stack's PATH) until it succeeds before it reports the service running. Stack runs it the same way where it checks readiness itself; one it cannot run does not pass, and the error says why |
| `ready_port` | none (postgres and redis: the assigned port) | A port the supervisor waits to accept connections before it reports the service running |
| `port` | `"auto"` | `"auto"` takes a free port from this machine's registry (20000-29999), stable per checkout; a number pins it, in the project's `stack.toml` or `[override.services]` only (`bundle_fixed_port` in a bundle) |
| `identity` | none | An [identity probe](identity-probes.md): without one a custom service is verified for liveness only |
| `watch` | `[]` | Files whose change after start `status`, `exec` and `run` report, with a `stack restart` hint |
| `setup` | none | Command run before each start of the service (migrations, seeding, `uv sync --frozen`): `sh -c` in the project, with the stack's tools and environment and `$PORT`, once the services it `depends_on` verified. Not run when `up` keeps a running service. A failure is `setup_failed` with its exit status and last lines, and the service is not started. A signal sent to Stack meanwhile is passed on to the setup and cancels the start (`cancelled`), however the setup exits |
| `depends_on` | `[]` | Services this one needs: they start, and pass their checks, before it does. `stack restart` of one restarts this one after it, and `up` restarting one (it died, say) stops this one first and starts it again after, so it reconnects. `stack restart` of this one alone needs the ones it depends on running and ready, or fails `dependency_not_ready` before stopping, setting up or starting anything. Names must be services of the stack, without cycles (`dependency_cycle`) |
| `init` | none | Tasks `up` runs, in order, once per new data directory, after every service verified, such as `["migrate", "seed"]`: unlike `setup`, which runs before every start, init runs once and reaches the running service. Composed as its own key: a table with only `init` adds it to another layer's service. See [service data](data.md) |

Whatever the readiness settings, `up` verifies every service itself: the supervisor reports it
running, its process is alive, and the assigned port accepts connections, plus the preset's
instance check or the identity probe. `{{bundle_dir}}` expands in `run`, `ready_cmd`,
`identity.command` and `watch`. A service that exits on its own is not restarted: `status`
reports it not running and `stack up` starts it again. Postgres is stopped with `SIGINT` (its
fast shutdown), everything else with `SIGTERM`, each sent to the service's whole process group,
which is killed 10 seconds later if it is still there.

## Tool options

A tool is a version request, or a table with `version` and options from a short allowlist:

```toml
[tools]
jq = "1.7.1"                                   # same as jq = { version = "1.7.1" }
rust = { version = "1.93", mr_boxington = true }
mbx = "1.22.0"
"packslip:git.example.com/acme/tool" = { version = "2", identity = "https://ci.example.com/acme/tool" }
```

| Option | Type | Tools |
|---|---|---|
| `mr_boxington` | boolean | `rust` (`core:rust`); needs `mbx` (or `mr-boxington`) in the tools and mise 2026.9.2 or newer |
| `pubkey` | string: the literal minisign public key line (`RW…`, 56 characters), never a path | packslip-backed tools |
| `identity`, `identity_prefix`, `issuer` | string | packslip-backed tools |

A packslip-backed tool is one named `packslip:<host>/<owner>/<repo>`, or a registry name such
as `fnox` that mise installs through packslip. Anything else is `invalid_tool`, with a hint
naming what the tool accepts: an unknown option, a wrong type, a table without `version`, a
nested value, `mr_boxington` without Mr Boxington, or template syntax in a version or option.

`"1.93"` and `{ version = "1.93" }` are the same value. Layers that differ in any option
conflict, and only `[override.tools]` resolves it by replacing the whole value. `stack.lock`
records the options with the pin. `install`, `up` and `doctor` fail with `provider_outdated`
when mise is too old for an option, instead of letting mise ignore it.

## Rust build caching with Mr Boxington

The [`rust-mbx`](../../examples/bundles/rust-mbx/bundle.toml) bundle pins Rust with
`mr_boxington = true` and Mr Boxington (`mbx`). mise then puts a Cargo wrapper first on the
stack's `PATH`, so `stack exec -- cargo build` and tasks run through `stack run` build through
mbx: each checkout's `target/` becomes a link into mbx's store, and worktrees of one crate share
compiled work. Stack adds no runtime code for it, and services, ports and sessions are
unaffected.

```toml
[[use]]
bundle = "path:../bundles/rust-mbx"   # or publish it to OCI like any bundle

[tasks.build]
run = "cargo build"
```

- mbx keeps its cache and settings per user (`mbx settings ls`), shared across projects; a
  bundle cannot point it elsewhere. Set `MBX_CACHE_DIR` in your own environment to move it.
- `mbx doctor` warns that its Cargo shim is not installed; it does not detect mise's option.
  There is no need to run `mbx setup`.
- Editors run Cargo themselves. As mise's documentation says, run them (or rust-analyzer's
  Cargo) through `stack exec` or mise's wrapper; the editor override `mbx setup` writes is not
  part of the mise option.

## Artifact lock settings

`[lock]` (`platforms`, `artifacts`) belongs to the project's `stack.toml` only; a bundle that
sets it is `bundle_invalid`. So does `[session]` (`default_ttl`, the lease a session gets when
the command starting it names none; see [leases](commands.md#leases)). Bundles cannot weaken or widen a project's artifact policy. See
[Artifact checksums](commands.md#artifact-checksums).

See [commands](commands.md) for compile options and [guarantees](guarantees.md) for locking and conflict rules.

[All docs](../README.md)
