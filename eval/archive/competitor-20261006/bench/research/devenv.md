# devenv research and benchmark recipe

Research date: 2026-10-06. Target: [cachix/devenv](https://github.com/cachix/devenv). This is implementation guidance, not a new benchmark result. Benchmark implementation belongs to Opus 5.5.

## Versions and evidence

| Item | Observed value | Evidence |
| --- | --- | --- |
| Latest released CLI | v2.4.0, published 2026-09-24 | [Official release](https://github.com/cachix/devenv/releases/tag/v2.4.0), GitHub releases API read on research date |
| Released source inspected | `b904dcb51fe48c30db250038241507f60752f222` | Shallow clone, fetch of v2.4.0, detached checkout |
| Main source initially inspected | `fe20b5cba7ab5e93ae73f956a8d3efc50e1753f4` | Main reported version 2.4.1, unreleased. Recipe and source links below use released source |
| Available local image | `ev-devenv:latest`, image ID prefix `98ecc336b969` | Read-only image inventory |
| CLI in that image | `devenv 2.3.1 (aarch64-linux)` | Executed `docker run --rm --network none ev-devenv:latest bash -c 'devenv version; nix --version; devenv shell --help; devenv up --help; devenv processes wait --help'` |
| Nix in that image | Determinate Nix 3.23.0, Nix 2.35.2 | Same version-only check |
| Native macOS tools | Neither `devenv` nor `nix` found on current PATH | `command -v` read-only check; Docker is available |

No service startup, dependency installation, integration tests, timings, crash tests, or persistence checks were executed during this research. Source-supported behavior below still needs benchmark receipts. The local version/help container had no network or user-service mounts and exited normally.

Historical context: `eval/REPORT.md`, `eval/configs/devenv/devenv.nix`, `eval/harness/devenv.sh`, and `eval/harness/current-benchmark.py` were read. The report's existing 2.3.1 results are historical. Its port/environment problem must not become an unqualified claim against the corrected 2.4.0 recipe. The v2.4.0 changelog specifically records a fix for shell/direnv resolving running allocated ports to base ports, issue #3208. The current harness already has an optional dynamic-port rewrite; the new implementation should make that the normal native recipe. [Released changelog](https://github.com/cachix/devenv/blob/b904dcb51fe48c30db250038241507f60752f222/CHANGELOG.md#L9).

## Supported workflow and fair boundary

devenv builds a development shell from locked Nix inputs. Its PostgreSQL and Redis modules install server/client packages, initialize state, generate start scripts, and supply readiness probes. The default manager for CLI 2.x is native. Detached `devenv up -d`, `devenv processes wait`, per-service restart, and `devenv down` are supported commands. This is a real service competitor. See the [released service modules](https://github.com/cachix/devenv/tree/b904dcb51fe48c30db250038241507f60752f222/src/modules/services) and [native manager module](https://github.com/cachix/devenv/blob/b904dcb51fe48c30db250038241507f60752f222/src/modules/process-managers/native.nix).

The recommended default is TCP services with native automatic port allocation, plus application URLs derived from allocated values. A separate supported socket-only variant can use PostgreSQL's empty `listen_addresses` and Redis `port = 0`; that avoids TCP contention but changes application transport. Keep its results separate from the comparable TCP task. The service modules create socket paths under the project's runtime directory. [PostgreSQL implementation](https://github.com/cachix/devenv/blob/b904dcb51fe48c30db250038241507f60752f222/src/modules/services/postgres.nix), [Redis implementation](https://github.com/cachix/devenv/blob/b904dcb51fe48c30db250038241507f60752f222/src/modules/services/redis.nix).

| Requirement | Native behavior | Project or benchmark responsibility |
| --- | --- | --- |
| Python and tools | Nix packages selected by locked inputs | Select interpreter/package family and record actual patch versions |
| Python application dependencies | uv package and optional initialization integration | Commit `pyproject.toml` and `uv.lock`; use `uv sync --frozen` |
| PostgreSQL initialization | Per-project `PGDATA`; initial database and role options | App migrations and seed data |
| Redis state | Per-project `REDISDATA` and foreground process | Set matching persistence policy across tools |
| Readiness | Modules declare exec probes; native wait command | Verify app connectivity and service identity after readiness |
| Two checkouts | Project state/runtime paths and automatic TCP allocation | Derive app URLs from resolved process ports; verify distinct data and sentinels |
| Restart and teardown | Native process-control commands and graceful shutdown | Observe PIDs, endpoints, saved data, and absence of leftovers |
| Reproducible copy | `devenv.lock` resolves Nix inputs | Copy config, locks, application source, and local imported modules, excluding live state |
| Reusable environment | Nix modules and YAML imports | Supply shared config as a pinned input or copied local module; evaluate this as config reuse |

Do not require a Stack-specific session JSON format, TTL, bundle CLI, or OCI package layout for the basic developer task. Score whether commands connect to the correct live instance and perform the same application work. A reusable config import is valid devenv delivery even when its format differs from Stack's bundle format.

## Concrete configuration

Use a shared fixture with a committed `uv.lock` and tests that require `DATABASE_URL` and `REDIS_URL`, with no default-port fallback. This example selects Python 3.13, PostgreSQL 17, Redis from locked nixpkgs, and uv from the same lock. `pkgs.python313` identifies a package family; the committed Nix lock determines its exact patch version and store output. If the benchmark requires an exact patch shared with other tools, select the corresponding package revision or supported `languages.python.version`, lock its added `nixpkgs-python` input, and verify `python --version` before sampling. Do not call unlike patch versions identical.

Use this `devenv.yaml`. The nixpkgs commit below is taken from the v2.4.0 repository's committed `devenv.lock`, rather than an invented package snapshot. The modules input is explicitly pinned to the same released source as the CLI. Resolve and retain the generated project lock before measurements.

```yaml
inputs:
  nixpkgs:
    url: github:NixOS/nixpkgs/addf7cf5f383a3101ecfba091b98d0a1263dc9b8
  devenv:
    url: github:cachix/devenv/b904dcb51fe48c30db250038241507f60752f222?dir=src/modules
```

Use this `devenv.nix`:

```nix
{ pkgs, config, ... }:
{
  languages.python = {
    enable = true;
    package = pkgs.python313;
    uv.enable = true;
    # Explicit dependency installation is a separately measured operation.
    uv.sync.enable = false;
  };

  services.postgres = {
    enable = true;
    package = pkgs.postgresql_17;
    listen_addresses = "127.0.0.1";
    port = 55432;
    initialDatabases = [{ name = "bench"; user = "bench"; }];
    initialScript = "GRANT pg_read_all_settings TO bench;";
    initdbArgs = [ "--auth=trust" "--encoding=UTF8" "--locale=C" ];
  };

  services.redis = {
    enable = true;
    package = pkgs.redis;
    bind = "127.0.0.1";
    port = 56379;
    extraConfig = ''
      save ""
      appendonly yes
      appendfsync always
    '';
  };

  env = {
    DATABASE_URL = "postgresql://bench@127.0.0.1:${toString config.processes.postgres.ports.main.value}/bench";
    REDIS_URL = "redis://127.0.0.1:${toString config.processes.redis.ports.main.value}/0";
  };
}
```

Trust authentication is appropriate for an isolated local fixture and does not imply a production deployment recommendation. The settings-reader grant allows the app user to inspect the server's data directory for identity verification. Redis AOF with `appendfsync always` makes the persistence task deterministic after acknowledged writes; configure every competitor with the same policy, or label different policies and exclude their timings from a direct persistence comparison. The native Python module exports `UV_PROJECT_ENVIRONMENT=$DEVENV_STATE/venv`, `UV_PYTHON_DOWNLOADS=never`, and system-Python preference. Explicit uv commands use that per-project environment. [Python implementation](https://github.com/cachix/devenv/blob/b904dcb51fe48c30db250038241507f60752f222/src/modules/languages/python/default.nix#L740).

For users who prefer installation during shell initialization, the idiomatic alternative is:

```nix
languages.python.uv.sync = {
  enable = true;
  arguments = [ "--frozen" ];
};
languages.python.venv.enable = true;
```

Measure that alternative as configured. The module caches a successful sync using the interpreter, `pyproject.toml` checksum, and sync arguments. Its checksum does not include `uv.lock`, so explicitly run `uv sync --frozen` when validating a dependency-lock-only change. Do not silently add automatic sync cost to every command, or omit installation altogether. This is source inference from [the uv initialization code](https://github.com/cachix/devenv/blob/b904dcb51fe48c30db250038241507f60752f222/src/modules/languages/python/default.nix#L187).

## Noninteractive commands

Prerequisite provisioning is separate from application setup. A Nix installation and network/cache access are needed on a fresh machine. For the primary current-version lane, install the released CLI into an isolated profile, rather than upgrading the user's global profile:

```sh
# BENCH_ROOT is an absolute, benchmark-owned directory supplied by the runner.
nix profile add --profile "$BENCH_ROOT/cli-profile" \
  --accept-flake-config \
  github:cachix/devenv/b904dcb51fe48c30db250038241507f60752f222#devenv
export PATH="$BENCH_ROOT/cli-profile/bin:$PATH"
devenv version
nix --version
```

The research did not execute that installation. It may download or build the CLI; report that cost separately. If using the old local image without an upgrade, label the lane `devenv 2.3.1`, record the modules revision too, and do not label it latest. A CLI can consume independently locked module inputs, so capture both versions.

For each checkout, start from the common app fixture plus the above config and committed locks. Use a non-root Unix user; PostgreSQL `initdb` refuses root. Nix container provisioning must arrange a writable project and usable Nix store/profile for that user.

```sh
cd "$CHECKOUT_A"
set -eu
devenv shell --no-tui -- python --version
devenv shell --no-tui -- uv --version
devenv shell --no-tui -- postgres --version
devenv shell --no-tui -- redis-server --version

# For fixture authoring only, before freezing a common uv.lock:
# devenv shell --no-tui -- uv lock
# Commit the resulting application lock before trials.

devenv shell --no-tui -- uv sync --frozen
devenv up -d --no-tui
devenv processes wait --no-tui --timeout 120
devenv processes list --no-tui
devenv eval --no-tui \
  processes.postgres.ports.main.value \
  processes.redis.ports.main.value
devenv shell --no-tui -- bash -euc \
  'pg_isready -d "$DATABASE_URL"; test "$(redis-cli -u "$REDIS_URL" ping)" = PONG'
devenv shell --no-tui -- uv run --frozen --no-sync pytest -q
```

Use the fresh `devenv shell` after startup to read endpoints. The v2.4.0 CLI seeds the allocator from a running manager before evaluating command environments. Before a manager exists, inspection may return base ports. URLs captured before `up` are not authoritative for a relocated instance. Port allocation reserves listeners during startup and replays allocations for evaluation caching. [Allocator](https://github.com/cachix/devenv/blob/b904dcb51fe48c30db250038241507f60752f222/devenv-core/src/ports.rs), [manager seeding](https://github.com/cachix/devenv/blob/b904dcb51fe48c30db250038241507f60752f222/devenv/src/devenv/mod.rs#L3600), [entrypoint](https://github.com/cachix/devenv/blob/b904dcb51fe48c30db250038241507f60752f222/devenv/src/main.rs#L1139).

The native wait verifies configured readiness probes; PostgreSQL's probe waits for its initialization marker and a SQL query, and Redis's probe invokes `redis-cli ping`. This is more than an open-port wait. Identity checks remain necessary because healthy foreign services can answer the same protocols. [Probe definitions](https://github.com/cachix/devenv/blob/b904dcb51fe48c30db250038241507f60752f222/src/modules/services/postgres.nix#L476), [Redis probe](https://github.com/cachix/devenv/blob/b904dcb51fe48c30db250038241507f60752f222/src/modules/services/redis.nix#L111).

Repeat application commands with `devenv shell --no-tui -- uv run --frozen --no-sync ...`; do not rerun `uv sync` inside every sampled command. Separately measure actual sync refresh when dependencies change. Always retain the executed command and exit status, and use no stdout pipeline that masks test failure.

`devenv test` is also a supported CI workflow: `enterTest` contains the test command and devenv starts/stops configured services. Benchmark it as a whole-test lifecycle lane, separate from repeated commands against detached services. The [released test module](https://github.com/cachix/devenv/blob/b904dcb51fe48c30db250038241507f60752f222/src/modules/tests.nix) supplies readiness waiting. Explicit `up`, native `wait`, tests, and `down` provide a clearer comparable breakdown for this benchmark.

## Exact identity checks

The common fixture should write and inspect a receipt using application clients, not parsed supervisor status. Use a fresh random project token with no default endpoint fallback. Record all of:

1. `sys.version`, `sys.executable`, PostgreSQL server version, and Redis server version.
2. Required `DATABASE_URL`, required `REDIS_URL`, resolved `PGPORT`, `PGDATA`, `REDISDATA`, `DEVENV_STATE`, and `DEVENV_RUNTIME`.
3. SQL `SELECT current_database(), current_user, current_setting('port'), current_setting('data_directory'), pg_postmaster_start_time()`.
4. SQL `CREATE TABLE IF NOT EXISTS bench_identity (token text PRIMARY KEY)` and insertion of the checkout's token. Read all tokens in a separate application command and require exactly that checkout's token.
5. Redis `INFO server` fields `run_id`, `process_id`, `tcp_port`, and `redis_version`; `CONFIG GET dir`, `CONFIG GET appendonly`, and `CONFIG GET appendfsync`.
6. Redis key `bench:identity`, created with `SET ... NX` and read back. Require the checkout's token; when reusing an existing instance require the existing token instead of replacing it.

Require the SQL port to equal the parsed database URL port and Redis `tcp_port` to equal the parsed cache URL port. Resolve server data paths and require them to match that command's `PGDATA` and `REDISDATA`. Use `psycopg.connect(..., connect_timeout=3)` and Redis client socket/connect timeouts. A SQL `SELECT 1` or Redis PONG alone does not prove isolation. Save the JSON receipt before sampling, not just human-readable CLI output.

For two checkouts A and B:

```sh
# Copy tracked app/config/lock files into A and B, excluding .devenv and .venv.
# Run A's setup and identity writer before B starts.
cd "$CHECKOUT_B"
devenv shell --no-tui -- uv sync --frozen
devenv up -d --no-tui
devenv processes wait --no-tui --timeout 120
# Run B's identity writer through a new devenv shell.
# Then run the read-only identity assertion separately from A and B.
```

A and B use identical base ports and identical lock bytes. With automatic allocation, require distinct actual PostgreSQL TCP ports and distinct actual Redis TCP ports, distinct canonical data directories, distinct Redis `run_id`, and no token from the other checkout. Do not hardcode that B gets exactly `55433` and `56380`; any available allocated ports are valid. Leave A running while stopping and restarting B, then require A's receipt and process identities to stay valid.

Devenv's state directory is normally `.devenv/state`; services use its `postgres` and `redis` subdirectories. Runtime paths hash the absolute dotfile path, keeping Unix sockets short and different between checkouts. Git worktrees should therefore get independent state without custom hashing scripts. Verify it in real paths rather than assuming it from the source. [State helper](https://github.com/cachix/devenv/blob/b904dcb51fe48c30db250038241507f60752f222/devenv/src/devenv/mod.rs#L1641), [runtime path implementation](https://github.com/cachix/devenv/blob/b904dcb51fe48c30db250038241507f60752f222/devenv-core/src/paths.rs#L85).

## Restart, stop, persistence, and copying

```sh
cd "$CHECKOUT_B"
devenv processes restart --no-tui redis
devenv processes wait --no-tui --timeout 120
# Assert B's cache token survived; its run_id should change.
devenv processes restart --no-tui postgres
devenv processes wait --no-tui --timeout 120
# Assert B's SQL token survived; postmaster start time should change.
devenv down --no-tui
# Poll B's recorded service PIDs and endpoints to confirm B stopped.
# Also check A's SQL/cache identity without stopping A.
devenv up -d --no-tui
devenv processes wait --no-tui --timeout 120
# Read B's tokens and data-directory identity again; retain data, refresh endpoints.
```

`down` stops the manager and its processes; it is not a database reset. PostgreSQL startup skips initial database creation when `PGDATA` already exists. Changing `initialScript` or `initialDatabases` does not migrate an existing cluster. Redis persistence follows the configured server policy. A fresh reproduction should have the same tools/dependency locks and empty independent data, rather than copied mutable databases. Copy the app's tracked files, `devenv.nix`, `devenv.yaml`, `devenv.lock`, `pyproject.toml`, `uv.lock`, and all local import files. Exclude `.devenv`, `.venv`, runtime sockets, CLI profiles, and receipts from config reproduction. Compare lock hashes before and after commands. [Official pinning guide](https://devenv.sh/pinning/).

When testing configuration edits, stop and start the manager after the edit. An attaching `devenv up` schedules into the existing manager's original configuration; it does not apply edited config. Repeated `up -d` should be checked for duplicate PIDs/state, but the repeated-command timing task should use ordinary shell commands, not repeated supervisor startup. [Official process workflow](https://devenv.sh/processes/).

A manager-kill recovery test is legitimate if applied equally to all competitors. Capture the manager PID and owned service process scopes first; kill only that disposable fixture's manager, then poll for cleanup, bound the observation deadline, and retain exact PID/start-time evidence. Current source contains an out-of-scope guardian using manager-pipe EOF and start-time-aware recovery records; therefore do not assume SIGKILL must orphan services. This is code inference, not an executed success. [Guardian implementation](https://github.com/cachix/devenv/blob/b904dcb51fe48c30db250038241507f60752f222/devenv-processes/src/process_guardian.rs).

## Platform and benchmark constraints

Released build targets include `x86_64-linux`, `i686-linux`, `aarch64-linux`, and `aarch64-darwin`. Intel macOS CLI builds were dropped in 2.2. Linux containers are appropriate for the Linux lane, with Nix and non-root PostgreSQL configured correctly. A Docker Linux result on this Apple Silicon host is not native macOS service evidence. Native Windows is not this workflow; WSL2 is a Linux environment. [Release platform list](https://github.com/cachix/devenv/blob/b904dcb51fe48c30db250038241507f60752f222/flake.nix#L81), [2.2 changelog](https://github.com/cachix/devenv/blob/b904dcb51fe48c30db250038241507f60752f222/CHANGELOG.md#L236), [installation docs](https://devenv.sh/getting-started/).

Separate machine provisioning, uncached Nix realization/downloads, locked app setup, fresh service initialization, warm service start, repeated app commands, and recreation from saved configuration. Report whether the Nix store, evaluation cache, Python wheel cache, and app venv are retained for each trial. Rebuilding a Docker image is provisioning work. A fresh checkout can still share the host's Nix store, so call that a fresh checkout with a warm package cache when appropriate.

Useful comparable tasks are one real application migration, writing/reading SQL rows plus cached records, rerunning integration tests after a source edit, two parallel working checkouts, a lock-only dependency update, service restart with persisted data, and clean reproduction from config/locks. Include separate lifecycle correctness and command latency results. Use the same application sources, Python dependency lock, test assertions, readiness deadline, and persistence policies across tools.

Common mistakes to avoid:

- Using `config.services.postgres.port` or `config.services.redis.port` in URLs. These are requested base ports, not actual allocated ports.
- Capturing URLs before services start, then treating that snapshot as live endpoint identity.
- Using generic `SELECT 1`, PONG, or default-port fallbacks as proof of the intended checkout.
- Calling `devenv processes list` structured JSON. It is a supported status command, but the inspected CLI offers no JSON-output flag for this subcommand. `devenv eval` returns JSON for configuration values; trace JSON is logging, not a service-status schema.
- Running `uv sync` without `--frozen` while claiming dependency-lock reproduction, or timing dependency downloads as command-entry overhead.
- Piping pytest through `tail` without `pipefail`, which can record a failed test as successful.
- Running PostgreSQL as root in a Nix container, or treating the refusal as missing service support.
- Copying `.devenv` between checkouts, which confounds independent state and may include stale ownership/absolute-path state.
- Expecting initialization scripts to rerun against an existing PostgreSQL data directory.
- Testing Redis persistence without a defined persistence policy.
- Killing services by broad process-name matching or comparing all host PostgreSQL processes. Scope every lifecycle observation to recorded owned PIDs and data directories.
- Comparing a cached 2.3.1 image against another tool's freshly upgraded release while calling all versions current.

The recipe requires runtime verification before any success or performance claim. Source reading establishes supported mechanisms and correct configuration; it does not establish that a given host, image, or application run succeeded.
