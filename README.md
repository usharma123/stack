<img src="assets/icon.svg" width="64" height="64" alt="">

# stack

Reusable, agent-safe development stacks.

Pick your tools and services, publish them as a **bundle** in a git repo, and use that bundle from any
project. Each project gets its own pinned, independent instance. stack composes bundles and generates
config for existing tools ([mise](https://mise.jdx.dev) installs tools; Pitchfork, via `mise daemons`,
runs services) instead of replacing them.

> Status: early prototype. The service/session scenarios run end to end against real mise +
> Pitchfork on Linux and macOS (`tests/e2e/native.sh`, Docker `tests/e2e/run.sh`) and an OCI
> registry. See [docs/DESIGN.md](docs/DESIGN.md) for what is and isn't covered yet.

## Quick look

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
stack exec --require postgres -- pytest
stack down                             # succeeds only once the processes are confirmed gone
```

## Guarantees

- **Pinned by commit.** `stack.lock` records each bundle's commit and content hash. Moving a tag
  upstream changes nothing until `stack compile --update`, which reports what moved.
- **Exact versions.** `stack.lock` also records, for every tool (including Pitchfork, which
  stack adds) and every Postgres/Redis preset service, the requested version and the exact
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
  (by data directory). Other services can opt in to an [identity probe](#identity-probes);
  without one they are verified for liveness only, and labelled so. Endpoints of unverified services are poisoned (host replaced with
  `unverified.stack.invalid`) so apps with hardcoded fallbacks fail loudly instead of reaching some
  other server. `--require` makes the command refuse to run instead.
- **Verified generations.** Session records fingerprint the complete compiled configuration and
  assigned ports. Changed bundles, project overrides, or ports make service checks unavailable
  until `stack up` restarts and verifies the new generation.
- **Owned lifetimes.** Sessions can lease on a TTL or a runner's PID. Active commands protect
  TTL sessions until completion; `stack gc` (and every `stack up`) reclaims expired idle ones,
  and `stack gc --watch` does so periodically in the foreground for a supervisor you choose.
  Services of a deleted project are stopped through the supervisor using what was recorded at
  launch, and only when it still runs the recorded process. `down` and `gc` retain ownership
  records if discovery or cleanup fails, and report success only after recorded processes are
  gone.
- **Honest failures.** `up` reports the steps it completed, whether anything changed, and whether
  retrying is safe.
- **Agent-friendly.** `--json` emits one object on stdout, including for argument errors and
  `exec` (whose output is captured into the object); errors have a stable `code`, a `hint` and
  `details`. A command that could not do its job reports `ok: false`. `stack mcp` serves the same
  contract over MCP. Git never prompts.

## Commands

| Command | Does |
|---|---|
| `stack compile [--update \| --locked] [--reassign-ports]` | Resolve, lock, assign ports, write provider config |
| `stack inspect` | Show the composed stack, origins and ports; writes nothing |
| `stack up [--ttl 30m] [--owner-pid N]` | Start services, verify them, record a session |
| `stack status` | Verify every service now; session and lease state (exit 1 if unhealthy) |
| `stack exec [--require S \| --require-all] [--timeout D] -- <cmd>` | Run with tools and env; unverified endpoints poisoned |
| `stack down` | Stop services and confirm they are gone |
| `stack renew` / `stack gc [--watch [--interval 60s]]` | Renew this session's lease / reclaim expired and deleted-project sessions machine-wide |
| `stack publish <dir> oci:<registry>/<repo>:<tag> [--force]` | Publish a bundle as an OCI artifact |
| `stack doctor` | Check mise, git and tar, Pitchfork's socket path, and that the project compiles |
| `stack mcp` | MCP server (stdio) exposing the same operations |

All accept `-C <dir>` and `--json`. `exec -C` runs in the selected project directory.

- `inspect` before the first `compile` previews what compile would lock; afterwards it fails on drift.
- `exec --json` captures at most 64 KiB of each stream into the result and exits with the
  command's code (124 when `--timeout` expires). Without `--json` the command keeps the terminal.
- `gc` fails with `gc_incomplete` if a session it reclaims could not be confirmed stopped;
  ownership records are kept so it can be retried. For a deleted (or replaced) project directory
  it asks Pitchfork, by the daemon ids recorded at launch, what it runs, and stops a daemon only
  when its PID and port are the recorded ones. It never signals a PID itself and never recreates
  the project. Data directories are kept (`mise daemons prune` removes them).
- `gc --watch` runs until terminated (or `--max-passes N`), one pass every `--interval`; with
  `--json` it prints one object per pass. stack installs no background service: run it under
  systemd, launchd or your agent runner if you want unattended expiry.
- `compile` fails with `resolve_failed` (before writing anything) when a version request matches
  no release or names an unknown tool. This happens at compile, earlier than the
  `install_failed` that `up` reports for a release that cannot be installed on this platform.
- `stack.lock` version 1 (stack 0.1.3 and earlier) has no exact versions. `stack compile`
  migrates it, keeping every bundle pin; `compile --locked`, `inspect`, `up`, `exec` and
  `status` refuse it with `lock_outdated` until then. Older stack releases cannot read version 2.
- `up` checks, before any download, that Pitchfork's supervisor socket
  (`$PITCHFORK_STATE_DIR`, else `$XDG_STATE_HOME/pitchfork` on Linux, else
  `$HOME/.local/state/pitchfork`, then `/sock/main.sock`) fits the platform's 104 (macOS) or 108
  (Linux) bytes, and fails with `socket_path_too_long` otherwise. On macOS Pitchfork ignores
  `XDG_STATE_HOME`; set `PITCHFORK_STATE_DIR` to a short directory instead.
- `publish` refuses to move an existing tag to different content (`tag_exists`) unless `--force`.
- `compile` warns about requests that name no release (`system`, `path:`, `ref:`) and preset
  services without a `version` or whose installed tool stack does not know, since the lock
  cannot pin those.
- Git sources accept only `ref=` and `dir=`; anything else is an error rather than ignored.
  Values are percent-decoded once, so `dir=a%26b` names the directory `a&b`.
- `up` and `exec` mark the generated mise config as trusted, so `run` commands from the bundles
  you use execute without mise's trust prompt. Review bundles as you would any dependency.
- `compile` also warns about service presets mise does not document (it currently documents
  cockroachdb, nats, postgres, redis and spicedb).
- For a custom service, connect with `http://127.0.0.1:$<NAME>_PORT`. mise also sets
  `<NAME>_URL` to a Pitchfork proxy hostname (`https://<name>.<project>.localhost`), which only
  answers when Pitchfork's proxy is running.
- A custom service's `run` should `exec` its server (`run = "exec python3 -m http.server $PORT"`),
  so the supervisor stops the server itself rather than a wrapping shell.
### Identity probes

A custom service can prove which instance it is. stack gives each checkout's service a random
token in `STACK_IDENTITY_<NAME>`; the probe asks the live service through the app's own
connection settings and must print exactly that token:

```toml
[services.web]
run = "exec python3 {{bundle_dir}}/server.py"     # serves $STACK_IDENTITY_WEB at /identity

[services.web.identity]
command = "python3 {{bundle_dir}}/probe.py"       # prints what $WEB_PORT/identity returns
timeout = "5s"                                    # default 5s, at most 30s
```

The probe runs with `sh -c` in the project, with the stack's environment minus every
`STACK_IDENTITY_*` variable, so it can only learn the token from the service. Exit status other
than 0, no output, any other output, more than 4 KiB, or the deadline (its whole process group is
killed) all fail verification and withhold the service's endpoints. A server that is healthy but
belongs to another checkout or to nothing stack started cannot pass. Probes, like `run`, are
trusted code from the bundle: they are bounded, not sandboxed. See
[examples/bundles/webid](examples/bundles/webid).

Registry credentials: `STACK_OCI_USERNAME` / `STACK_OCI_PASSWORD`. External token-service origins
require explicit approval in `STACK_OCI_AUTH_REALMS`, a comma-separated list such as
`https://auth.docker.io`. Credentials and authorization headers are never forwarded to external upload
origins or authentication redirects. HTTPS cannot redirect authentication to HTTP. Plain HTTP
is used only for loopback registries, or elsewhere with exactly `STACK_OCI_PLAIN_HTTP=1`.

MCP execution is bounded on Unix: at most 64 KiB of each output stream is retained, and the
command's process group is terminated on timeout or completion. Detached children cannot keep
output collection waiting for EOF.

## Install

Install the CLI with:

```sh
npm install -g @ushawarma/stack
stack --version
```

Prebuilt binaries cover macOS 13+ and Linux (static, any distribution or libc), on x64 and arm64.
Services need [mise](https://mise.jdx.dev) on PATH; stack installs everything else, including
Pitchfork, through it. Run `stack doctor` to check a machine.
Node.js 22.14+ is required. See [docs/RELEASING.md](docs/RELEASING.md) for CI checks,
trusted publishing setup, release tags, and recovery.

## Develop

```sh
cargo test                       # unit + integration tests (real git repos in temp dirs)
cargo run -- -C examples/app inspect
tests/e2e/run.sh                 # Docker: real mise + Pitchfork + OCI registry, all scenarios
tests/e2e/native.sh target/release/stack   # same scenarios on this macOS/Linux host (needs mise)
python3 eval/harness/pilot.py --stack target/release/stack   # scripted concurrency pilot
```

`eval/` holds the competitor evaluation (Flox, devbox, devenv, mise) that shaped this design:
[eval/REPORT.md](eval/REPORT.md).

Stack provider commands use only the generated Stack mise configuration. Project, parent
and global mise aliases cannot reinterpret locked releases. Put application variables and
tasks in Stack bundles or `stack.toml`; `MISE_*` variables in their `[env]` are rejected.
`stack exec` carries this same boundary into nested mise commands. Direct mise invocations
outside `stack exec` still follow mise's normal configuration rules.
