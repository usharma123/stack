# isola research for the application benchmark

Research date: 2026-10-06. Implementation owner: Opus 5.5. isola is a direct local worktree and service competitor. This note contains source inspection and install/help evidence, with no service execution or timing results.

## Version and installation

The latest official release observed was [v0.4.1](https://github.com/cyucelen/isola/releases/tag/v0.4.1), published 2026-09-09, source SHA `af852ae57c6d107e09daaacf8d744cc09e18fbd0`. Main resolved to `1a049b53dbeaeddcf80e4c44fdc4f29066e96646` in `/tmp/stack-bench-sources/isola`. Its only difference from v0.4.1 is one terminal regression-test line. The runtime implementation reviewed below is identical at both SHAs; use the release for measurement.

A private release download was checksum-verified and executed only for `version`, root help, and `accessory ls --help`:

| Artifact | Observed identity |
| --- | --- |
| macOS ARM64 archive | `isola_0.4.1_darwin_arm64.tar.gz`, SHA256 `ac269881269fd92d75a12a66a4671f4f8d12dd8ff30fe0268962224dc4ebb132` |
| Extracted binary | `/tmp/stack-bench-isola-tools/isola`, SHA256 `7dc7ea2c172e6ea854cda6c251bb40f310af807d2667d013eeee7e605af8a687` |
| Version output | `isola 0.4.1`, commit `af852ae57c6d107e09daaacf8d744cc09e18fbd0`, built `2026-09-09T09:06:03Z` |

No isola binary or redis-server was on this host's PATH before that download. Go, PostgreSQL and uv were present; their presence does not prove usable benchmark service versions. There is no existing isola benchmark image to report as tested. The proposed TOML parsed and both shell blocks passed `sh -n`; those checks do not establish service or integration behavior.

[Official installation guidance](https://github.com/cyucelen/isola/blob/af852ae57c6d107e09daaacf8d744cc09e18fbd0/skills/isola-init/references/install.md) offers Homebrew, Linux packages, release archives and Go installation. [Release configuration](https://github.com/cyucelen/isola/blob/af852ae57c6d107e09daaacf8d744cc09e18fbd0/.goreleaser.yml#L8) builds CGO-free Linux/macOS AMD64 and ARM64 binaries, with Windows excluded. The README calls Windows experimental, but the process implementation uses Unix process groups and flock. Use macOS or Linux for this study. A Linux container works as a run-owned test host with writable Git worktrees and reachable shared dependencies; Docker is an optional transport, not an isola dependency. Source builds declare Go `1.25.6` in [go.mod](https://github.com/cyucelen/isola/blob/af852ae57c6d107e09daaacf8d744cc09e18fbd0/go.mod#L1).

## Native behavior and benchmark boundary

| Requirement | Native isola behavior | Supplied by the benchmark/project |
| --- | --- | --- |
| Python/dependencies | Executes setup commands with resolved service environment | Installed pinned Python 3.13 and uv; fixture pyproject/uv.lock; `uv sync --frozen` |
| Worktree isolation | Separate service processes, ports, env files and accessory resources | Git checkout creation and distinct branch names; unique project/database prefix per run |
| PostgreSQL | Clones a complete database from a template on one existing cluster | Install/start/configure the cluster and prepare the template |
| Redis | Allocates one logical DB per worktree on one existing Redis server | Install/start/persistence settings and matching logical-DB count |
| Service lifecycle | `up`, `down`, `destroy`, logs, PID state; repeated live-service start is a no-op | Application readiness probes; shared dependency-server lifecycle |
| Repeated commands | No native `exec`, `run`, activation, package install or task command | Load its generated env file through `uv run --env-file`; execute fixture CLI |
| Reproduction | Version-controlled `.isola.toml`; per-service setup and optional ignored-file copy | Runtime versions, uv.lock, bootstrap script, template contents/dump and checksum |

The [PostgreSQL driver](https://github.com/cyucelen/isola/blob/af852ae57c6d107e09daaacf8d744cc09e18fbd0/internal/accessory/postgres/postgres.go#L147) does `CREATE DATABASE ... TEMPLATE ...`; these are separate databases, not separate schemas or clusters. It reuses an already-existing database with the resolved name. It terminates connections to the template before cloning, and reset/drop use `DROP DATABASE ... WITH (FORCE)`, requiring PostgreSQL 13+. It protects the template and maintenance database names and fits long database names to PostgreSQL's 63-byte limit using a hash. Use a dedicated quiescent template and a fresh run-owned namespace. Read the provisioned name instead of reconstructing it from a long branch.

The [Redis driver](https://github.com/cyucelen/isola/blob/af852ae57c6d107e09daaacf8d744cc09e18fbd0/internal/accessory/redis/redis.go#L105) claims logical DBs with `SET NX __isola_owner__ <project>:<slug>`, starting at an owner hash and probing other slots. It reuses the same owner's slot, defaults to 16 slots, and reset/drop use `FLUSHDB`. Drop checks the saved owner before flushing. Redis Cluster cannot support this strategy. An unowned slot may contain unrelated keys, so use a dedicated fresh Redis server, not the user's existing cache. Database namespaces provide ordinary application data separation; clients still share server availability, configuration, privileges and resource limits.

[Process startup](https://github.com/cyucelen/isola/blob/af852ae57c6d107e09daaacf8d744cc09e18fbd0/internal/process/manager.go#L101) provisions accessories before service setup, writes resolved env, runs setup synchronously, then starts `sh -c` in a process group. Its 400 ms startup grace detects immediate exit, not database/cache/application readiness. Services run in sorted order, with no dependency graph or native healthcheck. A service referencing a failed accessory is blocked. [Repeated up](https://github.com/cyucelen/isola/blob/af852ae57c6d107e09daaacf8d744cc09e18fbd0/internal/process/manager.go#L175) skips a live service and its service setup, while top-level setup still runs each time. There is no always-on service supervisor required. The optional machine-wide proxy starts on `up` and survives ordinary `down`; disable it for the CLI fixture so cleanup remains run-scoped.

## Concrete recipe for the shared CLI fixture

This is implementation input, not an executed integration result. Use `bench/fixtures/app` unchanged, its Python 3.13 constraint and uv.lock. Prepare a run-owned Git repository containing the fixture and recipe, with branch `a` and a linked worktree on branch `b`. Use one exact Python 3.13 patch version and uv version across adapters. Export `RWB_PYTHON` as that installed interpreter's absolute path and put the pinned uv/isola binaries on PATH in every fresh shell. isola itself does not resolve those tools.

The native accessory workflow needs existing servers. These commands illustrate the benchmark-owned bootstrap, after pinned PostgreSQL and Redis binaries have been installed. Choose confirmed-free ports and an empty directory; the benchmark should retain their versions, hashes and ownership receipts. `RWB_SHARED` must be an absolute run-owned directory. Run PostgreSQL as an unprivileged user, including in Linux containers. Never use these commands against an existing user cluster/cache.

```sh
set -eu
mkdir -p "$RWB_SHARED/redis"
initdb -D "$RWB_SHARED/pg" -U bench -A trust --no-locale --encoding=UTF8
pg_ctl -D "$RWB_SHARED/pg" -l "$RWB_SHARED/pg.log" \
  -o '-h 127.0.0.1 -p 25432' -w start
createdb -h 127.0.0.1 -p 25432 -U bench rwb_template
redis-server --bind 127.0.0.1 --port 26379 --databases 16 \
  --dir "$RWB_SHARED/redis" --appendonly yes --appendfsync everysec \
  --save '' --daemonize yes --pidfile "$RWB_SHARED/redis.pid" \
  --logfile "$RWB_SHARED/redis.log"
redis-cli -h 127.0.0.1 -p 26379 ping
```

An empty `rwb_template` keeps each checkout's migration workload comparable. For a separate seeded-template scenario, migrate/seed the template once, close all connections, export its dump and hash, and compare equivalent prepared-state startup for every tool. Do not silently remove migration work only from isola's first-ready measurement.

`.isola.toml`, with a unique prefix substituted by the implementer for each run:

```toml
project = "rwb-isola-20261006-01"
copy_files = []

[proxy]
enabled = false

[env_file]
enabled = true
create = true
path = ".env.isola"

[services.fixture]
# The common app is a CLI. This keeper only satisfies isola's service model.
# Replace it with the app's actual worker/web command when testing that scenario.
command = "exec .venv/bin/python -c 'import signal; signal.pause()'"
setup = 'uv sync --frozen --python "$RWB_PYTHON" && uv run --frozen --no-sync python -m rwbapp migrate'

[services.fixture.env]
DATABASE_URL = "${accessories.database.url}"
REDIS_URL = "${accessories.cache.url}"
RWB_CHECKOUT = "${ISOLA_BRANCH_SLUG}"
UV_PYTHON_DOWNLOADS = "never"

[accessories.database]
kind = "postgres"
server_url = "postgres://bench@127.0.0.1:25432/postgres"
clone_from = "rwb_template"
name = "rwb_isola_20261006_01_${ISOLA_BRANCH_SLUG}"

[accessories.cache]
kind = "redis"
server_url = "redis://127.0.0.1:26379"
databases = 16
```

Ignore `.venv/`, `.env.isola` and `.isola/` in the fixture repository. Runtime state lives under the main worktree's `.isola/`, shared by its linked worktrees. The [configuration validator](https://github.com/cyucelen/isola/blob/af852ae57c6d107e09daaacf8d744cc09e18fbd0/internal/config/config.go#L284) requires at least one service. The keeper above is explicit adapter overhead, not proof that the application is a running server. Do not compare its process startup to a competitor's real HTTP application startup. Native server supervision can be tested separately using the same actual worker/web application across tools.

For checkout A, each `uv run` is valid in a fresh noninteractive shell once PATH and the working directory are supplied. The env file is generated by isola; there is no hardcoded per-checkout endpoint in the application command.

```sh
cd "$RWB_A"
isola up
isola ls --json
isola accessory ls --json
uv run --env-file .env.isola --frozen --no-sync python -m rwbapp wait --timeout 60
uv run --env-file .env.isola --frozen --no-sync python -m rwbapp mark --checkout a
uv run --env-file .env.isola --frozen --no-sync python -m rwbapp crud --checkout a
uv run --env-file .env.isola --frozen --no-sync python -m rwbapp cache --checkout a
uv run --env-file .env.isola --frozen --no-sync pytest -q
```

Run the same sequence in B with `--checkout b`; then run `check --checkout a --forbid b` in A and its reciprocal in B. pytest uses the generated `RWB_CHECKOUT`, so it does not add a third checkout marker. Repeat `uv run --env-file .env.isola --frozen --no-sync true` and the app's `read` command for warm entry/work measurements. Label this as uv plus isola-generated environment, not native isola command entry.

## Checks, persistence and teardown

- Require app JSON `ok: true`, migrations `0001` and `0002`, CRUD steps `create/read/update/list/delete`, and cache sequence `miss/hit/miss/hit`. Preserve command exit status and JSON together.
- `isola accessory ls --json` emits `worktree`, `accessory`, `kind`, `provisioned`, and `resource`. Require PostgreSQL `resource.database` to equal the app's `current_database()` and Redis `resource.db` to equal the DB index parsed from REDIS_URL; require Redis owner markers `rwb-isola-20261006-01:a` and `...:b` in the respective logical DBs.
- Both checkouts should normally have the same PostgreSQL system identifier/data directory/port and the same Redis run_id/port. Require different PostgreSQL database names and Redis DB indices, plus reciprocal `check` receipts. The app's unprefixed `rwb:checkout` key is a useful collision probe. Do not accept different URLs alone as isolation proof.
- `isola down` stops the managed fixture process and retains both accessories. Call `persist --checkout a`, capture identity and keeper PID, run `isola down` then `isola up`, and require a new keeper PID plus `persisted --checkout a` with PostgreSQL and Redis values retained. Shared server identities should remain unchanged. This is application-process stop/start and data retention, not dependency-server restart.
- Native accessory reset is `isola accessory reset database` and `isola accessory reset cache`. Then migrate/mark again and confirm B's schema/markers/cache survived. Ordinary reset returns PostgreSQL to the template and Redis to empty plus its owner marker. Do not confuse reset with persistence.
- For a dependency-server restart task, explicitly stop/start the benchmark-owned shared servers and run `persisted` in A and B. It is user-managed, affects both worktrees and cannot demonstrate independent server availability. Report per-checkout dependency-server restart as unsupported by native accessories. Arbitrary PostgreSQL/Redis service scripts are possible, but would be a separate user-scripted variant.
- `isola destroy` stops the current worktree's process and drops its saved accessory resources. Capture database name and Redis DB index first; verify the database is absent and that Redis DB has no keys, including no owner marker. Confirm B still passes its isolation/read checks. [Destroy implementation](https://github.com/cyucelen/isola/blob/af852ae57c6d107e09daaacf8d744cc09e18fbd0/cmd/destroy.go#L14).
- Deleting a linked worktree does not synchronously run cleanup by itself. `isola down --prune` or a later `up` invokes [orphan reconciliation](https://github.com/cyucelen/isola/blob/af852ae57c6d107e09daaacf8d744cc09e18fbd0/internal/process/reconcile.go#L33); the optional proxy also reconciles periodically. `down --prune` is a separate prune operation, not ordinary down plus prune. Verify actual resource removal rather than the printed prune summary.
- Finish with destroy for both worktrees, then stop only the owned shared servers using `pg_ctl -D "$RWB_SHARED/pg" -m fast -w stop` and `redis-cli -h 127.0.0.1 -p 26379 shutdown save`. Keep ownership and failure evidence before removing run-owned data. Do not use machine-wide `isola proxy stop` or broad process cleanup.

## Fair tasks and common mistakes

Report two startup boundaries: full machine-to-ready includes pinned tools, shared-server bootstrap, template creation, accessory clone and dependency installation; additional-worktree readiness starts with those shared prerequisites ready. This is a real native advantage to measure, but one warmed shared server cannot be hidden inside an otherwise cold comparison. Measure repeated commands after one successful `up`, since repeated `up` still contacts the accessory servers and checks state.

The existing eval recipes are historical Python/PostgreSQL/Redis task context; there was no isola measurement to inherit. Current `bench/rwb/verify.py::distinct_instances` and `restart_changes` require separate clusters/processes and dependency-server restarts. They must not reject native isola data isolation or claim its ordinary down restarted shared servers. Keep data-isolation and availability-isolation results distinct. Missing shared-server bootstrap/version locking is user-supplied setup, not a discovered native package-install feature.

Failure scenarios should match what isola owns: an unreachable accessory server must prevent the referencing fixture service from starting; missing Python/failed setup must fail `up`; exhaustion of Redis logical DBs should return an allocation error; occupied app ports apply only to a real port-bearing service. Occupying 25432 or 26379 is a shared-provider bootstrap failure, not an isola auto-port-allocation test. Observe functional results before assigning success.

Do not source arbitrary generated dotenv as shell code. uv's `--env-file` reads the generated values. Env-file writes are best-effort warnings in [manager.go](https://github.com/cyucelen/isola/blob/af852ae57c6d107e09daaacf8d744cc09e18fbd0/internal/process/manager.go#L483), so require a freshly generated file and valid matching resource receipts. Separate clones that use the same `project` and branch slug share Redis ownership; a database name lacking a unique project/run prefix also collides across clones. Linked worktrees on distinct branches are the native primary scenario; use unique projects/prefixes for a separate-clone scenario.
