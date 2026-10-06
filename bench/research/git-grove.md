# GitGrove research and implementation recipe

Research date: 2026-10-06. Target: [christinabranson/git-grove](https://github.com/christinabranson/git-grove). This is source review and implementation input for Opus 5.5, with no application/service runs or timing results.

## Version and evidence

The latest available npm package observed is `@gitgrove/cli@0.1.0-alpha.1.8`, both `latest` and `alpha`. Its registry `gitHead` and Git tag resolve to `aa6f6315cbc9cc0096fcb81ca35fb4faf0123339`. The tarball SHA1 is `2da053b6488c68703e68200148f5a6dc5417d14c`, integrity `sha512-szaFvkSzi+a8JK8R1Ui+Yvv9u/6xnAAEw1Kvk61z+qRtOr+zlZD57Nr6yLzc+R21pZOhYm3X2WyD00TEaGC0mQ==`. [Official registry metadata](https://registry.npmjs.org/@gitgrove/cli), [release source](https://github.com/christinabranson/git-grove/tree/aa6f6315cbc9cc0096fcb81ca35fb4faf0123339).

The newest source tag is `v0.1.0-alpha.1.11`, current main `37c60858db66770387312caccfc8da6ef6052919`. That version returned npm E404. GitHub's releases API returned an empty array. Do not call alpha.1.11 an available npm release. Checked-in `package.json` still says alpha.1.1 because the publishing workflow changes version only in its CI checkout before publishing/tagging. [Publishing workflow](https://github.com/christinabranson/git-grove/blob/aa6f6315cbc9cc0096fcb81ca35fb4faf0123339/.github/workflows/publish.yml).

Executed evidence: shallow clone, release-revision checkout, `npm view`, npm tarball download/extraction, isolated installation under `/tmp/stack-bench-sources/git-grove-published/cli`, and CLI `--version`/command-help checks. The installed CLI reports alpha.1.8. Published bundle review confirmed the source-derived start/custom-shell behavior below. Local Node is v24.16.0, npm 11.13.0, Compose v2.40.3-desktop.1. No `grove` was on PATH before the isolated installation. Node >=18 is declared; Bash, Git and Docker/Compose are external prerequisites. Linux containers require access to a Docker daemon; Grove does not provision one. The CLI uses portable Node code and Bash scripts, with no platform-specific release binary required. [Installation source](https://github.com/christinabranson/git-grove/blob/aa6f6315cbc9cc0096fcb81ca35fb4faf0123339/docs/getting-started/installation.md).

Read-only image inventory found PostgreSQL 17.6 and Redis 8.10.2 cached, respectively `postgres@sha256:ef257d85f76e48da1c64832459b59fcaba1a4dac97bf5d7450c77753542eee94` and `redis@sha256:3811787313eba226a2ef38658c6ccb91cd5e110edc89c37767de373120a0e5a0`. No Python image was cached. These are local image identities, not claims about latest upstream patch versions. Freeze the same images/Python/uv pins as the Compose adapter.

Static recipe QA parsed the config JSON, passed Bash syntax checks for every shell block, and rendered the Compose snippet with the installed plugin's `config --format json`. It produced exactly app/postgres/redis. Parser validation used placeholder image tags without pulls; it does not validate immutable pins or runtime behavior.

## Native behavior and boundary

Grove manages Git worktrees, derives branch-specific Compose project names and ports, writes `.env.worktree`, and starts a Docker Compose or custom-shell provider. Python provision, dependency locking, application commands, useful healthchecks, database migrations, and Redis persistence belong to the project configuration. It is a directly relevant competitor for parallel application worktrees, with Compose doing container/data lifecycle work.

| Requirement | Native Grove behavior | Project/adapter work |
| --- | --- | --- |
| Code isolation | Git worktree creation and branch checkout | Separate clones are a distinct task; use run-unique project names across clones |
| Runtime/dependencies | No Python installer or package solver | Digest-pinned Python/uv Dockerfile and checked-in uv.lock |
| Service isolation | Generated COMPOSE_PROJECT_NAME and naming/ports | Compose services, project-scoped volumes/networks; avoid external/global names |
| Shared infrastructure | Separate named shared Compose stack | Database/schema creation and Redis namespaces must be explicit |
| Readiness | Native Compose provider invokes up -d --build | Custom-shell uses up --wait with real healthchecks |
| Repeated commands | No grove exec/task command | Compose exec -T against the retained application container |
| Persistence | Ordinary stop uses Compose down without -v | Named volumes, Redis AOF policy, assertions after restart |
| Cleanup | Stop, delete worktree, interactive teardown | Volume deletion and resource receipts; delete alone is insufficient |
| Copy/share | Checked-in config/scripts and normal Git | Copy config/locks, regenerate worktree env, distribute/build image separately |

Implementation pointers, all pinned to the published revision:

- [Config loading](https://github.com/christinabranson/git-grove/blob/aa6f6315cbc9cc0096fcb81ca35fb4faf0123339/src/data/groveConfig.ts#L11) ignores configurations without `enabled: true`, despite several documentation examples omitting it. [Provider selection](https://github.com/christinabranson/git-grove/blob/aa6f6315cbc9cc0096fcb81ca35fb4faf0123339/src/providers/discover.ts#L78) executes only the first configured provider. One custom-shell provider should launch the whole Compose project.
- [Start](https://github.com/christinabranson/git-grove/blob/aa6f6315cbc9cc0096fcb81ca35fb4faf0123339/src/commands/start.ts#L70) attaches using `<worktree-root>/<branch-with-slashes-replaced>`, not arbitrary existing paths. New branches accept an explicit `--base`. Released alpha.1.8 writes `.env.worktree` only when absent or with `--refresh-env`. The current main regenerates it on every start and has different env precedence; do not mix the tracks.
- [Naming](https://github.com/christinabranson/git-grove/blob/aa6f6315cbc9cc0096fcb81ca35fb4faf0123339/src/setup/naming.ts#L9) lowercases and replaces all non-alphanumeric characters with hyphens, truncating to 40 characters. Distinct branch names can therefore collide. Port allocation probes sockets and Docker-published ports but does not retain a reservation across concurrent starts. Start A and B serially.
- [Env contract](https://github.com/christinabranson/git-grove/blob/aa6f6315cbc9cc0096fcb81ca35fb4faf0123339/src/commands/start.ts#L109) supports passthrough, derived templates and required variables. Explicitly use `.env.example` as an input for alpha.1.8; its default source is `.env`, and it does not implement main's automatic `.env.example` bootstrap.
- [Custom-shell start/stop](https://github.com/christinabranson/git-grove/blob/aa6f6315cbc9cc0096fcb81ca35fb4faf0123339/src/providers/custom-shell.ts#L97) calls Bash with worktree cwd, injected `.env.worktree` values, and GROVE_ENV_FILE. Released precedence is generated values over inherited shell values. Script stdout inherits the CLI's stdout, so it can contaminate `start --json`.
- [Native Docker provider](https://github.com/christinabranson/git-grove/blob/aa6f6315cbc9cc0096fcb81ca35fb4faf0123339/src/providers/docker-compose.ts#L120) uses `up -d --build`, without `--wait`. Its returned environment ignores the queried container state. [Status JSON](https://github.com/christinabranson/git-grove/blob/aa6f6315cbc9cc0096fcb81ca35fb4faf0123339/src/commands/status.ts#L59) reports discovery success and URLs/provider, not running/healthy state.
- [Delete](https://github.com/christinabranson/git-grove/blob/aa6f6315cbc9cc0096fcb81ca35fb4faf0123339/src/commands/delete.ts) attempts Compose down directly and can continue after failure, then force-removes the worktree. It does not call the configured custom stop script, contrary to the example documentation. [Teardown](https://github.com/christinabranson/git-grove/blob/aa6f6315cbc9cc0096fcb81ca35fb4faf0123339/src/commands/docker.ts#L128) prompts for the exact project name and runs down -v; no --yes option exists.

## Suggested configuration

Use the existing common application `bench/fixtures/app`, including `uv.lock`, in a fresh disposable Git repository. Add the files below before the fixture's initial commit. Generate a run-unique lowercase project name such as `rwb-grove-<12-hex-run-id>` instead of the example `rwb-grove-example`. The implementer owns that fixture preparation commit; no research commit was created here.

`.grove/config.json`:

```json
{
  "enabled": true,
  "project": "rwb-grove-example",
  "providers": {
    "app": {"type": "custom-shell", "script": "bin/start.sh", "stopScript": "bin/stop.sh"}
  },
  "naming": {
    "composeProject": "${project}-${branch_safe}",
    "ports": {"DB_PORT": "auto", "REDIS_PORT": "auto"}
  },
  "envContract": {
    "strict": true,
    "sourceEnvFiles": [".env.example"],
    "passthrough": ["RWB_PYTHON_IMAGE", "RWB_UV_IMAGE", "RWB_POSTGRES_IMAGE", "RWB_REDIS_IMAGE"],
    "required": ["RWB_PYTHON_IMAGE", "RWB_UV_IMAGE", "RWB_POSTGRES_IMAGE", "RWB_REDIS_IMAGE"]
  }
}
```

`.env.example` contains immutable, architecture-compatible digest references for those four images. Use a Python 3.13 image and matching official uv image from the common adapter pins. Grove is not responsible for those image pins. Use trusted simple KEY=value entries without shell quoting. Ignore `.env`, `.env.worktree`, `.venv`, and `.grove/meta.json` in Git, but keep `.grove/config.json` tracked.

`Dockerfile`:

```dockerfile
ARG RWB_UV_IMAGE
ARG RWB_PYTHON_IMAGE
FROM ${RWB_UV_IMAGE} AS uv
FROM ${RWB_PYTHON_IMAGE}
COPY --from=uv /uv /uvx /usr/local/bin/
WORKDIR /app
COPY . /app
RUN uv sync --frozen --python /usr/local/bin/python --no-python-downloads
ENV PATH="/app/.venv/bin:${PATH}"
CMD ["python", "-c", "import time; time.sleep(2147483647)"]
```

Exclude `.git`, `.grove`, `.env*`, `.venv` and logs in `.dockerignore`. Do not exclude `rwbapp/SOURCE_TOKEN`. Prepare the token before building each checkout image so identity proves which code was baked into it. Each project defaults to its own built image name; do not assign a global shared `image:` name to the built app.

`compose.yaml`:

```yaml
services:
  postgres:
    image: ${RWB_POSTGRES_IMAGE:?}
    environment:
      POSTGRES_USER: bench
      POSTGRES_PASSWORD: bench
      POSTGRES_DB: app
    ports: ["127.0.0.1:${DB_PORT:?}:5432"]
    volumes: ["pgdata:/var/lib/postgresql/data"]
    healthcheck:
      test: [CMD-SHELL, "pg_isready -U bench -d app"]
      interval: 1s
      timeout: 3s
      retries: 60
  redis:
    image: ${RWB_REDIS_IMAGE:?}
    command: [redis-server, --appendonly, "yes", --appendfsync, always]
    ports: ["127.0.0.1:${REDIS_PORT:?}:6379"]
    volumes: ["redisdata:/data"]
    healthcheck:
      test: [CMD, redis-cli, ping]
      interval: 1s
      timeout: 3s
      retries: 60
  app:
    build:
      context: .
      args:
        RWB_PYTHON_IMAGE: ${RWB_PYTHON_IMAGE:?}
        RWB_UV_IMAGE: ${RWB_UV_IMAGE:?}
    environment:
      DATABASE_URL: postgresql://bench:bench@postgres:5432/app
      REDIS_URL: redis://redis:6379/0
    depends_on:
      postgres: {condition: service_healthy}
      redis: {condition: service_healthy}
    healthcheck:
      test: [CMD, python, -m, rwbapp, wait, --timeout, "2"]
      interval: 1s
      timeout: 5s
      retries: 60
volumes:
  pgdata:
  redisdata:
```

Host ports exercise Grove's native allocation and permit direct service verification. The application uses project-private DNS/network endpoints. The same names/path values inside two containers do not imply shared data; inspect container volume sources and server IDs. For a separate no-published-port variant, omit both `ports` entries and report it consistently across tools.

`bin/start.sh`, called by Grove:

```bash
#!/usr/bin/env bash
set -euo pipefail
: "${COMPOSE_PROJECT_NAME:?}" "${GROVE_ENV_FILE:?}"
docker compose -p "$COMPOSE_PROJECT_NAME" --env-file "$GROVE_ENV_FILE" \
  -f compose.yaml up --build --wait --wait-timeout 90 >&2
```

`bin/stop.sh`:

```bash
#!/usr/bin/env bash
set -euo pipefail
docker compose -p "$COMPOSE_PROJECT_NAME" --env-file "$GROVE_ENV_FILE" \
  -f compose.yaml down >&2
```

Do not copy the official script's `docker compose wait db`. That command waits for containers to stop. [Docker wait reference](https://docs.docker.com/reference/cli/docker/compose/wait/). `up --wait --wait-timeout` is the supported startup health barrier. [Docker up reference](https://docs.docker.com/reference/cli/docker/compose/up/).

## Command flow and required receipts

Install in a private tools directory and preserve its package lock for reproducing the CLI dependency graph:

```bash
npm install --prefix "$RWB_TOOLS/grove" --save-exact --no-audit --no-fund @gitgrove/cli@0.1.0-alpha.1.8
GROVE="$RWB_TOOLS/grove/node_modules/.bin/grove"
"$GROVE" --version
```

Run Bash from the disposable fixture repo with stdin closed. Set `GROVE_WORKTREE_ROOT` to a fresh absolute run directory. Precreate the two local branches/worktrees before adding the untracked source tokens, then let Grove attach to its expected paths. This avoids `grove start --new` building before the source tokens exist. `RWB_FIXTURE_BASE` is the immutable fixture commit including all checked-in config/locks. The app/source commit identities, token bytes and config hashes belong in the run manifest.

```bash
set -euo pipefail
: "${GROVE:?}" "${GROVE_WORKTREE_ROOT:?}" "${RWB_FIXTURE_BASE:?}"
mkdir -p "$GROVE_WORKTREE_ROOT"
git worktree add -b rwb-a "$GROVE_WORKTREE_ROOT/rwb-a" "$RWB_FIXTURE_BASE"
git worktree add -b rwb-b "$GROVE_WORKTREE_ROOT/rwb-b" "$RWB_FIXTURE_BASE"
printf '%s\n' source-a > "$GROVE_WORKTREE_ROOT/rwb-a/rwbapp/SOURCE_TOKEN"
printf '%s\n' source-b > "$GROVE_WORKTREE_ROOT/rwb-b/rwbapp/SOURCE_TOKEN"
"$GROVE" start rwb-a --json > "$RWB_RECEIPTS/start-a.json"
"$GROVE" start rwb-b --json > "$RWB_RECEIPTS/start-b.json"

# Define this in each fresh command shell; no activation shell is required.
dc() {
  local tree="$1"; shift
  docker compose --project-directory "$tree" --env-file "$tree/.env.worktree" \
    -f "$tree/compose.yaml" "$@"
}
a="$GROVE_WORKTREE_ROOT/rwb-a"
b="$GROVE_WORKTREE_ROOT/rwb-b"
dc "$a" exec -T app python -m rwbapp identity
dc "$a" exec -T app python -m rwbapp migrate
dc "$a" exec -T app python -m rwbapp mark --checkout a
dc "$b" exec -T app python -m rwbapp migrate
dc "$b" exec -T app python -m rwbapp mark --checkout b
dc "$a" exec -T app python -m rwbapp check --checkout a --forbid b
dc "$b" exec -T app python -m rwbapp check --checkout b --forbid a
dc "$a" exec -T -e RWB_CHECKOUT=a app uv run --frozen pytest -q
dc "$b" exec -T -e RWB_CHECKOUT=b app uv run --frozen pytest -q
dc "$a" exec -T app python -m rwbapp crud --checkout a
dc "$a" exec -T app python -m rwbapp cache --checkout a
```

The start script redirects its own output to stderr so the CLI's JSON is parseable. Require `metadata.provider=custom-shell`, exact worktree path, two distinct project names and two distinct generated DB/Redis host ports. Verify `docker compose config --format json` against `.env.worktree`, then query `ps --format json` separately for health. Clear inherited COMPOSE_PROJECT_NAME, COMPOSE_FILE, DB_PORT, REDIS_PORT and other fixture-interpolated variables so Compose shell precedence cannot redirect direct commands to another project.

For each app identity receipt require Python 3.13, PostgreSQL major 17 and Redis major 8, expected source token, unique PostgreSQL `system_identifier`, unique Redis `run_id`, exact checkout markers, and no forbidden marker. Inspect project labels and named-volume mount sources to prove data ownership. Repeated commands must execute actual CRUD/cache work; an `exec true` timing is a different task. Interpreter-only startup can be an additional clearly labeled measurement.

For durability, run `persist --checkout a`, `grove stop rwb-a`, then `grove start rwb-a --json`, followed by `persisted --checkout a` and `check --checkout a --forbid b`. Never reseed markers during the restart check. Ordinary down retains volumes; Redis AOF uses always fsync here. Verify B throughout. Also test repeated start without refresh; alpha.1.8 retains env values. Port contention after stop is a real lifecycle case; use `--refresh-env` only as an explicit recovery step.

For destructive reset, use direct `dc "$a" down --volumes --remove-orphans` or feed the exact project-name line to `grove docker teardown rwb-a`. Native teardown has no noninteractive flag. Keep teardown separate from persistence. Capture `docker ps -a`, `docker network ls` and `docker volume ls` filtered by `com.docker.compose.project=<exact-owned-name>` after each cleanup, require zero remaining owned resources, and verify B still answers. Remove worktrees only after verified container/volume cleanup with `grove delete rwb-a --yes`, then B. Native delete force-removes untracked files and is intended here only for disposable fixtures.

For two independent clones, keep `.grove/config.json`, Dockerfile, scripts, Compose file, pyproject/uv.lock and image pins identical apart from the unique project prefix. Do not copy `.env.worktree`; regenerate in each clone with distinct branch names or project prefixes. Same project plus same branch across clones produces the same Compose identity. This differs from two worktrees of one repo and must have its own result.

The historical `eval/harness/compose-benchmark.py` tests databases and `exec true`; `eval/fixture/tests/test_stack.py` only checks Python>=3.12, SELECT 1 and Redis ping. Neither is a new GitGrove application result. Evaluate the checked-in real app and its frozen dependency lock.

## Implementation limits

The recipe is source-derived and has not been run end to end. Installation/help and registry/image inspection are executed evidence; image builds, migrations, readiness, isolation, persistence, timings and cleanup still require execution. Compare GitGrove plus its supported custom-shell/Compose configuration fairly against the same application/data assertions. Attribute Compose health/data lifecycle to Compose and Grove worktree/env generation to Grove. Record custom-script lines and external requirements as setup cost, not missing application success. Source-head alpha.1.11 can be a separately labeled track, using `npm ci && npm run build` and `node dist/cli.js`; its stale package version needs the source SHA in the manifest. At the published alpha.1.8 source revision, `npm start` references nonexistent dist/index.js; the installed grove binary uses dist/cli.js. That stale development script does not make the published CLI unavailable.
