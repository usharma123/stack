# BranchBox research

Research date: 2026-10-06. This is a recipe and implementation review for the application benchmark. No application services, containers, or timing runs were started. Opus 5.5 implements and executes the benchmark.

## Version and evidence

GitHub's latest stable release API returned **v0.13.4**, published September 10, 2026. Its annotated tag object is `7d713966517522e00dfaa12d2d42a0924850ea82`; the source commit is `a00b3ee9a0acce000ee8f478dc0d6a8a24b3c7df`. Current main was inspected separately at `99bcd6868aa8676dab3bf71378b4e921fbb3e964`. Use the release for measurements and the release commit for implementation claims. Main contains newer behavior despite retaining package version 0.13.4. [Release](https://github.com/branchbox/branchbox/releases/tag/v0.13.4), [release source](https://github.com/branchbox/branchbox/tree/a00b3ee9a0acce000ee8f478dc0d6a8a24b3c7df).

Executed research evidence on this Darwin arm64 host:

- Downloaded the official Apple Silicon release into `/tmp/stack-bench-sources/branchbox-bin`, without global installation. Archive SHA-256 `3446e7462b9a42724034695e3cb3163d263aa792a3d97acac95d155aa1c22392` matches the published checksums file.
- The binary returned `branchbox 0.13.4` for `--version`. `init`, `feature start`, `devcontainer up`, and `devcontainer exec` help were read. `branchbox version --json` failed as an unrecognized command; that current-main interface cannot be assumed for this release.
- `branchbox` and `devcontainer` were absent from the initial PATH. Docker Compose is `v2.40.3-desktop.1`. Read-only image inspection found arm64 PostgreSQL 17.6 and Redis 8.10.2 behind local tags `postgres:17.6-alpine` and `redis:8-alpine`.
- Docker Hub manifest reads verified the three digest references below and Linux amd64/arm64 entries. No layers were pulled. Python 3.13.7 is a proposed common exact patch, not a latest-Python claim. Align all competitors to the study's chosen patch before running.

| Component | Pin |
| --- | --- |
| Python | `python:3.13.7-slim-bookworm@sha256:adafcc17694d715c905b4c7bebd96907a1fd5cf183395f0ebc4d3428bd22d92d` |
| PostgreSQL | `postgres:17.6-alpine@sha256:ef257d85f76e48da1c64832459b59fcaba1a4dac97bf5d7450c77753542eee94` |
| Redis | `redis:8.10.2-alpine@sha256:3811787313eba226a2ef38658c6ccb91cd5e110edc89c37767de373120a0e5a0` |
| uv | `0.12.23`, also verified as an existing PyPI release |

