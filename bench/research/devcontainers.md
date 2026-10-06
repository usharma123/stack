# Dev Containers CLI research

Research date: 2026-10-06. This is configuration and source research, not a benchmark result. Benchmark implementation belongs to Opus 5.5. No workload containers, services, or timing experiments were started for this document.

## Version and evidence

Use `@devcontainers/cli@0.89.0`. The npm registry returned that version as current and the installed temporary copy returned `0.89.0`. Its published integrity is `sha512-LzaoOGKQ/Zql6PsiZ4hVIYVZagzWkD65aG/1ou5/Kly5Y1PtjLg1yn7qu+LZCzVoAl6DZZ/pbz8qOO4RLNlqMg==`. The released source tag resolves to `5dc7533314b5ba7ec3875c30143dfe1aec644870`. The current main branch was also inspected at `155f8b29e1c521bf365e2aebd74f3258a9160718`, but implementation links below use the release commit. The specification was inspected at `c95ffeed1d059abfe9ffbe79762dc2fa4e7c2421`.

The CLI needs Node.js 20 or newer. This host has Node `24.16.0`, npm `11.13.0`, Docker client `28.1.1`, and Docker Compose `v2.40.3-desktop.1`. `devcontainer` was initially absent from PATH. Only its version and help commands were executed after installing into `/tmp/stack-bench-sources/devcontainers/npm`. No lifecycle claim below is executed evidence. [Released package metadata](https://github.com/devcontainers/cli/blob/5dc7533314b5ba7ec3875c30143dfe1aec644870/package.json), [release notes](https://github.com/devcontainers/cli/blob/5dc7533314b5ba7ec3875c30143dfe1aec644870/CHANGELOG.md).

Read-only Docker image inspection found `postgres:17.6-alpine` cached for arm64 and `redis:8-alpine` cached with `REDIS_VERSION=8.10.2`. The Python and exact Redis images below were not found in that cache. Do not report the floating Redis cache tag as Redis 8.0.3. Public Docker Hub manifest requests, without pulling layers, verified these immutable multi-platform indexes contain Linux amd64 and arm64 images:

| Runtime | Exact image reference |
| --- | --- |
| Python 3.12.11 | `python:3.12.11-slim-bookworm@sha256:519591d6871b7bc437060736b9f7456b8731f1499a57e22e6c285135ae657bf7` |
| PostgreSQL 17.6 | `postgres:17.6-alpine@sha256:ef257d85f76e48da1c64832459b59fcaba1a4dac97bf5d7450c77753542eee94` |
| Redis 8.0.3 | `redis:8.0.3-alpine@sha256:25c0ae32c6c2301798579f5944af53729766a18eff5660bbef196fc2e6214a9c` |

These are workload pins, not claims that these runtime releases are the latest. The public manifest endpoints checked were `/v2/library/python/manifests/3.12.11-slim-bookworm`, `/v2/library/postgres/manifests/17.6-alpine`, and `/v2/library/redis/manifests/8.0.3-alpine` on `registry-1.docker.io`. Keep workload versions identical across competitors or explicitly report any mismatch.

Historical context read: `eval/harness/compose-benchmark.py`, `eval/fixture/pyproject.toml`, and `eval/fixture/tests/test_stack.py`. That Compose benchmark starts database containers and runs repeated commands inside PostgreSQL; it does not benchmark a Python development container. Its prior results are not Dev Containers results. Its original Python dependency ranges also need a shared exact lock before a reproducibility comparison.

## Supported workflow and responsibility

Dev Containers is a direct competitor for container-based application development. Its CLI consumes `devcontainer.json`, starts a Compose development environment, applies container/user/environment settings, runs setup hooks, and executes application commands. Docker Compose owns service processes, networks, volumes, health checks, and teardown. No editor is required for the CLI workflow. [Official Compose guide](https://containers.dev/guide/dockerfile), [CLI command registration](https://github.com/devcontainers/cli/blob/5dc7533314b5ba7ec3875c30143dfe1aec644870/src/spec-node/devContainersSpecCLI.ts#L95).

| Requirement | Native support | Configuration or application work |
| --- | --- | --- |
| Pinned Python with PostgreSQL and Redis | Dockerfile/image pins, Compose services, Dev Containers runtime configuration | Supply images, dependency lock, connection URLs, and tests |
| Two independent checkouts | Separate Compose project networks and named volumes | Assign a distinct stable project name to each checkout; keep the binding in benchmark receipts |
| Service readiness | Compose health checks and `depends_on: condition: service_healthy` | Define meaningful probes; test authenticated connections from Python |
| Install dependencies | CLI runs `postCreateCommand` inside the application container | The installation command and Python lock are supplied by the project |
| Repeated application commands | `devcontainer exec --workspace-folder ...` | The command itself and its assertions |
| Restart and persistent state | Compose stop/start and named volumes | Pick and document persistence semantics; verify markers after restart |
| Cleanup | Compose `down` and `down --volumes` | Capture project ownership, call Compose, and assert container/network/volume absence |
| Sharing | Portable repository configuration, Dockerfile, OCI images; Feature locks where Features are used | Commit application locks and scripts, image digests, CLI package lock, and source revision |

Health gating here is Compose's supported dependency mechanism. The CLI calls `compose up -d`, not `compose up --wait`. Therefore `devcontainer up` alone is not a generic declaration that every service is healthy. [CLI start implementation](https://github.com/devcontainers/cli/blob/5dc7533314b5ba7ec3875c30143dfe1aec644870/src/spec-node/dockerCompose.ts#L340), [Docker startup-order documentation](https://docs.docker.com/compose/how-tos/startup-order/).

## Concrete configuration

Put these files in each fresh checkout containing the common Python application and integration tests. This example can reuse `eval/fixture/tests/test_stack.py`, which consumes `DATABASE_URL` and `REDIS_URL`. The marker, persistence, and dependency-change checks described below must extend that common fixture; its current `SELECT 1` and `PING` checks do not prove isolation.

`.devcontainer/devcontainer.json`:

```json
{
  "name": "python-postgres-redis",
  "dockerComposeFile": "compose.yaml",
  "service": "app",
  "runServices": ["app", "postgres", "redis"],
  "workspaceFolder": "/workspace",
  "remoteUser": "root",
  "updateRemoteUserUID": false,
  "userEnvProbe": "none",
  "overrideCommand": false,
  "shutdownAction": "none",
  "postCreateCommand": [
    "sh", "-c",
    "python -m pip install --disable-pip-version-check --no-input --no-deps -r requirements.lock && python -m pip check && touch /tmp/post-create.done"
  ],
  "postStartCommand": ["python", ".devcontainer/ready.py"],
  "waitFor": "postStartCommand"
}
```

`shutdownAction: none` makes persistence explicit for an editor that consumes this file. CLI process exit already leaves the containers running. The other shutdown values concern a supporting tool's window/session shutdown; they do not add a `devcontainer down` command. [Specification reference](https://github.com/devcontainers/spec/blob/c95ffeed1d059abfe9ffbe79762dc2fa4e7c2421/docs/specs/devcontainerjson-reference.md#general-properties).

`.devcontainer/Dockerfile`:

```dockerfile
FROM python:3.12.11-slim-bookworm@sha256:519591d6871b7bc437060736b9f7456b8731f1499a57e22e6c285135ae657bf7
RUN python -m venv /opt/venv
ENV PATH=/opt/venv/bin:$PATH \
    PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1
WORKDIR /workspace
```

`.devcontainer/compose.yaml`:

```yaml
services:
  app:
    build:
      context: ..
      dockerfile: .devcontainer/Dockerfile
    command: ["sleep", "infinity"]
    init: true
    volumes:
      - type: bind
        source: ..
        target: /workspace
    environment:
      DATABASE_URL: postgresql://bench:bench@postgres:5432/bench
      REDIS_URL: redis://redis:6379/0
      BENCH_MARKER: ${COMPOSE_PROJECT_NAME:?Set a unique checkout project name}
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
      test: ["CMD", "pg_isready", "-h", "127.0.0.1", "-U", "bench", "-d", "bench"]
      interval: 1s
      timeout: 3s
      retries: 60
  redis:
    image: redis:8.0.3-alpine@sha256:25c0ae32c6c2301798579f5944af53729766a18eff5660bbef196fc2e6214a9c
    command: ["redis-server", "--appendonly", "yes"]
    volumes:
      - redisdata:/data
    healthcheck:
      test: ["CMD", "redis-cli", "ping"]
      interval: 1s
      timeout: 3s
      retries: 60
volumes:
  pgdata:
  redisdata:
```

There are no fixed host ports, `container_name`, external networks, or globally named volumes. Python uses service DNS inside the project network. The same internal ports are correct in both projects. Redis enables AOF so the persistent-volume test measures configured durable behavior rather than default Redis's write-loss window.

`requirements.lock`, a complete version list for this Linux Python 3.12 example:

```text
psycopg[binary]==3.2.9
psycopg-binary==3.2.9
redis==6.2.0
pytest==8.4.1
iniconfig==2.1.0
packaging==25.0
pluggy==1.6.0
Pygments==2.19.2
typing_extensions==4.14.1
```

PyPI release metadata was checked for each version and its dependencies. Package installation was not executed. This supplies exact versions, but not artifact hashes. For an immutable, offline reproduction task, generate the shared hash-locked requirements and platform wheelhouse during fixture preparation, use `--require-hashes`, and give every competitor the same artifacts. Do not treat this version-only example as a completed artifact-verification test.

`.devcontainer/ready.py`:

```python
import os
import time
import psycopg
import redis

deadline = time.monotonic() + 60
while True:
    try:
        with psycopg.connect(os.environ["DATABASE_URL"], connect_timeout=2) as db:
            assert db.execute("SELECT 1").fetchone() == (1,)
        cache = redis.Redis.from_url(
            os.environ["REDIS_URL"], socket_connect_timeout=2, socket_timeout=2
        )
        assert cache.ping()
        print("application-connections-ready")
        break
    except (psycopg.Error, redis.RedisError, AssertionError):
        if time.monotonic() >= deadline:
            raise
        time.sleep(0.2)
```

This is an application probe supplied by the user. Compose owns the health checks. The CLI owns hook execution. Keep probe time in environment readiness, not outside the timed start while claiming an already usable environment.

## Commands for the benchmark author

These are commands to implement and later execute, not results from this research. Run with stdin detached and save stdout, stderr, exit status, and elapsed time separately. Both projects must contain the files above and the shared tests. Use absolute, canonical workspace paths, especially on macOS where `/tmp` aliases `/private/tmp`.

Install the CLI once outside timed workload stages into benchmark-owned storage. Commit the generated package lock and use `npm ci` for a fresh reproduction:

```sh
mkdir -p /tmp/devcontainers-bench-tools
npm install --prefix /tmp/devcontainers-bench-tools --save-exact --no-audit --no-fund @devcontainers/cli@0.89.0
export DEVCONTAINER=/tmp/devcontainers-bench-tools/node_modules/.bin/devcontainer
"$DEVCONTAINER" --version
```

For the following example, substitute actual absolute checkout paths. The names are unique to one run and remain stable through that run's restart/persistence checks. The benchmark implementation should generate and persist these bindings before attempting `up`, including when startup fails.

```sh
export CHECKOUT_A=/absolute/path/run-unique/a/app
export CHECKOUT_B=/absolute/path/run-unique/b/app
export PROJECT_A=dcbench-rununique-a
export PROJECT_B=dcbench-rununique-b
export RECEIPTS=/absolute/path/run-unique/receipts
mkdir -p "$RECEIPTS"

COMPOSE_PROJECT_NAME="$PROJECT_A" "$DEVCONTAINER" up --workspace-folder "$CHECKOUT_A" \
  --user-data-folder "$RECEIPTS/cli-a" --skip-post-attach \
  > "$RECEIPTS/up-a.json" 2> "$RECEIPTS/up-a.stderr" < /dev/null
COMPOSE_PROJECT_NAME="$PROJECT_B" "$DEVCONTAINER" up --workspace-folder "$CHECKOUT_B" \
  --user-data-folder "$RECEIPTS/cli-b" --skip-post-attach \
  > "$RECEIPTS/up-b.json" 2> "$RECEIPTS/up-b.stderr" < /dev/null

"$DEVCONTAINER" exec --workspace-folder "$CHECKOUT_A" python -m pytest -q < /dev/null
"$DEVCONTAINER" exec --workspace-folder "$CHECKOUT_B" python -m pytest -q < /dev/null
"$DEVCONTAINER" exec --workspace-folder "$CHECKOUT_A" python -m pip check < /dev/null
"$DEVCONTAINER" exec --workspace-folder "$CHECKOUT_A" python -c 'import os,sys; print(sys.version); print(os.getcwd())' < /dev/null
```

The benchmark must check each exit status before proceeding. Parse each `up` stdout as JSON and require `outcome == "success"`, a nonempty `containerId`, and the intended `composeProjectName`. Save Docker inspect output for the returned app ID and project-filtered database/cache IDs. `exec` with text log format preserves application output; using `--log-format json` changes how it is delivered. [Provision result fields](https://github.com/devcontainers/cli/blob/5dc7533314b5ba7ec3875c30143dfe1aec644870/src/spec-node/devContainers.ts#L84), [exec output and exit handling](https://github.com/devcontainers/cli/blob/5dc7533314b5ba7ec3875c30143dfe1aec644870/src/spec-node/devContainersSpecCLI.ts#L1356).

`COMPOSE_PROJECT_NAME` controls provisioning. `exec --workspace-folder` discovers the app through its workspace/config labels, so it does not need that environment variable. For a command that must remain bound to the exact returned container, pass `--container-id` together with `--workspace-folder`, then separately benchmark the normal workspace lookup path. Do not substitute direct `docker exec` timings for Dev Containers CLI entry timings.

Capture ownership before cleanup:

```sh
docker ps -a --filter "label=com.docker.compose.project=$PROJECT_A" \
  --format '{{.ID}} {{.Label "com.docker.compose.service"}}'
docker volume ls --filter "label=com.docker.compose.project=$PROJECT_A" --format '{{.Name}}'
docker network ls --filter "label=com.docker.compose.project=$PROJECT_A" --format '{{.ID}} {{.Name}}'
```

Stop and resume A without deleting its containers or volumes:

```sh
COMPOSE_PROJECT_NAME="$PROJECT_A" docker compose -p "$PROJECT_A" \
  -f "$CHECKOUT_A/.devcontainer/compose.yaml" stop
COMPOSE_PROJECT_NAME="$PROJECT_A" "$DEVCONTAINER" up --workspace-folder "$CHECKOUT_A" \
  --user-data-folder "$RECEIPTS/cli-a" --skip-post-attach < /dev/null
"$DEVCONTAINER" exec --workspace-folder "$CHECKOUT_A" python -m pytest -q < /dev/null
```

For container removal with data retention, use `down` without `--volumes`, then `devcontainer up` with the same workspace/project binding. For full teardown, use the declared Compose file and captured project name:

```sh
COMPOSE_PROJECT_NAME="$PROJECT_A" docker compose -p "$PROJECT_A" \
  -f "$CHECKOUT_A/.devcontainer/compose.yaml" down --volumes --remove-orphans
"$DEVCONTAINER" exec --workspace-folder "$CHECKOUT_B" python -m pytest -q < /dev/null
COMPOSE_PROJECT_NAME="$PROJECT_B" docker compose -p "$PROJECT_B" \
  -f "$CHECKOUT_B/.devcontainer/compose.yaml" down --volumes --remove-orphans
```

This configuration declares every service, volume, and network needed for teardown; generated CLI overrides add devcontainer settings to `app`. Capture `com.docker.compose.project.config_files` as diagnostic evidence rather than relying on temporary override paths remaining available. Save an immutable copy of the original teardown config and project binding outside the checkout before a deleted-workspace scenario. Never use a broad Docker prune. [Compose down semantics](https://docs.docker.com/reference/cli/docker/compose/down/), [generated overrides and restored config paths](https://github.com/devcontainers/cli/blob/5dc7533314b5ba7ec3875c30143dfe1aec644870/src/spec-node/dockerCompose.ts#L365).

For each cleaned project, require every query below to exit zero and return empty stdout. A failed Docker query is not proof of absence. Also inspect every captured container/network/volume identity and require the resource to be absent; keep raw query errors to distinguish disappearance from daemon unavailability.

```sh
docker ps -aq --filter "label=com.docker.compose.project=$PROJECT_A"
docker volume ls -q --filter "label=com.docker.compose.project=$PROJECT_A"
docker network ls -q --filter "label=com.docker.compose.project=$PROJECT_A"
```

## Lifecycle, identity, and reproducibility details

1. The Compose project name resolution checks the process's `COMPOSE_PROJECT_NAME`, a `.env` value, a Compose `name`, then directory-derived names. The fallback can collide when both checkouts have basename `app`. The Compose startup lookup uses project and service labels, so adding different `--id-label` values alone does not fix a shared Compose project name. Explicit distinct project names are the supported solution. [Project-name resolution and service lookup](https://github.com/devcontainers/cli/blob/5dc7533314b5ba7ec3875c30143dfe1aec644870/src/spec-node/dockerCompose.ts#L630).
2. Default devcontainer identity labels include `devcontainer.local_folder` and `devcontainer.config_file`. `exec` resolves these labels to find the app. Save them alongside `com.docker.compose.project`, `com.docker.compose.service`, app mounts, image IDs, and network attachments. A JSON success result does not prove the expected checkout was mounted. [Container/label resolution](https://github.com/devcontainers/cli/blob/5dc7533314b5ba7ec3875c30143dfe1aec644870/src/spec-node/utils.ts#L720).
3. `postCreateCommand` is creation-scoped and `postStartCommand` is start-scoped. Their marker files use creation/start timestamps. Repeating `up` against a running app does not reinstall Python dependencies, and `exec` does not run setup hooks. A changed requirements lock needs an explicit dependency-install command or recreation. Do not silently reset the container for every repeated command. [Hook execution and marker handling](https://github.com/devcontainers/cli/blob/5dc7533314b5ba7ec3875c30143dfe1aec644870/src/spec-common/injectHeadless.ts#L368).
4. A running app causes Compose provisioning to skip `startContainer`. Therefore repeated `devcontainer up` does not necessarily repair a stopped database alongside a still-running app. Test this as a recovery contract, distinct from healthy repeated start. Restart the declared service with Compose for the supported repair path. [Existing-container branch](https://github.com/devcontainers/cli/blob/5dc7533314b5ba7ec3875c30143dfe1aec644870/src/spec-node/dockerCompose.ts#L34).
5. With CLI defaults, user hooks run through `postStartCommand` and `postAttachCommand`; `waitFor` controls early stopping when `--skip-non-blocking-commands` is enabled. This example explicitly sets `waitFor: postStartCommand` and does not request early skipping. Do not use `--skip-post-create` during normal first-run timing because it skips dependency installation and later hooks. [CLI flags](https://github.com/devcontainers/cli/blob/5dc7533314b5ba7ec3875c30143dfe1aec644870/src/spec-node/devContainersSpecCLI.ts#L135), [hook ordering](https://github.com/devcontainers/cli/blob/5dc7533314b5ba7ec3875c30143dfe1aec644870/src/spec-common/injectHeadless.ts#L368).
6. Since CLI 0.87.0, Feature lock generation is enabled by default and `--frozen-lockfile` is stable. `devcontainer-lock.json` locks remote Features, not Python dependencies, container image tags, apt packages, or application source. This fixture uses no Features; the implementation returns early when none are configured, so do not claim an empty Feature lock verifies this environment. If benchmarking a Feature-based variant, commit the generated Feature lock and run a fresh build/up with `--frozen-lockfile`. [Feature configuration early return](https://github.com/devcontainers/cli/blob/5dc7533314b5ba7ec3875c30143dfe1aec644870/src/spec-configuration/containerFeaturesConfiguration.ts#L478), [Feature lock scope](https://github.com/devcontainers/spec/blob/c95ffeed1d059abfe9ffbe79762dc2fa4e7c2421/docs/specs/devcontainer-lockfile.md), [frozen validation](https://github.com/devcontainers/cli/blob/5dc7533314b5ba7ec3875c30143dfe1aec644870/src/spec-configuration/lockfile.ts#L48).

## Fair tasks and required receipts

| Task | Required evidence |
| --- | --- |
| Fresh usable checkout | Time through image acquisition/build, dependency install, readiness, and passing integration tests. Record warm Docker daemon separately. Save versions, image IDs/digests, lock hashes, command output, and exits. |
| Warm command entry | Reuse the already configured app. Repeat an application command with verified output, not `true` in the PostgreSQL container. Include CLI lookup/environment setup in timing. |
| Independent checkouts | Use the same checkout basename in distinct parent directories. Write marker A and marker B to the same table/key names. Read each through its own Python app and require exact equality to its marker and inequality to the other. |
| Independent teardown | Remove A with its project-scoped Compose command. Require B's database marker, cache marker, app container ID, and mounted source to remain correct. |
| Restart/data persistence | Verify both markers after stop/up and down/up without deleting named volumes. Verify the expected loss after full down with volumes and a fresh up. |
| Dependency change | Change the common pinned requirement in a disposable copy, explicitly reinstall or recreate, then assert the new imported package version and passing tests. Report the required user action. |
| Wrong endpoint rejection | Deliberately point A's Python client at B in an owned test fixture. Require the expected marker/identity assertion to fail. An ordinary successful `SELECT 1` is insufficient. |
| Startup failure and cleanup | Use an owned invalid service configuration. Preserve failed-up stdout/stderr and ownership binding. Run project-scoped cleanup in `finally`, including partial starts; then assert container, network, and volume absence by labels and captured IDs. |
| Fresh copy | Reproduce from the same application revision, configuration files, Dockerfile, dependency locks/wheelhouse, image digests, and CLI package lock. Change only checkout/project identity bindings. |

Do not make absence of Stack-specific bundle syntax, host port allocation, or lease commands an automatic failure. This workflow uses private service networks and containerized Python, which solve the common task differently. Compare application readiness, correct data, repeatability, independent projects, practical cleanup, and explicit recovery actions. Also report the Docker daemon/VM prerequisite and resource cost rather than concealing it in an already-running host.

## Platform constraints and untested areas

The shown shell/config works with Linux containers on macOS Docker Desktop and on Linux Docker Engine with Compose. The verified image indexes cover amd64 and arm64 without requiring emulation. Python inside the container runs on Linux, even when the host is macOS. Root simplifies this controlled fixture; on Linux it can create root-owned files in the source bind mount, so preserve `PYTHONDONTWRITEBYTECODE`, put the venv outside the mount, and use a matched non-root variant if developer ownership behavior is part of the study.

A benchmark running inside a container needs explicit access to a Docker daemon. Sibling-container workspace bind paths are interpreted on the daemon host; mounting its socket alone does not make arbitrary container-local source paths valid. Use host-native execution or an isolated Docker-in-Docker daemon with a consistent filesystem boundary. Keep that choice identical for other container workflows and report its startup cost.

Remote Docker, Windows/WSL, Podman, IDE attachment, `forwardPorts`, remote provisioning, GUI shutdown, and crash recovery were not executed or established by this research. Codespaces, DevPod, and Coder add provisioning/editor/session behavior and should remain separate competitors or adjacent tools. Do not infer their guarantees from the reference CLI. [Supported tools and services](https://containers.dev/supporting).

## Static validation performed

The JSON block parsed successfully. The Python readiness script passed an AST syntax check, and every shell block passed `sh -n`. Docker Compose `config --format json` accepted the Compose block and preserved its health-based dependencies. This command only resolved configuration and did not start services. Source reads, npm version/help, image metadata inspection, registry manifest metadata, and PyPI metadata checks support the findings above. Runtime success, isolation, cleanup, persistence, and performance remain to be measured by the benchmark implementation.
