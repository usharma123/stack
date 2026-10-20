# mise with Pitchfork

Research date: 2026-10-06. This is one competitor comprising mise's tool/environment/task manager and its supported Pitchfork daemon integration. Benchmark implementation belongs to Opus 5.5. This document supplies recipes and verification requirements; it contains no new timing results.

## Versions and evidence

| Component | Latest published release checked | Release commit inspected | Locally available evidence |
| --- | --- | --- | --- |
| mise | [2026.10.3](https://github.com/jdx/mise/releases/tag/v2026.10.3), published 2026-10-05 | `33a839a4d19acca617bc9ec0fe53f61266d60a9d` | Downloaded release binary prints `2026.10.3 linux-arm64 (2026-10-05)` in an ephemeral container |
| Pitchfork | [2.29.0](https://github.com/jdx/pitchfork/releases/tag/v2.29.0), published 2026-09-29 | `cfdea79f1d52b8449c0b99b29a03d9e771cd8ec3` | No host executable on PATH; old eval configuration requests 2.29.0, which does not prove installation in an image |
| Existing `ev-mise:latest` | Historical image, not the latest mise release | Image `sha256:bd95396340b127241897b2986f905907c1ff6d480e84fc701d2177a36ca587fc` | `mise --version` prints `2026.9.18 linux-arm64 (2026-09-30)`; image is Linux/arm64, created 2026-10-02 |

Both repositories were shallow-cloned under `/tmp/stack-bench-sources/`, then switched to these release tags. Tag object IDs differ from the peeled commit IDs above. Initial default-branch snapshots were mise `ce132c7c9b58b0a919692ef5e26f28a67cc52fa4` and Pitchfork `181b142c7b2f9f7ef648b7374e4d99a69cf3b11c`; the recommendations below use release source, not unreleased commits.

Executed lightweight checks: GitHub releases API lookup, repository/source reads, Docker image inspection, `mise --version` in the old image, and latest release `mise --version`, `mise tasks --json`, `mise daemons start --help`, `mise daemons status --help`, and `mise lock --help`. The latter checks ran in a new `docker run --rm --init --network none -u agent` container with the binary and candidate TOML mounted read-only. Task parsing accepted the candidate task dependencies and `daemons` declarations. A separate ephemeral network-enabled container ran `mise ls-remote` for Python, uv, PostgreSQL and Redis; exact-match filters returned `3.13.16`, `0.12.23`, `17.11` and `8.10.2`. This verifies version listing, not installation or cross-platform package availability. No services were started, no application dependencies were installed, and no existing services or containers were modified.

Historical context read: `eval/configs/mise/mise.toml`, `eval/configs/mise-daemons/mise.toml`, `eval/images/Dockerfile.mise`, `eval/harness/mise.sh`, `eval/harness/current-benchmark.py`, and `eval/REPORT.md`. The initial DIY-service fixture and its old results are not suitable evidence about the current combined competitor. The later historical daemon addendum is useful for choosing probes, but every runtime claim needs a fresh receipt in the new benchmark.

## What the supported stack supplies

Mise resolves tools and environment variables. Its experimental `[daemons]` declarations generate a project configuration that Pitchfork supervises. PostgreSQL and Redis have native presets, including initialization, commands, readiness probes, persistent directories, and connection exports. Tasks with `daemons = [...]` start their requirements and wait before the task runs. Services continue after that task exits. Explicit start/stop works in noninteractive shells; interactive activation is optional. [Mise release daemon guide](https://github.com/jdx/mise/blob/33a839a4d19acca617bc9ec0fe53f61266d60a9d/docs/daemons.md#L5-L88), [task startup implementation](https://github.com/jdx/mise/blob/33a839a4d19acca617bc9ec0fe53f61266d60a9d/src/daemons/tasks.rs).

| Requirement | Native facility | Application or benchmark responsibility |
| --- | --- | --- |
| Python and CLI pins | `[tools]`, `mise.lock`, locked installation | Freeze common versions and dependency lockfiles before timing |
| PostgreSQL setup | Preset runs `initdb`, supports a database option, exports libpq variables and `DATABASE_URL` | Schema migration, application seed, data identity assertions |
| Redis setup | Preset runs foreground Redis with append-only persistence and exports `REDIS_URL` | Cache schema/key convention and isolation assertions |
| Service readiness | Preset client probes; Pitchfork supports command, HTTP, TCP and output checks | Verify usable application queries and exact server identity |
| Repeat commands | `mise exec -- ...`; task daemon startup reuses running processes | Compare bare environment entry separately from service-backed tasks |
| Independent clones | Distinct generated state/data and default daemon namespaces | Configure different ports for clones sharing one host |
| Linked worktrees | Automatic path-derived port offsets and namespaces | Detect finite-slot collisions; use valid Git worktrees |
| Stop/restart | Project-scoped daemon commands and process supervision | Verify process/socket exit and persisted data after restart |
| Copy to a fresh machine | Committed config, tool lock and application lock | Record platform artifacts, preserve sidecars, avoid copying runtime state |
| Bundle reuse | Remote tool/env includes and task catalogs | Daemon declarations/settings are excluded from remote config fragments; keep them in consumer config |

The PostgreSQL preset binds loopback, disables the Unix socket, uses local trust authentication, and requests `SIGINT` on stop. Its default readiness command is `pg_isready`. Redis binds loopback, enables append-only persistence, and probes with `redis-cli ... ping`. These are developer services, not production deployment recipes. [PostgreSQL preset](https://github.com/jdx/mise/blob/33a839a4d19acca617bc9ec0fe53f61266d60a9d/registry/daemon-presets/postgres.toml), [Redis preset](https://github.com/jdx/mise/blob/33a839a4d19acca617bc9ec0fe53f61266d60a9d/registry/daemon-presets/redis.toml).

## Candidate application configuration

Use the same application, dependency versions, test semantics and server major versions as other competitors. The concrete pins below match versions recorded in this repository's recent eval artifacts and appeared in fresh version listings using mise 2026.10.3 on Linux/arm64. Before freezing the benchmark, validate installation on every target platform and either retain them for all compatible competitors or choose a common supported set. Keep requested versions and actual server versions in separate receipt fields.

`mise.toml`:

```toml
min_version = "2026.10.3"

[settings]
experimental = true
lockfile = true

[tools]
python = "3.13.16"
uv = "0.12.23"
pitchfork = "2.29.0"
postgres = "17.11"
redis = "8.10.2"

[env]
UV_PYTHON_DOWNLOADS = "never"
UV_PYTHON = "3.13.16"

[daemons.postgres]
preset = "postgres"
version = "17.11"
port = "auto"
data_dir = ".data/postgres"
options.database = "bench"

[daemons.redis]
preset = "redis"
version = "8.10.2"
port = "auto"
data_dir = ".data/redis"

[tasks.sync]
run = "uv sync --frozen"

[tasks.test]
depends = ["sync"]
daemons = ["postgres", "redis"]
run = "uv run --frozen pytest -q"
```

Do not manually hard-code `DATABASE_URL`, `PGPORT`, or `REDIS_URL` here. Presets export them with the resolved port; explicit `[env]` entries take precedence and can break that wiring. Keep all tool requests in `[tools]` as well as the preset request, so the tool lock is easy to inspect. Matching requests are validated. Relative `data_dir` resolves from the declaring project root; the `.data` layout is chosen for inspectable receipts, not because mise requires local data. With no override, data lives under `MISE_STATE_DIR/daemons/<project-hash>/data/`. [Preset expansion and exports](https://github.com/jdx/mise/blob/33a839a4d19acca617bc9ec0fe53f61266d60a9d/src/daemons/presets.rs#L676-L863), [tool compatibility checks](https://github.com/jdx/mise/blob/33a839a4d19acca617bc9ec0fe53f61266d60a9d/src/daemons/runtime.rs#L1196-L1236).

Use the shared fixture's committed `pyproject.toml` and `uv.lock`. If adapting the existing eval fixture, note that it currently has broad dependency ranges and no checked-in `uv.lock`; it needs a deliberate dependency freeze before measurement. Require the selected Python minor in that application's metadata. Add `.venv/`, `.data/`, and `mise.local.toml` to its `.gitignore`.

Prepare lockfiles outside the measured installation:

```sh
cd "$APP_A"
mise trust ./mise.toml
mise lock --platform linux-arm64,macos-arm64
mise install --locked
mise exec -- uv lock
mise exec -- uv sync --frozen
mise ls --json --current
mise exec -- python --version
mise exec -- uv --version
mise exec -- pitchfork --version
mise exec -- postgres --version
mise exec -- redis-server --version
```

Generate a fixture with these files, then benchmark fresh installs of that frozen fixture. Do not hide the dependency-freeze preparation inside a cold-start result. Include x64 target entries if x64 is tested. Commit `mise.lock`, `uv.lock`, and any native tool dependency sidecars identified by `mise lock --sidecars --json`. `mise install --locked` validates the tool lock; `uv sync --frozen` preserves application dependencies. Locked installation still needs network for uncached artifacts. [Tool lock command implementation](https://github.com/jdx/mise/blob/33a839a4d19acca617bc9ec0fe53f61266d60a9d/src/cli/lock.rs), [tool lock guidance](https://github.com/jdx/mise/blob/33a839a4d19acca617bc9ec0fe53f61266d60a9d/docs/dev-tools/mise-lock.md).

The release registry chooses Conda first for `postgres` and Redis on Linux/macOS. Record the actual backend from `mise.lock`; do not substitute a source-building plugin silently. Conda installation includes its native dependencies, and release source supports locked dependency package records. A plugin backend can have weaker artifact locking. [PostgreSQL registry](https://github.com/jdx/mise/blob/33a839a4d19acca617bc9ec0fe53f61266d60a9d/registry/postgres.toml), [Redis registry](https://github.com/jdx/mise/blob/33a839a4d19acca617bc9ec0fe53f61266d60a9d/registry/redis.toml), [Conda lock installation](https://github.com/jdx/mise/blob/33a839a4d19acca617bc9ec0fe53f61266d60a9d/src/backend/conda.rs#L531-L604).

## Fresh noninteractive commands

Run each measured command in a fresh `bash -c` subprocess with stdin closed and captured stdout/stderr. Supply explicit absolute project paths with `-C`. Set `MISE_YES=1` and `NO_COLOR=1` consistently. The benchmark runner must preserve exit codes instead of piping through `tail` or `grep` without `pipefail`.

Use dedicated benchmark directories for mise config/data/cache/state and Pitchfork config/state, keeping their values identical across A and B within a scenario. Example environment preparation for the runner:

```sh
export MISE_YES=1
export NO_COLOR=1
export MISE_CONFIG_DIR="$BENCH_ROOT/mise-config"
export MISE_DATA_DIR="$BENCH_ROOT/mise-data"
export MISE_CACHE_DIR="$BENCH_ROOT/mise-cache"
export MISE_STATE_DIR="$BENCH_ROOT/mise-state"
export PITCHFORK_CONFIG_DIR="$BENCH_ROOT/pitchfork-config"
export PITCHFORK_STATE_DIR="$BENCH_ROOT/pf"
```

`BENCH_ROOT`, `APP_A`, and `APP_B` are runner inputs. Choose a short benchmark root, especially on macOS. Do not redefine `HOME` or connect to the user's default Pitchfork supervisor. Reject inherited endpoint variables, mise profiles/config overrides/aliases, and `PITCHFORK_CONFIG`; use a container or a clean temporary ancestor hierarchy to avoid parent project configuration. If system configuration exists, inventory and isolate it too. Directory overrides alone are not proof that no parent/system configuration was read. [Mise directory environment definitions](https://github.com/jdx/mise/blob/33a839a4d19acca617bc9ec0fe53f61266d60a9d/crates/mise-util/src/env.rs#L142-L161), [Pitchfork path definitions](https://github.com/jdx/pitchfork/blob/cfdea79f1d52b8449c0b99b29a03d9e771cd8ec3/src/env.rs#L258-L286).

The normal service-backed sequence is:

```sh
mise -C "$APP_A" trust "$APP_A/mise.toml"
mise -C "$APP_A" install --locked
mise -C "$APP_A" exec -- uv sync --frozen
mise -C "$APP_A" daemons start postgres redis
mise -C "$APP_A" daemons ls --json
mise -C "$APP_A" daemons status postgres --json
mise -C "$APP_A" daemons status redis --json
mise -C "$APP_A" run test
mise -C "$APP_A" exec -- true
mise -C "$APP_A" daemons start postgres redis
mise -C "$APP_A" daemons restart postgres redis
mise -C "$APP_A" daemons stop postgres redis
```

Measure tool install, application sync, first ready start, test run, repeated environment entry and repeated service-backed task separately. For the combined user workflow, also measure `mise run test` from a fresh prepared checkout; it can install missing tools and start services itself. When requiring locked behavior throughout, supply `MISE_LOCKED=1` after locks have been frozen, including task and daemon invocations.

`mise daemons ls --json` produces one JSON array with `id`, `root`, `pid`, `status`, `port`, `port_auto`, `data_dir` and `ownership`. `mise daemons status postgres --json` produces a Pitchfork status object. Without a selected name, mise calls `pitchfork status` once per selected service, so `mise daemons status --json` can concatenate JSON objects. Use one service per status receipt or parse a JSON stream. A successful status command can describe a stopped daemon; inspect fields. [Mise listing and status dispatch](https://github.com/jdx/mise/blob/33a839a4d19acca617bc9ec0fe53f61266d60a9d/src/cli/daemons.rs#L525-L632), [Pitchfork JSON schemas](https://github.com/jdx/pitchfork/blob/cfdea79f1d52b8449c0b99b29a03d9e771cd8ec3/src/cli/json_output.rs#L1-L77).

## Two independent checkouts

Use two independent `git clone` checkouts, including a useful variant where their final directory names are identical. With no native `pitchfork.toml` or explicit namespace, mise's default namespace includes a path hash, so identical basename alone does not merge daemon identities. Avoid setting one shared explicit namespace across independent clones. [Namespace derivation](https://github.com/jdx/mise/blob/33a839a4d19acca617bc9ec0fe53f61266d60a9d/src/daemons/runtime.rs#L1023-L1091).

`port = "auto"` retains the base port for independent clones. It is not a free-port search. A primary Git checkout uses slot zero; linked worktrees use a stable hash in 511 nonzero slots. Distinct worktrees can hash to the same slot. This is an implementation-supported distinction, not a new runtime result. [Port allocation implementation](https://github.com/jdx/mise/blob/33a839a4d19acca617bc9ec0fe53f61266d60a9d/src/daemons/ports.rs#L1-L24), [slot and persisted allocation code](https://github.com/jdx/mise/blob/33a839a4d19acca617bc9ec0fe53f61266d60a9d/src/daemons/ports.rs#L116-L192).

Provide two scored scenarios:

1. Identical config with default ports. Record whether the second start rejects the conflict, names the conflicting service, and leaves A intact. This measures diagnosis and safety, not a configured-isolation success.
2. Supported configured isolation. Put fixed ports in a local override before starting either checkout. Allow this per-checkout endpoint configuration for other tools as well. Count preparation effort and configuration separately from runtime latency.

For A, write the following `mise.local.toml` and review/trust it:

```toml
[daemons.postgres]
preset = "postgres"
version = "17.11"
port = 15432
data_dir = ".data/postgres"
options.database = "bench"

[daemons.redis]
preset = "redis"
version = "8.10.2"
port = 16379
data_dir = ".data/redis"
```

B uses the same declarations with ports `25432` and `26379`. These are illustrative reserved ports; the runner must check availability and reserve equivalent ports fairly before each scenario. The whole higher-precedence daemon declaration replaces the original, so repeat preset, version, data directory and options. Do not supply only `port`, and do not patch connection URLs separately. Trust both local files. Preserve this scenario's local configuration alongside its evidence, though a project ordinarily gitignores it.

Start A, write marker `A`, start B, write marker `B`, and require that each sees only its own marker. Stop A and repeat B's application queries and test suite. Compare real data directories and daemon IDs, not just two successful `pytest` runs. Distinct ports plus separate directories plus distinct data is the isolation receipt. Then restart A and confirm both services retain A's marker.

Linked Git worktrees deserve a separate scenario with the shared `port = "auto"` configuration and no local override. Create them with `git worktree add`; a plain directory copy or an arbitrary `.git` file is not equivalent.

## Readiness and exact identity checks

Pitchfork startup waits for its readiness signal. It supports bounded object-form probes; multiple probes mean any successful probe, not all. The presets' string probes have no overall readiness deadline. Apply a common runner timeout to all tools. If testing configured timeouts, use an explicit object-form probe on each competitor and disclose that extra configuration. [Pitchfork readiness implementation](https://github.com/jdx/pitchfork/blob/cfdea79f1d52b8449c0b99b29a03d9e771cd8ec3/src/supervisor/lifecycle.rs#L1363-L1588), [bounded command-probe implementation](https://github.com/jdx/pitchfork/blob/cfdea79f1d52b8449c0b99b29a03d9e771cd8ec3/src/supervisor/lifecycle.rs#L136-L209).

Do not treat `pg_isready` or an exit-zero Redis client as instance identity. A client can reach another server. Execute independent application-level checks inside `mise exec` after successful daemon startup:

```sh
mise -C "$APP_A" exec -- sh -eu -c '
  psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -Atc "SELECT 1"
  psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -Atc "SHOW data_directory"
  psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -Atc "SHOW port"
  psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -Atc "SHOW server_version"
  redis-cli -u "$REDIS_URL" --raw PING
  redis-cli -u "$REDIS_URL" --raw CONFIG GET dir
  redis-cli -u "$REDIS_URL" --raw INFO server
'
```

The verifier must require `SELECT 1` to return `1`, PING to return exactly `PONG`, PostgreSQL's canonical data path to equal `<A>/.data/postgres`, Redis's canonical `dir` to equal `<A>/.data/redis`, and server ports/PIDs to agree with service receipts. Compare PostgreSQL port to both `PGPORT` and the URL. Parse Redis URL and `INFO server` for `tcp_port`, `process_id`, `run_id` and `redis_version`. Canonicalize `/tmp` versus `/private/tmp` on macOS. Record Python `sys.executable` and `sys.version` to prove the application runtime came from the selected environment.

Use a shared Python verification routine with psycopg and redis clients from the frozen fixture to write/read a PostgreSQL marker table and Redis marker key. Pass expected checkout identity explicitly. Use explicit exceptions and nonzero exits, not Python `assert`, which disappears under `python -O`. Redis clients must compare response content instead of accepting command exit code alone. Tests should also read/write application data, so the benchmark covers actual dependency use.

Mise exports preset URLs while resolving configuration, even before startup. `mise exec -- pytest` therefore means environment entry, not service startup or instance verification. Service-dependent task timing should use `mise run test`; direct-exec timings should be labelled separately. Test stale URLs and foreign listeners as optional recovery/safety scenarios using only benchmark-owned processes. [Preset environment generation](https://github.com/jdx/mise/blob/33a839a4d19acca617bc9ec0fe53f61266d60a9d/src/daemons/presets.rs#L775-L863), [task requirement handling](https://github.com/jdx/mise/blob/33a839a4d19acca617bc9ec0fe53f61266d60a9d/src/daemons/tasks.rs#L108-L173).

## Lifecycle, persistence and recovery

1. Capture initial IDs/PIDs, DB directory, Redis directory/run ID, ports and application markers.
2. Run `daemons start postgres redis` again. Require successful application checks and no duplicate running instance. PID stability is useful evidence for reuse, but report it rather than assuming it.
3. Run `daemons stop postgres redis` twice. Require both listeners closed and owned non-zombie service processes gone. A remaining benchmark-owned Pitchfork supervisor is separate from a leaked database. Do not count all machine-wide postgres/Redis processes.
4. Start again. Require the same data directories and markers, with valid new running processes. Redis AOF provides persistence; use graceful stop in this scenario. Do not require the same Redis run ID after restart.
5. Stop A while B remains active. Verify B's exact directory, markers and application queries.
6. In a dedicated optional crash scenario, kill only this run's recorded supervisor, inspect remaining owned services, restart the pinned supervisor, and check adoption/status/stop. Never infer cleanup from a status exit code or a restarted supervisor alone.

Pitchfork autostarts its supervisor for start operations. Release code puts Unix service processes in a new session and stops their process groups with configured signals, then escalation. Adoption logic reconciles processes left after a supervisor crash and checks recorded process start time. These are source findings; the crash scenario above must verify the behavior on the selected platform. [Supervisor connection/autostart](https://github.com/jdx/pitchfork/blob/cfdea79f1d52b8449c0b99b29a03d9e771cd8ec3/src/ipc/client.rs#L62-L95), [session creation](https://github.com/jdx/pitchfork/blob/cfdea79f1d52b8449c0b99b29a03d9e771cd8ec3/src/supervisor/lifecycle.rs#L1201-L1219), [process-group stop](https://github.com/jdx/pitchfork/blob/cfdea79f1d52b8449c0b99b29a03d9e771cd8ec3/src/procs.rs#L402-L526), [adoption implementation](https://github.com/jdx/pitchfork/blob/cfdea79f1d52b8449c0b99b29a03d9e771cd8ec3/src/supervisor/adopt.rs#L169-L342).

For teardown, stop project daemons first, verify they stopped, then run `mise exec -- pitchfork supervisor stop` within the isolated benchmark state if the supervisor is still up. Inspect `mise exec -- pitchfork supervisor status --json` before and after, since an already-stopped supervisor can make some commands return a different status. Use a cleanup `finally` block even when tests fail. Preserve receipts before removing only owned temporary directories.

Stopping preserves preset data. First-time initialization is serialized under a data-directory lock and uses a staging directory; existing data gets a stored major-version compatibility check. Test interrupted initialization separately from a normal restart if desired. Default generated state can be pruned after project deletion, but custom `.data/` directories remain the runner's responsibility. Never benchmark a reset by deleting shared tool caches or data from another checkout. [Initialization implementation](https://github.com/jdx/mise/blob/33a839a4d19acca617bc9ec0fe53f61266d60a9d/src/daemons/presets.rs#L1333-L1416), [prune command contract](https://github.com/jdx/mise/blob/33a839a4d19acca617bc9ec0fe53f61266d60a9d/src/cli/daemons.rs#L88-L109).

## Reproduction, portability and reuse

Copy the committed application, `mise.toml`, `mise.lock`, application lock, referenced tool sidecars and any task scripts into a fresh checkout. A local endpoint override is machine setup and should travel as a separately recorded scenario input. Do not copy `.venv`, `.data`, generated daemon state or old installed tools when claiming a cold reproduction. Warm-cache reproduction may share the tool store, but must recreate application state and record that distinction. Require hashes of all copied inputs to match, effective backend/version identities to match, and application tests plus exact endpoint identity to pass. Never assume Linux/macOS artifact hashes must be equal.

Remote config `include` supports tools/env and related configuration, while daemon definitions and settings are rejected by the release parser. Task catalogs use `task_config.includes`. For this benchmark, copying a complete checked-in project config and lock is a supported reproducible workflow. Do not require a proprietary bundle shape for the basic reproduction task. A separate reusable-service-template task may measure what must be copied or generated, with full Git commit pins for remote fragments. `mise.lock` tool artifact locking does not imply remote template commits were locked. [Remote fragment allowlist](https://github.com/jdx/mise/blob/33a839a4d19acca617bc9ec0fe53f61266d60a9d/src/config/config_file/mise_toml.rs#L613-L648), [remote include implementation](https://github.com/jdx/mise/blob/33a839a4d19acca617bc9ec0fe53f61266d60a9d/src/config/remote_include.rs).

| Target | Supported recipe and constraints |
| --- | --- |
| macOS native, arm64/x64 | Native release executables and Conda packages; no system service registration required. Use short Pitchfork state paths; Unix sockets permit 104 address bytes. Verify packages on the actual target and canonicalize paths. |
| Linux native, arm64/x64 | Native release executables and Conda packages on a compatible libc/runtime. No systemd is required for explicit daemon commands. Unix socket address limit is 108 bytes. |
| Linux container | Use a regular user for PostgreSQL. Run with `--init` for the normal CLI benchmark, keep the container alive between commands, and use compatible Linux artifacts. Separate containers per checkout hide same-host port conflicts, so A/B isolation must share one container/network namespace. |
| Alpine/musl or reduced base image | Treat as a separately qualified target. Native database packages and required libraries may not match the image; a CLI binary existing is insufficient. Do not call a glibc image failure a universal Linux limitation. |
| Windows | Out of this POSIX recipe's scope. Pitchfork has Windows builds, but the Redis preset lacks a native Windows build; WSL is a different target. |

Pitchfork's experimental `supervisor run --container --boot` can act as PID 1, reap children and handle container shutdown. That is an alternate deployment-style recipe, not required for `docker exec` development tests. If used, configure `boot_start`, record the different topology, and verify child failures because a daemon exit does not necessarily terminate the container. [Container-mode release guide](https://github.com/jdx/pitchfork/blob/cfdea79f1d52b8449c0b99b29a03d9e771cd8ec3/docs/guides/container-mode.md), [socket/path contract](https://github.com/jdx/pitchfork/blob/cfdea79f1d52b8449c0b99b29a03d9e771cd8ec3/docs/reference/file-locations.md), [Conda platform selection](https://github.com/jdx/mise/blob/33a839a4d19acca617bc9ec0fe53f61266d60a9d/docs/dev-tools/backends/conda.md).

## Recommended comparable tasks

| Task | Exact acceptance evidence | Timing boundary |
| --- | --- | --- |
| Fresh developer setup | Tool/backend versions match frozen config/locks; application dependencies match `uv.lock`; native service identity checks and tests pass | Tool install, dependency sync and first ready start separately, plus combined time |
| Return to existing checkout | New shell runs one real application command against the correct existing data | Bare `mise exec` and service-backed `mise run` separately |
| Run tests repeatedly | Every run passes semantic DB/cache checks; no dependency/lock changes | Warm dependency state, persistent running services |
| Two independent clones | Different IDs, ports, data directories and markers; stopping A leaves B correct | Record endpoint configuration effort separately; run both on one host |
| Linked worktree development | Shared config works with resolved ports exported to its application; identity is worktree-local | First worktree setup and warm commands |
| Stop and return tomorrow | No owned live service processes after stop; markers survive restart | Graceful stop and restart-to-usable-data separately |
| Copy config and locks | Fresh store/checkout installs recorded artifacts and dependencies without rewriting locks | Separate cold and warm-cache reproduction |
| Optional failure recovery | Clear nonzero result for occupied ports/bad pins; no accidental foreign data writes; independent checkout remains intact | Recovery elapsed time and manual/configuration steps, not folded into normal latency |

Do not score Stack-specific leases, bundle conflict metadata or owner-PID session semantics as mandatory basic developer workflow features. They can be optional capability tasks with explicit user value. Give competitors their supported supervisors and dependency managers. Keep install caches, artifact downloads, host process state, platform differences and output noise visible; avoid a single universal winner claim.

## Common harness mistakes

- Comparing current Stack to the old handwritten `eval/configs/mise` service script while omitting native mise daemons.
- Reporting mise 2026.10.3 while running the 2026.9.18 image binary. Capture executable path, version, SHA-256 and image ID; record the Pitchfork binary separately.
- Leaving `python = "3.13"`, `uv = "latest"` or unlocked dependencies while calling the setup reproducible.
- Assuming `port = "auto"` searches for a free port or isolates independent clones. It offsets linked worktrees only.
- Reusing one explicit namespace or copying a native `pitchfork.toml` with basename-derived identity into both clones.
- Trusting only `mise.toml` when a `mise.local.toml` override is also used.
- Overriding only a daemon's port in the local table; the whole declaration is replaced.
- Expanding `$DATABASE_URL` in the outer host shell rather than inside the entered environment, or leaking `PGHOSTADDR`/libpq overrides into the test.
- Calling `mise exec -- pytest` after startup failed, then accepting tests that reached another server.
- Parsing multiple `mise daemons status --json` objects as one JSON object, or treating exit zero as status `running`.
- Counting successful readiness calls without checking result contents, data directories and marker values.
- Losing a failure through `tail`, `|| true`, an unchecked subprocess result, or Python `assert`.
- Counting a retained supervisor or zombies as live database leaks, or ignoring real surviving child processes after a supervisor crash.
- Running clones in different containers, which removes the same-host port/isolation problem being measured.
- Requiring boot registration, shell hooks, a web UI or a proxy for explicit noninteractive service commands. They are optional adjacent workflows.
- Mixing startup, dependency resolution, tests and Docker transport into an unlabeled overhead number, or presenting old eval timings as a fresh campaign.
