# Plain Nix development shells

Research date: 2026-10-06. This is implementation guidance, not a new benchmark result. The benchmark implementation belongs to Opus 5.5.

## Scope and evidence

Plain Nix is a supported way to supply a reproducible development toolchain on Linux and macOS. It runs arbitrary commands in a development shell. PostgreSQL and Redis are packages in that shell. From the inspected CLI and `mkShell` implementation, plain Nix development shells do not provide a project service supervisor, readiness graph, port allocator, service ownership registry, or database lifecycle. Those parts require user scripting or a separately identified adjacent tool. This feature boundary is source-derived, not a failed runtime experiment.

I shallow-cloned [NixOS/nix](https://github.com/NixOS/nix) into `/tmp/stack-bench-sources/nix` and inspected `src/nix/develop.cc` and `src/libflake/flake.cc`. The inspected master commit was [`bea853d2fa027d1a386c72d685fd2e6cdf6e2a70`](https://github.com/NixOS/nix/commit/bea853d2fa027d1a386c72d685fd2e6cdf6e2a70), dated 2026-10-05. Its `.version` is `2.36.0`, which is a development-tree version, not evidence of a published stable release.

The newest upstream tag returned by a version-sorted live `git ls-remote --tags --refs` lookup was `2.35.2`. The annotated tag object is `a400e1f45939a4e0521f66e76470eea9e8ea666b`; its commit is [`2c73b59da29606068c0c98db015dd3a66955525d`](https://github.com/NixOS/nix/commit/2c73b59da29606068c0c98db015dd3a66955525d). I fetched that tag and read its development-shell implementation too. The [current upstream manual](https://nix.dev/manual/nix/2.35/command-ref/new-cli/nix3-develop) identifies itself as Nix 2.35.2. GitHub's releases page has no releases, so its `/releases/latest` API returns 404; this does not mean Nix has no published versions.

Local observations, executed without starting project services:

```text
Host command -v nix: no result
docker run --rm --entrypoint /nix/var/nix/profiles/default/bin/nix ev-nix:latest --version
nix (Determinate Nix 3.23.0) 2.35.2
```

`ev-nix:latest` was image `sha256:fbf87c18aced2d3c969df40d15b137943429882f4283fe256c2f481a7af17c6a`, architecture `arm64`, OS `linux`, created `2026-10-02T14:37:35.845470168Z`. Recheck the image ID at execution time. The repository's `eval/images/Dockerfile.nix` uses the unpinned Determinate installer. This is a Determinate Nix image, not a clean upstream Nix install. Benchmark either an upstream 2.35.2 image/install or explicitly label this distribution and record the full version string. Matching the trailing upstream version does not prove identical evaluation behavior.

The recipes below were reviewed against source and documentation and checked for Python/shell syntax. They were not run with Nix, PostgreSQL, Redis, or the application. There are no fresh timing, readiness, persistence, or isolation measurements in this note.

## Relevant implementation

| Source | What it establishes |
| --- | --- |
| [Stable `makeRcScript`, lines 348 onward](https://github.com/NixOS/nix/blob/2c73b59da29606068c0c98db015dd3a66955525d/src/nix/develop.cc#L348) | Nix restores the derivation environment, prepends its tool paths while preserving the caller's PATH, sets temporary-directory variables, and evaluates `shellHook`. A shell hook is arbitrary shell code, not a managed service declaration. |
| [Stable environment acquisition, lines 477 onward](https://github.com/NixOS/nix/blob/2c73b59da29606068c0c98db015dd3a66955525d/src/nix/develop.cc#L477) | Nix realizes an environment derivation, can save it in a profile, and reads the serialized build environment. Cached realizations and profiles are native supported optimizations. |
| [Stable command execution, lines 587 onward](https://github.com/NixOS/nix/blob/2c73b59da29606068c0c98db015dd3a66955525d/src/nix/develop.cc#L587) | `--command` produces an `exec` command after environment setup, and Nix executes Bash. It does not surround the application with a service supervisor or cleanup protocol. |
| [Stable print-dev-env implementation, lines 725 onward](https://github.com/NixOS/nix/blob/2c73b59da29606068c0c98db015dd3a66955525d/src/nix/develop.cc#L725) | `nix print-dev-env` emits Bash environment setup or JSON. Reusing a prepared shell is a supported workflow, and should be measured separately from repeatedly invoking the CLI. |
| [Current flake lock checking/writing](https://github.com/NixOS/nix/blob/bea853d2fa027d1a386c72d685fd2e6cdf6e2a70/src/libflake/flake.cc#L795) | Lock inputs are checked; changes fail when updates are forbidden. A committed lock plus `--no-update-lock-file` gives a concrete reproducibility gate. |
| [Nixpkgs `mkShell`](https://github.com/NixOS/nixpkgs/blob/151fa4e8ddfdd8dd25d945ad94ed54a13de9f6e4/pkgs/build-support/mkshell/default.nix) | `packages` contributes native build inputs and shell hooks are concatenated. There is no native services field here. |
| [Nixpkgs PostgreSQL versions](https://github.com/NixOS/nixpkgs/blob/151fa4e8ddfdd8dd25d945ad94ed54a13de9f6e4/pkgs/servers/sql/postgresql/default.nix) and [Redis package](https://github.com/NixOS/nixpkgs/blob/151fa4e8ddfdd8dd25d945ad94ed54a13de9f6e4/pkgs/by-name/re/redis/package.nix) | PostgreSQL 17 has an explicit package attribute. `redis` at this revision is 8.10.2; a generic package attribute is pinned by the Nixpkgs commit, not by its name. |

The Nixpkgs revision above was the live `nixos-unstable` head returned by `git ls-remote` during research. I fetched and read the listed package files from that exact revision. Its package availability on every target architecture still needs execution verification.

## Native features and user work

| Requirement | Plain Nix responsibility | User/application responsibility |
| --- | --- | --- |
| Pinned toolchain | Immutable store packages resolved from a pinned Nixpkgs input | Choose attributes, commit `flake.lock`, record resolved versions |
| Python dependencies | Supply Python and uv, or package the application in Nix | For the shared uv fixture, commit `uv.lock` and run `uv sync --frozen` |
| Integration tests | Run the command in the environment | Application tests, migrations, seed data, assertions |
| Service startup and readiness | Supply server/client binaries | Start servers, poll, reject wrong instances, retain logs, stop partial starts |
| Two checkouts | Share immutable package downloads | Distinct writable state, explicit ports, endpoint propagation and isolation checks |
| Repeated commands | `nix develop -c`, reusable profile, `print-dev-env` | Decide whether to re-enter per command or hold a shell/session |
| Persistence and teardown | No project-data policy | PostgreSQL data directory, Redis AOF/RDB policy, stop/restart/delete semantics |
| Config copy | Locked flake inputs and store content | Copy application source, scripts, uv lock; exclude machine-specific live state |

Use `Nix + project scripts` as the row name for a service-bearing scenario. Mark its native service lifecycle as unsupported. Do not convert a working script into a claim that Nix natively allocates ports or supervises services. Conversely, do not fail the whole application scenario simply because its idiomatic user script is needed.

## Configuration recipe

Save as `flake.nix` in each fixture checkout:

```nix
{
  description = "Pinned Python/PostgreSQL/Redis development tools";
  inputs.nixpkgs.url =
    "github:NixOS/nixpkgs/151fa4e8ddfdd8dd25d945ad94ed54a13de9f6e4";
  outputs = { self, nixpkgs }:
    let
      systems = [
        "aarch64-linux" "x86_64-linux"
        "aarch64-darwin" "x86_64-darwin"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
    in {
      devShells = forAllSystems (system:
        let pkgs = nixpkgs.legacyPackages.${system};
        in {
          default = pkgs.mkShellNoCC {
            packages = [
              pkgs.python313 pkgs.uv pkgs.postgresql_17 pkgs.redis
            ];
            UV_PYTHON_DOWNLOADS = "never";
            UV_PYTHON = "${pkgs.python313}/bin/python3";
          };
        });
    };
}
```

Python's patch version, uv, PostgreSQL's patch version, and Redis's version are fixed by the input commit. Record their actual outputs rather than assuming they match the existing eval image. For a matched-version experiment, first choose an available Nixpkgs revision or explicit package override that supplies the versions agreed for all competitors. Keep this modern pinned recipe as its own condition if matching requires different versions. The generated `flake.lock` also records input content hashes.

Prepare in a disposable benchmark checkout, not this research workspace:

```bash
set -euo pipefail
export NIX_CONFIG='experimental-features = nix-command flakes'
# Required in a Git checkout: Nix's Git flake source includes tracked files.
git add flake.nix scripts/with-services.py pyproject.toml uv.lock
nix flake lock
git add flake.lock
nix develop --no-update-lock-file --command bash -c '
  set -euo pipefail
  python --version
  uv --version
  postgres --version
  redis-server --version
  uv sync --frozen --python "$UV_PYTHON"
'
```

The shared application fixture must already have `uv.lock`. Create it once during benchmark fixture preparation, then copy the same bytes to every tool. The historical `eval/fixture` has unbounded Python dependencies and no lock, so `uv sync` there is not a pinned application-dependency condition. Python binary wheels may need native libraries on NixOS; verify the shared dependency set on the chosen platforms and record any required Nix library additions. The existing macOS/Linux container fixture uses `psycopg[binary]`, Redis, and pytest, but that is not a runtime portability proof.

There is deliberately no service startup in `shellHook`. Re-entering a shell should not accidentally start extra servers or reset data. If the experiment uses startup hooks instead, count their code and effects as user work and guard repeated activation.

## User-authored service wrapper recipe

This sample owns foreground PostgreSQL and Redis children for one command/session. It keeps state at `.bench-nix`, exports explicit application endpoints, waits up to a bounded deadline, checks service identity, and shuts its children down when the command completes. It supports a second checkout by requiring a different explicit port pair. It does not implement automatic port allocation, crash recovery, a supervisor CLI, or secure production authentication. Loopback trust authentication is for the disposable local fixture.

Save the following as `scripts/with-services.py`. This is a recipe to adapt and execute during benchmark implementation, not an executed service result.

```python
import json
import os
from pathlib import Path
import signal
import subprocess as sp
import sys
import time

if os.geteuid() == 0:
    raise SystemExit("Run this fixture as a non-root user; initdb rejects root")
if len(sys.argv) < 2:
    raise SystemExit("usage: python scripts/with-services.py command [args...]")

root = Path.cwd().resolve()
state = root / ".bench-nix"
pgdata = state / "postgres"
redisdir = state / "redis"
state.mkdir(exist_ok=True)
redisdir.mkdir(exist_ok=True)
pgport = int(os.environ.get("BENCH_PG_PORT", "15432"))
redisport = int(os.environ.get("BENCH_REDIS_PORT", "16379"))
if not (1024 <= pgport <= 65535 and 1024 <= redisport <= 65535):
    raise SystemExit("ports must be unprivileged valid TCP ports")
if pgport == redisport:
    raise SystemExit("PostgreSQL and Redis require distinct TCP ports")

# A held lock rejects concurrent wrappers for the same checkout.
# A crash can leave this lock; inspect ownership before removing it.
lock = state / "session.lock"
lock.mkdir()
(lock / "pid").write_text(str(os.getpid()))
ready_file = state / "ready.json"
ready_file.unlink(missing_ok=True)
env = os.environ.copy()
for key in ("PGHOSTADDR", "PGSERVICE", "PGSERVICEFILE", "PGPASSWORD",
            "PGOPTIONS", "PGDATABASE", "PGUSER", "PGHOST", "PGPORT"):
    env.pop(key, None)
env.update(
    PGHOST="127.0.0.1", PGPORT=str(pgport), PGUSER="postgres",
    PGDATABASE="postgres", PGCONNECT_TIMEOUT="1",
    DATABASE_URL=f"postgresql://postgres@127.0.0.1:{pgport}/postgres",
    REDIS_URL=f"redis://127.0.0.1:{redisport}/0",
)
servers = []
log_files = []
payload = None

def interrupted(signum, frame):
    raise SystemExit(128 + signum)

signal.signal(signal.SIGTERM, interrupted)
signal.signal(signal.SIGINT, interrupted)

def capture(args):
    return sp.check_output(args, env=env, text=True, timeout=2).strip()

def start(args, name):
    log = (state / f"{name}.log").open("ab", buffering=0)
    log_files.append(log)
    child = sp.Popen(args, env=env, stdin=sp.DEVNULL,
                     stdout=log, stderr=log, start_new_session=True)
    servers.append(child)
    return child

try:
    if not (pgdata / "PG_VERSION").exists():
        sp.run(["initdb", "-D", str(pgdata), "-U", "postgres",
                "--auth=trust", "--encoding=UTF8", "--locale=C"],
               env=env, check=True, timeout=60)
    pg = start(["postgres", "-D", str(pgdata), "-h", "127.0.0.1",
                "-p", str(pgport), "-k", ""], "postgres")
    rd = start(["redis-server", "--bind", "127.0.0.1",
                "--port", str(redisport), "--daemonize", "no",
                "--protected-mode", "yes", "--dir", str(redisdir),
                "--appendonly", "yes", "--appendfsync", "everysec",
                "--save", ""], "redis")
    redis_cli = ["redis-cli", "-h", "127.0.0.1", "-p", str(redisport), "--raw"]
    deadline = time.monotonic() + 30
    while True:
        if pg.poll() is not None or rd.poll() is not None:
            raise RuntimeError("server exited; inspect .bench-nix/*.log")
        try:
            actual_pgdata = capture(["psql", "-X", "-At", "-v", "ON_ERROR_STOP=1",
                                     "-c", "show data_directory"])
            actual_redisdir = capture(redis_cli + ["CONFIG", "GET", "dir"]).splitlines()[-1]
            info = dict(line.split(":", 1) for line in
                        capture(redis_cli + ["INFO", "server"]).splitlines()
                        if ":" in line and not line.startswith("#"))
            if Path(actual_pgdata).resolve() != pgdata.resolve():
                raise RuntimeError("PostgreSQL endpoint belongs to another checkout")
            if Path(actual_redisdir).resolve() != redisdir.resolve():
                raise RuntimeError("Redis endpoint belongs to another checkout")
            if int(info["process_id"]) != rd.pid:
                raise RuntimeError("Redis endpoint is not the child started here")
            if int((pgdata / "postmaster.pid").read_text().splitlines()[0]) != pg.pid:
                raise RuntimeError("PostgreSQL data directory has another owner")
            if capture(redis_cli + ["PING"]) != "PONG":
                raise RuntimeError("Redis did not acknowledge PING")
            break
        except (sp.CalledProcessError, sp.TimeoutExpired, FileNotFoundError,
                IndexError, KeyError):
            if time.monotonic() >= deadline:
                raise RuntimeError("service readiness deadline expired")
            time.sleep(0.1)
    receipt = dict(pg_pid=pg.pid, redis_pid=rd.pid,
                   pg_data=str(pgdata), redis_data=str(redisdir),
                   database_url=env["DATABASE_URL"], redis_url=env["REDIS_URL"])
    ready_file.write_text(json.dumps(receipt, sort_keys=True) + "\n")
    print(json.dumps(receipt, sort_keys=True), flush=True)
    payload = sp.Popen(sys.argv[1:], env=env, start_new_session=True)
    result = payload.wait()
    raise SystemExit(result if result >= 0 else 128 - result)
finally:
    ready_file.unlink(missing_ok=True)
    if payload is not None and payload.poll() is None:
        os.killpg(payload.pid, signal.SIGTERM)
        try:
            payload.wait(timeout=10)
        except sp.TimeoutExpired:
            os.killpg(payload.pid, signal.SIGKILL)
            payload.wait(timeout=5)
    for child in reversed(servers):
        if child.poll() is None:
            # PostgreSQL SIGINT is a fast shutdown; Redis SIGTERM flushes AOF.
            child.send_signal(signal.SIGINT if child is servers[0] else signal.SIGTERM)
            try:
                child.wait(timeout=15)
            except sp.TimeoutExpired:
                child.kill()
                child.wait(timeout=5)
    for log in log_files:
        log.close()
    (lock / "pid").unlink(missing_ok=True)
    lock.rmdir()
```

This sample is intentionally a session wrapper. The benchmark adapter must retain its PID and logs, impose an outer timeout, and verify cleanup after failures. It should strengthen cleanup so one unexpected cleanup exception cannot skip other children, and record whether escalation was needed. It should also use an atomic ready-receipt write and verify live ownership before trusting a ready file. A stale file alone is not readiness. These are user-script requirements, not missing Nix configuration fields.

PostgreSQL also supplies a supported detached control tool, [pg_ctl](https://www.postgresql.org/docs/17/app-pg-ctl.html), with waiting start/stop/restart and a data-directory argument. A project can build detached `up`/`down` scripts with that tool. Redis supplies [graceful SHUTDOWN](https://redis.io/docs/latest/commands/shutdown/) and [AOF/RDB persistence](https://redis.io/docs/latest/operate/oss_and_stack/management/persistence/). Those are server features. Prefer the foreground owned-child wrapper for the benchmark because its cleanup can be tied to one session without trusting arbitrary pidfiles.

## Application commands and two checkouts

From a fresh noninteractive shell in checkout A:

```bash
set -euo pipefail
export NIX_CONFIG='experimental-features = nix-command flakes'
nix develop --no-update-lock-file --command bash -c '
  set -euo pipefail
  uv sync --frozen --python "$UV_PYTHON"
  BENCH_PG_PORT=15432 BENCH_REDIS_PORT=16379 \
    python scripts/with-services.py uv run --frozen pytest -q
'
```

The command exits with pytest's status. Its normal exit stops both servers; `.bench-nix/postgres` and `.bench-nix/redis` remain. Running the command again starts new processes over the same data. Clean restart persistence is supported by this user script and server storage. Crash durability must be a separate scenario because Redis AOF `everysec` has a different durability contract from synchronous database commits.

For simultaneous independent checkouts, prepare the same source/config/locks in A and B and launch separate wrappers with these explicit ports:

```bash
# Checkout A
BENCH_PG_PORT=15432 BENCH_REDIS_PORT=16379 \
  nix develop --no-update-lock-file --command \
  python scripts/with-services.py python -c 'import time; time.sleep(600)'

# Checkout B, in another shell/session
BENCH_PG_PORT=25432 BENCH_REDIS_PORT=26379 \
  nix develop --no-update-lock-file --command \
  python scripts/with-services.py python -c 'import time; time.sleep(600)'
```

The adapter can launch these without a TTY, wait for live ownership plus ready receipts, and run fresh command entries while both holders live. Because the endpoint variables belong to the wrapper's child environment, an independent entry must read the receipt and explicitly propagate its two URLs. Do not assume Nix installs these dynamic values into later shell entries. Use argv/environment APIs to propagate JSON fields, not shell `eval` of a receipt.

Stop the owned holder with SIGTERM and wait for its exit. The wrapper then stops its sleep payload and servers. Stopping A must leave B running. A new holder in A should use A's original data and explicit ports. Changing the configured ports while a holder runs is not a supported live reconfiguration.

## Required correctness checks

Use the same application mutations and assertions for all competitors. The historical fixture only checks `SELECT 1` and `PING`, which cannot establish isolated state or persistence.

1. Inside each environment, record `sys.executable`, Python version, uv version, PostgreSQL/Redis versions, and installed dependency versions. Require the expected Python minor and the frozen lock. Save `flake.lock` and `uv.lock` hashes before and after the task.
2. Require PostgreSQL `SHOW data_directory` to equal that checkout's physical `.bench-nix/postgres` path. Require Redis `CONFIG GET dir` to equal its `.bench-nix/redis` path, and `INFO server` process ID to match the owned child. URLs, PIDs, and files must describe live processes rather than a previous run.
3. Create the same table/key name in both instances with distinct values. For example, `bench_identity(id text primary key, value text)` with `('owner', 'A-<run-id>')` or `B-<run-id>`, and Redis key `bench:owner` with the matching value. Assert A reads A and B reads B through their application URLs. Use transactions that commit.
4. Insert application rows and cache keys beyond identity markers, run the shared integration tests, stop A, and prove B can still mutate/query its own rows and keys. All commands need bounded client timeouts.
5. Restart A over unchanged state and assert the same committed PostgreSQL rows and Redis keys. Record new server PIDs. Check `INFO persistence` for AOF enabled and successful status. Do not call a clean-shutdown persistence result a power-loss durability result.
6. Stop both holders, wait for exits, and verify their tracked server PIDs are gone and their service ports no longer accept connections. Preserve logs and receipts before any removal. A timeout/escalation is a separate failure outcome, not a successful clean teardown.
7. For explicit deletion, first prove no holder or tracked child remains, then remove only that fixture checkout's `.bench-nix`. A subsequent session should have empty application state. Do not run global `pkill`, Nix garbage collection, or Docker-wide cleanup.
8. Occupy one selected port with a benchmark-owned blocker. Startup must fail, emit the failed server log, and clean the other server if it started. No test may connect to the blocker and declare service success. If the script fails these checks, report a script/workflow failure and repair it before collecting latency samples.

Reproducibility means a clean source copy with `flake.nix`, `flake.lock`, the scripts, `pyproject.toml`, `uv.lock`, tests, and migrations reproduces the tools and installs the same application dependencies. Do not copy `.venv`, `.bench-nix`, lock directories, or live ready receipts to the new checkout. A different Nix-supported architecture can resolve different store artifacts for the same declared packages; compare versions and behavior, not store-path byte equality across architectures.

## Repeated-command conditions

Report at least two supported conditions:

```bash
# Fresh process per entry, warm caches, services held separately.
nix develop --no-update-lock-file --command python -c 'print("probe")'

# Record and then reuse a native environment profile.
mkdir -p .bench-nix
nix develop --no-update-lock-file --profile "$PWD/.bench-nix/tool-profile" \
  --command true
nix develop "$PWD/.bench-nix/tool-profile" --command python -c 'print("probe")'

# One prepared Bash shell/session, multiple application commands.
nix print-dev-env --no-update-lock-file > .bench-nix/env.bash
bash --noprofile --norc -c '
  set -euo pipefail
  source .bench-nix/env.bash
  python -c "print(\"probe\")"
  python -c "print(\"probe\")"
'
```

`nix develop --profile` and `print-dev-env` are documented in the [develop manual](https://nix.dev/manual/nix/2.35/command-ref/new-cli/nix3-develop) and [print-dev-env manual](https://nix.dev/manual/nix/2.35/command-ref/new-cli/nix3-print-dev-env). Profiles keep environment store references, not PostgreSQL or Redis state. Generated Bash code is local prepared state and can contain store paths; regenerate it from the committed flake on a clean machine. Do not share this file as the cross-machine lock.

Measure preparation once and repeated command execution separately. A persistent shell reduces the re-entry cost but has its own lifetime and environment propagation policy. Compare that condition with equivalent persistent-session workflows for other tools. Do not silently compare cached Nix shell code with full fresh-entry CLI costs, or vice versa.

## Platform and container boundaries

The upstream [supported platforms](https://nix.dev/manual/nix/2.35/installation/supported-platforms) are Linux on i686, x86_64, and aarch64, and macOS on x86_64 and aarch64. This flake declares the four mainstream 64-bit combinations. The wrapper uses Unix process groups and is not a native Windows recipe. A Linux container run on a Mac measures Linux in a VM, not native Darwin.

Nix installation is a setup prerequisite with an immutable store at `/nix`. The [binary installation manual](https://nix.dev/manual/nix/2.35/installation/installing-binary) describes multi-user installation with a Nix daemon on supported systemd Linux and macOS, and single-user Linux installation. The Nix daemon realizes store packages; it is not a development-service daemon. Record setup policy and whether the Nix daemon was already running.

For a Linux benchmark container, run the application as a non-root account, give that account writable checkout/state directories, and ensure its Nix client can access the configured store/daemon. Use the container's system architecture in the flake. Do not hardcode `aarch64-linux` in a general host recipe, which the historical eval config does. Avoid disabling Redis/PostgreSQL durability only for Nix to improve timings. Use mounted state when testing persistence across container replacement; stopping and starting a process in the same container is a different condition.

Network is needed for missing Nix inputs, substitute binaries or source builds, and the first uv dependency install. Record whether each cache is empty/warm: Nix store, Nix fetch/evaluation cache, uv cache, `.venv`, PostgreSQL initialized state, and Redis persisted state. Nix's store can be shared across checkouts while service data remains separate. Do not purge a shared host store to simulate cold starts. Use disposable isolated stores/images for that measurement.

## Historical harness mistakes to avoid

- `eval/harness/current-benchmark.py` treats Nix service start as `true` and excludes Nix from readiness, test, identity, and second-checkout service phases. Its Nix entry numbers are historical toolchain observations, not an application workflow comparison.
- `eval/configs/nix/flake.nix` points at the moving `nixos-unstable` branch and supplies no checked-in application lock. A flake evaluation may generate a lock, but independent fixture preparation without copying that lock does not pin the same input for both checkouts.
- The existing image uses an unpinned installer and contains Determinate Nix. An image tag named `ev-nix` cannot identify upstream Nix by itself.
- Successful shell activation, a created profile, or an environment JSON receipt cannot establish service readiness or isolation.
- `nix develop --command` still executes `shellHook`. A hook that always starts servers changes repeated-command behavior and can leave processes after `exec`; native cleanup should not be inferred.
- The caller's PATH survives ordinary develop setup. Require the expected tool executable paths or use a deliberately controlled environment. Do not count a Homebrew or base-image Python/Redis as the pinned Nix package.
- Git-backed flakes omit untracked files. A new Nix file must be tracked before evaluation. Generated project state should be ignored so it does not bloat source copies or enter the flake source tree.
- `nix flake check` only runs declared flake checks. It does not automatically discover pytest or start the application's two external services.
- Automatic port allocation, crash restarts, and zero-configuration service discovery are user-script or adjacent-tool work. Score explicit-port concurrent checkouts as a valid supported workflow, and score automatic allocation separately.

## Recommended benchmark tasks

The main comparable task is a pinned application lifecycle: materialize the environment, install frozen dependencies, start owned services, wait and identify them, migrate/seed, run integration tests, and stop cleanly. Split environment realization, Python sync, initialization, readiness, tests, and teardown into observed phases so network/download time does not masquerade as command overhead.

The concurrency task should permit explicit endpoint configuration for all tools. Run two independent checkouts, prove distinct state, stop one without affecting the other, and restart with persisted data. Record user config and script lines separately from runtime success. An optional automatic-port condition can measure native automation or supplied script code, with those responsibilities labeled.

The reproducibility task should copy only committed config/source/locks into a fresh checkout and use `--no-update-lock-file` plus `uv --frozen`. It should establish package and dependency identity and rerun the same service-backed application behavior. Failure injection should cover an occupied port and a failed application command, with cleanup validated in both cases.

NixOS modules, devenv, Flox, Devbox, process-compose, and direnv/nix-direnv are adjacent workflows. Adding one changes the tool stack and must get a separately named row. A NixOS system service declaration is not a macOS/Linux plain development-shell service feature. Plain Nix remains a strong pinned-toolchain baseline and a legitimate service-backed baseline when its user scripting is visible and tested.
