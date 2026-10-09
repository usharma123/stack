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
your own. Pitchfork supervises both through mise.

```toml
[services.web]
run = "exec python3 -m http.server $PORT --bind 127.0.0.1"   # `exec`: stopping it stops the server
ready_cmd = "curl -fsS http://127.0.0.1:$PORT/ >/dev/null"
port = "auto"                                                  # the default
watch = ["index.html"]
```

| Field | Default | Meaning |
|---|---|---|
| `preset` | none | Use mise's preset for that server; `version` picks its release (default `latest`, pinned in stack.lock) |
| `version` | `latest` | Preset services only |
| `run` | the preset's | Shell command Pitchfork starts. It gets `$PORT`, the port stack assigned; the app gets it as `$<NAME>_PORT` |
| `ready_cmd` | none | Command Pitchfork runs until it succeeds before it reports the service started |
| `ready_port` | none (postgres and redis: the assigned port) | A port Pitchfork waits to accept connections before it reports the service started |
| `port` | `"auto"` | `"auto"` takes a free port from this machine's registry (40000-49999), stable per checkout; a number pins it, in the project's `stack.toml` or `[override.services]` only (`bundle_fixed_port` in a bundle) |
| `identity` | none | An [identity probe](identity-probes.md): without one a custom service is verified for liveness only |
| `watch` | `[]` | Files whose change after start `status`, `exec` and `run` report, with a `stack restart` hint |

Whatever the readiness settings, `up` verifies every service itself: Pitchfork reports it
running, its process is alive, and the assigned port accepts connections, plus the preset's
instance check or the identity probe. `{{bundle_dir}}` expands in `run`, `ready_cmd`,
`identity.command` and `watch`. Services need Pitchfork, which has no Intel macOS build (see
[install](install.md)).

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

See [commands](commands.md) for compile options and [guarantees](guarantees.md) for locking and conflict rules.

[All docs](../README.md)

## Artifact lock settings

`[lock]` (`platforms`, `artifacts`) belongs to the project's `stack.toml` only; a bundle that
sets it is `bundle_invalid`. Bundles cannot weaken or widen a project's artifact policy. See
[Artifact checksums](commands.md#artifact-checksums).

