# DevPod research

Research date: 2026-10-06. Implementation owner: Opus 5.5. This note supplies a local Docker recipe and source findings. It contains no workload execution or timing results. No containers or services were started, stopped, or deleted during research.

## Version and evidence

The official repository is `loft-sh/devpod`. GitHub's latest stable release resolved to [v0.6.15](https://github.com/loft-sh/devpod/releases/tag/v0.6.15), commit `33d20ff8806a3fee86d8f56ed50db6108b945fc2`. Main was inspected initially at `5a0efcbff6610ab114b421f68a890739a452e66b`, then the shallow source clone at `/tmp/stack-bench-sources/devpod` was switched to the release commit. All implementation links below use that release.

`devpod` was absent from this host's PATH. The official release binary downloaded to `/tmp/stack-bench-sources/devpod-v0.6.15-darwin-arm64` returned `v0.6.15`; `up --help`, `ssh --help`, and `status --help` also ran successfully. Its observed SHA-256 is `0c50934f4199732ff37e89708bc5c028ecda75d854a151cb2644e4ba39f4d7f7`. This is a locally measured hash, not a comparison against a publisher checksum. No existing benchmark image's DevPod version was verified. The built-in Docker provider definition declares `v0.0.1`; that is the provider schema's version, not the DevPod CLI version. [Provider definition](https://github.com/loft-sh/devpod/blob/33d20ff8806a3fee86d8f56ed50db6108b945fc2/providers/docker/provider.yaml).

Historical context read: `eval/REPORT.md`. It has no DevPod measurement. The current `bench/fixtures/app/pyproject.toml` requires Python 3.13 and pins application dependencies through `uv.lock`; use that fixture, not earlier researchers' Python 3.12 examples.

