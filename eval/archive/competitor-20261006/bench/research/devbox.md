# Devbox research and benchmark recipe

Researched on 2026-10-06. This is an implementation recipe, not a new benchmark result. Package resolution, a disposable container's CLI version/help, current official docs, and source were checked. No application installation, service startup, integration test, or latency sample was executed for this research.

## Identity and versions

The official repository is [jetify-com/devbox](https://github.com/jetify-com/devbox). A shallow source checkout at `/tmp/stack-bench-sources/devbox` resolved `main` to `849c9d212a37f0950f637eb59aafd167cac5118a`, committed October 5. The latest stable release API returned [0.18.4](https://github.com/jetify-com/devbox/releases/tag/0.18.4), published September 25, at commit `ed729044dcdaaa379c3ac98a64efc2d68ba97c76`. Recipes below target that release. Its source pins the separately installed process-compose utility to `1.116.0`. [Utility installation](https://github.com/jetify-com/devbox/blob/ed729044dcdaaa379c3ac98a64efc2d68ba97c76/internal/devbox/util.go#L20)

The available local `ev-devbox:latest` image reports `devbox version` as `0.18.4`; image identity is `ev-devbox@sha256:d03da616d6cc65beac75b6e3f0a5dbfcf8a40203e26b9d531b78251c49324cca`, ARM64 Linux, created October 2. It has no configured non-root image user. No host `devbox` executable was found. `jetpackio/devbox:latest` is also present, but its Devbox version was not checked. The historical `eval/images/Dockerfile.devbox` installs via a mutable installer, so future rebuilds must record their actual binary version rather than inherit this identity.

Live requests to the official package search endpoint resolved these candidate pins:

| Request | Resolved exact version | Systems reported by index |
| --- | --- | --- |
| `python@3.13` | `3.13.15` | ARM64 macOS, ARM64 Linux, x86-64 Linux |
| `uv@latest` | `0.12.22` | ARM64 macOS, ARM64 Linux, x86-64 Linux |
| `postgresql@17` | `17.10` | ARM64 and x86-64 macOS/Linux |
| `redis@latest` | `8.10.2` | ARM64 macOS, ARM64 Linux, x86-64 Linux |

These are index responses, not installed binaries or successful builds. Query used `https://www.nixsearch.com/v2/resolve?name=<name>&version=<constraint>`. Index omission of x86-64 macOS does not prove the package cannot build there; Devbox can fall back to another system's installable. Choose a supported shared version set for every competitor before measurement, then record actual versions. Current index choices need not equal older evaluation pins. [Official search API contract](https://github.com/jetify-com/devbox/blob/849c9d212a37f0950f637eb59aafd167cac5118a/SEARCH_API.md)

## What Devbox provides

Devbox resolves system tools through Nix, supports a project JSON configuration and lockfile, executes project scripts, and manages background services through process-compose. PostgreSQL and Redis have built-in plugins activated by adding those packages. The plugins supply commands, readiness probes, environment defaults, and local data/config paths. PostgreSQL initialization and application dependency installation are still project responsibilities. [Services guide](https://www.jetify.com/docs/devbox/guides/services), [PostgreSQL example](https://www.jetify.com/docs/devbox/devbox-examples/databases/postgres), [Redis example](https://www.jetify.com/docs/devbox/devbox-examples/databases/redis)

| Requirement | Native behavior | Project or benchmark responsibility |
| --- | --- | --- |
| Pinned Python and server binaries | Version selectors plus `devbox.lock` with resolved Nix references and system outputs | Commit the lock; verify actual runtime versions |
| Python dependencies | Python plugin creates a checkout-local venv and exports `UV_PROJECT_ENVIRONMENT` | Commit `uv.lock`; run `uv sync --frozen` |
| PostgreSQL and Redis lifecycle | `devbox services up -b`, `start`, `restart`, `stop`, `ls` | Initialize PGDATA, wait for readiness and prove endpoint ownership |
| Two independent checkout managers | Supervisor PID and API port registry keyed by project directory | Assign distinct PGPORT/REDIS_PORT and corresponding application URLs |
| Persistent checkout data | Plugin defaults put data under `.devbox/virtenv` | Test orderly stop/restart and define explicit reset policy |
| Automatic port selection | Picks an unused process-compose control API port | Database/cache ports are separate and default to 5432/6379 |
| Portable project definition | Share JSON, lockfile, scripts, plugin configuration | Preserve dependency lock and local environment contract; keep runtime data out of copy |

[PostgreSQL plugin](https://github.com/jetify-com/devbox/blob/ed729044dcdaaa379c3ac98a64efc2d68ba97c76/plugins/postgresql.json) defines checkout-local PGDATA and Unix socket PGHOST. Its [service command](https://github.com/jetify-com/devbox/blob/ed729044dcdaaa379c3ac98a64efc2d68ba97c76/plugins/postgresql/process-compose.yaml) passes PGPORT to postgres and pg_isready, and uses `pg_ctl stop -m fast` for shutdown. The [Redis plugin](https://github.com/jetify-com/devbox/blob/ed729044dcdaaa379c3ac98a64efc2d68ba97c76/plugins/redis.json) creates `devbox.d/redis/redis.conf`; its [service command](https://github.com/jetify-com/devbox/blob/ed729044dcdaaa379c3ac98a64efc2d68ba97c76/plugins/redis/process-compose.yaml) passes REDIS_PORT and probes with redis-cli. The default [Redis configuration](https://github.com/jetify-com/devbox/blob/ed729044dcdaaa379c3ac98a64efc2d68ba97c76/plugins/redis/redis.conf) binds loopback and writes data under `.devbox/virtenv/redis/`.

Devbox isolates tool environments and service configuration. It does not supply a container network namespace for each checkout. Do not interpret its Nix shell as a security sandbox. The two-checkout test should accept explicit port configuration as a supported solution and report the manual step separately.

## Recommended native recipe

Use the built-in service plugins as the primary test. A custom process-compose file is supported, but is unnecessary for this fixture and would obscure whether the native plugins work. Run all commands as a normal user from fresh noninteractive shells. The application fixture must contain `pyproject.toml`, a compatible committed `uv.lock`, and actual PostgreSQL/Redis integration tests.

Put this `devbox.json` in the fixture. The exact versions are current index candidates above; the implementer should replace them with the final shared package set if other competitors require different common versions.

```json
{
  "packages": [
    "python@3.13.15",
    "uv@0.12.22",
    "postgresql@17.10",
    "redis@8.10.2"
  ],
  "env_from": "bench.local.env",
  "env": {
    "UV_PYTHON_DOWNLOADS": "never",
    "PGUSER": "postgres",
    "PGDATABASE": "postgres"
  },
  "shell": {
    "init_hook": [
      "export DATABASE_URL=\"postgresql://postgres@127.0.0.1:${PGPORT}/postgres\"",
      "export REDIS_URL=\"redis://127.0.0.1:${REDIS_PORT}/0\""
    ],
    "scripts": {
      "setup": "sh bench-devbox/setup.sh",
      "ready": "sh bench-devbox/ready.sh",
      "test": "uv run --frozen --no-sync pytest -q"
    }
  }
}
```

Keep the plugins enabled. Their environment values are merged into the project. URLs are computed in the init hook so they use the final PGPORT and REDIS_PORT without relying on JSON environment-map expansion order. Devbox runs these hooks for commands and services. `devbox install` installs tools but does not execute init hooks. [Configuration environment expansion](https://github.com/jetify-com/devbox/blob/ed729044dcdaaa379c3ac98a64efc2d68ba97c76/internal/conf/env.go), [run and install implementation](https://github.com/jetify-com/devbox/blob/ed729044dcdaaa379c3ac98a64efc2d68ba97c76/internal/devbox/devbox.go#L273)

Create `bench-devbox/setup.sh`:

```sh
#!/bin/sh
set -eu
: "${PGDATA:?}" "${PGPORT:?}" "${REDIS_PORT:?}" "${BENCH_ID:?}"
mkdir -p "$PGDATA"
if [ ! -f "$PGDATA/PG_VERSION" ]; then
  initdb -D "$PGDATA" -U postgres --auth=trust --locale=C
fi
uv sync --frozen --python "$(command -v python)"
```

This setup is deliberately outside the init hook. Repeated commands should not reinstall Python dependencies or initialize PostgreSQL. The native Python plugin creates `.venv` and directs uv there. Its existing-venv check can ask for overwrite when a venv belongs to another interpreter, so use fresh checkouts and never copy `.venv`. [Python plugin](https://github.com/jetify-com/devbox/blob/ed729044dcdaaa379c3ac98a64efc2d68ba97c76/plugins/python.json), [venv creation](https://github.com/jetify-com/devbox/blob/ed729044dcdaaa379c3ac98a64efc2d68ba97c76/plugins/python/venvShellHook.sh)

Create `bench-devbox/ready.sh`:

```sh
#!/bin/sh
set -eu
i=0
while [ "$i" -lt 120 ]; do
  if pg_isready -q -h 127.0.0.1 -p "$PGPORT" -U postgres -d postgres &&
     [ "$(redis-cli -h 127.0.0.1 -p "$REDIS_PORT" --raw ping 2>/dev/null)" = PONG ]; then
    exit 0
  fi
  i=$((i + 1))
  sleep 0.25
done
echo 'PostgreSQL or Redis did not become ready within 30 seconds' >&2
exit 1
```

A single `devbox run ready` avoids counting a fresh Devbox entry for every polling iteration. Readiness is a liveness gate; run the ownership checks below before reporting successful startup.

Generate and commit `devbox.lock` from the approved template before timed fresh-checkout installation. Its lock entries store version, resolved source, plugin version and per-system outputs. Do not fabricate lock JSON. First lock generation is a separate preparation/resolution phase, because fresh projects with uncommitted selectors can resolve different revisions later. [Lockfile reuse](https://github.com/jetify-com/devbox/blob/ed729044dcdaaa379c3ac98a64efc2d68ba97c76/internal/lock/lockfile.go#L83), [lock entry fields](https://github.com/jetify-com/devbox/blob/ed729044dcdaaa379c3ac98a64efc2d68ba97c76/internal/lock/package.go)

### Fresh checkout A

Start with an independent checkout or fixture copy, omitting `.devbox`, `.venv`, and any runtime data. Use a short path, since PostgreSQL Unix socket paths have platform length limits. Here ports are example reservations, not guaranteed free ports. The benchmark controller must reserve or preflight all four ports immediately before startup and reject collisions without touching other services.

```sh
cd /tmp/dbx-bench/app-a
cat > bench.local.env <<'ENV'
BENCH_ID=devbox-a
PGPORT=55431
REDIS_PORT=56378
ENV
devbox install
devbox run setup
devbox services up -b
devbox run ready
devbox run test
devbox services ls
```

### Fresh checkout B while A runs

Use identical committed application/configuration/locks and a second local environment file. The copied config is unchanged.

```sh
cd /tmp/dbx-bench/app-b
cat > bench.local.env <<'ENV'
BENCH_ID=devbox-b
PGPORT=55432
REDIS_PORT=56379
ENV
devbox install
devbox run setup
devbox services up -b
devbox run ready
devbox run test
devbox services ls
```

Commit a `bench.local.env.example`, while leaving the generated `bench.local.env` untracked. This file intentionally differs by checkout. Services and test commands read it themselves, so no interactive shell activation or inherited benchmark environment is needed. Run both under the same normal user and shared Devbox registry to exercise the native manager's project routing. Giving each checkout an isolated HOME would hide registry interaction and change the task.

## Exact ownership and isolation checks

A successful ping can hit somebody else's server. For each checkout, record configured URLs, actual process versions and executable paths, PostgreSQL's data directory and server port, Redis's data directory and run ID, and the process-compose API port. Paths must resolve under the intended checkout, and A/B data paths and Redis run IDs must differ.

```sh
devbox run -- sh -eu -c '
  printf "BENCH_ID=%s DATABASE_URL=%s REDIS_URL=%s\n" "$BENCH_ID" "$DATABASE_URL" "$REDIS_URL"
  python --version
  uv --version
  postgres --version
  redis-server --version
  command -v python uv postgres redis-server
  uv run --frozen --no-sync python -c "import sys; print(sys.executable); print(sys.version)"
  psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -Atc "SHOW data_directory; SHOW port; SELECT version();"
  redis-cli -h 127.0.0.1 -p "$REDIS_PORT" --raw CONFIG GET dir
  redis-cli -h 127.0.0.1 -p "$REDIS_PORT" --raw INFO server
'
devbox services pcport
```

PostgreSQL should report `/tmp/dbx-bench/app-a/.devbox/virtenv/postgresql/data` for A, subject to canonical `/private/tmp` on macOS, and port `55431`; B should report its own path and `55432`. Redis `CONFIG GET dir` must canonicalize into the intended checkout's `.devbox/virtenv/redis`; INFO exposes run_id and tcp_port. Compare normalized canonical paths rather than literal `/tmp` prefixes.

Seed the same SQL table/key and Redis key with a different value in each checkout. Do this only after data-directory ownership passes.

```sh
devbox run -- sh -eu -c '
  psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -v owner="$BENCH_ID" <<'"'"'SQL'"'"'
CREATE TABLE IF NOT EXISTS benchmark_owner (id integer PRIMARY KEY, owner text NOT NULL);
INSERT INTO benchmark_owner VALUES (1, :'owner')
ON CONFLICT (id) DO UPDATE SET owner = EXCLUDED.owner;
SQL
  redis-cli -h 127.0.0.1 -p "$REDIS_PORT" SET benchmark_owner "$BENCH_ID"
'
```

Read both back and require exact strings `devbox-a` and `devbox-b`:

```sh
devbox run -- sh -eu -c '
  sql_owner=$(psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -Atc "SELECT owner FROM benchmark_owner WHERE id=1")
  redis_owner=$(redis-cli -h 127.0.0.1 -p "$REDIS_PORT" --raw GET benchmark_owner)
  test "$sql_owner" = "$BENCH_ID"
  test "$redis_owner" = "$BENCH_ID"
  printf "SQL_OWNER=%s REDIS_OWNER=%s\n" "$sql_owner" "$redis_owner"
'
```

Use this same ownership contract for every competitor. Application tests should perform actual writes, reads and rollback/cache behavior through DATABASE_URL and REDIS_URL, rather than merely invoke ping. A pytest process must fail the step on failure; never pipe it into `tail` without preserving its exit status.

## Repeated commands, restart and cleanup

Measure the supported noninteractive entry `devbox run -- <command>` from a fresh process outside a Devbox shell. Collect separate cases for a no-op, Python version/interpreter assertion, application CLI, and warm integration test. Then collect a separate already-activated session case if the suite covers shell workflows. Calling `devbox run` from an already active Devbox environment skips state recomputation and re-running hooks; that is a different workload. [RunScript implementation](https://github.com/jetify-com/devbox/blob/ed729044dcdaaa379c3ac98a64efc2d68ba97c76/internal/devbox/devbox.go#L273)

For start-while-running, `devbox services up -b` deliberately returns an already-running error. The supported lifecycle alternatives are `devbox services start postgresql redis` or explicitly restarting. Record repeat-up's result without turning that command's non-idempotence into inability to reuse services. `start` on an existing manager sends start requests for the named services; implementation prints individual request errors without necessarily propagating a nonzero result. Capture its output and independently recheck identity/readiness. [Manager already-running check](https://github.com/jetify-com/devbox/blob/ed729044dcdaaa379c3ac98a64efc2d68ba97c76/internal/services/manager.go#L114), [start behavior](https://github.com/jetify-com/devbox/blob/ed729044dcdaaa379c3ac98a64efc2d68ba97c76/internal/devbox/services.go#L18)

Use named services for restart:

```sh
devbox services restart postgresql redis
devbox run ready
```

Check the same data-directory paths and markers again. Redis run_id should change after restart while its marker persists if saved. For a deterministic persistence test, issue Redis SAVE on the owned instance before stop. This tests orderly persistence, not crash durability of an unsaved cache write.

```sh
devbox run -- sh -eu -c 'test "$(redis-cli -p "$REDIS_PORT" --raw SAVE)" = OK'
devbox services stop
```

Poll owned endpoint refusal and owned PID exit with a bounded deadline after `stop`; its implementation signals process-compose and returns without waiting for children to terminate. Recheck that B's markers and integration tests still work after A stops. Restart A using `devbox services up -b`, wait and verify markers. Stop both separately by checkout. Never use `devbox services stop --all-projects`, global `pkill`, or `killall` in a shared environment. [Background startup and stop implementation](https://github.com/jetify-com/devbox/blob/ed729044dcdaaa379c3ac98a64efc2d68ba97c76/internal/services/manager.go#L214)

After both have stopped and owned processes exited, an explicit fresh-data reset may delete only the benchmark-owned PostgreSQL data and Redis persistence files. Do not equate `services stop` with wiping data. Do not remove `.devbox` before stopping its services, because that removes useful ownership/configuration evidence.

For recovery testing, use a disposable container or isolated normal-user home and kill only the recorded owned supervisor PID. Record the difference between supervisor termination, endpoint termination, and service-command behavior. The source's background path uses a new process group and stores a supervisor PID, which is insufficient by itself to establish that abrupt supervisor death cleans up children. Historical orphan claims are not fresh evidence. A user-scripted recovery must be labelled and must check ownership before terminating remaining processes.

## Portability and execution constraints

The CLI publishes macOS and Linux binaries. The practical binary-cache targets for this task are ARM64/x86-64 Linux and macOS, subject to each selected package's index/cache support. Nix must already work before timing Devbox application setup. Treat CLI/Nix bootstrap as a separate onboarding phase. Native Windows is not a build target; use a Linux environment such as WSL and report that environment explicitly. [Release targets](https://github.com/jetify-com/devbox/blob/ed729044dcdaaa379c3ac98a64efc2d68ba97c76/.goreleaser.yaml)

For a standalone CLI provisioned outside timing, use the release binary rather than the changing installer. This command needs curl, tar, sha256sum or shasum, and an already functioning Nix installation. Use a benchmark-owned `cli-dir`, then put its absolute path on the controller's PATH. Do not replace the user's installed CLI.

```sh
set -eu
mkdir -p cli-dir
cd cli-dir
case "$(uname -s)" in Darwin) cli_os=darwin ;; Linux) cli_os=linux ;; *) exit 1 ;; esac
case "$(uname -m)" in arm64|aarch64) cli_arch=arm64 ;; x86_64) cli_arch=amd64 ;; *) exit 1 ;; esac
cli_asset="devbox_0.18.4_${cli_os}_${cli_arch}.tar.gz"
cli_url=https://github.com/jetify-com/devbox/releases/download/0.18.4
curl -fsSLO "$cli_url/$cli_asset"
curl -fsSLO "$cli_url/checksums.txt"
expected=$(awk -v name="$cli_asset" '$2 == name {print $1}' checksums.txt)
test -n "$expected"
if command -v sha256sum >/dev/null 2>&1; then
  actual=$(sha256sum "$cli_asset" | awk '{print $1}')
else
  actual=$(shasum -a 256 "$cli_asset" | awk '{print $1}')
fi
test "$actual" = "$expected"
tar -xzf "$cli_asset"
./devbox version
```

Require version `0.18.4`, record the extracted binary's SHA-256, and repeat the version check at the end. The release asset list was checked; downloading/executing this bootstrap snippet was not part of this research.

Devbox can generate an OCI Dockerfile and devcontainer from its config. That does not make a generated image identical to the host environment. Keep native host and Linux-container results separate. The [development Dockerfile template](https://github.com/jetify-com/devbox/blob/ed729044dcdaaa379c3ac98a64efc2d68ba97c76/internal/devbox/generate/tmpl/dev.Dockerfile.tmpl) uses a Devbox base image, copies JSON/lock and defaults to a shell. It does not automatically start the fixture's PostgreSQL and Redis. [Generate Dockerfile reference](https://www.jetify.com/docs/devbox/cli-reference/devbox-generate-dockerfile)

Run PostgreSQL initialization and the services as a non-root user, including in containers. PostgreSQL refuses root initdb regardless of Devbox. Supply writable checkout, HOME/XDG registry directories and a functioning Nix store/daemon. ARM64 Docker on this Mac measures Linux, not native Darwin. Include container launch/exec transport only in separately labelled end-to-end figures, or subtract nothing and disclose it consistently for all tools.

For config transfer, copy `devbox.json`, `devbox.lock`, `pyproject.toml`, `uv.lock`, the setup/readiness scripts, and any customized `devbox.d` files. Exclude `.devbox`, `.venv`, database/cache data, supervisor registry, and machine-specific `bench.local.env`. Generate the destination's local env file, then run install/setup/start/readiness/tests. Recompare lock resolved references and actual runtime versions; lock serialization can add system outputs on a new platform, so check semantics as well as raw-file hashes. A `devbox install --tidy-lockfile` is an explicit preparation mutation, not a silent timing-step repair.

## Fair tasks and failure interpretation

| Comparable user task | Suggested boundary and proof |
| --- | --- |
| Join a Python application project | Existing committed locks to installed tools, frozen deps and passing test; separate CLI/Nix bootstrap |
| Run a command repeatedly | One fresh CLI invocation per sample; separate no-op, meaningful Python command and already-activated session |
| Start local app dependencies | Services command plus readiness and ownership, not command return alone |
| Work in two checkouts | Same fixture/locks, explicit local endpoint setup allowed; independent SQL/cache markers |
| Stop one project | A refuses connections and owned PIDs exit; B retains markers and passes tests |
| Resume work | Stop/start same checkout, expected persistence and identity rechecked |
| Share config with a colleague | Clean copied source/config/locks, new machine env file, same runtime versions and tests |
| Diagnose startup conflict | Benchmark-owned occupied port, useful diagnostic/status, no writes to wrong endpoint |
| Recover from supervisor crash | Disposable environment, exact owned PID, service/PID checks and separately reported manual recovery |

Cold Nix package downloads/builds, warm shared Nix store, fresh checkout with warm store, venv dependency download cache, and repeated activated commands are separate conditions. Record them rather than pool them. Service utility preparation can install process-compose on first use even when application tools are already installed. Either include it in first-service-use time or prewarm it for all warm cases and disclose that choice.

The old `eval/harness/devbox.sh` and `eval/configs/devbox/devbox.json` used default 5432/6379 endpoints, broad selectors, a repeat `up`, and `uv sync` plus piped pytest. The later historical `eval/harness/current-benchmark.py` explicitly sets B's ports/URLs. Read those as prior harness context, not a current performance/correctness result. New checks must retain stderr, exit statuses, readiness deadlines, identity and data-isolation evidence. `devbox services ls --json` is not a supported flag in the inspected release; use its table output and protocol probes. `devbox search --json` was also rejected by the local 0.18.4 CLI during this research.

The startup source returns after spawning the supervisor and recording its PID/API port. Service readiness probes exist inside process-compose, but a zero exit from `up -b` is not an all-services-ready receipt. Status output and HTTP supervisor reachability likewise do not prove SQL/cache ownership. [Supervisor implementation](https://github.com/jetify-com/devbox/blob/ed729044dcdaaa379c3ac98a64efc2d68ba97c76/internal/services/manager.go), [service status client](https://github.com/jetify-com/devbox/blob/ed729044dcdaaa379c3ac98a64efc2d68ba97c76/internal/services/client.go)

Finally, do not attribute unreleased `main` changes to installed 0.18.4. The inspected main branch changed run-script completion to initialize lazily, which can affect command-entry work; that code is absent from the release. Any main-build experiment needs its own source SHA, binary hash and version label. [Main run command](https://github.com/jetify-com/devbox/blob/849c9d212a37f0950f637eb59aafd167cac5118a/internal/boxcli/run.go#L85)
