# pkgx and dev

Checked 2026-10-06. Treat pkgx as a tool and environment provider. PostgreSQL and Redis lifecycle, readiness, ports, and persistent data belong to checked-in application scripts. `dev` adds project discovery and shell activation. It does not add a supervisor. Benchmark the completed application workflow and label the service management as scripted.

## Versions and executed evidence

| Component | Latest release checked | Source inspected |
| --- | --- | --- |
| pkgx | [v2.11.0](https://github.com/pkgxdev/pkgx/releases/tag/v2.11.0), published 2026-07-22 | Stable `99d47b6960ec6ff865d73c6c673b156507e62a33`; main `6de1d7e953b98061f69db95d4bd45a2a6ee5d7da` |
| dev | [v1.8.1](https://github.com/pkgxdev/dev/releases/tag/v1.8.1), published 2025-05-07 | Stable `fee70184d83a2822966e5f8fd812f2c6be8b2147`; main `7818a5380166c3b5e72bc1e4ef571d7c785dfbe9` |
| Pantry | Repository main, no release selected | `2df061bd184985428bc17aba4a8a8c1e2fd39781` |

The CLI's stable-to-main diff changes Cargo dependencies and CI only. The dev diff adds quality gates, an activation-message spelling fix, and a documented shellcode invocation change. `src/sniff.ts` and `src/dump.ts` are unchanged. Use released binaries for measurement and the stable permalinks below for implementation claims.

No pkgx or dev command was initially installed on this Darwin ARM64 host. Downloaded the official v2.11.0 Darwin ARM64 release into `/tmp/stack-bench-sources/pkgx-cli`, confirmed a Mach-O ARM64 executable, and executed only `--version` and `--help`. Output was `pkgx 2.11.0`. No package installation, activation, service startup, integration test, or timing was executed. The local Docker daemon reports no `pkgxdev/pkgx:latest` image. No image version or digest can therefore be claimed as measured.

Read-only HTTP checks fetched platform-specific inventory and sent HEAD requests to actual bottles. All five proposed pins below returned HTTP 200 for Darwin ARM64, Linux x86-64, and Linux ARM64. This proves distribution availability, not runtime compatibility.

| Package ID | Proposed common pin | Newest matching inventory observed |
| --- | --- | --- |
| `python.org` | `3.13.7` | `3.13.16` on all three platforms |
| `postgresql.org` | `17.2.0` | `17.2.0`; only `17.0.0` and `17.2.0` are listed in the 17.x line |
| `redis.io` | `8.0.0` | Darwin ARM64 `8.10.2`; Linux x86-64 and ARM64 `8.10.0` |
| `astral.sh/uv` | `0.8.22` | `0.12.23` on all three platforms |
| `pkgx.sh/dev` | `1.8.1` | `1.8.1` on all three platforms |

Example checked URLs: [Python inventory](https://dist.pkgx.dev/python.org/darwin/aarch64/versions.txt), [PostgreSQL inventory](https://dist.pkgx.dev/postgresql.org/linux/x86-64/versions.txt), [Redis inventory](https://dist.pkgx.dev/redis.io/linux/x86-64/versions.txt), [Python bottle](https://dist.pkgx.dev/python.org/darwin/aarch64/v3.13.7.tar.xz), [PostgreSQL bottle](https://dist.pkgx.dev/postgresql.org/linux/x86-64/v17.2.0.tar.xz), [Redis bottle](https://dist.pkgx.dev/redis.io/linux/aarch64/v8.0.0.tar.xz), [uv bottle](https://dist.pkgx.dev/astral.sh/uv/linux/x86-64/v0.8.22.tar.xz), [dev bottle](https://dist.pkgx.dev/pkgx.sh/dev/darwin/aarch64/v1.8.1.tar.xz). Repeat these preflights for the execution platform. Package metadata is not sufficient because source and bottle inventories can differ, as the [official API documentation](https://docs.pkgx.sh/appendix/packaging/pantry-api.md) states.

## Installation and platform boundary

Pin the CLI release rather than running an installer that follows latest. For an unprivileged benchmark installation:

```sh
set -eu
case "$(uname -s)" in Darwin) pkgx_platform=darwin ;; Linux) pkgx_platform=linux ;; *) exit 2 ;; esac
case "$(uname -m)" in arm64|aarch64) pkgx_arch=aarch64 ;; x86_64) pkgx_arch=x86-64 ;; *) exit 2 ;; esac
BENCH_TOOL_DIR="$PWD/.bench-tools"
mkdir -p "$BENCH_TOOL_DIR"
curl -fsSL "https://github.com/pkgxdev/pkgx/releases/download/v2.11.0/pkgx-2.11.0%2B${pkgx_platform}%2B${pkgx_arch}.tar.gz" \
  -o "$BENCH_TOOL_DIR/pkgx.tar.gz"
tar -xzf "$BENCH_TOOL_DIR/pkgx.tar.gz" -C "$BENCH_TOOL_DIR"
export PKGX="$BENCH_TOOL_DIR/pkgx"
export PATH="$BENCH_TOOL_DIR:$PATH"
"$PKGX" --version
```

Record the downloaded artifact hash and exact executable path. Native pkgx supports Darwin and Linux x86-64/ARM64. Windows binaries exist, but the [official install documentation](https://github.com/pkgxdev/pkgx/blob/99d47b6960ec6ff865d73c6c673b156507e62a33/docs/installing-pkgx.md) calls Windows packages limited. The [dev README](https://github.com/pkgxdev/dev/blob/fee70184d83a2822966e5f8fd812f2c6be8b2147/README.md) supports macOS/Linux and Bash/Zsh for shell hooks. Do not classify this Unix shell scenario as verified Windows support.

The [default Dockerfile](https://github.com/pkgxdev/pkgx/blob/99d47b6960ec6ff865d73c6c673b156507e62a33/docker/Dockerfile.debian#L6) ships Debian plus development libraries; `slim` omits those libraries. Both use pkgx as ENTRYPOINT. For a container benchmark, pin the image digest and verify the contained CLI version. Run PostgreSQL under a writable, non-root UID, since `initdb` rejects root. Comparing a native run to a container run must account for that boundary. Do not add sudo/pkgm global installation to the native benchmark merely to activate a project.

## Configuration and package resolution

Check in this `pkgx.yaml` with the application, lifecycle scripts, `pyproject.toml`, `.python-version`, and `uv.lock`:

```yaml
dependencies:
  python.org: '=3.13.7'
  postgresql.org: '=17.2.0'
  redis.io: '=8.0.0'
  astral.sh/uv: '=0.8.22'
```

Use `.python-version` containing `3.13.7` and the shared fixture's Python requirement. `dev` reads all recognizable files in a project. It detects `uv.lock`, but its `pyproject.toml` parser selects pip or Poetry and does not parse the project's Python requirement. A Python version file or explicit pkgx dependency is needed. See [discovery and YAML parsing](https://github.com/pkgxdev/dev/blob/fee70184d83a2822966e5f8fd812f2c6be8b2147/src/sniff.ts).

The most direct noninteractive environment entry uses exact fully qualified package IDs and an explicit system shell:

```sh
export PKGX_DIR="$BENCH_CACHE/pkgx"
"$PKGX" +python.org=3.13.7 +postgresql.org=17.2.0 +redis.io=8.0.0 +astral.sh/uv=0.8.22 \
  -- /bin/bash --noprofile --norc scripts/scenario.sh
```

`BENCH_CACHE` must be an absolute path. Share it across checkouts for a warm-cache test; allocate fresh caches for cold-install measurements. Tools may be shared while database/cache data must be separate. Set cache policy before measuring.

For the integrated dev variant, use temporary activation inside a new shell, without modifying shell RC files:

```sh
set -euo pipefail
export PATH="$(dirname "$PKGX"):$PATH"
dev_env="$("$PKGX" +pkgx.sh/dev=1.8.1 -- dev)"
eval "$dev_env"
python --version
postgres --version
redis-server --version
uv --version
/bin/bash --noprofile --norc scripts/scenario.sh
```

The official [temporary activation route](https://github.com/pkgxdev/dev/blob/fee70184d83a2822966e5f8fd812f2c6be8b2147/README.md) supports `eval "$(pkgx dev)"`. Capture first so an outer command failure cannot vanish inside `eval`. Version and path checks remain required, since [dump.ts](https://github.com/pkgxdev/dev/blob/fee70184d83a2822966e5f8fd812f2c6be8b2147/src/dump.ts#L27) awaits the inner pkgx subprocess but does not inspect its success flag. Include the extra dev/Deno/package discovery costs only in the integrated row. The package-only row can use direct environment entry above.

Neither pkgx nor dev has a project lockfile. Exact pins constrain the named packages, but dependencies and companions still come from the Pantry and installed cellar. [CLI resolution](https://github.com/pkgxdev/pkgx/blob/99d47b6960ec6ff865d73c6c673b156507e62a33/crates/cli/src/resolve.rs#L73) adds companions and hydrates dependencies. [Library resolution](https://github.com/pkgxdev/pkgx/blob/99d47b6960ec6ff865d73c6c673b156507e62a33/crates/lib/src/resolve.rs#L30) prefers an installed matching version before consulting inventory. A broad constraint can therefore resolve differently in fresh and populated caches.

Freeze Pantry by checking out the recorded SHA and setting absolute `PKGX_PANTRY_DIR` to that clone. [sync.rs](https://github.com/pkgxdev/pkgx/blob/99d47b6960ec6ff865d73c6c673b156507e62a33/crates/lib/src/sync.rs#L21) builds its local SQLite metadata and respects a supplied Pantry clone. Record `--json=v1` environment output with the same exact package arguments, plus the versions and hashes of all downloaded bottles. v1 has an installation array. [v2 uses a map keyed by project](https://github.com/pkgxdev/pkgx/blob/99d47b6960ec6ff865d73c6c673b156507e62a33/crates/cli/src/dump.rs#L33), so it can overwrite entries when OpenSSL/ICU/abseil versions coexist. A resolution receipt is an audit artifact, not a native replay lock. Reusing only `pkgx.yaml` and `uv.lock` pins the top-level tools and Python dependencies, but does not prove identical transitive system libraries.

## Scripted service lifecycle

This proposed `scripts/services.sh` is an application script, not a pkgx API. It assumes an exclusively assigned pair of loopback ports, a checkout path without newlines, and one lifecycle action at a time. The parent harness must call `down` in its final cleanup even when `up` or tests fail. Never kill a process just because it owns a requested port.

```bash
#!/bin/bash
set -euo pipefail
: "${PGPORT:?assign a unique port}" "${REDIS_PORT:?assign a unique port}"
ROOT=$(pwd -P)
STATE="$ROOT/.state"
export PGDATA="$STATE/pg" PGHOST=127.0.0.1 PGUSER=postgres PGDATABASE=postgres
export DATABASE_URL="postgresql://postgres@127.0.0.1:$PGPORT/postgres"
export REDIS_URL="redis://127.0.0.1:$REDIS_PORT/0"
mkdir -p "$STATE/redis"
mkdir "$STATE/lifecycle.lock" || { echo 'lifecycle operation already running' >&2; exit 1; }
trap 'rmdir "$STATE/lifecycle.lock"' EXIT

redis_owned() {
  local info pid
  test -s "$STATE/redis.pid" || return 1
  pid=$(cat "$STATE/redis.pid")
  kill -0 "$pid" 2>/dev/null || return 1
  info=$(redis-cli -u "$REDIS_URL" --raw INFO server | tr -d '\r') || return 1
  test "$(printf '%s\n' "$info" | sed -n 's/^process_id://p')" = "$pid" &&
  test "$(printf '%s\n' "$info" | sed -n 's/^config_file://p')" = "$STATE/redis.conf"
}
ready() {
  pg_ctl -D "$PGDATA" status >/dev/null &&
  test "$(psql "$DATABASE_URL" -X -Atqc 'SHOW data_directory')" = "$PGDATA" &&
  test "$(psql "$DATABASE_URL" -X -Atqc 'SHOW port')" = "$PGPORT" &&
  redis_owned &&
  test "$(redis-cli -u "$REDIS_URL" --raw PING)" = PONG
}
case "${1:-}" in
up)
  test -f "$PGDATA/PG_VERSION" || initdb -D "$PGDATA" -U postgres --auth=trust
  if ! pg_ctl -D "$PGDATA" status >/dev/null; then
    pg_ctl -D "$PGDATA" -l "$STATE/postgres.log" -w -t 60 \
      -o "-p $PGPORT -c listen_addresses=127.0.0.1 -c unix_socket_directories=''" start
  fi
  if test -s "$STATE/redis.pid" && kill -0 "$(cat "$STATE/redis.pid")" 2>/dev/null; then
    redis_owned || { echo 'Redis ownership mismatch' >&2; exit 1; }
  else
    cat > "$STATE/redis.conf" <<EOF
bind 127.0.0.1
port $REDIS_PORT
daemonize yes
pidfile "$STATE/redis.pid"
logfile "$STATE/redis.log"
dir "$STATE/redis"
save ""
appendonly yes
appendfsync everysec
EOF
    redis-server "$STATE/redis.conf"
  fi
  for ((i=0; i<60; i++)); do
    if ready; then exit 0; fi
    sleep 1
  done
  echo 'services did not become ready' >&2; exit 1
  ;;
status) ready ;;
down)
  if test -s "$STATE/redis.pid" && kill -0 "$(cat "$STATE/redis.pid")" 2>/dev/null; then
    redis_owned || { echo 'Redis ownership mismatch' >&2; exit 1; }
    redis-cli -u "$REDIS_URL" SHUTDOWN
    for ((i=0; i<60; i++)); do
      test ! -f "$STATE/redis.pid" && break
      sleep 1
    done
    test ! -f "$STATE/redis.pid"
  fi
  if test -f "$PGDATA/PG_VERSION" && pg_ctl -D "$PGDATA" status >/dev/null; then
    pg_ctl -D "$PGDATA" -m fast -w -t 60 stop
  fi
  ;;
*) echo 'usage: services.sh up|status|down' >&2; exit 2 ;;
esac
```

PostgreSQL owns its cluster PID and stop operation through `pg_ctl -D`. Its `-w` option waits for readiness, but a timeout may leave startup continuing in the background. The harness still needs cleanup after a timeout. See the [PostgreSQL 17 pg_ctl documentation](https://www.postgresql.org/docs/17/app-pg-ctl.html). Redis uses an explicit data directory and AOF so state survives orderly stop/start. See [Redis persistence documentation](https://redis.io/docs/latest/operate/oss_and_stack/management/persistence/). Keep those persistence settings equal across tools. The historical eval Redis configuration disabled persistence and must not be reused to claim a restart-persistence result.

Within the chosen environment, install the shared fixture's dependencies and run integration tests without allowing uv to download an alternate interpreter:

```sh
export UV_PYTHON_DOWNLOADS=never
uv sync --locked --python "$(command -v python)"
PGPORT=55431 REDIS_PORT=56381 bash scripts/services.sh up
export DATABASE_URL=postgresql://postgres@127.0.0.1:55431/postgres
export REDIS_URL=redis://127.0.0.1:56381/0
uv run --locked --no-sync --python "$(command -v python)" pytest -q
PGPORT=55431 REDIS_PORT=56381 bash scripts/services.sh down
```

Use `--locked` because it checks that project metadata still matches `uv.lock`, as described in [uv's locking documentation](https://docs.astral.sh/uv/concepts/projects/sync/). A complete scenario script must install an EXIT trap for cleanup before starting services and return the original failure if cleanup also fails. Give checkout B `PGPORT=55432 REDIS_PORT=56382`; both use their own `.state` and `.venv`. Restart is `down` then `up`, preserving `.state`. Destructive reset removes only that checkout's `.state`, after verified stop, and is a separate task.

## Fair tasks and receipts

- Fresh setup includes CLI provision if the same cost is included for other tools, package resolution/download, uv sync, cluster initialization, readiness, and successful integration tests. Warm entry and repeated test commands retain package caches, `.venv`, and service state under declared rules.
- Repeated `up` must preserve PostgreSQL PID and Redis `run_id`. A shell exit must leave the explicitly daemonized services alive. Record this separately from services tied to a foreground supervisor's lifetime.
- A/B isolation writes a different marker in the same SQL table and Redis key in each checkout. Read back both values after alternate test runs. Check actual SQL `data_directory`, `port`, `server_version_num=170002`, Redis `INFO server` version/PID/config file/run ID, `CONFIG GET dir`, and `INFO persistence` AOF status. PING alone is not identity evidence.
- Restart writes durable SQL rows and Redis values, performs orderly `down/up`, and reads the same values. Require new service process identity and unchanged data identity. End with ports closed, owned processes absent, and logs captured. Keep failure and cleanup receipts even if a later retry succeeds.
- Config-copy reproducibility copies config, scripts, project metadata, and `uv.lock` into a new checkout with empty state. Record all resolved system packages and interpreter path. Report top-level pin agreement separately from complete transitive package agreement.
- Exact executable identity includes pkgx release/hash, real paths and hashes for `python`, `postgres`, `redis-server`, and uv, plus `sys.executable` and Python patch in `.venv`. The checked-in Python lock belongs to uv; the pkgx config is not an application dependency lock.

Avoid `pkgx postgres@17` without an exact patch, implicit command aliases, global `dev integrate`, and tool discovery from an already activated shell. `PKGX_NO_INSTALL=1` can test a prepared offline cache, but missing packages then fail instead of downloading. Explicit `@latest` on the current CLI consults inventory, while dev turns `latest` into a broad constraint, so those routes cannot stand in for stable pins. The existing `eval` files provide historical context only; pkgx has no recorded lifecycle result there.
