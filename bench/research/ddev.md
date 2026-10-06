# DDEV research for the application benchmark

Research date: 2026-10-06. This note is an implementation handoff for Opus 5.5. No timing tests or service lifecycle operations were executed.

## Version and applicability

The current official release observed was [DDEV v1.25.4](https://github.com/ddev/ddev/releases/tag/v1.25.4). Its checked-out source is `5da91aeb9ebab0b0e66171c450b72099308d332c`, at `/tmp/stack-bench-sources/ddev`. DDEV is a container development environment with a PHP web service and PHP/CMS project types. It has native PostgreSQL configuration and supported custom Compose services. Python 3.13 is applicable through a custom app container; calling Python a built-in DDEV language environment would be inaccurate. The [project-type matrix](https://github.com/ddev/ddev/blob/5da91aeb9ebab0b0e66171c450b72099308d332c/pkg/ddevapp/apptypes.go#L88) and [web/db template](https://github.com/ddev/ddev/blob/5da91aeb9ebab0b0e66171c450b72099308d332c/pkg/ddevapp/app_compose_template.yaml) establish this boundary.

Read-only local inspection found Docker at `/opt/homebrew/bin/docker`, no `ddev` on PATH, and no cached DDEV images. PostgreSQL `17.6-alpine` and floating `redis:8-alpine` are cached, but that does not prove their runtime version or a working DDEV installation. A benchmark must install a checksum-verified v1.25.4 binary privately and record its hash and `ddev version` output. Do not silently substitute another release.

Official [runtime installation instructions](https://docs.ddev.com/en/stable/users/install/docker-installation/) cover macOS and Linux Docker providers, Windows/WSL2, and Podman. This recipe targets macOS/Linux with Docker and Linux amd64/arm64 containers. It requires bind mounts of both checkout directories into the Docker VM, registry access for uncached layers, and a running Docker daemon. On Colima/Lima the checkout must be in a mounted host directory. Running DDEV inside a Linux container additionally requires Docker socket access and matching host-visible paths for bind mounts; a container with neither is a blocked preflight, not an unsupported Python result. The default router uses ports 80/443 and is shared across projects. Capture its state and startup cost; do not power off existing user projects.

## Native behavior and recipe work

| Requirement | DDEV behavior | Work supplied by the recipe |
| --- | --- | --- |
| Python and dependency pins | Custom service image/build supported | Dockerfile with Python/uv image digests and fixture `uv.lock` |
| PostgreSQL | Native `database: {type: postgres, version: "17"}` | Explicit patch image/build pin for equal workload versions |
| Redis | Official add-on available; not a built-in database type | Pinned custom Compose Redis service and persistence settings |
| Independent checkouts | Project name controls container, network and database volume identities | Distinct names such as `rwbddev-a-RUN` and `rwbddev-b-RUN` |
| Readiness | Start waits on labelled project containers and Docker healthchecks | Meaningful Redis/app probes, then application identity verification |
| Repeated commands | `ddev exec --service app` in retained container | Explicit setup, test and application commands |
| Stop/restart/persistence | Stop removes project containers while keeping data; start recreates them | Verify PostgreSQL/Redis markers before and after recreation |
| Teardown | Project-scoped delete removes database and nonexternal service volumes | Save receipts and inspect project labels afterward |
| Reproducible config | Commit `.ddev` configuration/custom files; add-ons can distribute them | Dependency lock, digest pins, tool binary pin, generated-config receipts |

The official [Redis add-on](https://github.com/ddev/ddev-redis) is relevant evidence of a maintained idiomatic extension. Source reviewed at `d2d7035c2d98ab4da347daf6a45071250d5c01aa` mounts a project-scoped `redis:/data` volume and configuration files, with persistence enabled by default. [Compose service](https://github.com/ddev/ddev-redis/blob/d2d7035c2d98ab4da347daf6a45071250d5c01aa/docker-compose.redis.yaml), [installation actions](https://github.com/ddev/ddev-redis/blob/d2d7035c2d98ab4da347daf6a45071250d5c01aa/install.yaml). Its default image is floating `redis:7` and its service has no explicit healthcheck. The benchmark recipe below uses the same supported custom-service mechanism with common version pins and a ping probe, avoiding Drupal-specific add-on actions. A separate add-on-sharing task can use the official add-on with an immutable release and committed installed files.

## Implementation checked

These are code-derived expectations, not executed lifecycle results.

- [Compose generation and fixups](https://github.com/ddev/ddev/blob/5da91aeb9ebab0b0e66171c450b72099308d332c/pkg/ddevapp/compose_yaml.go#L200) merge custom files, inject ownership labels and attach every service to both the project default network and shared `ddev_default`. Use the unique container names in application URLs instead of trusting generic `db`/`redis` aliases on the shared network. This is endpoint selection, not a security boundary between untrusted projects.
- [Startup](https://github.com/ddev/ddev/blob/5da91aeb9ebab0b0e66171c450b72099308d332c/pkg/ddevapp/ddevapp.go#L2067) waits for web/db, then [all labelled additional containers](https://github.com/ddev/ddev/blob/5da91aeb9ebab0b0e66171c450b72099308d332c/pkg/ddevapp/ddevapp.go#L2150), before post-start hooks. [Health inspection](https://github.com/ddev/ddev/blob/5da91aeb9ebab0b0e66171c450b72099308d332c/pkg/dockerutil/containers.go#L493) treats a running container without a healthcheck as healthy. A sleeping Python container is therefore not proof that PostgreSQL or Redis can answer application queries.
- [Exec command](https://github.com/ddev/ddev/blob/5da91aeb9ebab0b0e66171c450b72099308d332c/cmd/ddev/cmd/exec.go#L40) can start a stopped project automatically. Use explicit startup before repeated-command measurements. Pass `--raw` explicitly to preserve argv; despite its declared default, raw execution is selected by whether the flag was supplied. Flags must precede the command. [Exec implementation](https://github.com/ddev/ddev/blob/5da91aeb9ebab0b0e66171c450b72099308d332c/pkg/ddevapp/ddevapp.go#L2570) suppresses TTY allocation unless stdin/stdout are real terminals and propagates command exit status.
- [Identity names](https://github.com/ddev/ddev/blob/5da91aeb9ebab0b0e66171c450b72099308d332c/pkg/ddevapp/ddevapp.go#L3897) derive the PostgreSQL volume from the project name, not checkout path. Copying identical names into two checkouts is not an isolation test. Capture unique names and absolute approots.
- [Stop/delete](https://github.com/ddev/ddev/blob/5da91aeb9ebab0b0e66171c450b72099308d332c/pkg/ddevapp/ddevapp.go#L3448) keep data unless removal is requested. Delete removes nonexternal custom volumes as well as the native PostgreSQL volume. [CLI deletion](https://github.com/ddev/ddev/blob/5da91aeb9ebab0b0e66171c450b72099308d332c/cmd/ddev/cmd/delete.go#L95) supports `--yes --omit-snapshot`; snapshots otherwise add work to teardown. Use `--clean-containers=false` to avoid the default broad cleanup of obsolete DDEV containers.
- [Image selection](https://github.com/ddev/ddev/blob/5da91aeb9ebab0b0e66171c450b72099308d332c/pkg/docker/images.go#L60) uses floating `postgres:17` for native PostgreSQL 17. Major-version configuration alone does not pin a patch image. [Generated DB Dockerfile](https://github.com/ddev/ddev/blob/5da91aeb9ebab0b0e66171c450b72099308d332c/pkg/ddevapp/config.go#L1257) adds DDEV's healthcheck. Preserve it when pinning the base build argument below.

## Concrete recipe for the canonical fixture

Copy `bench/fixtures/app` into two fresh independent checkout directories first. The recipe assumes each checkout root contains its `pyproject.toml`, `uv.lock`, `rwbapp`, tests and migrations. This fixture requires Python 3.13. Do not reuse a Python 3.12 recipe from an older competitor note.

Anonymous manifest reads in this research verified `python:3.13.16-slim-bookworm`, `postgres:17.6-bookworm` and `ghcr.io/astral-sh/uv:0.12.23`, including Linux amd64/arm64 indexes, without pulling layers. These are workload pins, not latest-runtime claims. Redis uses the common Compose research digest; the implementation must independently verify it before execution. DDEV's native PostgreSQL image customization runs Bash and apt commands, so use Debian PostgreSQL rather than the cached Alpine variant. Both run PostgreSQL 17.6, but record the OS/image difference in the comparison.

`.ddev/config.yaml`:

```yaml
name: rwbddev-a-RUN
type: php
docroot: ""
database:
  type: postgres
  version: "17"
dbimage: postgres:17.6-bookworm
omit_containers: [ddev-ssh-agent]
performance_mode: none
```

Replace `RUN` with a lowercase run identifier and choose `rwbddev-b-RUN` in B. Do this before first start. Do not attempt `omit_containers: [web]`; supported per-project omissions are db and ssh-agent, so the PHP service overhead is part of this workflow. [Configuration reference](https://docs.ddev.com/en/stable/users/configuration/config/#omit_containers).

`.ddev/app/Dockerfile`:

```dockerfile
FROM python:3.13.16-slim-bookworm@sha256:a1165e272e578941b84abc79e4ab38a0305cd12803a5c4247979ac7655f4d641
COPY --from=ghcr.io/astral-sh/uv:0.12.23@sha256:61d393e44e249f2e4b526b6c7ddcecce245946826e608e11c93ad4f5bba55b21 /uv /usr/local/bin/uv
WORKDIR /workspace
CMD ["sleep", "infinity"]
```

`.ddev/docker-compose.workload.yaml`:

```yaml
services:
  db:
    build:
      args:
        BASE_IMAGE: postgres:17.6-bookworm@sha256:f3bd19c606e442c3d7bdfa8002e03fe260a1023351e0ea4598032022b68dd6e3
  app:
    container_name: ddev-${DDEV_SITENAME}-app
    image: ddev-${DDEV_SITENAME}-app-built
    build:
      context: app
    working_dir: /workspace
    user: "${DDEV_UID}:${DDEV_GID}"
    environment:
      DATABASE_URL: postgresql://db:db@ddev-${DDEV_SITENAME}-db:5432/db
      REDIS_URL: redis://ddev-${DDEV_SITENAME}-redis:6379/0
      UV_PYTHON_DOWNLOADS: never
      UV_CACHE_DIR: /tmp/uv-cache
      HOME: /tmp
    volumes:
      - ../:/workspace
    restart: "no"
    healthcheck:
      test: [CMD, python, -c, "import socket; socket.create_connection(('ddev-${DDEV_SITENAME}-db',5432),2).close(); socket.create_connection(('ddev-${DDEV_SITENAME}-redis',6379),2).close()"]
      interval: 1s
      timeout: 5s
      retries: 60
  redis:
    container_name: ddev-${DDEV_SITENAME}-redis
    image: redis:8.10.2-alpine@sha256:3811787313eba226a2ef38658c6ccb91cd5e110edc89c37767de373120a0e5a0
    command: [redis-server, --appendonly, "yes", --appendfsync, always]
    volumes:
      - redisdata:/data
    restart: "no"
    healthcheck:
      test: [CMD-SHELL, "redis-cli ping | grep -qx PONG"]
      interval: 1s
      timeout: 3s
      retries: 60
volumes:
  redisdata: {}
```

The app probe checks ports only; the subsequent fixture `wait` and `identity` commands are the SQL/Redis readiness and endpoint receipts. The DB retains DDEV's own generated healthcheck. Overriding `BASE_IMAGE` pins the actual build base while keeping `dbimage` a valid tag for DDEV's generated built-image name, which appends the project name. Putting a digest directly in `dbimage` would produce an invalid suffixed image reference in that template. Confirm the merged build argument through `ddev utility compose-config` before building.

From either checkout, in a fresh noninteractive shell with the private v1.25.4 binary on PATH:

```sh
set -eu
export DDEV_NONINTERACTIVE=true DDEV_NO_INSTRUMENTATION=true NO_COLOR=1
ddev version
ddev utility compose-config > ddev-compose-receipt.yaml
ddev start </dev/null
ddev describe --json-output > ddev-describe-receipt.json
ddev exec --service app --raw -- python --version
ddev exec --service app --raw -- uv --version
ddev exec --service app --raw -- uv sync --locked --python /usr/local/bin/python
ddev exec --service app --raw -- uv run --no-sync python -m rwbapp wait
ddev exec --service app --raw -- uv run --no-sync python -m rwbapp identity
ddev exec --service app --raw -- uv run --no-sync python -m rwbapp migrate
ddev exec --service app --raw -- uv run --no-sync python -m rwbapp mark --checkout a
ddev exec --service app --raw -- env RWB_CHECKOUT=a uv run --no-sync pytest -q
ddev exec --service app --raw -- uv run --no-sync python -m rwbapp crud --checkout a
ddev exec --service app --raw -- uv run --no-sync python -m rwbapp cache --checkout a
ddev exec --service app --raw -- uv run --no-sync python -m rwbapp check --checkout a --forbid b
```

Use marker `b`, `RWB_CHECKOUT=b` and `--forbid a` in B. Set `RWB_CHECKOUT` on every pytest invocation, because its default marker is `pytest`, which would overwrite the cache marker and contaminate the isolation check. Keep both projects running for the isolation phase. Repeat CRUD/cache/test through `exec` without another `start` or `sync`. Include both CLI and container-exec overhead in each command measurement. Do not claim the pinned app image contains dependency installation; setup installs from the fixture lock into the bind-mounted checkout `.venv`. Commit only source/config/locks, not that platform-specific virtualenv. Account separately for first build, dependency installation and warm commands.

For persistence, execute `rwbapp persist --checkout a`, then `ddev stop` and `ddev start` in A, followed by `rwbapp persisted --checkout a` and the isolation check in B. `ddev restart` tests a different path and can be recorded separately. Compare stable PostgreSQL `system_identifier` and stored markers, while expecting Redis `run_id` and process start times to change on recreation. Finally, for this run's projects only:

```sh
ddev delete --yes --omit-snapshot --clean-containers=false
```

Capture Docker container/volume/network IDs, mounts, ownership labels, image digests and project approots before deletion. Verify no containers with this `com.ddev.site-name` remain and no run-specific PostgreSQL/Redis volumes remain. Shared router/global-cache resources may remain by design. Never use `ddev poweroff`, `ddev stop --all`, `ddev delete --all` or global Docker pruning in the harness.

## Fair benchmark checks and failure cases

- Require fixture JSON `ok: true`, migrations `0001`/`0002`, expected CRUD steps, cache sequence `miss/hit/miss/hit`, and checkout-specific SQL/cache markers. Parse each application's stdout separately from DDEV progress output.
- Compare PostgreSQL system IDs and Redis run IDs between A/B, together with container IDs and mounted volume identities. Both containers can legitimately report internal port 5432/6379 and identical in-container data paths. Those fields alone cannot prove a shared instance or isolation failure.
- An occupied host port 5432/6379 should not break container-internal service URLs. The DB publishes a dynamically assigned host port by default. Read DDEV's receipt for host tools; never point container code at host localhost. Existing user listeners on router ports are a separate Docker/router preflight condition.
- Alter a Python/dependency pin to an unavailable version and require a nonzero build/lock-sync result. Run a failing integration test and require a nonzero exec result. Test a deliberately unhealthy Redis probe and require startup failure rather than relying on status text.
- Copy committed source, `.ddev` files and `uv.lock` into another fresh checkout, assign a new project name and rebuild. Record image references and resolved digests; DDEV has no universal language/service lock that replaces the image and Python dependency locks. Its generated PostgreSQL Dockerfile installs Debian packages through apt without a snapshot pin, so a pinned base digest alone does not freeze the derived image. Distribute the resulting image by digest or freeze the apt inputs if byte-identical builds are a criterion. Global `.ddev` configuration, environment files and Dockerfiles can affect results, so capture or isolate them rather than assuming a fresh project ignores global settings.
- Report this as the supported custom Python workflow, including PHP web/router overhead. It is useful for teams already using DDEV and for extensible container environments. A separate native PHP/CMS workflow measures DDEV's core strengths; this Python workload alone cannot rank those workflows.

Historical `eval/REPORT.md` covered other tools and older fixtures. It supplies no DDEV measurement evidence. This research establishes source-backed applicability and a recipe for later execution, not performance, successful installation or lifecycle correctness on this host.
