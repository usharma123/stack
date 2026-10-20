# Tilt research for the application benchmark

Research date: 2026-10-06. Recipe and implementation inspection for Opus 5.5. No application, service lifecycle, or timing experiment ran during this research.

## Recommendation and verified version

Include Tilt as a container development orchestrator. Its supported [Docker Compose backend](https://docs.tilt.dev/docker_compose.html) requires Docker and Compose, with no Kubernetes cluster. Compare its normal `tilt ci` startup plus retained application-container commands. Report that command execution uses Compose, since Tilt has no general Compose application `exec` command.

The latest stable release observed was [v0.37.8](https://github.com/tilt-dev/tilt/releases/tag/v0.37.8), published 2026-10-01, source SHA `9f48972fd201a27565380c093f2bac8001a7b130`. Moving HEAD observed separately was `3ae48d2c15a56e32edd57eee3ec83436faa86900`. Source review used the release checkout at `/tmp/stack-bench-sources/tilt`.

Executed observations:

- No Tilt binary was initially on PATH. Downloaded official release archives into private `/tmp/stack-bench-tools`, without changing global installation.
- `/tmp/stack-bench-tools/tilt-0.37.8/tilt version` returned `v0.37.8, built 2026-10-01`. Archive SHA256 `2d396b13c479f74deb19cb2161a3f0858f6e03f26f15f8d3b3be982d09ff4484` matched the GitHub release asset digest. Extracted binary SHA256 `190255a6e64023b4cfe7a2bbecb34b41113d8c5db7d74f74555b8827e38cfb79`.
- Linux ARM64 binary is available at `/tmp/stack-bench-tools/tilt-0.37.8-linux.arm64/tilt`. Archive SHA256 `7a21281e830930be158202b7f8405ba62445613788b4d9708512ffe51f5cce90` matched the release asset digest. It was not executed on macOS.
- Release help confirms `ci --port 0 --timeout`, `up --stream --port 0`, and `down --delete-volumes`. `tilt alpha shell` explicitly selects a Kubernetes pod and opens an interactive shell; it is unsuitable for this Compose task. `tilt docker` forwards Docker commands, rather than resolving an application resource for exec.
- Existing private `eval/results/latest-2026-10-05/bin/mac/docker-compose version` returned `v5.6.0`. Host `uv` is older, `0.8.0`; the recipe supplies its own pinned uv in the application image.
- Anonymous registry manifest reads resolved `python:3.13.16-slim-bookworm` and `ghcr.io/astral-sh/uv:0.12.23` to the digests below. These are metadata observations, not image pulls or successful builds. uv's official latest release API reported `0.12.23`, published 2026-10-03.
- Extracted the recipe into `/tmp/stack-tilt-recipe-validation`. `bash -n`, Compose v5.6.0 `config --quiet`, and Tilt v0.37.8 `alpha tiltfile-result` passed. Evaluation returned exactly `postgres`, `redis`, and `app`, explicit app dependencies on both services, the requested project, and `wait=true`. A second evaluation with `RWB_RUN_TESTS=1` added `integration-tests`, with dependency on `app` and the selected private Compose executable; automatic Docker pruning was confirmed disabled. This validates parsing and the planned graph, not successful image build, dependency installation, or runtime tasks.

## Implementation facts that affect the adapter

These are code-derived conclusions, pending runtime validation.

- [`docker_compose` loads an explicit project name and optional `wait`, default false](https://github.com/tilt-dev/tilt/blob/9f48972fd201a27565380c093f2bac8001a7b130/internal/tiltfile/docker_compose.go#L60). A missing name falls back to the first Compose-file directory basename. Two copies named `app` therefore need explicit unique names. Compose named volumes/networks then provide separate project storage and DNS.
- [`Up` runs build per service and then `up --no-deps --remove-orphans --no-build -d SERVICE`](https://github.com/tilt-dev/tilt/blob/9f48972fd201a27565380c093f2bac8001a7b130/internal/dockercompose/client.go#L104). `wait=True` adds Compose's `--wait`. Compose service dependencies [become Tilt resource dependencies](https://github.com/tilt-dev/tilt/blob/9f48972fd201a27565380c093f2bac8001a7b130/internal/tiltfile/docker_compose.go#L464), and [block each resource's first build](https://github.com/tilt-dev/tilt/blob/9f48972fd201a27565380c093f2bac8001a7b130/internal/engine/buildcontrol/build_control.go#L128). Do not assume Compose itself starts dependencies under Tilt's `--no-deps` invocation.
- [CI's generic Compose runtime target treats a running resource as ready](https://github.com/tilt-dev/tilt/blob/9f48972fd201a27565380c093f2bac8001a7b130/internal/controllers/core/session/conv.go#L214). The source explicitly says it does not support Compose healthchecks in that readiness path. Configure `wait=True` and useful Compose healthchecks, then verify the app's actual DB/cache connectivity. A bare successful `tilt ci` with default `wait=False` is insufficient readiness evidence.
- [`Down` invokes `compose down --remove-orphans`, adding `--volumes` only on request](https://github.com/tilt-dev/tilt/blob/9f48972fd201a27565380c093f2bac8001a7b130/internal/dockercompose/client.go#L149). Ordinary `tilt down` preserves named volumes. It removes containers and the project network, so it is recreation rather than a process-only pause.
- [`TILT_DOCKER_COMPOSE_CMD` selects one executable path](https://github.com/tilt-dev/tilt/blob/9f48972fd201a27565380c093f2bac8001a7b130/internal/dockercompose/client.go#L361); otherwise Tilt tries `docker compose`, then `docker-compose`. Set it to the private current Compose binary to avoid silently using the older Desktop plugin. It accepts a path, not the string `docker compose` with arguments.
- [Exiting `tilt up` retains Compose containers](https://docs.tilt.dev/cli/tilt_up.html). Tilt's local `serve_cmd` processes have different lifetime semantics. Ending a Tilt monitor is not a service-stop receipt.
- [Local resources](https://docs.tilt.dev/local_resource.html) can express an application test task with dependencies and manual triggering. This is native task orchestration around a user-supplied command. Python package resolution, schema migrations, Redis durability settings, and application assertions remain application work.

## Concrete configuration

Copy the canonical `bench/fixtures/app` files into each checkout. Use that Python 3.13 fixture and its existing `uv.lock`; the older `eval/fixture` uses looser requirements and only smoke probes. Put the following files beside `pyproject.toml`, `uv.lock`, `rwbapp`, `migrations`, and `tests`.

`Dockerfile`:

```dockerfile
FROM ghcr.io/astral-sh/uv:0.12.23@sha256:61d393e44e249f2e4b526b6c7ddcecce245946826e608e11c93ad4f5bba55b21 AS uv
FROM python:3.13.16-slim-bookworm@sha256:a1165e272e578941b84abc79e4ab38a0305cd12803a5c4247979ac7655f4d641
COPY --from=uv /uv /usr/local/bin/uv
WORKDIR /app
ENV UV_PYTHON_DOWNLOADS=never UV_PYTHON_PREFERENCE=only-system
COPY pyproject.toml uv.lock ./
RUN uv sync --frozen --group dev --no-install-project
COPY rwbapp ./rwbapp
COPY migrations ./migrations
COPY tests ./tests
ENV PATH="/app/.venv/bin:$PATH"
CMD ["sleep", "infinity"]
```

`compose.yaml`:

```yaml
services:
  postgres:
    image: postgres:17.6-alpine@sha256:ef257d85f76e48da1c64832459b59fcaba1a4dac97bf5d7450c77753542eee94
    environment:
      POSTGRES_USER: postgres
      POSTGRES_PASSWORD: bench
      POSTGRES_DB: postgres
    volumes: [pgdata:/var/lib/postgresql/data]
    healthcheck:
      test: [CMD-SHELL, "pg_isready -U postgres -d postgres"]
      interval: 1s
      timeout: 3s
      retries: 60
  redis:
    image: redis:8.10.2-alpine@sha256:3811787313eba226a2ef38658c6ccb91cd5e110edc89c37767de373120a0e5a0
    command: [redis-server, --appendonly, "yes", --appendfsync, always]
    volumes: [redisdata:/data]
    healthcheck:
      test: [CMD, redis-cli, ping]
      interval: 1s
      timeout: 3s
      retries: 60
  app:
    image: ${RWB_PROJECT:?set RWB_PROJECT}-app:fixture
    build: .
    environment:
      DATABASE_URL: postgresql://postgres:bench@postgres:5432/postgres
      REDIS_URL: redis://redis:6379/0
      RWB_CHECKOUT: ${RWB_CHECKOUT:?set RWB_CHECKOUT}
    depends_on:
      postgres: {condition: service_healthy}
      redis: {condition: service_healthy}
    healthcheck:
      test: [CMD, python, -m, rwbapp, wait, --timeout, "2"]
      interval: 1s
      timeout: 5s
      retries: 60
volumes:
  pgdata: {}
  redisdata: {}
```

PostgreSQL/Redis digest references are shared with the [Compose research recipe](compose.md). Revalidate image identities in the centralized preparation step. Do not publish host ports or set global `container_name`, explicit volume `name`, or an external shared network for this container-internal task.

`Tiltfile`:

```python
project = os.getenv('RWB_PROJECT', '')
if not project:
    fail('RWB_PROJECT is required')
docker_prune_settings(disable=True)
docker_compose('compose.yaml', project_name=project, wait=True)
dc_resource('app', resource_deps=['postgres', 'redis'])
# Optional task lane. Leave unset for persistence/restart checks so tests
# cannot rewrite markers before the harness checks their survival.
if os.getenv('RWB_RUN_TESTS', '0') == '1':
    compose_binary = os.getenv('TILT_DOCKER_COMPOSE_CMD', '')
    compose_cmd = [compose_binary] if compose_binary else ['docker', 'compose']
    local_resource(
        'integration-tests',
        cmd=compose_cmd + ['-p', project, '-f', 'compose.yaml',
             'exec', '-T', 'app', 'python', '-m', 'pytest', '-q'],
        resource_deps=['app'],
    )
```

The optional task selects the same Compose executable as Tilt. Disabling Tilt's automatic Docker pruning keeps shared image-cache changes outside this study's lifecycle task.

## Fresh-shell command recipe

The runner must create unique run/checkout names, prepare isolated checkout directories, and replace these example absolute paths. Define each checkout's project/marker outside portable source files. No shell activation is needed.

```sh
set -eu
TILT=/tmp/stack-bench-tools/tilt-0.37.8/tilt
COMPOSE=/Users/utsavsharma/.t3/projects/stack/eval/results/latest-2026-10-05/bin/mac/docker-compose
export TILT_DOCKER_COMPOSE_CMD="$COMPOSE"
export RWB_PROJECT=rwbtilt20261006a RWB_CHECKOUT=a RWB_RUN_TESTS=0
cd /absolute/path/to/checkout-a
"$COMPOSE" -p "$RWB_PROJECT" -f compose.yaml config --quiet
"$TILT" ci --port 0 --timeout 5m --output-snapshot-on-exit tilt-ci.json </dev/null
app() { "$COMPOSE" -p "$RWB_PROJECT" -f compose.yaml exec -T app "$@" </dev/null; }
app python -m rwbapp wait --timeout 60
app python -m pytest -q
app python -m rwbapp migrate
app python -m rwbapp mark --checkout a
app python -m rwbapp crud --checkout a
app python -m rwbapp cache --checkout a
app python -m rwbapp check --checkout a --forbid b
app python -m rwbapp identity
app python -m rwbapp persist --checkout a
"$TILT" down
"$TILT" ci --port 0 --timeout 5m </dev/null
app python -m rwbapp persisted --checkout a
app python -m rwbapp check --checkout a --forbid b
app python -m rwbapp identity
# Destructive cleanup only after persistence evidence has been saved:
"$TILT" down --delete-volumes
```

Run checkout B with a separate directory, `RWB_PROJECT=rwbtilt20261006b`, `RWB_CHECKOUT=b`, and checkout/forbid arguments swapped. Start A and B serially while retaining both stacks, then interleave marker/identity checks. The application has no host-port dependency; its URLs use isolated project DNS.

Use `tilt up --stream --port 0` for the long-running development-monitor lane, with a managed runner subprocess, explicit readiness receipts, and recorded shutdown. It does not finish on startup. Use `ci` for a bounded startup gate. For a literal pause with retained containers, use the same project's `compose stop` and then `tilt ci` for start; label this as Compose stop through Tilt's supported backend. Ordinary `tilt down` is the native resource-removal operation and preserves named data volumes.

Repeat work using `app python -m rwbapp read --checkout a`, `identity`, or the functional suite. Do not substitute a fresh `tilt ci` for every command: it reloads the entire project and can run builds/tasks. If testing native task reruns, keep `tilt up` alive with an explicit unique HTTP port and invoke `tilt trigger --port PORT integration-tests`; `trigger` is asynchronous, so save completion/exit evidence from the task rather than treating trigger acceptance as test success. A disabled HTTP server cannot service `trigger`.

## Required assertions and fair comparison

1. Require command exit zero plus parsed fixture JSON `ok=true`. Save the full migration, CRUD, cache, identity, and persistence receipts. Assert actual Python 3.13 patch, Python dependency versions, PG17 version, and Redis8 version inside the application/services.
2. Require separate PostgreSQL `system_identifier`, separate Redis `run_id`, separate project-labeled container IDs, volume names/IDs, and network IDs while A and B coexist. Require exclusive A/B markers. Equal `/var/lib/postgresql/data`, `/data`, or internal ports in different containers are expected and do not show shared storage. Use `bench/rwb/verify.py`'s `container` boundary with adapter container/volume receipts.
3. Repeated unchanged startup should retain service identity and markers. Native `down`/`ci` should change container/process identity while retaining PostgreSQL cluster identity and both durable markers. Do not rerun mark/pytest before persistence assertions. Redis `run_id` should change on restart; it identifies a process, not durable data.
4. After final `down --delete-volumes`, query Docker successfully for this project's labels and require no owned containers, networks, or declared volumes. Retained image caches are outside resource cleanup. Never use daemon-wide pruning.
5. Copy `Tiltfile`, Compose file, Dockerfile, canonical source, migrations, tests, `pyproject.toml`, and `uv.lock` byte-for-byte into B. Save hashes, versioned Tilt/Compose binary identity, Docker daemon/platform identity, and resolved image references. Tilt has no separate Python/application dependency lock; image digests plus uv.lock and exact source provide this recipe's reproducibility.

Allow Tilt its native service graph, health-based Compose wait, image caching, optional `local_resource` tasks, and retained app exec. Python runtime/dependency installation belongs to the Dockerfile, not Tilt itself. The configuration is containerized, so report Docker image build/pull and VM/daemon cost separately from host command entry. Cold and warm runs need the same cache policy across container competitors. Live Update is a supported adjacent development capability, but do not imply it was measured by the static app recipe.

macOS uses Docker Desktop's Linux VM; Linux can use Engine directly. Official release assets cover Linux ARM64/x86_64 and Alpine builds, plus macOS ARM64/x86_64. A benchmark inside a container needs a reachable daemon and Compose executable. This Dockerfile copies source at build time, avoiding runtime bind-path assumptions on a remote daemon. Docker socket sharing still shares the daemon and image cache. Record those conditions and use native architecture images. A successful Tilt installation does not prove daemon access or application image pulls/builds.

No historical Tilt adapter or result was found under `eval/configs` or `eval/harness`. Existing Compose microcommand timings and legacy Python smoke tests are historical context only. This recipe needs centralized execution before claiming startup success, isolation, durability, cleanup, or performance.
