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
as `fnox` that mise's registry installs through packslip (`compile` asks `mise registry`). The
trust options need mise 2026.9.2 or newer. Everything else is `invalid_tool`, with a hint
naming what the tool accepts: an unknown option (including packslip's `pin`, which is a
packslip command-line flag, not a mise option), a wrong type, a table without `version`, a
nested value, `mr_boxington` without Mr Boxington, or template syntax (`{{`, `{%`, `{#`) in a
version or option string, which mise would render (`exec()` included) wherever stack asks it
about the tool. Extending the list is a code change.

`"1.93"` and `{ version = "1.93" }` are the same value. Layers that differ in any option
conflict, and only `[override.tools]` resolves it by replacing the whole value. `stack.lock`
records the options with the pin, version resolution sees them (packslip trust options decide
which releases mise can list), and the generated config renders them with the exact release.
`install`, `up` and `doctor` fail with `provider_outdated` when mise is older than an option
needs, instead of letting mise ignore it.

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
- mise publishes the wrapper under its own data directory (`command-wrappers/bin`); it is on
  `PATH` only in environments mise activates for this stack.

See [commands](commands.md) for compile options and [guarantees](guarantees.md) for locking and conflict rules.

[All docs](../README.md)