Anonymous registry manifest reads verified these immutable image indexes without pulling layers. All contain Linux amd64 and arm64 images. These pins match the proposed common Python 3.13 workload; they are not latest-runtime claims. Record the actual pulled platform manifest and image ID in run evidence. [Official Python image source](https://github.com/docker-library/python), [PostgreSQL image source](https://github.com/docker-library/postgres), [Redis image source](https://github.com/redis/docker-library-redis), [uv Docker instructions](https://docs.astral.sh/uv/guides/integration/docker/).

```text
python:3.13.16-slim-bookworm@sha256:a1165e272e578941b84abc79e4ab38a0305cd12803a5c4247979ac7655f4d641
postgres:17.11-alpine@sha256:b0f9560a2de083e2cc7382e75f808c7381a32852a7ec49117deedb300e552b24
redis:8.10.2-alpine@sha256:3811787313eba226a2ef38658c6ccb91cd5e110edc89c37767de373120a0e5a0
ghcr.io/astral-sh/uv:0.12.23@sha256:61d393e44e249f2e4b526b6c7ddcecce245946826e608e11c93ad4f5bba55b21
```

## Benchmark scope

DevPod overlaps directly with Stack for repeatable developer workspaces, application commands, isolated service environments, and lifecycle management. The fair local comparison is its Docker provider consuming a committed Dev Container plus Compose. DevPod adds workspace identity, source acquisition, SSH command entry, editor integration, and provider orchestration to that configuration. Database/cache processes and network/volume behavior are implemented by Docker Compose. Its cloud VM and Kubernetes provisioning are adjacent capabilities; local timings cannot assess those. [Architecture](https://github.com/loft-sh/devpod/blob/33d20ff8806a3fee86d8f56ed50db6108b945fc2/docs/pages/how-it-works/overview.mdx), [official Compose example](https://github.com/loft-sh/devpod/tree/33d20ff8806a3fee86d8f56ed50db6108b945fc2/examples/compose).

| Requirement | Supported path | Benchmark-owned work |
| --- | --- | --- |
| Pinned Python, PostgreSQL, Redis | Native Dev Container Dockerfile and Compose configuration | Supply images, dependency lock, service credentials and URLs |
| Dependency installation | Native lifecycle hook execution | `uv sync --frozen`, common application lock |
| Two independent local checkouts | Native workspace IDs and Compose projects | Assign distinct IDs, independent source folders, avoid global resource names |
| Service readiness | Compose health dependency conditions; synchronous DevPod lifecycle hooks | Define probes and authenticate from the application |
| Repeated commands | Native `devpod ssh ID --command ...` | Workload assertions and evidence collection |
| Stop/restart | Native `devpod stop` and `devpod up` | Verify every project service, not only workspace status |
| Persistent database/cache | Compose named volumes; optional Redis AOF | Choose durability settings and prove stored-marker persistence |
| Full cleanup | Native workspace deletion removes Compose containers/network | Named volumes require additional scoped cleanup |
| Portable reproduction | Repository config, Dockerfile, image digests, Git commit selection | Application lock, tool/provider pins, frozen reinstall check |
| Consolidated environment lock / frozen CLI mode | No such mode found in release source/help | Reproduce through committed inputs and dependency/image locks |
| Structured status | Native `status --output json`, `list --output json` | Compose/Docker inspect for service health and ownership |
| Wrong-instance protection | Workspace/container selection by IDs and labels | SQL/cache identity comparison; no native database-instance guard found |

The release's `.workspace.lock` files serialize local workspace operations. They are mutex files, not package or source locks. Do not count them as reproducibility evidence. [Workspace locking](https://github.com/loft-sh/devpod/blob/33d20ff8806a3fee86d8f56ed50db6108b945fc2/pkg/client/clientimplementation/workspace_client.go#L223).

## Configuration

Copy the common application, migrations, tests, `pyproject.toml`, and `uv.lock` into each independent checkout. Add these files. This recipe is source-grounded implementation input and remains unexecuted.

`.devcontainer/devcontainer.json`:

```json
{
  "name": "rwb-python",
  "dockerComposeFile": "compose.yaml",
  "service": "app",
  "runServices": ["app", "postgres", "redis"],
  "workspaceFolder": "/workspace",
  "remoteUser": "root",
  "updateRemoteUserUID": false,
  "userEnvProbe": "none",
  "overrideCommand": false,
  "shutdownAction": "none",
  "postCreateCommand": ["uv", "sync", "--frozen"],
  "postStartCommand": ["/workspace/.venv/bin/python", ".devcontainer/ready.py"]
}
```

`.devcontainer/Dockerfile`:

```dockerfile
FROM ghcr.io/astral-sh/uv:0.12.23@sha256:61d393e44e249f2e4b526b6c7ddcecce245946826e608e11c93ad4f5bba55b21 AS uv
FROM python:3.13.16-slim-bookworm@sha256:a1165e272e578941b84abc79e4ab38a0305cd12803a5c4247979ac7655f4d641
COPY --from=uv /uv /uvx /usr/local/bin/
ENV UV_PYTHON_DOWNLOADS=never UV_PYTHON_PREFERENCE=only-system
WORKDIR /workspace
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
      - type: bind
        source: ..
        target: /workspace
    environment:
      DATABASE_URL: postgresql://bench:bench@postgres:5432/bench
      REDIS_URL: redis://redis:6379/0
    depends_on:
      postgres:
        condition: service_healthy
      redis:
        condition: service_healthy
  postgres:
    image: postgres:17.11-alpine@sha256:b0f9560a2de083e2cc7382e75f808c7381a32852a7ec49117deedb300e552b24
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
  pgdata:
  redisdata:
```

There are no published ports, `container_name`, external networks, or explicit global volume names. Each app reaches its project services through Compose DNS. Two projects can use the same internal ports and paths. Redis's AOF and `appendfsync always` support a deterministic immediate persistence check; use the same durability policy across comparisons that test durable Redis writes.

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

DevPod calls `compose up -d`, without `--wait`. The app's `depends_on` health conditions gate initial dependency startup; the supplied post-start probe checks actual application credentials on every container restart. Probe execution belongs inside setup/readiness time. DevPod's release hook implementation waits for each process and returns errors; do not infer health from `up` output alone. [Compose startup](https://github.com/loft-sh/devpod/blob/33d20ff8806a3fee86d8f56ed50db6108b945fc2/pkg/devcontainer/compose.go#L385), [hook execution](https://github.com/loft-sh/devpod/blob/33d20ff8806a3fee86d8f56ed50db6108b945fc2/pkg/devcontainer/setup/lifecyclehooks.go#L23).

## Noninteractive command sequence

Supply absolute canonical checkout paths and globally unique lowercase workspace IDs per run. The harness should retain these values in its run manifest and pass the global flags on every fresh subprocess invocation. Do not rely on an interactive shell, current directory, auto-selected workspace, or user provider state. The following is a complete shell sequence once the source/config files above exist.

```sh
set -eu
DEVPOD=/tmp/stack-bench-sources/devpod-v0.6.15-darwin-arm64
RUN_DIR=$(mktemp -d /tmp/rwb-devpod.XXXXXX)
APP_A=/absolute/path/to/checkout-a
APP_B=/absolute/path/to/checkout-b
APP_C=/absolute/path/to/checkout-c
ID_A=rwb-unique-run-a
ID_B=rwb-unique-run-b
ID_C=rwb-unique-run-c
unset COMPOSE_PROJECT_NAME COMPOSE_FILE

dp() {
  "$DEVPOD" --devpod-home "$RUN_DIR/state" --context rwb --log-output raw "$@"
}

# Pin and preserve this definition in benchmark-owned config storage.
curl --fail --location --silent --show-error \
  https://raw.githubusercontent.com/loft-sh/devpod/33d20ff8806a3fee86d8f56ed50db6108b945fc2/providers/docker/provider.yaml \
  -o "$RUN_DIR/docker-provider.yaml"
dp provider add "$RUN_DIR/docker-provider.yaml" </dev/null
dp context set-options -o TELEMETRY=false -o SSH_ADD_PRIVATE_KEYS=false

dp up "$APP_A" --id "$ID_A" --provider docker --ide none \
  --ssh-config "$RUN_DIR/ssh_config" </dev/null
dp ssh "$ID_A" --user root --agent-forwarding=false --start-services=false \
  --command 'cd /workspace && uv run --frozen python -m rwbapp identity' </dev/null
dp ssh "$ID_A" --user root --agent-forwarding=false --start-services=false \
  --command 'cd /workspace && uv run --frozen pytest -q' </dev/null

dp up "$APP_B" --id "$ID_B" --provider docker --ide none \
  --ssh-config "$RUN_DIR/ssh_config" </dev/null
dp status "$ID_A" --output json </dev/null
dp list --output json </dev/null

# Application migrations/markers/CRUD/cache checks use the same SSH entry.
# Repeat this fresh process for command-overhead observations.
dp ssh "$ID_A" --user root --agent-forwarding=false --start-services=false \
  --command true </dev/null

dp stop "$ID_A" </dev/null
# Check A's container states with Docker, and B's identity through dp ssh.
dp up "$ID_A" --ide none --ssh-config "$RUN_DIR/ssh_config" </dev/null

# C is a fresh copy of source/config/uv.lock, without .venv or service data.
dp up "$APP_C" --id "$ID_C" --provider docker --ide none \
  --ssh-config "$RUN_DIR/ssh_config" </dev/null
dp ssh "$ID_C" --user root --agent-forwarding=false --start-services=false \
  --command 'cd /workspace && uv sync --frozen && uv run --frozen python -m rwbapp identity' </dev/null

# Preserve container/volume/network receipts before deletion.
dp delete "$ID_A" </dev/null
dp delete "$ID_B" </dev/null
dp delete "$ID_C" </dev/null
```

Use a Linux release binary in a Linux runner. Pin its release URL and observed hash separately. `--ide none` is documented; `--open-ide=false` alone can still install the IDE backend and would add irrelevant setup work. The private `--ssh-config` prevents the workflow from editing the user's usual SSH config. `--start-services=false` disables SSH credential/port-forwarding helpers, not PostgreSQL or Redis. Native inactivity tracking remains at its default; no timeout is configured in this recipe. [CLI quickstart](https://github.com/loft-sh/devpod/blob/33d20ff8806a3fee86d8f56ed50db6108b945fc2/docs/pages/getting-started/quickstart-devpod-cli.mdx), [up flags](https://github.com/loft-sh/devpod/blob/33d20ff8806a3fee86d8f56ed50db6108b945fc2/cmd/up.go#L105), [SSH flags](https://github.com/loft-sh/devpod/blob/33d20ff8806a3fee86d8f56ed50db6108b945fc2/cmd/ssh.go#L107).

## Identity, lifecycle, and cleanup checks

Capture Docker inspections by `com.docker.compose.project=<recorded-workspace-id>` and service label. DevPod computes the project name from workspace ID unless `COMPOSE_PROJECT_NAME` overrides it. IDs must be globally unique even if DevPod home/context differ because the generated Compose name does not include the context. Never reuse a fixed ID such as `app` across simultaneous runs. [Project naming and lookup](https://github.com/loft-sh/devpod/blob/33d20ff8806a3fee86d8f56ed50db6108b945fc2/pkg/compose/helper.go#L85).

For A/B isolation, require distinct app/PostgreSQL/Redis container IDs, distinct Docker volume source names, distinct PostgreSQL `system_identifier`, distinct Redis `run_id`, and segregated application markers. `data_directory=/var/lib/postgresql/data`, `dir=/data`, and ports 5432/6379 can correctly be identical across containers. Absolute path differences are an invalid cross-container requirement. URLs can also match because their service DNS resolves inside separate networks.

`devpod ssh` can start a stopped workspace before running the command. It must not be used as the negative endpoint probe after stopping A. Use `devpod status --output json` plus `docker inspect` to assert A's three containers are stopped, then execute B's identity check and confirm its original identifiers remain. Restart A explicitly with `up`; assert the same volume identities, PostgreSQL system identifier and stored rows. Redis's process `run_id` should change after restart while durable values remain. [SSH auto-start](https://github.com/loft-sh/devpod/blob/33d20ff8806a3fee86d8f56ed50db6108b945fc2/cmd/ssh.go#L194), [workspace status JSON](https://github.com/loft-sh/devpod/blob/33d20ff8806a3fee86d8f56ed50db6108b945fc2/cmd/status.go#L95).

The source chooses Compose stop/down from the app container's project label. `stop` calls `compose stop`; `delete` calls `compose down` without `--volumes`. Thus named volumes survive native delete. Record that behavior before supplying cleanup. For each previously recorded volume, inspect `com.docker.compose.project` and `com.docker.compose.volume`, require exact run/checkout ownership and membership in the pre-deletion receipt, then remove only that volume ID/name. Assert all recorded containers/networks/volumes are absent. Additional cleanup is `scripted`; do not credit it as native deletion. A cleanup failure is a failed receipt. `delete --force` can erase local workspace state despite provider failure, so omit it from success paths. [Stop/delete dispatch](https://github.com/loft-sh/devpod/blob/33d20ff8806a3fee86d8f56ed50db6108b945fc2/pkg/devcontainer/delete.go), [Compose stop/down arguments](https://github.com/loft-sh/devpod/blob/33d20ff8806a3fee86d8f56ed50db6108b945fc2/pkg/compose/helper.go#L130), [force-delete docs](https://github.com/loft-sh/devpod/blob/33d20ff8806a3fee86d8f56ed50db6108b945fc2/docs/pages/developing-in-workspaces/delete-a-workspace.mdx).

Record startup failure resources too. A failed dependency hook or image pull can leave a partial workspace; nonzero exit alone does not demonstrate cleanup. Validate a bad image digest and a failing post-start hook in new scratch workspace IDs. Host-port conflict testing is not applicable to the no-published-port recipe. A separately configured published-port test can assess Docker's failure reporting, but it must not replace the supported isolated-network baseline.

## Copy and lock boundaries

For C, copy committed configuration, source, `uv.lock`, image digests and the pinned provider definition, excluding `.venv`, state folders and volumes. Hash inputs before and after `uv sync --frozen`; verify actual Python/dependency versions and fresh service identity. That proves the supplied configuration/locks reproduce this workload. It does not prove a DevPod environment lock. Release source/help did not expose a package lock/frozen mode or a `devcontainer-lock.json` consumer. Features resolve their declared OCI references and cache extracted results, so a mutable Feature tag is not a substitute for an immutable artifact. This recipe avoids Features and pins Docker inputs explicitly. [Feature resolution](https://github.com/loft-sh/devpod/blob/33d20ff8806a3fee86d8f56ed50db6108b945fc2/pkg/devcontainer/feature/features.go#L90).

DevPod supports multiple workspaces from one Git repository through distinct `--id` values. A Git source can select a specific commit using `repo@sha256:<commit>` as documented. Branch/tag selection must not be presented as immutable locking. Verify `git rev-parse HEAD` inside the resulting workspace. An existing workspace ordinarily skips reacquiring source; `up ID` is a resume, not a fresh clone/update. Use a new ID for C. Local Docker workspaces use the existing source folder; remote providers copy local source, so a remote transfer result is a different scenario. [Source selection](https://github.com/loft-sh/devpod/blob/33d20ff8806a3fee86d8f56ed50db6108b945fc2/docs/pages/developing-in-workspaces/create-a-workspace.mdx), [local source mapping](https://github.com/loft-sh/devpod/blob/33d20ff8806a3fee86d8f56ed50db6108b945fc2/pkg/agent/agent.go#L188), [source acquisition/reuse](https://github.com/loft-sh/devpod/blob/33d20ff8806a3fee86d8f56ed50db6108b945fc2/cmd/agent/workspace/up.go#L259).

## Execution constraints and fair reporting

The CLI has official macOS/Linux amd64/arm64 binaries. The Docker provider requires an already usable Docker installation and does not install Docker itself. Compose is required for this recipe; DevPod first tries `docker-compose`, then `docker compose`. Record the executable that wins, since a private current standalone CLI and an older Desktop plugin can differ. A Linux container running the CLI also needs access to the daemon and source paths visible to that daemon. A mounted host Docker socket with an unmapped container-only bind path is not a valid local workspace setup. Prefer native host CLI with Docker Desktop on macOS or a Docker-enabled Linux host. Container-in-container isolation requires a separate deliberate daemon/provider arrangement and separate reporting. [Official install targets](https://github.com/loft-sh/devpod/blob/33d20ff8806a3fee86d8f56ed50db6108b945fc2/docs/pages/getting-started/install.mdx), [Compose CLI selection](https://github.com/loft-sh/devpod/blob/33d20ff8806a3fee86d8f56ed50db6108b945fc2/pkg/compose/helper.go#L60).

Measure cold workspace creation, warm new-checkout creation, SSH command entry, restart and frozen application reinstall separately. Creation includes Docker build/pull and DevPod agent setup; repeated SSH includes native tunnel/status work. Do not substitute `docker exec` for its command-entry timings. Compare it with the Dev Containers CLI and Compose baselines to expose DevPod's added orchestration cost and lifecycle capability. Report those overlapping scenarios without scoring untested remote provisioning as a local failure or win.
