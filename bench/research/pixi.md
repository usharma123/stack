# Pixi research and runnable benchmark recipe

Researched 2026-10-06. Pixi is a Conda/PyPI environment manager and task runner. Use its own dependency resolver for the primary application row. PostgreSQL and Redis can come from the same locked Conda environment, but service launch, readiness, endpoint ownership, persistence policy and shutdown below are user-authored code. Score that composed workflow separately from Pixi's native capabilities.

## Evidence and version boundary

| Evidence | Value | Status |
| --- | --- | --- |
| Official latest release | [v0.81.0](https://github.com/prefix-dev/pixi/releases/tag/v0.81.0) | Verified from the official release redirect on 2026-10-06 |
| Release source commit | `abd7c0d32ebaf208939516e95e2776bf5ce2a6c9` | Resolved annotated tag to its commit and read implementation locally |
| Current main source | `0e01a05d12a7921b913d0724b482fb2efcf55be9` | Shallow clone, committed 2026-10-06; release source is the reference for the recipe |
| Fresh macOS ARM64 CLI | `pixi 0.81.0` | Downloaded official release tarball into `/tmp/stack-bench-sources/pixi-cli`; executed version and help commands only |
| macOS CLI SHA-256 | `f389557fc5de929cc33a042f6ae5abbaeb087ffa1affd55aa6ab8da3222d1122` | Locally measured binary hash, not an independently verified release checksum |
| Existing Linux ARM64 CLI | `eval/results/latest-2026-10-05/bin/linux/pixi`, ELF AArch64 | File inspected; historical `pixi/version.stdout` reports `pixi 0.81.0` |
| Existing Linux CLI SHA-256 | `7fcd20f7c18339f13552a6767ab146ce6b439ce29db46cc03ece005ca9696bb1` | Locally measured on the existing artifact; not executed in this research |
| Local images | `ev-base:latest` exists; no dedicated Pixi image in the inspected image listing | Read-only `docker image ls`; Pixi is supplied separately by the old runner |

No dependency solve/install, application integration test, service startup, cleanup or timing measurement ran during this research. The manifest received a CLI parse/task-list check; Python and shell recipe blocks received static syntax checks. That verifies syntax, not package availability or lifecycle behavior. Implementation conclusions below are source-derived. Opus 5.5 owns the actual benchmark implementation and runtime validation.

Historical context only: `eval/harness/pixi-benchmark.py` creates two projects, installs Pixi's environment, then runs `uv sync` and `uv run pytest`. Its manifest requests Python `3.13.*`, PostgreSQL `17.*` and Redis `8.*`; old receipts report Python `3.13.15`, uv `0.12.21`, PostgreSQL `17.11` and Redis `8.10.2`. Those are recorded old versions, not a current package-resolution claim. The runner does not supply a committed `uv.lock`, uses broad requirements, disables Redis persistence, and records only PostgreSQL data directories for endpoint identity. Do not copy its timing numbers or call that lifecycle recipe a native Pixi supervisor.

## What is native

| Requirement | Pixi support and fair interpretation |
| --- | --- |
| Python, native libraries and service binaries | Conda resolution and installation, with platform-specific selections in `pixi.lock` |
| Python application dependencies | PyPI dependencies from `[project].dependencies` in `pyproject.toml`, or `[pypi-dependencies]` in `pixi.toml`; no separate uv environment is necessary |
| Tasks | Named commands, dependency graph, per-task environment/cwd and argument handling |
| Repeated commands | Fresh `pixi run` invocation or shell-hook activation; ordinary run validates/installs as required; `--as-is` skips lock updates and installation |
| File-producing task caching | Optional inputs/outputs hashing; useful for builds, unsuitable for caching a live database integration test |
| Separate checkout dependency prefixes | Defaults to each checkout's `.pixi/envs/<environment>`; detached environments use a project path hash |
| PostgreSQL/Redis startup and shutdown | Supplied by PostgreSQL/Redis executables plus project scripts |
| Readiness and endpoint identity | Supplied by project probes; task dependencies can order finite setup/probe commands |
| Distinct ports/data and persistence | Supplied by checkout-local session configuration and service flags |
| Reproducible sharing | Share manifest, `pixi.lock`, application code and referenced scripts; installed prefixes and database files are separate artifacts |

The CLI command enum contains environment, package, shell and task commands, plus extension dispatch. It has no built-in service lifecycle command. The run loop executes a task graph, activates its environment and returns its exit status. A foreground server task blocks its dependent tasks; a daemonized service started by a project script has lifecycle behavior defined by that script. This is a code inference from [the CLI enum](https://github.com/prefix-dev/pixi/blob/abd7c0d32ebaf208939516e95e2776bf5ce2a6c9/crates/pixi_cli/src/lib.rs#L170-L211) and [the run loop](https://github.com/prefix-dev/pixi/blob/abd7c0d32ebaf208939516e95e2776bf5ce2a6c9/crates/pixi_cli/src/run.rs#L456-L684), consistent with the documented [task model](https://pixi.prefix.dev/latest/workspace/advanced_tasks/).

## Primary fixture

Use a real Python application with a small write/read integration test. The existing `eval/fixture` is a starting point, but its `select 1` and Redis `PING` tests alone cannot establish data isolation or persistence. Extend the common application fixture for every competitor to insert and read an application record and a cache entry, with an owner marker supplied separately by the runner.

For a project that already has `pyproject.toml`, add Pixi tables to that file. This avoids duplicating the application's PyPI dependencies and avoids timing an extra uv sync. Pixi translates `[project].dependencies` into its default PyPI feature and allows `[tool.pixi.dependencies].python` to override the broad `requires-python`. Source: [pyproject interpretation](https://github.com/prefix-dev/pixi/blob/abd7c0d32ebaf208939516e95e2776bf5ce2a6c9/crates/pixi_manifest/src/pyproject.rs#L207-L260), [official Python manifest guide](https://pixi.prefix.dev/latest/python/pyproject_toml/).

Example complete manifest, using the old fixture's application requirements:

```toml
[project]
name = "fixture"
version = "0.1.0"
requires-python = ">=3.12"
dependencies = ["psycopg[binary]>=3.2", "redis>=5", "pytest>=8"]

[tool.pixi.workspace]
channels = ["conda-forge"]
platforms = ["linux-64", "linux-aarch64", "osx-arm64", "osx-64"]
requires-pixi = "==0.81.0"

[tool.pixi.dependencies]
python = "3.13.*"
postgresql = "17.*"
redis-server = "8.*"

[tool.pixi.tasks]
up = "python scripts/services.py up"
probe = "python scripts/services.py probe"
seed = "python scripts/services.py seed"
identity = "python scripts/services.py identity"
down = "python scripts/services.py down"
test = "python scripts/services.py test"
noop = "python -c 'pass'"

[tool.pixi.tasks.integration]
depends-on = ["probe", "test"]
```

The `3.13.*` manifest constraint chooses a minor line. The checked-in `pixi.lock` pins the actual Python patch, Conda build, transitive packages, and PyPI selections. Generate that lock once before any measured replay, record resolved versions, and reuse identical bytes across checkouts. If the experiment requires a particular Python patch or server version, replace those constraints with the selected exact versions before producing the lock. Do not resolve independently for A and B. A four-platform lock requests four solves; use a single-platform manifest for a single-platform latency row, and report the multi-platform lock as a distinct portability task. Lock behavior: [release lock command](https://github.com/prefix-dev/pixi/blob/abd7c0d32ebaf208939516e95e2776bf5ce2a6c9/crates/pixi_cli/src/lock.rs#L1-L126), [official lock documentation](https://pixi.prefix.dev/latest/workspace/lock_file/).

For an importable application, retain its existing build metadata and add its own editable PyPI dependency, for example `my-app = { path = ".", editable = true }` under `[tool.pixi.pypi-dependencies]`. The current test-only fixture needs no editable self-package. An alternate Pixi-plus-uv row is valid if the application already standardizes on uv, but it must include `uv.lock`, `uv sync --frozen`, the selected interpreter and `.venv` identity. Label it as a two-manager workflow.

## Checkout-local service recipe

Save the following as `scripts/services.py` in the prepared application. This is authored benchmark glue, not Pixi implementation. It uses subprocess argument arrays, so paths containing spaces do not require nested shell quoting. It runs on Unix with Python clients installed by Pixi. Ports and owner markers come from an untracked `.bench-session.json`; they are never baked into the portable lock.

```python
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import time

import psycopg
import redis

ROOT = Path(__file__).resolve().parents[1]
SESSION = json.loads((ROOT / ".bench-session.json").read_text())
STATE = ROOT / ".bench-state"
PGDATA = STATE / "postgres"
RDIR = STATE / "redis"
RPID = STATE / "redis.pid"
PGPORT = int(SESSION["pgport"])
RPORT = int(SESSION["redisport"])
OWNER = SESSION["owner"]
DBURL = f"postgresql://postgres@127.0.0.1:{PGPORT}/postgres"
RURL = f"redis://127.0.0.1:{RPORT}/0"
RC = redis.Redis.from_url(RURL, decode_responses=True,
                         socket_connect_timeout=1, socket_timeout=1)

def require(ok, message):
    if not ok:
        raise RuntimeError(message)

def command(*args):
    subprocess.run(list(map(str, args)), check=True, timeout=60,
                   stdin=subprocess.DEVNULL)

def identity(check_markers=False):
    with psycopg.connect(DBURL, connect_timeout=1) as conn:
        data = conn.execute("SHOW data_directory").fetchone()[0]
        port = conn.execute("SELECT inet_server_port()").fetchone()[0]
        cluster = str(conn.execute(
            "SELECT system_identifier FROM pg_control_system()"
        ).fetchone()[0])
        require(Path(data).resolve() == PGDATA.resolve(), "wrong PostgreSQL data directory")
        require(port == PGPORT, "wrong PostgreSQL port")
        if check_markers:
            rows = conn.execute("SELECT owner FROM bench_identity").fetchall()
            require(rows == [(OWNER,)], "wrong PostgreSQL marker")
    require(RC.ping(), "Redis did not answer PING")
    directory = RC.config_get("dir")["dir"]
    info = RC.info("server")
    require(Path(directory).resolve() == RDIR.resolve(), "wrong Redis directory")
    require(int(info["tcp_port"]) == RPORT, "wrong Redis port")
    require(RPID.exists(), "missing checkout-local Redis PID receipt")
    require(int(RPID.read_text().strip()) == int(info["process_id"]), "wrong Redis PID")
    require(RC.config_get("appendonly")["appendonly"] == "yes", "Redis AOF is disabled")
    if check_markers:
        require(RC.get("bench:owner") == OWNER, "wrong Redis marker")
    return {"owner": OWNER, "root": str(ROOT), "python": sys.executable,
            "pgdata": data, "pgport": port, "pg_cluster_id": cluster,
            "redis_dir": directory, "redis_port": info["tcp_port"],
            "redis_pid": info["process_id"], "redis_run_id": info["run_id"]}

def wait_ready():
    deadline = time.monotonic() + 30
    last = None
    while time.monotonic() < deadline:
        try:
            return identity()
        except (OSError, psycopg.Error, redis.RedisError, RuntimeError) as error:
            last = error
            time.sleep(0.1)
    raise RuntimeError(f"service readiness timed out: {last}")

def ports_closed():
    for port in [PGPORT, RPORT]:
        with socket.socket() as client:
            client.settimeout(0.2)
            if client.connect_ex(("127.0.0.1", port)) == 0:
                return False
    return True

action = sys.argv[1]
if action == "up":
    require(os.geteuid() != 0, "initdb requires a non-root user")
    # Preflight is a collision check, not an atomic port allocator.
    for port in [PGPORT, RPORT]:
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", port))
    STATE.mkdir(exist_ok=True)
    RDIR.mkdir(exist_ok=True)
    if not (PGDATA / "PG_VERSION").exists():
        command("initdb", "-D", PGDATA, "-U", "postgres", "--auth=trust",
                "--encoding=UTF8", "--locale=C")
    # Only benchmark-owned cluster files are modified.
    (PGDATA / "postgresql.auto.conf").write_text(
        "listen_addresses = '127.0.0.1'\n"
        "unix_socket_directories = ''\n"
        f"port = {PGPORT}\n")
    command("pg_ctl", "-D", PGDATA, "-l", STATE / "postgres.log",
            "-w", "-t", "30", "start")
    command("redis-server", "--bind", "127.0.0.1", "--port", RPORT,
            "--protected-mode", "yes", "--daemonize", "yes",
            "--dir", RDIR, "--save", "", "--appendonly", "yes",
            "--appendfsync", "always", "--pidfile", RPID,
            "--logfile", STATE / "redis.log")
    print(json.dumps(wait_ready(), sort_keys=True))
elif action == "seed":
    identity()
    with psycopg.connect(DBURL) as conn:
        conn.execute("CREATE TABLE IF NOT EXISTS bench_identity (owner text NOT NULL)")
        conn.execute("DELETE FROM bench_identity")
        conn.execute("INSERT INTO bench_identity VALUES (%s)", [OWNER])
    RC.set("bench:owner", OWNER)
    print(json.dumps(identity(True), sort_keys=True))
elif action in ["probe", "identity"]:
    print(json.dumps(identity(True), sort_keys=True))
elif action == "test":
    identity(True)
    env = dict(os.environ, DATABASE_URL=DBURL, REDIS_URL=RURL,
               BENCH_OWNER=OWNER)
    result = subprocess.run([sys.executable, "-m", "pytest", "-q"],
                            cwd=ROOT, env=env, stdin=subprocess.DEVNULL,
                            timeout=120)
    sys.exit(result.returncode)
elif action == "down":
    # Refuse to send shutdown to endpoints without the expected identity.
    identity(True)
    command("pg_ctl", "-D", PGDATA, "-m", "fast", "-w", "-t", "30", "stop")
    RC.shutdown()
    deadline = time.monotonic() + 30
    while not ports_closed() and time.monotonic() < deadline:
        time.sleep(0.1)
    require(ports_closed(), "service endpoints survived shutdown")
    print(json.dumps({"owner": OWNER, "ports_closed": True}))
else:
    raise RuntimeError(f"unknown action: {action}")
```

The sample uses local trust authentication and loopback listeners for an isolated development fixture. It has deliberately explicit persistence: PostgreSQL retains its data directory, and Redis enables AOF with `appendfsync always`. Apply the same Redis durability policy in every competitor row. A cache-only row can disable persistence for every tool instead; do not mix those policies in the same startup or test timing comparison.

The `up` action is for a stopped checkout. It fails if either port is in use. A second `up` while running should therefore be tested and reported as the scripted workflow's behavior, not Pixi's behavior. Concurrent start coordination, partial-start rollback and recovery after one service crashes belong to the benchmark runner. For cleanup after partial startup, stop only independently verified processes owned by this checkout: `pg_ctl -D <its PGDATA> ... stop` and Redis shutdown after checking its directory, port and PID receipt. Do not fall back to killing a global process name or shutting down a conventional port. Preserve cleanup failures alongside the primary failure.

## Noninteractive execution

The commands below assume a prepared, disposable application template at `$PIXI_TEMPLATE`, containing `pyproject.toml`, `scripts/services.py`, and application tests. Use an absolute release CLI at `$PIXI_BIN`. The runner creates and owns `$PIXI_RUN_ROOT`; the two directories must be absent. A dedicated Docker container network namespace is the simplest place for these example ports. For a host row, assign four distinct available ports before writing session files. This is runner configuration, with the same allocation policy for every tool.

One-time preparation, outside replay timing:

```bash
set -euo pipefail
"$PIXI_BIN" lock --no-config --manifest-path "$PIXI_TEMPLATE/pyproject.toml"
test -s "$PIXI_TEMPLATE/pixi.lock"
```

Copy source and lock, create independent machine-local sessions, then install without modifying either lock:

```bash
set -euo pipefail
mkdir "$PIXI_RUN_ROOT"
cp -R "$PIXI_TEMPLATE" "$PIXI_RUN_ROOT/appA"
cp -R "$PIXI_TEMPLATE" "$PIXI_RUN_ROOT/appB"
python3 - "$PIXI_RUN_ROOT" <<'PY'
import json
from pathlib import Path
import sys
import uuid
root = Path(sys.argv[1])
for name, pgport, redisport in [("appA", 45432, 46379), ("appB", 45433, 46380)]:
    session = {"owner": uuid.uuid4().hex, "pgport": pgport, "redisport": redisport}
    (root / name / ".bench-session.json").write_text(json.dumps(session) + "\n")
PY
for app in appA appB; do
  "$PIXI_BIN" install --no-config --locked \
    --manifest-path "$PIXI_RUN_ROOT/$app/pyproject.toml"
done
cmp "$PIXI_RUN_ROOT/appA/pixi.lock" "$PIXI_RUN_ROOT/appB/pixi.lock"
```

Do not copy `.pixi`, `.venv`, `.bench-state`, `.bench-session.json`, activation caches or task output caches into the template. The `cp -R` example assumes that exclusion already holds. For Git checkouts, use a clean commit containing the template and lock, then write sessions after cloning.

Commands do not depend on an interactive shell or ambient `DATABASE_URL`. The Python wrapper derives endpoints from each checkout's own session file and passes them to tests explicitly:

```bash
set -euo pipefail
for app in appA appB; do
  manifest="$PIXI_RUN_ROOT/$app/pyproject.toml"
  "$PIXI_BIN" run --no-config --locked --manifest-path "$manifest" up
  "$PIXI_BIN" run --no-config --locked --manifest-path "$manifest" seed
  "$PIXI_BIN" run --no-config --locked --manifest-path "$manifest" integration
  "$PIXI_BIN" run --no-config --locked --manifest-path "$manifest" identity
done
"$PIXI_BIN" run --no-config --locked \
  --manifest-path "$PIXI_RUN_ROOT/appA/pyproject.toml" down
"$PIXI_BIN" run --no-config --locked \
  --manifest-path "$PIXI_RUN_ROOT/appB/pyproject.toml" integration
"$PIXI_BIN" run --no-config --locked \
  --manifest-path "$PIXI_RUN_ROOT/appA/pyproject.toml" up
# Deliberately do not re-seed. Both original markers must have survived.
"$PIXI_BIN" run --no-config --locked \
  --manifest-path "$PIXI_RUN_ROOT/appA/pyproject.toml" identity
"$PIXI_BIN" run --no-config --locked \
  --manifest-path "$PIXI_RUN_ROOT/appA/pyproject.toml" integration
for app in appA appB; do
  "$PIXI_BIN" run --no-config --locked \
    --manifest-path "$PIXI_RUN_ROOT/$app/pyproject.toml" down
done
```

The implementation should capture every command's arguments, exit status, stdout/stderr and duration, and parse identity JSON from successful steps. Place cleanup in a runner `finally` block with per-service ownership verification. The sample sequence alone has no failure cleanup trap.

For repeated commands, collect separate ordinary and already-installed rows. Execute each in a new noninteractive process, after successful installation and interpreter identity checks:

```bash
"$PIXI_BIN" run --no-config --locked --manifest-path "$manifest" noop
"$PIXI_BIN" run --no-config --as-is --manifest-path "$manifest" noop
"$PIXI_BIN" run --no-config --locked --manifest-path "$manifest" test
```

`--as-is` is explicitly `--frozen --no-install`; it skips freshness checking and repair, so it must not silently replace the ordinary workflow row. A shell-hook batch is a valid additional row if all tools get the same already-activated-shell boundary. Avoid `pixi exec` for project-entry timing, since it manages an ephemeral environment instead of this workspace. CLI behavior was verified with release `--help` and follows [the release flags](https://github.com/prefix-dev/pixi/blob/abd7c0d32ebaf208939516e95e2776bf5ce2a6c9/crates/pixi_cli/src/cli_config.rs#L205-L258).

## Required receipts and realistic tasks

| Task | Required check |
| --- | --- |
| Clean checkout install | `pixi install --locked` exit 0; lock hash unchanged; executable resolves under the checkout prefix; record `pixi list --json`, Python patch and server binary versions |
| Reproduce a teammate's environment | Byte-identical manifest, lock and scripts in B; successful locked install; matching package selections for the same platform; a different Python prefix |
| Integration write/read | Application SQL row and Redis value contain A's marker in A and B's marker in B; fail explicitly on mismatch |
| Independent checkouts | Different `root`, `pgdata`, `pg_cluster_id`, `redis_dir`, Redis process PID/run ID and owner marker; declared endpoints agree with observed ports |
| Readiness | Successful SQL query and Redis protocol response plus observed identity; preserve elapsed time separately from package install and cluster initialization |
| Repeated command entry | Fresh CLI process each time, exit 0 and interpreter receipt; label Docker exec/shell transport; compare ordinary and already-installed rows separately |
| Stop A, retain B | A's endpoint probes fail after verified stop; B's identity/marker and application test still pass; B Redis run ID and PostgreSQL start time should remain unchanged |
| Restart with persistence | A's PostgreSQL cluster ID remains the same and old SQL/cache markers remain without reseeding; Redis run ID changes after restart; test real application data too |
| Lock mismatch | Change a manifest requirement in a disposable copy, keep the old lock; `install --locked` must fail and leave lock bytes unchanged |
| Service misconfiguration | Occupy a selected port, corrupt a local endpoint, or provide a wrong directory; count results as scripted workflow recovery behavior and setup effort |
| Offline replay | Use the same committed lock and a prepared package cache, then an offline locked install in a fresh prefix; classify cache miss separately; never assume a lock embeds package bytes |

The sample identity JSON does not record PostgreSQL start time. Add `SELECT pg_postmaster_start_time()` to the implementation receipt for the B-survival check. Compare process identities only within the same OS/container PID namespace. A PostgreSQL system identifier is strong supporting evidence, while the actual data directory and owner-marker checks remain required. Do not rely on a query result of `1` or `PING` to establish ownership.

Keep portable recreation distinct from sharing live service state. A lockfile reconstructs dependencies. Database/cache migration or backup requires explicit application data handling. Do not give Pixi a failure for lacking Stack-specific session leases or bundle APIs. A realistic recovery task can ask every tool to refuse a wrong endpoint or retain independent checkout data; report which portion is native and which portion required code.

## Platform and container constraints

- Pixi supports multi-platform Conda/PyPI environments, including Windows. This Unix daemon recipe covers macOS and Linux, and should not be advertised as a Windows service recipe. Redis server package availability must be checked for each selected platform before benchmarking. Source: [platform configuration](https://pixi.prefix.dev/latest/workspace/multi_platform_configuration/).
- Conda environments are prefixes rather than kernel-isolated service containers. Two Pixi checkouts still share host ports and the same OS unless an external container or VM supplies isolation.
- Prefer a non-root Ubuntu/Debian runner on Linux. `initdb` rejects root. macOS must run its matching native binary and package platform; a Linux ARM64 binary cannot establish a native macOS result.
- The release defaults for bare platform names include Linux kernel/glibc and macOS constraints. Read the lock and runner host requirements, including PyPI wheel compatibility. New per-platform virtual-package declarations replace the deprecated `system-requirements` table in the inspected release. [Release requirements documentation](https://github.com/prefix-dev/pixi/blob/abd7c0d32ebaf208939516e95e2776bf5ce2a6c9/docs/workspace/system_requirements.md).
- Official container documentation uses `ghcr.io/prefix-dev/pixi:0.81.0`. Record the pulled image digest and architecture before a benchmark; do not equate a tag or old `ev-base` image with a verified currently running version. For a long-lived `docker exec` runner, provide a keepalive and init process, and run service commands as a non-root user. No image was pulled during research. [Official container workflow](https://pixi.prefix.dev/latest/deployment/container/).
- Copying an installed Conda prefix to a different path is not the primary reproducibility workflow. Official container examples keep the prefix path identical between build and production images. Copy config/lock and reinstall for the fresh-checkout task.
- `--no-config` still reads project-local `.pixi/config.toml` and does not erase environment variable overrides. Use a controlled runner environment, record allowed proxy/cache variables, and inspect any local config. Source: [config selection](https://github.com/prefix-dev/pixi/blob/abd7c0d32ebaf208939516e95e2776bf5ce2a6c9/crates/pixi_config/src/lib.rs#L165-L190).
- `--clean-env` is optional Unix shell-environment filtering, not service isolation. The inspected release rejects that mode on Windows. Avoid inheriting a preactivated environment or ambiguous task environment; specify `-e default` if several environments exist. [Environment construction](https://github.com/prefix-dev/pixi/blob/abd7c0d32ebaf208939516e95e2776bf5ce2a6c9/crates/pixi_core/src/activation.rs#L492-L533).

## Implementation pointers and common mistakes

| Source inspected | Mechanism and benchmark implication |
| --- | --- |
| [Install CLI](https://github.com/prefix-dev/pixi/blob/abd7c0d32ebaf208939516e95e2776bf5ce2a6c9/crates/pixi_cli/src/install.rs#L13-L86) | Installation can be implicit during run; measure first use separately from warm command entry |
| [Task execution](https://github.com/prefix-dev/pixi/blob/abd7c0d32ebaf208939516e95e2776bf5ce2a6c9/crates/pixi_task/src/executable_task.rs#L163-L243) | Uses `deno_task_shell`, defaults cwd to workspace root, and does not inject fail-fast shell behavior into multiline tasks |
| [Task cache eligibility](https://github.com/prefix-dev/pixi/blob/abd7c0d32ebaf208939516e95e2776bf5ce2a6c9/crates/pixi_task/src/task_hash.rs#L75-L98) | No inputs/outputs means no cache entry; omit file caching for database-dependent integration tests |
| [Run preparation and exit handling](https://github.com/prefix-dev/pixi/blob/abd7c0d32ebaf208939516e95e2776bf5ce2a6c9/crates/pixi_cli/src/run.rs#L548-L684) | Checks cache, installs prefix when allowed, activates environment, and propagates task failure |
| [Environment directory](https://github.com/prefix-dev/pixi/blob/abd7c0d32ebaf208939516e95e2776bf5ce2a6c9/crates/pixi_core/src/workspace/environment.rs#L98-L104), [detached directory](https://github.com/prefix-dev/pixi/blob/abd7c0d32ebaf208939516e95e2776bf5ce2a6c9/crates/pixi_core/src/workspace/mod.rs#L796-L820) | Dependency prefixes are checkout-specific by default; this does not assign distinct service ports |
| [Lock satisfiability](https://github.com/prefix-dev/pixi/tree/abd7c0d32ebaf208939516e95e2776bf5ce2a6c9/crates/pixi_core/src/lock_file/satisfiability) | Implementation for determining whether manifest/platform/package selections still match a lock |
| [Cleanup CLI](https://github.com/prefix-dev/pixi/blob/abd7c0d32ebaf208939516e95e2776bf5ce2a6c9/crates/pixi_cli/src/clean.rs#L34-L65) | Cleans environment/cache files; this is not a service shutdown command |

Common benchmark errors:

1. Starting PostgreSQL or Redis from an activation script. Activation happens during command entry and can distort every repeated invocation or create duplicate processes.
2. Defining `test` as depending on a foreground server task. The server never exits, so the test never runs. Use a finite launch/probe task with explicit cleanup, or an external supervisor as a separately labeled dependency.
3. Embedding Bash loops, traps or complex nested quotes directly into task strings. Pixi uses Deno's task shell. Use external Python or Bash scripts and check every subprocess status.
4. Calling a multiline task successful because its final command succeeded. Use explicit subprocess `check=True`, `&&`, or an external script with fail-fast behavior.
5. Allowing `pixi run` to change the lock during a supposedly pinned replay. Use `--locked` for ordinary runs; `--frozen` intentionally skips manifest freshness checks.
6. Caching an integration task by source-file hashes. A cache hit can skip checking a stopped database or wrong cache instance.
7. Timing `uv run` while claiming Pixi installed the application's PyPI dependencies. Choose native Pixi or record both managers and both locks.
8. Calling fixed benchmark ports automatic Pixi allocation, or separate prefixes service isolation. Inspect the actual endpoints, server data directories and marker values.
9. Using `pixi clean` or deleting `.pixi` to stop services. Stop verified services first; then remove only runner-owned state/prefix directories.
10. Counting a failed first recipe or container keepalive mistake as a product limitation. Preserve the failure and repeat a corrected, supported workflow before reaching a capability conclusion.

Recommended positioning for this benchmark: include Pixi as a full application-environment workflow with explicitly authored local service management. Its native dependency/platform/task capabilities deserve their own results. The service comparison should report user-visible completion, configuration burden, recovery behavior and receipts for the composed workflow, with no invented supervisor features or global winner score.
