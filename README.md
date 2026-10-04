<img src="assets/icon.svg" width="64" height="64" alt="">

# stack

Reusable, agent-safe development stacks.

Pick your tools and services, publish them as a **bundle** in a git repo, and use that bundle from any
project. Each project gets its own pinned, independent instance. stack composes bundles and generates
config for existing tools ([mise](https://mise.jdx.dev) installs tools; Pitchfork, via `mise daemons`,
runs services) instead of replacing them.

> Status: early prototype, tested end to end on Linux against real mise + Pitchfork and an OCI
> registry. See [docs/DESIGN.md](docs/DESIGN.md) for what is and isn't covered yet.

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

A project uses bundles from git, an OCI registry, or a local path in `stack.toml`:

```toml
[[use]]
bundle = "git+https://github.com/acme/pybase?ref=v1"

[[use]]
bundle = "oci:ghcr.io/acme/obs:2.0.0"

[tasks.test]
run = "uv sync -q && uv run pytest -q"
services = ["postgres", "redis"]

[override.env]
LOG_LEVEL = "warn"     # both bundles set LOG_LEVEL; the project must choose
```

```sh
stack compile                          # resolve, lock, assign ports, write .config/mise/conf.d/stack.toml
stack up --ttl 30m                     # start services, verify each is *this* instance, record a session
stack exec --require postgres -- pytest
stack down                             # succeeds only once the processes are confirmed gone
```

## Guarantees

- **Pinned by commit.** `stack.lock` records each bundle's commit and content hash. Moving a tag
  upstream changes nothing until `stack compile --update`, which reports what moved.
- **No silent conflicts.** If two layers define the same key differently, compile fails with every
  conflict listed. Only `[override.*]` resolves one, and the output records what it replaced.
- **Bundles carry files.** `{{bundle_dir}}` and `paths.bin` resolve to the bundle's own files.
- **Instance values stay out of bundles.** Bundles cannot pin ports. Each checkout gets its own
  ports from a machine-wide registry (40000-49999, never service defaults), stable across restarts.
- **Never the wrong instance.** Before every `exec`, each service is checked live. Postgres and
  Redis are confirmed over the app's own `DATABASE_URL`/`REDIS_URL` to be this checkout's server
  (by data directory). Endpoints of unverified services are poisoned (host replaced with
  `unverified.stack.invalid`) so apps with hardcoded fallbacks fail loudly instead of reaching some
  other server. `--require` makes the command refuse to run instead.
- **Verified generations.** Session records fingerprint the complete compiled configuration and
  assigned ports. Changed bundles, project overrides, or ports make service checks unavailable
  until `stack up` restarts and verifies the new generation.
- **Owned lifetimes.** Sessions can lease on a TTL or a runner's PID. Active commands protect
  TTL sessions until completion; `stack gc` (and every `stack up`) reclaims expired idle ones.
  `down` retains ownership records if discovery or cleanup fails, and reports success only after
  recorded processes and ports are gone.
- **Honest failures.** `up` reports the steps it completed, whether anything changed, and whether
  retrying is safe.
- **Agent-friendly.** `--json` emits one object on stdout; errors have a stable `code`, a `hint`
  and `details`. `stack mcp` serves the same contract over MCP. Git never prompts.

## Commands

| Command | Does |
|---|---|
| `stack compile [--update \| --locked] [--reassign-ports]` | Resolve, lock, assign ports, write provider config |
| `stack inspect` | Show the composed stack, origins and ports; writes nothing |
| `stack up [--ttl 30m] [--owner-pid N]` | Start services, verify them, record a session |
| `stack status` | Verify every service now; session and lease state (exit 1 if unhealthy) |
| `stack exec [--require S \| --require-all] -- <cmd>` | Run with tools and env; unverified endpoints poisoned |
| `stack down` | Stop services and confirm they are gone |
| `stack renew` / `stack gc` | Renew this session's lease / reclaim expired sessions machine-wide |
| `stack publish <dir> oci:<registry>/<repo>:<tag>` | Publish a bundle as an OCI artifact |
| `stack mcp` | MCP server (stdio) exposing the same operations |

All accept `-C <dir>` and `--json`. `exec -C` runs in the selected project directory.
Registry credentials: `STACK_OCI_USERNAME` / `STACK_OCI_PASSWORD`. External token-service origins
require explicit approval in `STACK_OCI_AUTH_REALMS`, a comma-separated list such as
`https://auth.docker.io`. Credentials and authorization headers are never forwarded to external upload
origins or authentication redirects. HTTPS cannot redirect authentication to HTTP.

MCP execution is bounded on Unix: at most 64 KiB of each output stream is retained, and the
command's process group is terminated on timeout or completion. Detached children cannot keep
output collection waiting for EOF. Services and sessions are still tested end to end on Linux.

## Develop

```sh
cargo test                       # unit + integration tests (real git repos in temp dirs)
cargo run -- -C examples/app inspect
tests/e2e/run.sh                 # Docker: real mise + Pitchfork + OCI registry, all scenarios
```

`eval/` holds the competitor evaluation (Flox, devbox, devenv, mise) that shaped this design:
[eval/REPORT.md](eval/REPORT.md).