These version and metadata checks do not establish that the application recipe works. Sources for artifact preparation are the [official checksums](https://github.com/branchbox/branchbox/releases/download/v0.13.4/checksums.txt), [Docker Python image definitions](https://github.com/docker-library/python), and [uv package metadata](https://pypi.org/pypi/uv/0.12.23/json). The inspected historical `eval/harness/compose-benchmark.py` measures database containers and commands inside PostgreSQL, not BranchBox. No historical result transfers to this lane.

## Supported workflow and responsibility

BranchBox is a direct competitor for managing feature workspaces around a container development environment. The supported local path is `init`, `feature start --runtime container`, then explicit `devcontainer up` and `devcontainer exec`. The released native devcontainer runtime invokes Docker/Compose itself. This recipe does not require `@devcontainers/cli`, an editor, a BranchBox daemon, a cloud account, or an AI credential. [CLI definitions](https://github.com/branchbox/branchbox/blob/a00b3ee9a0acce000ee8f478dc0d6a8a24b3c7df/cli/src/commands/devcontainer.rs#L26), [native runtime](https://github.com/branchbox/branchbox/blob/a00b3ee9a0acce000ee8f478dc0d6a8a24b3c7df/core/src/devcontainer_runtime/runtime.rs#L399).

| Task | BranchBox supplies | Project or benchmark supplies |
| --- | --- | --- |
| Separate feature code | Git worktrees and branches; copied Dev Container config | A source token in each checkout; verify the module actually executed |
| Python, PostgreSQL, Redis | Dev Container and Compose execution | Dockerfile, exact images, Python lock, service declarations and URLs |
| Separate service data | Compose project runtime and module configuration | Unique directory basenames, project-scoped volumes, no shared external data |
| Dependency installation | Executes configured creation hooks | `uv sync --frozen`, lock file, explicit successful exec verification |
| Readiness | Starts the selected Compose service and its dependencies | Health checks, authenticated app connection and identity probes |
| Repeated commands | Native `devcontainer exec`, child exit propagation | App command, pytest suite, bounded subprocess timeout |
| Persistence | `devcontainer down` preserves Compose volumes by default | Redis persistence settings and marker checks without reseeding |
| Destruction | `down --volumes`; feature teardown removes owned Compose resources and worktree | Capture ownership before teardown; verify absence and unaffected B |
| Sharing | Config copying and devcontainer sync | Portable source/config/locks, image digests, binary checksum, platform receipts |

Docker must be running and accessible, with the Compose plugin on PATH. The release has macOS arm64/x86_64 and Linux GNU arm64/x86_64 archives. Source builds require Rust 1.89 or newer and Cargo lock discipline. On macOS, Docker Desktop or an equivalent Linux Docker VM adds its own startup and filesystem costs. In a containerized benchmark host, `feature start` rejects container execution unless `--allow-container` is supplied; that flag does not install Docker, grant daemon access, or make bind paths valid on a remote daemon. Use the same host/daemon topology across the container competitors. [Init options](https://github.com/branchbox/branchbox/blob/a00b3ee9a0acce000ee8f478dc0d6a8a24b3c7df/cli/src/commands/init.rs#L12), [feature options](https://github.com/branchbox/branchbox/blob/a00b3ee9a0acce000ee8f478dc0d6a8a24b3c7df/cli/src/commands/feature.rs#L53).

## Application configuration

Use the shared `bench/fixtures/app` source, `pyproject.toml`, `uv.lock`, migrations, and integration tests. Its Python constraint is 3.13, and its receipt includes server identity, source token, and the module path. Place the following files alongside that fixture in a disposable Git repository called `main`. Provide no real `.env` or credential directories in this repository.

`.devcontainer/devcontainer.json`:

```json
{
  "name": "rwb-python-postgres-redis",
  "dockerComposeFile": "compose.yaml",
  "service": "app",
  "workspaceFolder": "/workspaces/${localWorkspaceFolderBasename}",
  "remoteUser": "root",
  "postCreateCommand": ["uv", "sync", "--frozen"],
  "overrideCommand": false,
  "shutdownAction": "none"
}
```

`.devcontainer/Dockerfile`:

```dockerfile
FROM python:3.13.7-slim-bookworm@sha256:adafcc17694d715c905b4c7bebd96907a1fd5cf183395f0ebc4d3428bd22d92d
RUN python -m pip install --no-input --disable-pip-version-check uv==0.12.23
ENV UV_PYTHON_DOWNLOADS=never UV_PYTHON_PREFERENCE=only-system \
    UV_PROJECT_ENVIRONMENT=/opt/rwb-venv PYTHONUNBUFFERED=1
```

`.devcontainer/compose.yaml`:

```yaml
services:
  app:
    build:
      context: ..
      dockerfile: .devcontainer/Dockerfile
    command: [sleep, infinity]
    init: true
    volumes:
      - ../..:/workspaces:cached
    labels:
      devcontainer.local_folder: ${RWB_WORKSPACE:?Set the exact absolute workspace path}
      devcontainer.config_file: ${RWB_WORKSPACE}/.devcontainer/devcontainer.json
    environment:
      DATABASE_URL: postgresql://bench:bench@postgres:5432/bench
      REDIS_URL: redis://redis:6379/0
    depends_on:
      postgres:
        condition: service_healthy
      redis:
        condition: service_healthy
  postgres:
    image: postgres:17.6-alpine@sha256:ef257d85f76e48da1c64832459b59fcaba1a4dac97bf5d7450c77753542eee94
    environment:
      POSTGRES_USER: bench
      POSTGRES_PASSWORD: bench
      POSTGRES_DB: bench
    volumes:
      - pgdata:/var/lib/postgresql/data
    healthcheck:
      test: [CMD, pg_isready, -h, 127.0.0.1, -U, bench, -d, bench]
      interval: 1s
      timeout: 3s
      retries: 60
  redis:
    image: redis:8.10.2-alpine@sha256:3811787313eba226a2ef38658c6ccb91cd5e110edc89c37767de373120a0e5a0
    command: [redis-server, --appendonly, "yes", --appendfsync, always]
    volumes:
      - redisdata:/data
    healthcheck:
      test: [CMD, redis-cli, ping]
      interval: 1s
      timeout: 3s
      retries: 60
volumes:
  pgdata: {}
  redisdata: {}
```

No host ports, `container_name`, external networks, or fixed volume names are necessary. Python connects to service DNS in its own project network. The supplied workspace labels support exact cleanup discovery, because the released native Compose runtime otherwise finds containers by project/service and does not add the standard workspace labels. Pass `RWB_WORKSPACE` on every host operation that renders this configuration.

The parent-directory mount is deliberate. BranchBox's normal `init` and feature configuration rewrite `workspaceFolder` and insert `../..:/workspaces:cached` on the main service. It permits Git's shared worktree metadata to work inside the container, and also makes sibling checkouts visible and writable. Prove that commands execute each checkout's own code; do not claim filesystem access isolation from this local-container recipe. A stricter custom mount layout would be a separate configuration lane. [Workspace mutation](https://github.com/branchbox/branchbox/blob/a00b3ee9a0acce000ee8f478dc0d6a8a24b3c7df/core/src/modules/devcontainer.rs#L857).

`.devcontainer/ready.py`:

```python
import json
from pathlib import Path
import sys
import time
from rwbapp import core

expected = sys.argv[1]
root = Path.cwd().resolve()
deadline = time.monotonic() + 60
while True:
    try:
        result = core.cmd_identity(None)
        source = result["source"]
        if source["token"] != expected or Path(source["module"]).resolve() != root / "rwbapp":
            raise RuntimeError(f"wrong checkout source: {source}")
        if result["python"]["version"] != "3.13.7":
            raise RuntimeError(f"wrong Python: {result['python']}")
        if result["pg"]["database"] != "bench" or result["redis"]["db"] != 0:
            raise RuntimeError("wrong database identity")
        print(json.dumps(result, sort_keys=True))
        break
    except (ConnectionError, OSError):
        if time.monotonic() >= deadline:
            raise
        time.sleep(0.25)
```

Invoke this file with `uv run --frozen python -c`, as below, so the shared app's root is on Python's module path. The connection probes use the same URLs and client libraries as the application. Docker health checks alone do not authenticate the Python client or identify the checkout.

## Noninteractive command recipe

This is a runnable template after placing the shared fixture and the files above in `$BENCH_ROOT/main`. Use Bash, an absolute checksum-verified `$BRANCHBOX_BIN`, a fresh absolute `$BENCH_ROOT` under the host's Docker-shared filesystem, and a unique lowercase hexadecimal `$RWB_RUN_ID`. `init -y` avoids prompts and, in the inspected release, does not automatically relocate a temporary repository. The fixture preparation commit below is in the disposable benchmark repository, never in Stack.

```bash
set -euo pipefail
: "${BRANCHBOX_BIN:?absolute path to the pinned BranchBox binary}"
: "${BENCH_ROOT:?absolute path to this fresh benchmark directory}"
: "${RWB_RUN_ID:?unique lowercase hexadecimal identifier for this run}"
repo="$BENCH_ROOT/main"
mkdir -p "$BENCH_ROOT/receipts"
cd "$repo"
git init -q
git config user.name 'Stack benchmark'
git config user.email 'benchmark@example.invalid'
printf '.branchbox/\n.venv/\n__pycache__/\n.pytest_cache/\n.devcontainer/.branchbox.env\n' > .gitignore
git add .
git commit -qm 'Prepare shared application fixture'
RWB_WORKSPACE="$repo" "$BRANCHBOX_BIN" init -y --no-parent-structure --no-coding-agents --skip-env
# Capture any config mutations, so new worktrees branch from the actual config.
git add .devcontainer .gitignore
git diff --cached --quiet || git commit -qm 'Record BranchBox workspace configuration'

feature_a="rwb-${RWB_RUN_ID}-a"
feature_b="rwb-${RWB_RUN_ID}-b"
marker_a="rwb${RWB_RUN_ID}a"
marker_b="rwb${RWB_RUN_ID}b"
for feature in "$feature_a" "$feature_b"; do
  RWB_WORKSPACE="$repo" "$BRANCHBOX_BIN" feature start "$feature" \
    --repo "$repo" --runtime container --skip-module database \
    --skip-module tunnel --skip-module specs --json \
    > "$BENCH_ROOT/receipts/$feature.start.json"
done
checkout_a="$BENCH_ROOT/$feature_a"
checkout_b="$BENCH_ROOT/$feature_b"
# Parse start receipts in the final adapter and require these exact worktree paths.
printf '%s\n' "$feature_a" > "$checkout_a/rwbapp/SOURCE_TOKEN"
printf '%s\n' "$feature_b" > "$checkout_b/rwbapp/SOURCE_TOKEN"

bbexec() {
  local workspace="$1"
  shift
  RWB_WORKSPACE="$workspace" "$BRANCHBOX_BIN" devcontainer exec \
    --workspace-folder "$workspace" -- "$@" </dev/null
}
ready() {
  bbexec "$1" uv run --frozen python -c \
    'exec(compile(open(".devcontainer/ready.py").read(), ".devcontainer/ready.py", "exec"))' "$2"
}
for workspace in "$checkout_a" "$checkout_b"; do
  marker="$marker_a"
  if [ "$workspace" = "$checkout_b" ]; then marker="$marker_b"; fi
  RWB_WORKSPACE="$workspace" "$BRANCHBOX_BIN" devcontainer up "$workspace" --json \
    > "$BENCH_ROOT/receipts/$(basename "$workspace").up.json"
  # Required: creation-hook success is not checked by this release's native runtime.
  bbexec "$workspace" uv sync --frozen
  ready "$workspace" "$(basename "$workspace")"
  bbexec "$workspace" uv run --frozen python -m rwbapp migrate
  bbexec "$workspace" uv run --frozen python -m rwbapp mark --checkout "$marker"
  bbexec "$workspace" uv run --frozen python -m rwbapp identity \
    > "$BENCH_ROOT/receipts/$(basename "$workspace").identity.json"
  RWB_WORKSPACE="$workspace" "$BRANCHBOX_BIN" devcontainer exec -w "$workspace" \
    --remote-env "RWB_CHECKOUT=$marker" -- uv run --frozen pytest -q </dev/null
done
bbexec "$checkout_a" uv run --frozen python -m rwbapp check --checkout "$marker_a" --forbid "$marker_b"
bbexec "$checkout_b" uv run --frozen python -m rwbapp check --checkout "$marker_b" --forbid "$marker_a"
```

The database module is skipped intentionally. Its Python/PostgreSQL detection prepares a database-name variable and suggests a Rails setup command; it neither creates nor migrates this application database. Compose owns these fixture services, and the app performs its migrations. Skipping tunnel/specs keeps unrelated agent workflow work out of the application lane. Record skips as configuration, not missing service support. [Database setup](https://github.com/branchbox/branchbox/blob/a00b3ee9a0acce000ee8f478dc0d6a8a24b3c7df/core/src/modules/database.rs#L199).

Repeat the final pytest and `rwbapp identity/check` exec commands in fresh noninteractive subprocesses. For repeated startup, issue `devcontainer up` again, then explicitly run `ready` and compare PostgreSQL system identifier/start time, Redis `run_id`, and Docker container IDs to the earlier receipts. Code inference: the native runtime returns immediately for an already running app, and starts only the existing app when stopped. It does not reconcile stopped/missing dependencies in that branch. Keep a dependency-failure recovery task separate from ordinary repeated entry.

## Restart, teardown, and receipts

For the persistence task, keep worktrees and named volumes. Persist the fixture's keeper row and Redis durable key, run A down/up, and verify persistence before any reseeding:

```bash
bbexec "$checkout_a" uv run --frozen python -m rwbapp persist --checkout "$marker_a"
RWB_WORKSPACE="$checkout_a" "$BRANCHBOX_BIN" devcontainer down "$checkout_a" --json
# Verify A's containers/network are absent and its pgdata/redisdata volumes remain.
bbexec "$checkout_b" uv run --frozen python -m rwbapp check --checkout "$marker_b" --forbid "$marker_a"
RWB_WORKSPACE="$checkout_a" "$BRANCHBOX_BIN" devcontainer up "$checkout_a" --json
bbexec "$checkout_a" uv sync --frozen
ready "$checkout_a" "$feature_a"
bbexec "$checkout_a" uv run --frozen python -m rwbapp persisted --checkout "$marker_a"
bbexec "$checkout_a" uv run --frozen python -m rwbapp check --checkout "$marker_a" --forbid "$marker_b"

# Final cleanup, only after the persistence receipts have been saved.
RWB_WORKSPACE="$checkout_a" "$BRANCHBOX_BIN" devcontainer down "$checkout_a" --volumes --json
bbexec "$checkout_b" uv run --frozen python -m rwbapp check --checkout "$marker_b" --forbid "$marker_a"
RWB_WORKSPACE="$checkout_b" "$BRANCHBOX_BIN" devcontainer down "$checkout_b" --volumes --json
```

The final harness must capture Docker IDs and ownership before starting measured work, check every subprocess return code, and inspect resource absence after cleanup. For each project, enumerate containers with `docker ps -aq --filter label=com.docker.compose.project=<basename>`, networks with `docker network ls -q --filter ...`, and volumes with `docker volume ls -q --filter ...`. Inspect these exact resources to retain project labels, workspace label, image ID, bind source, named-volume identity, and network IDs. Require exactly the expected three service containers. A failed enumeration is an error, never proof of absence. Validate both the actual native project basename and the generated managed project name before using `feature teardown`.

Feature teardown is destructive final cleanup, not a stop/persistence operation. Preview it with `feature teardown <feature> --repo "$repo" --keep-branch --dry-run --json`; inspect module ownership before executing. This recipe writes source tokens and generated configuration in disposable worktrees, so the final cleanup may need `--discard-changes`. Keep the source/receipt artifacts first. The released Compose module discovers exact workspace-labelled projects, runs `down --volumes --remove-orphans`, and verifies project-labelled resources. Its separate managed project identity can differ from the native runtime's basename. The supplied labels allow actual-runtime discovery while containers still exist. Release cleanup does not retain that discovered project identity for retries after its containers have disappeared. A native `devcontainer down --volumes` while the worktree/config remain is the simpler explicitly scoped final cleanup for this lane. [Release teardown](https://github.com/branchbox/branchbox/blob/a00b3ee9a0acce000ee8f478dc0d6a8a24b3c7df/core/src/modules/compose.rs#L373), [release down](https://github.com/branchbox/branchbox/blob/a00b3ee9a0acce000ee8f478dc0d6a8a24b3c7df/core/src/devcontainer_runtime/runtime.rs#L935).

The verifier should declare the `container` isolation boundary. Internal data paths and ports may repeat; require distinct PostgreSQL system identifiers, Redis process `run_id`, service container IDs, volume IDs, and networks across A/B. Each source token must match its checkout and the module must be `/workspaces/<that-basename>/rwbapp`. Require A's unique marker in PostgreSQL's unprefixed `checkouts` table and Redis's unprefixed `rwb:checkout` key, then B's unique marker in its stores. Verify after initial setup, repeated commands, A teardown while B survives, and A restart. Across restart, the PostgreSQL cluster identifier and volume identity stay stable, postmaster start time and Redis `run_id` change, and durable markers survive. Do not reseed to hide data loss. This extends the existing fixture and `bench/rwb/verify.py` rather than replacing app work with `true`.

## Release limitations and fair comparison

These are source inferences pending execution, not measured failures:

- `feature start --runtime container` creates/configures the worktree; the container provider's `start_environment` is a no-op. `feature exec` with the recorded container runtime executes on the host in the worktree through its recorded container provider, so use `devcontainer exec` for application execution inside this fixture. [Provider implementation](https://github.com/branchbox/branchbox/blob/a00b3ee9a0acce000ee8f478dc0d6a8a24b3c7df/core/src/runtime/mod.rs#L319).
- Native Compose `up`, `exec`, and `down` pass `-p <workspace-basename>`. They do not use `COMPOSE_PROJECT_NAME` from the generated `.branchbox.env` as the runtime identity. Same-basename copies under different parents can collide. Unique run-specific feature names are necessary, and a same-basename collision task should report the observed result rather than silently renaming everything. [Project selection](https://github.com/branchbox/branchbox/blob/a00b3ee9a0acce000ee8f478dc0d6a8a24b3c7df/core/src/devcontainer_runtime/runtime.rs#L253).
- The native creation lifecycle calls `docker.exec` but discards its returned `DockerOutput.success/exit_code`. An executed hook that fails can still yield a successful `up` receipt. `devcontainer exec` does propagate child exit codes. Run dependency installation and readiness through explicit exec and gate on their receipts. Track this extra check as the adapter's work. [Lifecycle](https://github.com/branchbox/branchbox/blob/a00b3ee9a0acce000ee8f478dc0d6a8a24b3c7df/core/src/devcontainer_runtime/runtime.rs#L713), [exit propagation](https://github.com/branchbox/branchbox/blob/a00b3ee9a0acce000ee8f478dc0d6a8a24b3c7df/cli/src/commands/devcontainer.rs#L401).
- Native runtime parses more Dev Container fields than it implements. Its inspected execution path does not install `features`, execute `initializeCommand`/`postAttachCommand`, apply `userEnvProbe`, or implement `runServices`/`waitFor` semantics. Avoid assuming specification-complete behavior from accepted JSON. This plain Dockerfile and Compose dependency recipe avoids those requirements. If comparing an editor or `@devcontainers/cli` instead, identify it as a separate BranchBox-plus-Dev-Containers lane.
- A reused app's `up` receipt has `composeProjectName: null`; retain the original project identity and inspect labels. Registry/module `ready` status, a JSON `outcome`, and a running container are each insufficient evidence of authenticated app readiness.
- Ordinary template workflows share tool credential directories read-write across feature containers. The generic adapter also copies `.env`, `.env.local`, `.env.development`, and `.secrets` when present. Here `init --no-coding-agents`, a custom config without credential mounts, and no secret files keep authentication outside the workload. Data isolation and own-checkout execution do not establish credential isolation. [Credential injection](https://github.com/branchbox/branchbox/blob/a00b3ee9a0acce000ee8f478dc0d6a8a24b3c7df/core/src/modules/devcontainer.rs#L1117), [generic secret copying](https://github.com/branchbox/branchbox/blob/a00b3ee9a0acce000ee8f478dc0d6a8a24b3c7df/core/src/adapters/generic.rs#L26).
- Current main improves host-container cleanup and retains actual Compose project identity for retries. Those improvements are not evidence for the downloaded 0.13.4 binary. Pin main's commit and build with `cargo build --locked --release -p branchbox-cli --bin branchbox` only for a separately labelled source lane. [Current cleanup](https://github.com/branchbox/branchbox/blob/99bcd6868aa8676dab3bf71378b4e921fbb3e964/core/src/runtime/host_container.rs), [retained project cleanup](https://github.com/branchbox/branchbox/blob/99bcd6868aa8676dab3bf71378b4e921fbb3e964/core/src/modules/compose.rs#L632).

Before benchmarking, validate the recipe end to end serially, with a fresh host directory and bounded command deadlines. No benchmark lane is verified by this document. Practical tasks are cold and warm feature-plus-app setup, authenticated readiness, migrations, CRUD/cache integration tests, repeated commands, concurrent A/B markers, failed dependency recovery, persistent down/up, scoped final teardown, and copying/rebuilding the same configuration in another checkout. Report the worktree/config step separately from service start and dependency install so BranchBox's configuration-only startup is comparable to competitors' actual running applications.

For sharing, retain portable `devcontainer.json`, Dockerfile, Compose config, app source, `pyproject.toml`, `uv.lock`, scripts, runtime image digests, binary checksum, and source revision. Regenerate `.branchbox` registry and `.branchbox.env`, and allocate a new unique basename at the destination. Do not copy A's registry, source token, `.env` credentials, volume data, or resolved absolute workspace labels into B. The `RWB_WORKSPACE` variable binds the common config to the new checkout. Digest pins and `uv.lock` improve reproducibility; cold rebuilding still needs network access unless the benchmark separately provides an identical wheelhouse/image archive to every tool. BranchBox's sync is configuration distribution, not a bundled/offline dependency or database snapshot.
