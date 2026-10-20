# Flox implementation research

Research date: 2026-10-06. This is an implementation and benchmark design note, not a new benchmark result. The recipes below have not been run end to end. Only repository reads, image inspection, and disposable container `--version`/`--help` calls were executed. No user services were started or stopped.

## Version and evidence boundary

- Official repository: [flox/flox](https://github.com/flox/flox).
- Current main inspected at `99b3a397f7bdccdb6141e239cf25862f26234744`, shallow clone in `/tmp/stack-bench-sources/flox`.
- Latest published release returned by GitHub's release API is [v1.17.0](https://github.com/flox/flox/releases/tag/v1.17.0), published 2026-09-22. Its commit is `486737b3e68b0f094e89b4b3e27260e9f6c91b4a`. A detached release worktree in `/tmp/stack-bench-sources/flox-v1.17.0` was read separately. Use this release for implementation claims that apply to the available images.
- Local `ev-flox:latest` image ID and digest are `sha256:61613637e02c263efc9a16e13f76c823a8d9210a3e920449bbe85207d95c5e66`. Created 2026-10-02. Executed `docker run --rm ev-flox:latest flox --version` returned `1.17.0-g486737b`.
- Local `ghcr.io/flox/flox:latest` image ID and digest are `sha256:785dc66693f0d560fb09dc13e606005437f23a727a9195409281e67f40f5bd26`. Executed `docker run --rm ghcr.io/flox/flox:latest /root/.nix-profile/bin/bash -lc 'flox --version'` returned `1.17.0`. This image has no entrypoint. `docker run IMAGE --version` fails because it tries to execute a file named `--version`.
- No host `flox` executable was found on `PATH`. Local availability is therefore Linux-container availability, not evidence that native macOS Flox was installed or tested.
- `eval/harness/flox.sh`, `eval/configs/flox/manifest.toml`, `eval/images/Dockerfile.flox`, and `eval/REPORT.md` were inspected as historical context. Their timing and failure observations must not be copied into new results. The old image build uses `github:flox/flox/latest`, a moving reference; future builds need a release/commit pin and recorded image digest.

Live documentation can describe unreleased main behavior. In particular, current main has `flox services persist`, which writes Linux systemd user units. The v1.17.0 CLI help and release source do not have this command. Do not give the release credit for it or try to use it in this benchmark. The current-main implementation is [persist.rs](https://github.com/flox/flox/blob/99b3a397f7bdccdb6141e239cf25862f26234744/cli/flox/src/commands/services/persist.rs), and explicitly rejects other operating systems.

## What Flox natively provides

Flox packages tools into a Nix-backed environment, exports manifest variables and activation-hook variables, and supervises manifest-defined services with process-compose. It supports path environments in a checkout, environment composition, and remote FloxHub environments. It is a full service-capable comparator for this workload.

| Requirement | Release behavior | Benchmark responsibility |
| --- | --- | --- |
| Runtime and tool pinning | Catalog package resolution stored in `.flox/env/manifest.lock`, including version, source revision, derivation and output store paths | Freeze and copy the lock, record runtime/tool versions, prevent uv downloading another interpreter |
| Python dependencies | Tool packaging plus activation hooks; uv or pip performs application dependency installation | Commit `pyproject.toml` and `uv.lock`; use `uv sync --frozen` |
| PostgreSQL and Redis supervision | Native `[services]` definitions translated into process-compose configuration | Supply initialization, foreground commands and storage paths |
| Service readiness | Start/order supervision; no native health-check declaration in release service descriptor | Probe actual DB/cache requests with a deadline before testing |
| Two path checkouts | Manager socket derived from canonical environment path; `.flox/cache` local to checkout | Assign nonconflicting ports and checkout-local PG/Redis storage |
| Repeated commands | Multiple activations can attach to an existing activation and services | Keep at least one activation alive between commands |
| Automatic teardown | Services stop after the final live activation terminates | Await cleanup and verify owned endpoints/PIDs rather than only trusting CLI exit |
| Data persistence | Services leave ordinary application files behind when stopped | Explicit PGDATA and Redis AOF settings; distinguish stopping from deleting files |
| Persistent background start across all shell exits | Not a release workflow; normal lifecycle is activation-scoped | Report the lifecycle and support cost; a held activation is user-written automation |
| Composition | Native local-directory or FloxHub includes merge tools, vars, hooks, services | Freeze included definitions/locks and verify resulting merged environment |

The implementation details matter:

- [`start.rs`](https://github.com/flox/flox/blob/486737b3e68b0f094e89b4b3e27260e9f6c91b4a/cli/flox/src/commands/services/start.rs#L38) calls `guard_is_within_activation` before starting. [`services/mod.rs`](https://github.com/flox/flox/blob/486737b3e68b0f094e89b4b3e27260e9f6c91b4a/cli/flox/src/commands/services/mod.rs#L260) enforces an active matching environment for start and restart. Merely passing `-d` outside activation does not satisfy this requirement.
- [`process_compose.rs`](https://github.com/flox/flox/blob/486737b3e68b0f094e89b4b3e27260e9f6c91b4a/cli/flox-rust-sdk/src/providers/services/process_compose.rs) defines the supported process configuration and injects `flox_never_exit` so stopping every user service does not immediately destroy the manager. Starting services is not proof that their endpoint is ready.
- [`path_environment.rs`](https://github.com/flox/flox/blob/486737b3e68b0f094e89b4b3e27260e9f6c91b4a/cli/flox-rust-sdk/src/models/environment/path_environment.rs#L363) returns `.flox/cache`, `.flox/log`, project directory, and `.flox/env/manifest.lock`; the service socket uses its path hash. [`environment/mod.rs`](https://github.com/flox/flox/blob/486737b3e68b0f094e89b4b3e27260e9f6c91b4a/cli/flox-rust-sdk/src/models/environment/mod.rs#L1233) builds the runtime socket path and checks Unix socket path length. Do not globally override `FLOX_SERVICES_SOCKET` across checkouts.
- [`lockfile/catalog.rs`](https://github.com/flox/flox/blob/486737b3e68b0f094e89b4b3e27260e9f6c91b4a/cli/flox-manifest/src/lockfile/catalog.rs#L17) captures catalog version/revision/derivation/outputs/system. [`PathEnvironment::build`](https://github.com/flox/flox/blob/486737b3e68b0f094e89b4b3e27260e9f6c91b4a/cli/flox-rust-sdk/src/models/environment/path_environment.rs#L350) calls `ensure_locked` to avoid rewriting a current lock or doing a catalog round trip. This supports a meaningful warm reactivation measurement.
- [`services/status.rs`](https://github.com/flox/flox/blob/486737b3e68b0f094e89b4b3e27260e9f6c91b4a/cli/flox/src/commands/services/status.rs#L104) serializes a pretty JSON array. Objects contain `name`, `status`, `pid`, `exit_code`; `is_running` is omitted. Parse an array, require the expected names, `status == "Running"`, and a positive PID. Do not use `exit_code == null` alone as running evidence.
- [`flox-activations/start.rs`](https://github.com/flox/flox/blob/486737b3e68b0f094e89b4b3e27260e9f6c91b4a/cli/flox-activations/src/start.rs#L122) fails if the activation script fails or never writes its completion marker. A non-final failing command in a shell hook can still be masked by a later successful command. Make initialization failures explicit and verify the initialized cluster.
- [`services.bats`](https://github.com/flox/flox/blob/486737b3e68b0f094e89b4b3e27260e9f6c91b4a/cli/tests/services.bats) covers shutdown after the final activation, layered/remote activations, duplicate starts and stale sockets. These upstream tests are evidence of intended coverage, not an executed local result.

## Fresh-shell application recipe

Use a path environment in each application checkout. Keep project-specific endpoints in a gitignored `bench.local.env`, as explicit machine-local configuration. The same committed manifest/locks can then be copied unchanged. Default examples below use ports 25432/26379 for A and 25433/26380 for B. The benchmark must first reserve/check these ports and fail if an unrelated listener owns them. Flox does not allocate TCP ports automatically.

Assume the common application fixture supplies `pyproject.toml`, `uv.lock`, integration tests, and the scripts below. Start from a non-root user with a writable checkout. Initialize outside the timing interval:

```bash
export FLOX_DISABLE_METRICS=true
flox init --no-auto-setup --bare -d "$CHECKOUT"
flox edit -d "$CHECKOUT" -f "$PREPARED_MANIFEST"
```

`flox edit -f` validates/builds and can download packages. Count that work as environment setup if setup is measured; do not hide it in fixture preparation while calling first activation a cold installation. If prebuilt locked config is the input scenario, copying a frozen `.flox` definition is legitimate and should be described as such.

Suggested `.flox/env/manifest.toml`:

```toml
schema-version = "1.16.0"
minimum-cli-version = "1.17.0"

[install]
python.pkg-path = "python313"
uv.pkg-path = "uv"
postgresql.pkg-path = "postgresql_17"
redis.pkg-path = "redis"

[vars]
UV_PYTHON_DOWNLOADS = "never"
UV_PYTHON_PREFERENCE = "only-system"
PGUSER = "postgres"
PGDATABASE = "postgres"

[hook]
on-activate = '''
  test -r "$FLOX_ENV_PROJECT/bench.local.env" || return 1
  source "$FLOX_ENV_PROJECT/bench.local.env" || return 1
  export PGPORT REDIS_PORT BENCH_INSTANCE
  export PGDATA="$FLOX_ENV_CACHE/pgdata"
  export PGHOST=127.0.0.1
  export REDIS_DATA="$FLOX_ENV_CACHE/redis"
  export DATABASE_URL="postgresql://postgres@127.0.0.1:$PGPORT/postgres"
  export REDIS_URL="redis://127.0.0.1:$REDIS_PORT/0"
  mkdir -p "$FLOX_ENV_CACHE" "$REDIS_DATA" || return 1
  if [ ! -s "$PGDATA/PG_VERSION" ]; then
    initdb -D "$PGDATA" -U postgres --auth=trust > "$FLOX_ENV_CACHE/initdb.log" 2>&1 || return 1
  fi
  test -s "$PGDATA/PG_VERSION" || return 1
'''

[services.postgres]
command = 'exec postgres -D "$PGDATA" -p "$PGPORT" -c listen_addresses=127.0.0.1 -c unix_socket_directories= -c cluster_name="$BENCH_INSTANCE"'
shutdown.command = 'pg_ctl -D "$PGDATA" -m fast -w stop'
shutdown.timeout-seconds = 30

[services.redis]
command = 'exec redis-server --bind 127.0.0.1 --port "$REDIS_PORT" --dir "$REDIS_DATA" --appendonly yes --appendfsync always --save "" --daemonize no'
shutdown.timeout-seconds = 30

[options]
systems = ["aarch64-darwin", "x86_64-darwin", "aarch64-linux", "x86_64-linux"]
```

This is development-only trust authentication on loopback. The benchmark has no production-authentication objective. Empty `unix_socket_directories` avoids platform pathname-length limits and socket collisions; tests consistently use TCP. `exec` lets process-compose supervise the server PID rather than an extra Bash parent. Redis AOF with `appendfsync always` deliberately makes the persistence check deterministic after acknowledged writes; use the same durability semantics in every comparator.

The catalog selectors above are resolved once during fixture preparation. The generated, committed `.flox/env/manifest.lock` is the exact pin. `python313` alone is a moving package selector. If all tools must share specific exact version constraints, add `version = "<available exact version>"` to their descriptors after checking the catalog and verifying every comparator can supply that version. Do not invent available patch versions or resolve independently in A/B. Preserve the original lock before and after activation and report any changes. Multi-system lock entries can differ by platform while satisfying the agreed major/minor target. Do not compare their store hashes across architectures as if they should be identical.

Each checkout supplies a small, trusted local file:

```bash
cat > "$CHECKOUT_A/bench.local.env" <<'EOF'
PGPORT=25432
REDIS_PORT=26379
BENCH_INSTANCE=stack-bench-a
EOF
cat > "$CHECKOUT_B/bench.local.env" <<'EOF'
PGPORT=25433
REDIS_PORT=26380
BENCH_INSTANCE=stack-bench-b
EOF
```

Keep machine-local env loading out of `[profile]`. `flox activate -- COMMAND` skips profiles; activation hooks are the right place to export values needed by commands and services. Hooks run once per new activation build, with concurrent activations attaching to existing state. Do not assume editing `bench.local.env` changes variables in an already running activation. Stop/release existing activations before changing endpoints.

## One activation for CI

This is Flox's lowest-complexity supported flow. Services start, the test process runs, and services end with that activation. `scripts/ci.sh` should use `set -euo pipefail`, install dependencies, wait for both endpoints, run tests, and preserve the actual exit status:

```bash
#!/usr/bin/env bash
set -euo pipefail
cd "$FLOX_ENV_PROJECT"
uv sync --frozen --python "$(command -v python)"
.venv/bin/python scripts/wait-services.py
.venv/bin/python -m pytest -q
```

Run it from an unactivated, noninteractive shell:

```bash
flox activate -d "$CHECKOUT_A" --start-services -- bash "$CHECKOUT_A/scripts/ci.sh"
```

For a dependency-only phase, use `flox activate -d "$CHECKOUT_A" -- bash -c 'cd "$FLOX_ENV_PROJECT"; uv sync --frozen --python "$(command -v python)"'`. Record that duration separately from service startup and tests. A fresh virtualenv needs dependency downloads or cached wheels even when the Nix tools are cached. A committed `uv.lock` plus `--frozen` avoids changing dependencies. Repeat test commands with `.venv/bin/python`; avoid a bare `uv run pytest` that could silently sync or choose a downloaded interpreter.

Example `scripts/wait-services.py`, using the installed fixture clients:

```python
import os
import time
import psycopg
import redis

deadline = time.monotonic() + 60
last = None
while time.monotonic() < deadline:
    try:
        with psycopg.connect(os.environ["DATABASE_URL"], connect_timeout=2) as db:
            row = db.execute("select current_setting('cluster_name'), current_setting('port'), current_setting('data_directory')").fetchone()
            expected = (os.environ["BENCH_INSTANCE"], os.environ["PGPORT"], os.environ["PGDATA"])
            if row != expected:
                raise RuntimeError(f"wrong postgres identity: {row!r}, expected {expected!r}")
        cache = redis.Redis.from_url(os.environ["REDIS_URL"], socket_connect_timeout=2, socket_timeout=2)
        if cache.ping() is not True:
            raise RuntimeError("redis ping failed")
        cfg = cache.config_get("dir", "port")
        if cfg.get("dir") != os.environ["REDIS_DATA"] or cfg.get("port") != os.environ["REDIS_PORT"]:
            raise RuntimeError(f"wrong redis identity: {cfg!r}")
        break
    except (psycopg.OperationalError, redis.exceptions.ConnectionError, redis.exceptions.TimeoutError, OSError) as exc:
        last = exc
        time.sleep(0.1)
else:
    raise SystemExit(f"services not ready: {last!r}")
```

The identity assertion is deliberate. `pg_isready` and `PONG` alone can pass against an unrelated or the other checkout's service. Connection errors retry; identity mismatches raise immediately. Record startup as successful only after these application-level checks pass, regardless of the activate command's exit code.

## Repeated commands from independent shells

For an agent/editor session that issues many independent commands, the supported service lifecycle requires a long-lived activation. Provide an owned holder, rather than hiding it or classifying it as a native detached service feature. A Python sleep works on both macOS and Linux; the host macOS `sleep` need not support `infinity`.

From a clean Bash controller process:

```bash
set -euo pipefail
export FLOX_DISABLE_METRICS=true
flox activate -d "$CHECKOUT_A" --start-services -- python -c 'import time; time.sleep(86400)' > "$CHECKOUT_A/holder.log" 2>&1 &
holder_a=$!
flox activate -d "$CHECKOUT_B" --start-services -- python -c 'import time; time.sleep(86400)' > "$CHECKOUT_B/holder.log" 2>&1 &
holder_b=$!

cleanup() {
  flox services stop -d "$CHECKOUT_A" || true
  flox services stop -d "$CHECKOUT_B" || true
  kill "$holder_a" "$holder_b" 2>/dev/null || true
  wait "$holder_a" 2>/dev/null || true
  wait "$holder_b" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

flox activate -d "$CHECKOUT_A" -- bash "$CHECKOUT_A/scripts/ci.sh"
flox activate -d "$CHECKOUT_B" -- bash "$CHECKOUT_B/scripts/ci.sh"
flox services status -d "$CHECKOUT_A" --json
flox services status -d "$CHECKOUT_B" --json
flox activate -d "$CHECKOUT_A" -- bash -c 'cd "$FLOX_ENV_PROJECT"; .venv/bin/python -m pytest -q'
flox activate -d "$CHECKOUT_B" -- bash -c 'cd "$FLOX_ENV_PROJECT"; .venv/bin/python -m pytest -q'
```

The real controller should impose a startup deadline, check the holder is still alive, capture holder errors, and explicitly verify cleanup after this trap. The `|| true` in an EXIT trap prevents masking the main failure and is not a successful teardown receipt. Do not claim teardown passed until endpoints close and owned processes disappear. Keep holder startup and cleanup overhead in the session lifecycle measurement. Repeated command latency should measure the actual `flox activate -- COMMAND` entry, with services already ready, alongside the common workload.

Start/restart commands must themselves run within the matching activation:

```bash
flox activate -d "$CHECKOUT_A" -- flox services start
flox activate -d "$CHECKOUT_A" -- flox services restart
flox services stop -d "$CHECKOUT_A"
flox activate -d "$CHECKOUT_A" -- flox services start
```

An existing holder preserves the activation across those fresh shells. Stop/status/logs can target `-d` outside activation. All-service restart can use the latest environment build via an ephemeral activation; restarting named services while others run uses the existing manager configuration. Explicit stop then start is the safest workflow after configuration changes or when dependency ordering must be reapplied. Do not benchmark a named restart as if it refreshes every service's manifest.

## Isolation, persistence, copy, and failure checks

Implement these against the shared application fixture, with real requests and explicit exceptions rather than Python `assert`:

1. After readiness, create a `bench_identity(instance text primary key, token text)` table in each PostgreSQL cluster. Insert distinct random run tokens for A and B. Write the same Redis key, such as `bench:identity`, with different run tokens. Read both back from each checkout's exported URLs. Each endpoint must contain its own token and lack the other instance's SQL row. Distinct Redis logical databases alone do not prove distinct Redis processes.
2. Save DB `data_directory`, `cluster_name`, `port`, backend server version and cluster `system_identifier` from `pg_control_system()`. Save Redis `CONFIG GET dir port`, `INFO server` run ID, and `INFO persistence` AOF status. Query the released status JSON array and capture both PIDs. `flox services-socket -d A/B` is available in the executed release help; require distinct sockets for distinct canonical checkouts. Same environment display name is acceptable.
3. Start twice and compare server identities/PIDs. A duplicate start should not create another PostgreSQL/Redis process. Distinct command success is insufficient evidence.
4. Run an intentional occupied-port case in a benchmark-owned namespace and directory. Expect readiness/identity failure even if the manager start returns success. Retain service logs. Never point this case at an existing user listener.
5. Stop services while holder A remains alive, await closed endpoints, then start within a fresh activation and await readiness. Verify SQL and Redis tokens survive. Redis `run_id` and service PID should change after restart; PG cluster `system_identifier` and data directory should remain. Stopping Flox services does not erase `.flox/cache`.
6. Release the last A activation and await service/process termination. B must remain ready with its own tokens. Then release B and verify all owned server PIDs/process trees, ports and manager sockets are inactive. A stale socket file's existence alone is not a live-manager test.
7. Copy only project source, `pyproject.toml`, `uv.lock`, and committed `.flox/env/manifest.toml`, `.flox/env/manifest.lock`, plus the environment pointer `.flox/env.json` when distributing a path environment. Exclude `.flox/cache`, `.flox/log`, generated run symlinks, `.venv`, holder files, local endpoint file and live service state. The resulting checkout C gets its own `bench.local.env`, its own newly initialized data and same pinned packages/dependencies. Check lock hashes before/after and exact tool versions. On one host/system, compare output store paths from the lock and resolved executable targets, not activation wrapper bytes embedding checkout paths.
8. A failed command in the activated application must produce a nonzero controller result and still trigger cleanup. Use direct subprocess return codes; `pytest | tail` without `pipefail` can mask a test failure. Repeated-command setup must not quietly recreate an already running cluster or alter a copied lock.

A machine reboot or SIGKILL of the controller differs from graceful lifecycle release. Test it only in disposable benchmark-owned containers and classify abrupt recovery separately. Release notes say 1.17.0 recovers from a killed manager's stale socket. That is a source/release claim here, not proof that every descendant/server is cleaned up. Capture pre/post process trees and server identity before claiming recovery. Do not conflate killing a holder with killing process-compose or the activation executive.

## Service ordering and composition

Release 1.17.0 supports `depends-on` with `process_started`, `process_completed`, and `process_completed_successfully`. It does not support `process_healthy` or arbitrary process-compose YAML passthrough. A migration service can itself wait for PostgreSQL readiness, then run a migration; the application can depend on its successful completion. This is a fair additional workload if every comparator is allowed the same readiness/migration script:

```toml
[services.migrate]
command = 'bash "$FLOX_ENV_PROJECT/scripts/wait-and-migrate.sh"'
depends-on.postgres = { condition = "process_started" }

[services.app]
command = 'exec "$FLOX_ENV_PROJECT/.venv/bin/python" -m benchmark_app'
depends-on.migrate = { condition = "process_completed_successfully" }
depends-on.redis = { condition = "process_started" }
```

This waits for a process state, not a healthy Redis endpoint. The app/readiness script must still handle Redis readiness. Start all services together for dependency ordering. Named starts do not recursively start absent dependencies. The release [manifest manual](https://github.com/flox/flox/blob/486737b3e68b0f094e89b4b3e27260e9f6c91b4a/cli/flox/doc/manifest.toml.md#L619) specifies this boundary.

For reusable project environment testing, Flox includes are meaningful and should get credit. A consumer may use:

```toml
schema-version = "1.16.0"
[include]
environments = [{ dir = "../shared-python-services" }]
```

Prefer an immutable bundled local included environment for offline/comparable runs, keeping relative topology stable when copying. FloxHub sharing is a separate authenticated/network scenario, not a local timing prerequisite. Included vars/services/hooks/packages merge natively; consumer configuration and later includes take priority according to the [release composition manual](https://github.com/flox/flox/blob/486737b3e68b0f094e89b4b3e27260e9f6c91b4a/cli/flox/doc/manifest.toml.md#L679). Do not score intentional overrides as failed conflict detection merely because another product rejects them. Check the merged configuration with `flox list --config`, locks, and observed behavior.

## Platform and fairness constraints

- The official release has Linux/macOS packages for x86_64 and aarch64. These are native host processes, not per-checkout network namespaces. Windows should use a separately specified Linux/WSL route; native Windows execution was not established here.
- PostgreSQL `initdb` rejects root. Both locally available images default to root, so a fair container recipe needs an unprivileged benchmark user, writable owned checkout, and usable Nix/Flox installation/store. Report failures setting this up as provisioning failures. Do not call them a Flox service failure or run initialization as root and proceed after a masked hook error. Compare competitors under the same user/account permissions.
- Pin the selected Docker image digest, CPU architecture, assigned resources and Nix/cache state. The `ev-flox` image's entrypoint initializes Nix; the official image has a Bash command and no entrypoint. They are not interchangeable command lines or provisioning costs.
- Cold tool/cache materialization, cold dependency installation, fresh DB initialization, warm activation, warm test execution and background-session setup need separate measurements. Nix cache sharing and uv wheel sharing can help Flox; those are legitimate warm-cache scenarios when disclosed and applied consistently. Container creation is not a native-shell latency measurement.
- Fresh-shell environment entry is not hermetic execution. The host's broader PATH may remain reachable. Validate interpreter/executable identity; do not assume every bare command is from the pinned environment.
- No benchmark criterion should require Stack-specific bundles, automatic ports, always-on services, session IDs or JSON inspection commands. Compare successful application setup, repeated tests, concurrent checkout correctness, reproducible config copy, restart/data preservation and cleanup. Report native versus scripted effort separately.

## Historical harness corrections before implementation

- `eval/harness/flox.sh` uses `flox list --json`; executed 1.17.0 help has no such option. Use `.flox/env/manifest.lock` for structured package identity and `flox list --all`/`--extended` for human output.
- The service status man page says one JSON object per line, but release implementation emits one JSON array. Use implementation behavior when building the parser.
- Starting with `flox services start` outside activation should fail by design. Use the CI or held-session recipe according to the task, and count the necessary holder automation.
- A short `flox activate --start-services -- true` is expected to release the last activation and tear down its services. It is not a persistent detached-start API.
- Two projects deliberately using 5432/6379 test collision handling, not whether independent checkouts can work. The useful isolation task assigns each a port pair and verifies distinct storage, tokens and process identity. Keep the occupied-port failure scenario separately.
- Existing Redis configuration disables persistence. That setup cannot support a fair Redis persistence task. Enable the agreed durability policy across competitors.
- Old initialization runs in a container as root. Capture initdb exit and `PG_VERSION`; a masked failure plus successful activation is not a healthy environment.
- Do not copy the historical timing table or anecdotal failure result into the new report. Freeze these recipes, let the implementer run them sequentially, and retain raw receipts for each claimed result.
