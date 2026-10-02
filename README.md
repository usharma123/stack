<img src="assets/icon.svg" width="64" height="64" alt="">

# stack

Reusable, agent-safe development stacks.

Pick your tools and services, publish them as a **bundle** in a git repo, and use that bundle from any
project. Each project gets its own pinned, independent instance. stack composes bundles and generates
config for existing tools ([mise](https://mise.jdx.dev) installs tools; Pitchfork, via `mise daemons`,
runs services) instead of replacing them.

> Status: early prototype. `stack compile` works end to end. The session layer (per-instance ports,
> verified endpoints, ownership) is next. See [docs/DESIGN.md](docs/DESIGN.md).

## Quick look

A bundle is a git repo with a `bundle.toml` and whatever files it needs:

```toml
# bundle.toml
[bundle]
name = "pybase"
version = "1.0.0"

[tools]
python = "3.13"
uv = "latest"

[services.postgres]
preset = "postgres"
version = "17"

[tasks.seed]
run = "psql \"$DATABASE_URL\" -f {{bundle_dir}}/fixtures/seed.sql"
services = ["postgres"]

[paths]
bin = ["bin"]          # bundle-shipped CLIs go on PATH
```

A project uses bundles in `stack.toml`:

```toml
[[use]]
bundle = "git+https://github.com/acme/pybase?ref=v1"

[[use]]
bundle = "git+https://github.com/acme/obs?ref=v2"

[tasks.test]
run = "uv sync -q && uv run pytest -q"
services = ["postgres", "redis"]

[override.env]
LOG_LEVEL = "warn"     # both bundles set LOG_LEVEL; the project must choose
```

```sh
stack compile          # resolve bundles, write stack.lock + .config/mise/conf.d/stack.toml
mise run test          # plain mise now sees the stack
stack exec -- pytest   # or go through stack (refuses to run on a stale lock)
```

## Guarantees

- **Pinned by commit.** `stack.lock` records each bundle's commit and content hash. Moving a tag
  upstream changes nothing until `stack compile --update`, which reports what moved.
- **No silent conflicts.** If two layers define the same key differently, compile fails with every
  conflict listed. Only `[override.*]` resolves one, and the output records what it replaced.
- **Bundles carry files.** `{{bundle_dir}}` and `paths.bin` resolve to the bundle's own files.
- **Instance values stay out of bundles.** Bundles cannot pin ports; projects can.
- **Agent-friendly.** `--json` emits one object on stdout; errors have a stable `code`, a `hint`
  and `details`. `--locked` fails instead of changing the lock. Git never prompts.

## Commands

| Command | Does |
|---|---|
| `stack compile [--update \| --locked]` | Resolve, lock, compose, write provider config |
| `stack inspect` | Show the composed stack and origins; writes nothing |
| `stack up` / `stack down` | Start / stop services (delegates to `mise daemons`) |
| `stack exec -- <cmd>` | Run with the stack's tools and env (delegates to `mise exec`) |

All accept `-C <dir>` and `--json`.

## Develop

```sh
cargo test                       # integration tests use real git repos in temp dirs
cargo run -- -C examples/app inspect
```

`eval/` holds the competitor evaluation (Flox, devbox, devenv, mise) that shaped this design:
[eval/REPORT.md](eval/REPORT.md).
