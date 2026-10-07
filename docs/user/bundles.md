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

[tasks.test]
run = "uv sync -q && uv run pytest -q"
services = ["postgres", "redis"]

[override.env]
LOG_LEVEL = "warn"     # both bundles set LOG_LEVEL; the project must choose
```

```sh
stack compile                          # resolve, lock, assign ports, write .config/mise/conf.d/stack.toml
stack up --ttl 30m                     # start services, verify each is *this* instance, record a session
stack run test                         # the [tasks.test] command, once the services verify
stack exec --require postgres -- psql  # any other command
stack down                             # succeeds only once the processes are confirmed gone
```

See [commands](commands.md) for compile options and [guarantees](guarantees.md) for locking and conflict rules.

[All docs](../README.md)
