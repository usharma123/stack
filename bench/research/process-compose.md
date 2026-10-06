# Process Compose

Research date: 2026-10-06. Implementation owner: Opus 5.5. This note contains source research, version/help checks, and YAML validation, with no service or timing runs.

## Recommendation and scope

Include Process Compose as a supervisor building block. Name the complete application lane **Nix-pinned toolchain + Process Compose 1.122.0 + project configuration**. Process Compose starts existing executables, supervises them, gates dependencies, checks readiness, exposes JSON process status, and stops/restarts processes. It does not install Python/PostgreSQL/Redis, lock their packages, allocate distinct checkout ports, choose data directories, or inject a prepared environment into arbitrary external application commands. Nix supplies the toolchain in the recipe below; uv supplies Python dependency installation. Count and disclose their setup and command-entry costs separately.

This is a useful comparison with a complete environment manager when the combined recipe is explicit. Process Compose alone is an adjacent tool. Its lack of package provisioning is `unsupported` for that feature, not a failed application benchmark. Devbox's embedded Process Compose is a separate integration, not evidence about a direct Process Compose run.

## Versions and inspected evidence

- Official [latest release](https://github.com/F1bonacc1/process-compose/releases/tag/v1.122.0): v1.122.0, published 2026-08-17. Live GitHub API returned this release on the research date.
- Shallow clone: `/tmp/stack-bench-sources/process-compose`, main SHA [`23b0acacc937d745279fb1551337f4031c4fc865`](https://github.com/F1bonacc1/process-compose/commit/23b0acacc937d745279fb1551337f4031c4fc865). Meaningful implementation read includes the loader, dependency runner, readiness state/probes, Unix process signalling, detached launcher, API handlers, and client commands. Source links below pin this SHA.
- The annotated release tag resolves to `673850dd20683ef14b33890444ca3416d02c751c`. `git diff v1.122.0 HEAD --stat` showed only a `default.nix` package-version change; inspected Go behavior is identical. The downloaded release binary actually reports commit `23b0aca`. Retain both the tag commit and binary metadata rather than assuming equality.
- Host initially had no `process-compose` or `nix` on PATH. The Docker image inventory had no dedicated Process Compose image. Existing `ev-devbox` may contain an embedded binary; its version was not checked. Do not substitute it without recording its real version and configuration.
- I downloaded the official Darwin arm64 archive into `/tmp/stack-bench-sources/process-compose-bin`, verified its SHA-256 against the release checksum file, and executed `version`, `up --help`, `project is-ready --help`, and `process restart --help`. Version output was v1.122.0, commit `23b0aca`, UTC date `2026-08-17T22:59:01Z`. This proves CLI availability only.
- I extracted the proposed YAML below into `/tmp/stack-bench-sources/process-compose-configcheck` and invoked the downloaded binary with `up --dry-run --disable-dotenv -f <config> -t=false postgres redis`, explicit dummy environment variables, and a scratch log/socket path. Exit was 0, with `Validated 5 configured processes from 2 files.` The count includes the default env-file entry even though dotenv loading was disabled. No commands, probes, or services ran; this validates configuration shape, not toolchain realization or application behavior.

## Exact lifecycle behavior

**Readiness.** `up --detached` forks the same executable using a new Unix session, polls the control server's `/live` endpoint for up to five seconds, then returns. This is manager readiness, not PostgreSQL/Redis readiness. [Detached launcher](https://github.com/F1bonacc1/process-compose/blob/23b0acacc937d745279fb1551337f4031c4fc865/src/cmd/project_runner_unix.go#L14).

`project is-ready` checks all returned process states and exits nonzero when any fails the ready predicate. With `--wait` it retries every second indefinitely, including API errors. There is no timeout flag. Wrap the native wait in a separately disclosed deadline. Disabled processes count as ready; successful completed one-shot processes can also count as ready. A running process without a probe is not proof its protocol is ready. Verify the expected services are present, running, healthy, and actually serving the expected checkout. [Wait implementation](https://github.com/F1bonacc1/process-compose/blob/23b0acacc937d745279fb1551337f4031c4fc865/src/cmd/is_ready.go#L14), [ready predicate](https://github.com/F1bonacc1/process-compose/blob/23b0acacc937d745279fb1551337f4031c4fc865/src/types/process.go#L406).

Exec probes run the configured shell command with the process environment and working directory; command failure means probe failure. Defaults are initial delay 0s, period 10s, timeout 1s, failure threshold 3. The implementation calls a failure fatal when contiguous failures equal the threshold. A fatal readiness failure invokes the process's internal stop; the configured availability policy determines whether it restarts. `success_threshold` is accepted but not implemented. Liveness's fatal callback marks a daemon stopped; use readiness probes for the foreground servers here. [Probe execution](https://github.com/F1bonacc1/process-compose/blob/23b0acacc937d745279fb1551337f4031c4fc865/src/health/exec_checker.go#L22), [threshold handling](https://github.com/F1bonacc1/process-compose/blob/23b0acacc937d745279fb1551337f4031c4fc865/src/health/health_checks.go#L92), [process callbacks](https://github.com/F1bonacc1/process-compose/blob/23b0acacc937d745279fb1551337f4031c4fc865/src/app/process.go#L1134), [official probe guide](https://f1bonacc1.github.io/process-compose/health/).

**Dependencies.** `process_started` is the default launch gate. `process_completed` permits any exit code; `process_completed_successfully` checks successful exit status, including configured `success_exit_codes`. `process_healthy` waits for readiness; use an actual readiness probe, not merely a liveness declaration. `process_log_ready` waits for a matching log line. These are startup gates, not continuous guarantees that a dependency remains healthy. Selection with `up postgres redis` includes their dependencies. A failed prerequisite can skip its dependent; `availability.exit_on_skipped: true` makes that a project failure. [Dependency runner](https://github.com/F1bonacc1/process-compose/blob/23b0acacc937d745279fb1551337f4031c4fc865/src/app/project_runner.go#L362), [conditions](https://github.com/F1bonacc1/process-compose/blob/23b0acacc937d745279fb1551337f4031c4fc865/src/types/process.go#L543).

**Shutdown.** Set `ordered_shutdown: true` to wait for dependents to complete before stopping their prerequisites. The default is unordered. With no shutdown command, Unix shutdown signals the process group by default; `parent_only: true` targets the direct process instead. Default signal is SIGTERM. An explicit `timeout_seconds` enables SIGKILL fallback; do not interpret the shutdown-command default of 10 seconds as an implicit signal-path timeout. With `shutdown.command`, Process Compose instead runs that command using the process's environment and working directory, with a default 10-second command deadline, and sends SIGKILL on command failure/timeout. [Shutdown runner](https://github.com/F1bonacc1/process-compose/blob/23b0acacc937d745279fb1551337f4031c4fc865/src/app/project_runner.go#L1213), [stop implementation](https://github.com/F1bonacc1/process-compose/blob/23b0acacc937d745279fb1551337f4031c4fc865/src/app/process.go#L530), [Unix signalling](https://github.com/F1bonacc1/process-compose/blob/23b0acacc937d745279fb1551337f4031c4fc865/src/command/stopper_unix.go#L15).

The recipe starts PostgreSQL with `exec` and sends SIGINT to its direct supervisor for fast, orderly shutdown. PostgreSQL SIGTERM is smart shutdown and can wait for open clients. Redis runs in the foreground and receives SIGTERM. Both have explicit 30-second fallback deadlines. [PostgreSQL signal semantics](https://www.postgresql.org/docs/17/server-shutdown.html).

`down` requests project shutdown. The API constructs the success response before invoking teardown and ignores teardown's returned error; internal shutdown also logs individual stop errors. Treat CLI exit zero as a request receipt and verify endpoints, owned PIDs, and manager disappearance. Process Compose does not remove database files on down. [API handler](https://github.com/F1bonacc1/process-compose/blob/23b0acacc937d745279fb1551337f4031c4fc865/src/api/pc_api.go#L482).

**Isolation.** Native Unix-socket control supports a user-supplied socket per manager. The default TCP control port is 8080, and the automatic UDS pathname includes the caller PID, so fresh client invocations need an explicit stable path. Namespaces select/group processes within a manager; they do not isolate TCP listeners or filesystem data. The server rejects a connectable existing socket and removes an unconnectable socket path before binding. Never use another project's pathname or manually unlink a live socket. User configuration must assign distinct PostgreSQL/Redis ports, data directories, socket directories, and log destinations. [Client guide](https://f1bonacc1.github.io/process-compose/client/), [UDS binding](https://github.com/F1bonacc1/process-compose/blob/23b0acacc937d745279fb1551337f4031c4fc865/src/api/server.go#L16), [default socket path](https://github.com/F1bonacc1/process-compose/blob/23b0acacc937d745279fb1551337f4031c4fc865/src/config/config.go#L323).

## Combined pinned recipe

This is a proposed recipe, not executed application evidence. Run as a non-root Unix user. Supply the shared fixture, `pyproject.toml`, and committed `uv.lock` in each checkout. Keep the flake in a static `toolchain/` subdirectory so repeated path-flake evaluation does not copy growing database state into the Nix store.

Save `toolchain/flake.nix`:

```nix
{
  inputs.nixpkgs.url =
    "github:NixOS/nixpkgs/151fa4e8ddfdd8dd25d945ad94ed54a13de9f6e4";
  outputs = { self, nixpkgs }:
    let systems = [ "aarch64-linux" "x86_64-linux"
                    "aarch64-darwin" "x86_64-darwin" ];
    in {
      devShells = nixpkgs.lib.genAttrs systems (system:
        let pkgs = nixpkgs.legacyPackages.${system};
        in { default = pkgs.mkShellNoCC {
          packages = [ pkgs.python313 pkgs.uv pkgs.postgresql_17
                       pkgs.redis pkgs.bash ];
          UV_PYTHON = "${pkgs.python313}/bin/python3";
          UV_PYTHON_DOWNLOADS = "never";
          shellHook = ''
            export PGDATA="$(pwd -P)/.state/postgres"
            export REDISDATA="$(pwd -P)/.state/redis"
            export PGPORT="''${RWB_PGPORT:?set RWB_PGPORT}"
            export REDIS_PORT="''${RWB_REDISPORT:?set RWB_REDISPORT}"
            export PGHOST="''${RWB_RUNTIME:?set RWB_RUNTIME}/pg"
            export DATABASE_URL="postgresql://postgres@127.0.0.1:$PGPORT/postgres"
            export REDIS_URL="redis://127.0.0.1:$REDIS_PORT/0"
          '';
        }; });
    };
}
```

At this pinned Nixpkgs revision, the inspected package sources specify Python 3.13.15, PostgreSQL 17.11, Redis 8.10.2, and uv 0.12.22. These are source versions, not installed version receipts. Check the runtime outputs and platform availability before a run. Match the plain Nix lane's revision or disclose any difference. [Python package](https://github.com/NixOS/nixpkgs/blob/151fa4e8ddfdd8dd25d945ad94ed54a13de9f6e4/pkgs/development/interpreters/python/default.nix), [PostgreSQL package](https://github.com/NixOS/nixpkgs/blob/151fa4e8ddfdd8dd25d945ad94ed54a13de9f6e4/pkgs/servers/sql/postgresql/17.nix), [Redis package](https://github.com/NixOS/nixpkgs/blob/151fa4e8ddfdd8dd25d945ad94ed54a13de9f6e4/pkgs/by-name/re/redis/package.nix), [uv package](https://github.com/NixOS/nixpkgs/blob/151fa4e8ddfdd8dd25d945ad94ed54a13de9f6e4/pkgs/by-name/uv/uv/package.nix).

Save `process-compose.yaml`:

```yaml
version: "0.5"
name: rwb
is_strict: true
disable_env_expansion: true
ordered_shutdown: true
shell:
  shell_command: bash
  shell_argument: -euc
processes:
  init-postgres:
    command: |
      umask 077
      mkdir -p "$PGHOST" "$(dirname "$PGDATA")"
      if [ ! -f "$PGDATA/PG_VERSION" ]; then
        initdb -D "$PGDATA" -U postgres --auth=trust --encoding=UTF8 --locale=C
      fi
    availability: { restart: exit_on_failure }
  init-redis:
    command: 'umask 077; mkdir -p "$REDISDATA"'
    availability: { restart: exit_on_failure }
  postgres:
    command: 'exec postgres -D "$PGDATA" -k "$PGHOST" -p "$PGPORT" -c listen_addresses=127.0.0.1'
    depends_on:
      init-postgres: { condition: process_completed_successfully }
    readiness_probe:
      exec:
        command: 'psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -Atc "SELECT 1"'
      period_seconds: 1
      timeout_seconds: 2
      failure_threshold: 120
    availability: { restart: exit_on_failure, exit_on_skipped: true }
    shutdown: { signal: 2, parent_only: true, timeout_seconds: 30 }
  redis:
    command: 'exec redis-server --bind 127.0.0.1 --port "$REDIS_PORT" --dir "$REDISDATA" --daemonize no --save "" --appendonly no'
    depends_on:
      init-redis: { condition: process_completed_successfully }
    readiness_probe:
      exec:
        command: 'test "$(redis-cli -u "$REDIS_URL" ping)" = PONG'
      period_seconds: 1
      timeout_seconds: 2
      failure_threshold: 120
    availability: { restart: exit_on_failure, exit_on_skipped: true }
    shutdown: { signal: 15, timeout_seconds: 30 }
  integration:
    disabled: true
    command: 'exec uv run --frozen --no-sync pytest -q'
    depends_on:
      postgres: { condition: process_healthy }
      redis: { condition: process_healthy }
    availability: { exit_on_end: true, exit_on_skipped: true }
```

Initialization and probe commands are project-owned payloads using native supervision/gates. `disable_env_expansion` leaves shell variables for execution; without it, escape them as `$$` to avoid loader substitution. Use explicit `-f` because discovery can select `compose.yaml` before `process-compose.yaml`. Keep servers foreground; `is_daemon: true` describes an executable that forks away, which needs separate monitoring and shutdown. [Loader](https://github.com/F1bonacc1/process-compose/blob/23b0acacc937d745279fb1551337f4031c4fc865/src/loader/loader.go#L147), [official lifecycle guide](https://f1bonacc1.github.io/process-compose/launcher/).

The primary recipe disables automatic Redis snapshots and AOF. For persistence, the shared fixture's explicit `persist` task issues `SAVE`, then `persisted` reads the durable key after restart. Record this as application-scripted durability and use the same policy across tools. If the task instead requires every acknowledged Redis write to survive a stop, use `--appendonly yes --appendfsync always` consistently across competitors and disclose its write cost.

## Noninteractive operations

Provision Nix separately and record its full version/distribution. Provision the Process Compose binary separately from the immutable v1.122.0 release URL, verify the archive checksum, and retain the binary SHA-256. Observed official archive digests:

```text
darwin_arm64 eaa1238a1d6c300e928ef855d36a3d95aab8da64997104d74ea73d91d3cb6c60
darwin_amd64 b4f76e881759e13f7913a5b2fed16a0e75275c04d3a61e656d40448921f1c3ec
linux_arm64  52fa7d5a2d5e0db470faec5976204fc215ed7e3d13689e930cf522becfb63778
linux_amd64  9b6dbc38324c0b0481f1cd1dd828ffdc78117129ec797678f4bf8c4023311281
```

Archive URL pattern: `https://github.com/F1bonacc1/process-compose/releases/download/v1.122.0/process-compose_<os>_<arch>.tar.gz`. Only Darwin arm64 was downloaded/executed here. [Installation options](https://f1bonacc1.github.io/process-compose/installation/).

The following shell functions are proposed adapter glue, not Process Compose features. `RWB_PC_BIN` is the absolute verified binary path. The parent must supply available distinct port pairs and a fresh, short absolute runtime directory for each checkout. Use owned directories under `/tmp` to stay within macOS Unix-socket path limits. Restore these variables/functions in every fresh command shell; do not depend on an interactive activation.

```sh
cd "$CHECKOUT_A"
set -eu
export RWB_PC_BIN="$VERIFIED_PC_BIN"
export RWB_PGPORT=55432 RWB_REDISPORT=56379 RWB_CHECKOUT=a
export RWB_RUNTIME="$OWNED_RUNTIME_A"
umask 077
mkdir -p "$RWB_RUNTIME" .state

tc() {
  nix --extra-experimental-features 'nix-command flakes' develop \
    "path:$PWD/toolchain" --no-update-lock-file --command "$@"
}
pc() {
  tc "$RWB_PC_BIN" --unix-socket "$RWB_RUNTIME/pc.sock" \
    --log-file "$PWD/.state/supervisor.log" "$@"
}

# Initial lock authoring only. Save toolchain/flake.lock before locked trials.
nix --extra-experimental-features 'nix-command flakes' flake lock "path:$PWD/toolchain"
tc python --version
tc uv --version
tc postgres --version
tc redis-server --version
pc version
tc uv sync --frozen
pc up --disable-dotenv -f process-compose.yaml -t=false --detached postgres redis

# Native readiness with a separately scripted 120-second outer deadline.
tc python -c 'import os, subprocess; subprocess.run([os.environ["RWB_PC_BIN"], "--unix-socket", os.environ["RWB_RUNTIME"] + "/pc.sock", "project", "is-ready", "--wait"], timeout=120, check=True)'
pc process list --output json
tc uv run --frozen --no-sync python -m rwbapp identity
tc uv run --frozen --no-sync python -m rwbapp migrate
tc uv run --frozen --no-sync python -m rwbapp mark --checkout a
tc uv run --frozen --no-sync python -m rwbapp crud --checkout a
tc uv run --frozen --no-sync python -m rwbapp cache --checkout a
tc uv run --frozen --no-sync pytest -q
```

The parent should bound every external command, capture logs on failure, and clean partial starts by the recorded owned socket/PIDs. Process Compose's HTTP clients have no configured general request timeout; a timeout wrapper is useful beyond readiness. Separate uncached Nix realization, Python installation, database initialization, warm restart, and ordinary command entry. A warm `tc true` measures Nix environment entry. It is not Process Compose exec latency. [Nix command entry](https://nix.dev/manual/nix/2.35/command-ref/new-cli/nix3-develop), [client construction](https://github.com/F1bonacc1/process-compose/blob/23b0acacc937d745279fb1551337f4031c4fc865/src/client/client.go#L56).

For checkout B, copy static app/config/lock files only and repeat with `RWB_CHECKOUT=b`, `RWB_PGPORT=55433`, `RWB_REDISPORT=56380`, and `RWB_RUNTIME=$OWNED_RUNTIME_B`, while A remains live. Those assignments are scripted isolation. Do not hardcode these example ports without an availability check or reservation strategy. The same-process network namespace also needs distinct service ports inside a shared benchmark container; separate containers would hide the checkout-isolation requirement.

Repeat warm app reads through `tc uv run --frozen --no-sync python -m rwbapp read --checkout a`. Do not reinstall dependencies or invoke `pc up` for each read. To restart an existing manager's process, use `pc process restart postgres` or `pc process restart redis`, wait for readiness again, and refresh identity receipts. `up` creates a manager, not an idempotent attachment operation.

For a separate complete-test lifecycle lane, after verifying the detached manager has stopped, run `pc run --disable-dotenv -f process-compose.yaml integration`. This selects the configured foreground test and its dependency graph; `run` sets exit-on-end for the main process and propagates its exit code through the project runner. `exit_on_skipped` covers a prerequisite failure. The explicitly selected process is admitted despite `disabled: true`. Do not run this while the same socket/ports are owned by the detached manager. [Foreground command](https://github.com/F1bonacc1/process-compose/blob/23b0acacc937d745279fb1551337f4031c4fc865/src/cmd/run.go#L11), [main-process behavior](https://github.com/F1bonacc1/process-compose/blob/23b0acacc937d745279fb1551337f4031c4fc865/src/app/project_runner.go#L291).

## Required receipts and remaining validation

Use the shared fixture JSON and raw `pc process list --output json`. Require `postgres` and `redis` states to be present, `is_running: true`, `is_ready: "Ready"`, and `status: "Running"`. Compare Redis `pid` with the fixture's Redis PID and PostgreSQL `pid` with the first line of the owned `$PGDATA/postmaster.pid`, retaining PID start times. The list command emits an array of native process-state objects; `project state` is human-readable output. [JSON list implementation](https://github.com/F1bonacc1/process-compose/blob/23b0acacc937d745279fb1551337f4031c4fc865/src/cmd/list.go#L37), [field names](https://github.com/F1bonacc1/process-compose/blob/23b0acacc937d745279fb1551337f4031c4fc865/src/types/process.go#L278).

Verify `ok: true`, Python/runtime versions, exact URLs and ports, canonical PostgreSQL `data_directory`, PostgreSQL system identifier/start time, Redis `run_id`/PID/port/`dir`, SQL checkout markers, and Redis `rwb:checkout`. Require A to contain only A's marker and B only B's, with distinct canonical data paths, PG system identifiers, Redis run IDs, control sockets, and listener ports. Probe success or an open TCP listener alone does not establish identity. A successful custom identity assertion is benchmark-owned checking, not a native wrong-instance guard.

Run `tc ... rwbapp persist --checkout a`, then `pc down`. Confirm A's recorded service PIDs and manager are gone, A's endpoints refuse connections, and B still passes `rwbapp check --checkout b --forbid a`. Start A again using the same data directories and original port/socket assignment, wait, then require A's SQL marker/keeper and saved Redis durable key to survive. Explicitly assert `result.redis_durable == true`; the current fixture's `persisted` command only raises on the missing PostgreSQL keeper. Require new server process identities/start times and verify B's identities stayed unchanged. Neither stop nor restart should run broad `pkill` or remove another manager's files.

Reproduce checkout C from the same `toolchain/flake.nix`, `toolchain/flake.lock`, Process Compose YAML, binary release/digest declaration, app source, `pyproject.toml`, and `uv.lock`. Exclude `.state`, `.venv`, live runtime paths, logs, and PIDs. Supply fresh runtime/port assignments and use `--no-update-lock-file` plus `uv sync --frozen`; compare both lock hashes before/after. Reproduction should produce the same tool/dependency versions with a fresh independent database, not a copy of A's mutable state.

Test unknown executable/config keys, failing init, bad toolchain package selection, occupied service ports, and a failed integration test. Attribute package-resolution rejection to Nix; Process Compose has no package-version resolver. Record incomplete startup as failure/error with cleanup evidence, not readiness success. A manager-crash scenario is optional and must capture ownership before killing only that fixture's manager. Source inspection does not establish cleanup after SIGKILL, so leave that behavior unmeasured until runtime verification.

Linux containers and native macOS are supported deployment choices for this Unix recipe. The release has Linux/macOS binaries for the listed architectures. Nix and PostgreSQL require appropriate non-root user/store permissions, and the pinned package derivations may download or build per architecture. Native Windows release binaries exist, but detached Unix sessions/UDS and this Nix recipe are not a native Windows workflow; use a separately labelled Linux/WSL lane. Report container image ID, architecture, CLI distribution, package store cache state, uv cache state, and retained venv/data state.

Historical `eval/` was read as context only. Direct Process Compose was explicitly unbenchmarked there, and Devbox's old manager-kill harness used broad process-name matching. Neither old results nor that cleanup pattern should become new direct Process Compose evidence. Remaining work belongs to Opus/parent: implement the isolated adapter, validate this recipe with real services, then run comparable migration/CRUD/cache/tests, concurrent checkouts, persistence, lock reproduction, and cleanup tasks. No fresh performance or lifecycle pass is claimed here.
