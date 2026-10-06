# Lando

Researched 2026-10-06. This note supplies an implementation recipe, not benchmark results. Official source, release/NPM metadata, and registry manifests were inspected. No Lando installation, service start, integration test, or timing sample ran. Historical `eval/harness/compose-benchmark.py` uses plain Compose and is not evidence about Lando.

## Versions and scope

Lando is a Docker application orchestrator with native service builders and container command routing. Use stable API 3 services here; API 4 is a separate beta service path. The current stable release is **3.26.9**, published 2026-08-20, with separate Linux/macOS ARM64 and x64 binaries. No `lando` executable was on the inspected host PATH, and the local Docker image inventory contained no Lando benchmark image. Build a pinned image or provision a pinned host binary rather than claiming an existing local version. [Release](https://github.com/lando/core/releases/tag/v3.26.9), [requirements](https://docs.lando.dev/getting-started/requirements.html)

| Component | Latest NPM release checked | Source inspected |
| --- | --- | --- |
| `@lando/core` | 3.26.9 | `7a87f80576c5cdb5c7d616108bc9aff81150d463`, also release tag |
| `@lando/python` | 1.4.3 | main `ad8930c62ec5cb8f97deb1f7d810b7a96fc74421`; release `1da46479ed5bd185ff3ac411bbdefa651381fb15` |
| `@lando/postgres` | 1.6.0 | main `986358f9b8bdc342f717777a0711ebe92109d89d`; release `b12ad2fbcc5d26ef5d2c5186a35e4957004e91f1` |
| `@lando/redis` | 1.3.0 | main `5b325429c9121a32ec410ef52b1deab80c4460f3`; release `76e20605ac8aa2e68804ec87ea80091f1bf66c75` |

The Python/Postgres/Redis builders had no differences between inspected main and these release tags. [Python builder](https://github.com/lando/python/blob/1da46479ed5bd185ff3ac411bbdefa651381fb15/builders/python.js), [Postgres builder](https://github.com/lando/postgres/blob/b12ad2fbcc5d26ef5d2c5186a35e4957004e91f1/builders/postgres.js), [Redis builder](https://github.com/lando/redis/blob/76e20605ac8aa2e68804ec87ea80091f1bf66c75/builders/redis.js)

## Recipe

Lando has no bundled Django-style Python/Postgres/Redis recipe to assume here. Compose the three official service types in `.lando.yml`. The following exact versions are verified image examples; replace them with the benchmark's agreed common pins before freezing configuration. The Python plugin supports 3.13 patch selectors, the Postgres plugin maps `17` to Bitnami PostgreSQL 17.6.0, and the Redis plugin supports 8.2 patch selectors.

```yaml
name: rwblando
services:
  appserver:
    type: python:3.13.7
    command: tail -f /dev/null
    build:
      - python -m pip install --user uv==0.12.22
    overrides:
      image: python:3.13.7@sha256:fe841081ec55481496a4ab25e538833741295d57d2abdec8d38d74d65fb4715b
      environment:
        DATABASE_URL: postgresql://postgres@database:5432/rwb
        REDIS_URL: redis://cache:6379/0
        UV_PYTHON_DOWNLOADS: never
        UV_PYTHON_PREFERENCE: only-system
  database:
    type: postgres:17
    portforward: false
    creds:
      database: rwb
    overrides:
      image: bitnamilegacy/postgresql:17.6.0-debian-12-r4@sha256:926356130b77d5742d8ce605b258d35db9b62f2f8fd1601f9dbaef0c8a710a8d
  cache:
    type: redis:8.2.1
    persist: true
    portforward: false
    healthcheck:
      command: redis-cli ping
      retry: 25
      delay: 1000
    overrides:
      image: redis:8.2.1@sha256:5fa2edb1e408fa8235e6db8fab01d1afaaae96c9403ba67b70feceb8661e8621
      volumes:
        - type: volume
          source: data_cache
          target: /data
tooling:
  uv:
    service: appserver
    cmd: uv
    dir: /app
  app:
    service: appserver
    cmd: uv run --frozen --no-sync python -m rwbapp
    dir: /app
  test:
    service: appserver
    cmd: uv run --frozen --no-sync pytest -q
    dir: /app
```

Keep `pyproject.toml`, `uv.lock`, fixture source/migrations, and `.lando.yml` together at the application root. Lando mounts that root at `/app`. The Python service otherwise defaults to a CLI container and does not automatically install Python dependencies. `uv` installation is a user-specified build step; `uv sync --frozen` is the package installation phase. Tooling entries are native Lando command routing. [App mount](https://github.com/lando/core/blob/7a87f80576c5cdb5c7d616108bc9aff81150d463/utils/get-app-mounts.js), [tooling](https://github.com/lando/core/blob/7a87f80576c5cdb5c7d616108bc9aff81150d463/docs/landofile/tooling.md)

The named Redis `/data` mount is intentional. The Redis builder's `persist: true` adds `--appendonly yes`, but the builder does not mount its declared named data volume at `/data`. The official image supplies an anonymous volume there. Core rebuild uses Compose `rm -v`, which removes anonymous volumes. Mounting Lando's existing declared `data_cache` through supported overrides makes Redis data survive container replacement too. Use long Compose volume syntax as shown: Lando's short-syntax override normalizer resolves `data_cache:/data` as a host bind path. This is a source inference; test restart and rebuild separately. PostgreSQL already mounts `data_database:/bitnami/postgresql`. [Named volume declaration and overrides](https://github.com/lando/core/blob/7a87f80576c5cdb5c7d616108bc9aff81150d463/builders/_lando.js#L186), [override normalization](https://github.com/lando/core/blob/7a87f80576c5cdb5c7d616108bc9aff81150d463/utils/normalize-overrides.js#L15), [remove behavior](https://github.com/lando/core/blob/7a87f80576c5cdb5c7d616108bc9aff81150d463/lib/compose.js#L28)

## Independent checkouts and readiness

Different checkout paths alone do not isolate Lando. `name` determines the Compose project, compose/tooling caches, and generated configuration directory. The normalizer removes underscores, hyphens, and dots, so `a-b` and `ab` collide. Use unmistakably distinct alphanumeric names in a checkout-specific `.lando.local.yml`, such as `rwblandoa` and `rwblandob`. Native named volumes then get distinct Compose project prefixes. [App identity](https://github.com/lando/core/blob/7a87f80576c5cdb5c7d616108bc9aff81150d463/utils/get-app.js#L24), [project normalization](https://github.com/lando/core/blob/7a87f80576c5cdb5c7d616108bc9aff81150d463/utils/docker-composify.js), [Compose project flag](https://github.com/lando/core/blob/7a87f80576c5cdb5c7d616108bc9aff81150d463/lib/compose.js#L64)

Run each sequence from its checkout root in a fresh noninteractive shell with the benchmark's Lando binary directory on PATH:

```sh
printf 'name: rwblandoa\n' > .lando.local.yml
lando start
lando uv sync --frozen --python /usr/local/bin/python3
lando app wait --timeout 60
lando app migrate
lando app mark --checkout a
lando test
lando app identity
lando exec appserver -- true
lando app persist --checkout a
lando stop
lando start
lando app wait --timeout 60
lando app persisted --checkout a
lando destroy --yes
```

For B use `rwblandob` and `--checkout b`. The local config generation is an explicit user configuration cost. Do not copy A's name unchanged into B and then characterize their collision as failed automatic port allocation. No host port forwarding is required when all application commands execute in `appserver`. If host access is a separate task, use `portforward: true` and read actual ports from `lando info --format json`; fixed 5432/6379 forwarding defeats the documented collision avoidance. [Postgres configuration](https://github.com/lando/postgres/blob/b12ad2fbcc5d26ef5d2c5186a35e4957004e91f1/docs/config.md), [Redis configuration](https://github.com/lando/redis/blob/76e20605ac8aa2e68804ec87ea80091f1bf66c75/docs/config.md)

Postgres has a native default `psql --host=database --username=postgres --dbname=rwb -c "\\l"` probe. Redis needs the explicit healthcheck above. Core runs healthchecks before ordinary post-start hooks and retries them. The implementation uses the **`retry`** key, although the docs example says `retries`. Most importantly, exhausted checks add warnings and mark the service unhealthy instead of rejecting `lando start`. Require an application-level readiness/identity receipt, not merely a zero start exit code or the startup banner. `rwbapp wait` is a benchmark-authored confirmation, recorded separately from native healthcheck behavior. [Default PG probe](https://github.com/lando/postgres/blob/b12ad2fbcc5d26ef5d2c5186a35e4957004e91f1/utils/get-default-healthcheck.js), [normalization](https://github.com/lando/core/blob/7a87f80576c5cdb5c7d616108bc9aff81150d463/utils/normalize-healthcheck.js), [checks and warning behavior](https://github.com/lando/core/blob/7a87f80576c5cdb5c7d616108bc9aff81150d463/hooks/app-add-healthchecks.js#L145), [hook ordering](https://github.com/lando/core/blob/7a87f80576c5cdb5c7d616108bc9aff81150d463/app.js#L183)

## Installation and platform constraints

Release binaries exist for Linux ARM64 and macOS ARM64. On Linux use Docker Engine; on macOS use Docker Desktop. Registry manifest requests executed in this research confirmed both Linux amd64 and ARM64 entries for the three image digests shown above. This proves availability, not successful execution on either host.

The official installer supports `--version v3.26.9 --arch arm64 --yes --dest <absolute-directory>`. For 3.24+ it installs the CLI without automatically running setup. Prefer downloading the exact release binary and checksum list into a dedicated benchmark directory, avoiding global PATH/shell mutations:

```sh
# Execute in a dedicated bootstrap directory, not an application checkout.
RWB_LANDO_OS=linux       # macos on Apple Silicon
RWB_LANDO_ASSET="lando-${RWB_LANDO_OS}-arm64-v3.26.9"
RWB_LANDO_RELEASE=https://github.com/lando/core/releases/download/v3.26.9
curl -fL "$RWB_LANDO_RELEASE/$RWB_LANDO_ASSET" -o "$RWB_LANDO_ASSET"
curl -fL "$RWB_LANDO_RELEASE/sha256sum.txt" -o sha256sum.txt
python3 - "$RWB_LANDO_ASSET" <<'PY'
import hashlib, pathlib, sys
asset = sys.argv[1]
checks = dict((row.split()[1].lstrip('*'), row.split()[0])
              for row in pathlib.Path('sha256sum.txt').read_text().splitlines() if row.strip())
actual = hashlib.sha256(pathlib.Path(asset).read_bytes()).hexdigest()
if checks.get(asset) != actual:
    raise SystemExit('Lando release checksum mismatch')
print(asset, actual)
PY
chmod +x "$RWB_LANDO_ASSET"
mkdir -p bin
ln -s "../$RWB_LANDO_ASSET" bin/lando
export PATH="$PWD/bin:$PATH"
# Dedicated absolute state directory; keep this value for every invocation.
export LANDO_CORE_USERCONFROOT="$PWD/lando-state"
mkdir -p "$LANDO_CORE_USERCONFROOT"
cat > "$LANDO_CORE_USERCONFROOT/config.yml" <<'YAML'
proxy: OFF
scanner: false
setup:
  orchestrator: 2.40.3
  skipInstallCa: true
  skipCommonPlugins: true
YAML
lando version --all
lando setup --yes --skip-common-plugins --skip-install-ca \
  --plugin @lando/python@1.4.3 --plugin @lando/postgres@1.6.0 \
  --plugin @lando/redis@1.3.0
lando version --all
```

`LANDO_CORE_USERCONFROOT` is the source-supported legacy runtime-selector override and directly controls CLI plugin/cache paths. Keep the benchmark state root isolated from the user's `.lando`. Setup creates networking and can install/start Docker and Compose, so run provisioning only on the authorized benchmark host/container. Pin and record the selected orchestrator too; current core defaults to Compose 2.40.3 and supports Compose 1/2, not Compose 5.6.0. Do not silently replace Lando's orchestrator with the suite's latest Compose comparator. A benchmark image needs the Docker CLI and a reachable daemon, plus host-visible bind-mount paths for the application and Lando state directory. A socket-mounted client container is not a complete runnable setup if those paths exist only inside that container. [Setup options/plugin version parsing](https://github.com/lando/core/blob/7a87f80576c5cdb5c7d616108bc9aff81150d463/tasks/setup.js), [ARM64 orchestrator selection](https://github.com/lando/core/blob/7a87f80576c5cdb5c7d616108bc9aff81150d463/hooks/lando-setup-orchestrator.js), [runtime override](https://github.com/lando/core/blob/7a87f80576c5cdb5c7d616108bc9aff81150d463/bin/lando#L71), [Docker/Compose support matrix](https://github.com/lando/core/blob/7a87f80576c5cdb5c7d616108bc9aff81150d463/config.yml), [installer source](https://github.com/lando/setup-lando/blob/3299b497202e3f57a2c02859cdaa08294a5adf30/setup-lando.sh)

## Comparable tasks and receipts

- Provision and install locked dependencies. Record core/plugin versions, binary hashes, images/digests and selected platforms, generated Compose configuration, Python/uv versions, and `pyproject.toml`/`uv.lock` hashes. Lando has configuration/image pins, not a native Python dependency lock; `uv.lock` owns that part.
- Start and migrate; run CRUD/cache integration tests. Capture native healthcheck warnings and the application's own successful connection and migration receipts.
- Execute repeated warm no-op, Python identity, application CLI, and pytest commands through `lando exec` or named tooling. Keep dependency installation outside repeated commands. Native tooling/exec reads a Compose cache to avoid full app initialization. [Exec implementation](https://github.com/lando/core/blob/7a87f80576c5cdb5c7d616108bc9aff81150d463/tasks/exec.js#L44)
- Start A and B simultaneously. Inspect actual container IDs, `com.docker.compose.project`, source bind-mount paths, and volume names. Require different PostgreSQL system identifiers and Redis run IDs, then write distinct markers into the same logical table/key and check neither sees the other's marker. Internal URLs, ports, and data-directory strings can legitimately match across containers. Do not require container data paths to start with host checkout paths.
- Stop/restart A and verify PostgreSQL rows plus a persisted Redis key survive; check B still serves its own marker. Test `rebuild --yes` separately if claiming persistence across container replacement. `stop` preserves containers/data; `destroy --yes` stops then purges app volumes and caches. [Lifecycle](https://github.com/lando/core/blob/7a87f80576c5cdb5c7d616108bc9aff81150d463/lib/app.js#L229)
- Copy source/config/locks to a clean directory, generate a new local app name, and reproduce versions and tests. Exclude `.venv`, runtime data and Lando caches. Preserve plugin/version provisioning metadata too, since plugins live outside the application. Distinguish checkout recreation from dependency cache coldness.
- Destroy only the named app, then verify its containers/project volumes are gone and the other app survives. Lando has shared infrastructure such as Landonet and potentially a proxy. Shared infrastructure remaining after app teardown is not an orphaned app. Do not invoke global `lando poweroff` or Docker prune.

Disable the shared HTTP proxy/scanner in a dedicated benchmark global config if this fixture exposes no HTTP service; report that choice. Keep Lando's application/service networking intact. Benchmark normal container orchestration and developer commands as supported, while recording the explicit naming, dependency installation, Redis volume override, and application verification scripts as user configuration.
