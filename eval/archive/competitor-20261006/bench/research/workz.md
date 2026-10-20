# workz research and implementation recipe

Research date: 2026-10-06. Target: [rohansx/workz](https://github.com/rohansx/workz), the environment engine for agent worktrees. [Alex-Izquierdo/workz](https://github.com/Alex-Izquierdo/workz) is a separate namesake and is not this adapter. No application, service, container, or timing run was executed. The recipe below is source-derived implementation input for Opus 5.5.

## Version and executed evidence

Latest published stable observed: [v0.11.0](https://github.com/rohansx/workz/releases/tag/v0.11.0), published 2026-07-02, source `23999fdca292788b4d68030678beb76ce70a415e`. GitHub's release API and [docs.rs latest](https://docs.rs/crate/workz/latest) agree. The crates.io API returned HTTP 403, so registry JSON was not available. The source was shallow-cloned into `/tmp/stack-bench-sources/workz`, then the stable tag was fetched and checked out for review.

Main currently reports `0.15.0` at `84b6688bee54229c043eb2dac837b142a07221e5`. Its [changelog](https://github.com/rohansx/workz/blob/84b6688bee54229c043eb2dac837b142a07221e5/CHANGELOG.md) includes newer release sections and unreleased fixes. Do not label that revision a published stable release, or assign its capabilities to v0.11.0. A separately labeled source-head track is reasonable.

Read-only CLI checks downloaded the official macOS ARM64 release archive into `/tmp/stack-bench-sources/workz-release-mac`. The extracted binary reports `workz 0.11.0`; `sync --help` confirms `--isolated --json --no-install --create-db --from-db`. Its SHA256 is `72994c049c43989e4ec868dd3741389548f70aa15342acd34cd88349c5feef97`. There was no `workz` on this host's PATH before that download. No local workz container image was identified or checked. Obtain the matching Linux ARM64/AMD64 release asset for Linux execution, or build the stable revision with `cargo build --release --locked`; record that binary separately. Release binaries cover Linux GNU, Linux x86_64 musl, and macOS Intel/ARM64. The [release workflow](https://github.com/rohansx/workz/blob/23999fdca292788b4d68030678beb76ce70a415e/.github/workflows/release.yml) has no Windows target.

Historical `eval/REPORT.md` explicitly lists Worktrunk/workz as untested. This note supplies no new benchmark result.

## Stable capabilities and limits

| Requirement | Native stable behavior | Recipe work |
| --- | --- | --- |
| Independent code | Git worktree creation/removal; sync existing worktree | Use two worktrees of one fixture repo; report separately from two independent clones |
| Python dependencies | Detect `uv.lock`, run `uv sync` when neither source nor target has a venv; normally symlink source `.venv` | Provide pinned Python/uv externally; use `uv sync --locked` and separate venvs for mutation isolation |
| PostgreSQL | Assign `DB_NAME`; optional `createdb`, including `-T` template clone | Provide a server and libpq client tools; creation is best effort |
| Redis | No service provisioner or Redis URL rewriting | Compose/script creates a separate instance and URL |
| Ports | Reserve a branch-slug range, write `PORT` into `.env.local` | Derive Redis port from the range; start/wait for services |
| Compose | Write `COMPOSE_PROJECT_NAME`; `start --docker` invokes Compose `up -d`; `done` attempts Compose `down` | Explicitly load managed env; declare images, volumes, healthchecks, persistent Redis settings |
| Repeated commands | Worktree selection and sync; no stable `exec` or task runner | Execute the app in its worktree with its env and venv |
| Sharing | Checked-in `.workz.toml`, source/locks, project-over-global config merge | Git distribution and service configuration; no native portable service bundle registry |
| Snapshots | Native PostgreSQL template copy via `--from-db` | No stable uncommitted-code carry or filesystem clone strategy |

Implementation read:

- [Git root/worktree paths and creation](https://github.com/rohansx/workz/blob/23999fdca292788b4d68030678beb76ce70a415e/src/git.rs#L28): default path is `../<repo>--<branch-with-slashes-replaced-by-dashes>`. A linked worktree resolves back to its main repo.
- [Python detection](https://github.com/rohansx/workz/blob/23999fdca292788b4d68030678beb76ce70a415e/src/sync.rs#L199) and [installation](https://github.com/rohansx/workz/blob/23999fdca292788b4d68030678beb76ce70a415e/src/sync.rs#L428): `uv sync` has no `--locked`; failed install becomes a warning, not a failed command. An existing source venv suppresses installation even if an override ignores it. Always verify actual interpreter/dependency identity.
- [Registry/allocation](https://github.com/rohansx/workz/blob/23999fdca292788b4d68030678beb76ce70a415e/src/isolation.rs#L50): `dirs::config_dir()/workz/ports.json`; macOS uses Application Support, Linux follows the usual config directory. Stable keys by branch slug globally, not repository. Allocate serially and use run-unique branch names; source uses a plain read/write JSON file without a lock. Only the base port is checked for an existing listener. The configured `base_port` is not passed into this stable allocator, which starts from registry/default 3000.
- [Database creation](https://github.com/rohansx/workz/blob/23999fdca292788b4d68030678beb76ce70a415e/src/isolation.rs#L194): `createdb` inherits libpq `PGHOST`, `PGPORT`, `PGUSER`; it does not connect using the generated `DATABASE_URL`. All nonzero statuses are reported as skipped. Stable has no Docker PostgreSQL fallback.
- [Start/Compose](https://github.com/rohansx/workz/blob/23999fdca292788b4d68030678beb76ce70a415e/src/main.rs#L115): `post_start` runs before isolation; stable does not supply `WORKZ_*` hook context. `--docker` then calls `podman-compose up -d` if present, otherwise `docker compose up -d`, without loading `.env.local` or setting project env. Failure is a warning. Use explicit Compose commands below rather than counting `ready!` as service readiness.
- [Done](https://github.com/rohansx/workz/blob/23999fdca292788b4d68030678beb76ce70a415e/src/main.rs#L438): attempts Compose down, releases registry entry, optionally drops database, runs hook, removes worktree. Stable does not reap host processes or remove Compose volumes. Capture resource receipts and explicitly clean owned volumes.

## Runnable stable recipe

Prerequisites: Bash, Git, stable workz, a pinned externally supplied Python 3.13 executable, pinned uv, and Docker plus Compose supporting `up --wait`. The common fixture is `bench/fixtures/app`, including its existing `pyproject.toml` and `uv.lock`. Freeze exact Python/uv versions and image digests with the other adapters. Workz does not install those tools. Run native Python against published service ports on macOS/Linux. For a Linux Docker execution host, keep the app and Compose daemon in a network arrangement where `127.0.0.1` reaches the published ports. Mounting the host Docker socket inside an unrelated container does not satisfy that condition.

Create a fresh Git fixture repo with the common app, the following files, and a commit before starting worktrees. Do not use the Stack checkout or existing user services as that repo. `.workz.toml` disables shared mutable dependencies; `copy` remains available for a separately reported warm-sync task:

```toml
[sync]
symlink = []
copy = []

[isolation]
port_range_size = 10
```

`compose.yaml`:

```yaml
services:
  postgres:
    image: ${RWB_POSTGRES_IMAGE:?supply frozen image digest}
    environment:
      POSTGRES_USER: bench
      POSTGRES_PASSWORD: bench
      POSTGRES_DB: ${DB_NAME:?}
    ports: ["127.0.0.1:${PORT:?}:5432"]
    volumes: ["pgdata:/var/lib/postgresql/data"]
    healthcheck:
      test: [CMD-SHELL, "pg_isready -U bench -d $$POSTGRES_DB"]
      interval: 1s
      timeout: 3s
      retries: 60
  redis:
    image: ${RWB_REDIS_IMAGE:?supply frozen image digest}
    command: [redis-server, --appendonly, "yes", --appendfsync, always]
    ports: ["127.0.0.1:${REDIS_PORT:?}:6379"]
    volumes: ["redisdata:/data"]
    healthcheck:
      test: [CMD, redis-cli, ping]
      interval: 1s
      timeout: 3s
      retries: 60
volumes:
  pgdata: {}
  redisdata: {}
```

Use one manifest-index digest for each chosen image across all architectures. PostgreSQL 17 requires the data mount above; changing to PostgreSQL 18 requires its supported mount layout. Do not use shared bind directories, fixed `container_name`, fixed volume `name`, or external volumes. Digest values must come from the frozen run manifest, not moving tags.

Run from that fixture's main checkout. Set `RWB_RUN_ID` to a unique lowercase alphanumeric ID, `RWB_PYTHON` to the pinned interpreter path, and the image variables to the frozen digest references. The adapter should pass environment as structured subprocess arguments; the following Bash recipe shows the same operations:

```bash
set -euo pipefail
: "${RWB_RUN_ID:?}" "${RWB_PYTHON:?}" "${RWB_POSTGRES_IMAGE:?}" "${RWB_REDIS_IMAGE:?}"
export UV_PYTHON_DOWNLOADS=never
repo=$(git rev-parse --show-toplevel)
parent=$(dirname "$repo")
name=$(basename "$repo")
case "$RWB_RUN_ID" in *[!a-z0-9]*|'') exit 2;; esac

for checkout in a b; do
  branch="${RWB_RUN_ID}_${checkout}"
  workz start "$branch" --isolated --no-sync
  wt="$parent/$name--$branch"
  (
    cd "$wt"
    workz sync --isolated --no-install --json > "$parent/$branch-sync.json"
    set -a
    . ./.env.local
    set +a
    export REDIS_PORT=$((PORT + 1))
    export DATABASE_URL="postgresql://bench:bench@127.0.0.1:$PORT/$DB_NAME"
    export REDIS_URL="redis://127.0.0.1:$REDIS_PORT/0"
    # .env also lets stable workz done select this exact Compose project.
    # The input here is the fixture's generated env, with no user secrets.
    cp .env.local .env
    printf '\nREDIS_PORT=%s\nDATABASE_URL=%s\nREDIS_URL=%s\n' \
      "$REDIS_PORT" "$DATABASE_URL" "$REDIS_URL" >> .env
    printf 'RWB_POSTGRES_IMAGE=%s\nRWB_REDIS_IMAGE=%s\n' \
      "$RWB_POSTGRES_IMAGE" "$RWB_REDIS_IMAGE" >> .env
    uv sync --locked --python "$RWB_PYTHON"
    docker compose --env-file .env -p "$COMPOSE_PROJECT_NAME" up -d --wait --wait-timeout 90
    .venv/bin/python -m rwbapp wait --timeout 60
    .venv/bin/python -m rwbapp migrate
    .venv/bin/python -m rwbapp mark --checkout "$checkout"
    RWB_CHECKOUT="$checkout" .venv/bin/python -m pytest -q
    .venv/bin/python -m rwbapp identity > "$parent/$branch-identity.json"
  )
done

app() (
  checkout=$1; shift
  cd "$parent/$name--${RWB_RUN_ID}_${checkout}"
  set -a; . ./.env; set +a
  exec .venv/bin/python -m rwbapp "$@"
)
dc() (
  cd "$parent/$name--${RWB_RUN_ID}_$1"; shift
  set -a; . ./.env; set +a
  exec docker compose --env-file .env -p "$COMPOSE_PROJECT_NAME" "$@"
)

app a check --checkout a --forbid b
app b check --checkout b --forbid a
app a crud --checkout a
app a cache --checkout a
app a persist --checkout a
dc a down
dc a up -d --wait --wait-timeout 90
app a persisted --checkout a
app b check --checkout b --forbid a

# Delete only this run's owned resources after receipts have been saved.
dc a down --volumes --remove-orphans
workz done "${RWB_RUN_ID}_a" --force --delete-branch
app b check --checkout b --forbid a
dc b down --volumes --remove-orphans
workz done "${RWB_RUN_ID}_b" --force --delete-branch
```

Keep the helper's environment loading inside each subprocess. Otherwise inherited A variables can override B's Compose interpolation. `workz` prints a shell-change marker; it does not change the parent process directory without shell integration. Use the explicit path above in a noninteractive shell. `--force` is limited to disposable fixture worktrees with verified receipts. Capture each project's container and volume names/labels before removal, then require zero remaining owned containers, networks, and volumes after final cleanup. Do not run global prune or `doctor --fix` against user state.

## Comparable tasks and checks

Measure tool installation/image acquisition separately from provision-to-ready, dependency sync, test execution, repeated commands, persistence/restart, and cleanup. Label the primary recipe `workz 0.11.0 + external Python/uv + Docker Compose`; a workz-only timing cannot be presented as managed service startup. Native schema isolation on an existing shared PostgreSQL server is another valid workload, but it is a distinct topology with shared server identity. Redis key prefixes alone are weaker isolation than separate instances and require application changes, so the proposed shared app uses separate Redis containers.

For each phase retain command argv, exit status, stdout/stderr, resolved Python/uv/workz/service versions, fixture/config/lock hashes, and Compose resolved config. Verify A/B worktree paths and venv realpaths differ; Python is the frozen 3.13 patch; `uv.lock` remains unchanged; database names and host port bindings differ; PostgreSQL system identifiers differ for this separate-container recipe; Redis `run_id` differs; A has only marker `a`, B only `b`. Identical `/var/lib/postgresql/data` strings inside two containers do not prove shared storage. Compare Docker volume IDs and project labels. Run the fixture's `check`, `crud`, `cache`, and `persisted` commands and verify successful structured JSON, not just liveness or healthy status. After restarting/removing A, B's identities and markers must remain valid.

The warm dependency-sync task should also use supported defaults: install the seed venv, run `workz start`, and prove symlink identity. Record the shared-mutation consequence; do not penalize an intentional symlink for failing a separate-venv contract. Stable `copy` overrides offer directory copying, but relocating a venv can retain absolute paths, so always inspect the actual interpreter/import paths. Copying configuration and locks to another repo is a Git/template task; stable global branch-slug names require distinct branches across repos.

For native PostgreSQL snapshots, use an isolated benchmark server with `createdb`, `psql`, and `dropdb` on PATH. Set `PGHOST/PGPORT/PGUSER` to that server; place its credential/host/port URL in the seed `.env.local`; choose a closed-session seeded database `rwb_template`. Then `workz start <unique-branch> --isolated --create-db --from-db rwb_template` clones via `createdb -T`. Verify copied rows, database name, source rows unchanged after mutations, and actual DB absence after `workz done --cleanup-db`. Stable can exit 0 after failed template creation, so query PostgreSQL and run the app before recording success. This copies database contents, not a server process or Redis state.

## Optional source-head track

At main `84b6688bee54229c043eb2dac837b142a07221e5`, separately test [named service ports and repo-keyed names](https://github.com/rohansx/workz/blob/84b6688bee54229c043eb2dac837b142a07221e5/src/isolation.rs#L161), `.workz.toml` name templates such as `db_name = "{repo}_{slug}"`, hook context supplied after provisioning, `run/preview`, and process reaping. Redis startup still needs Compose or hooks. `--create-db` fallback uses `postgres:16-alpine`, so it cannot silently replace the frozen PostgreSQL cohort.

Source-head [dependency clone](https://github.com/rohansx/workz/blob/84b6688bee54229c043eb2dac837b142a07221e5/src/sync.rs#L554) invokes `cp --reflink=always` on Linux or `cp -c` on macOS, falling back to recursive copy. Report APFS/btrfs/XFS support and actual fallback; ordinary container filesystems may not reflink. [Uncommitted-code carry](https://github.com/rohansx/workz/blob/84b6688bee54229c043eb2dac837b142a07221e5/src/git.rs#L276) uses `git stash create` for tracked changes and lists/copies untracked paths. It is not an atomic whole-environment snapshot: untracked bytes are copied later, ignored dependency/service data is excluded, and PostgreSQL/Redis state is separate. Test source hashes/status unchanged, tracked+untracked destination contents, source/destination mutation independence, and config/lock copies as distinct checks.
