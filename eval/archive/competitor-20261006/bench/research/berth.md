# Berth research

Research date: 2026-10-06. Handoff to Opus 5.5. Source review and configuration checks only. No services were started, stopped, restarted, removed, or timed.

## Identity and availability

Official source: [zoltanersek/berth](https://github.com/zoltanersek/berth). The shallow clone at `/tmp/stack-bench-sources/berth` resolved main to `3b93287584dcc5c7c26298a62379d8ce001400ff`, dated 2026-07-08, message `snapshots and reset`. [Cargo.toml](https://github.com/zoltanersek/berth/blob/3b93287584dcc5c7c26298a62379d8ce001400ff/Cargo.toml#L1-L5) declares version `0.1.0`, Rust edition 2024, and minimum Rust `1.85`. The GitHub releases API returned an empty list and `git ls-remote --tags` returned no tags. There is no observed release binary or official container image to compare against a cached image. Pin the source SHA and retain Cargo.lock.

This host has Cargo `1.93.1`, rustc `1.93.1`, and Compose `v2.40.3-desktop.1`; `command -v berth` found no installed binary. A build was not executed. The documented source installation is `cargo install --path .`; use the locked version below. [CI](https://github.com/zoltanersek/berth/blob/3b93287584dcc5c7c26298a62379d8ce001400ff/.github/workflows/ci.yml) tests Ubuntu with stable Rust, format, Clippy and Rust tests. It does not establish macOS lifecycle results or end-to-end Compose success.

Executed static checks extracted the snippets into an owned temporary directory, passed `bash -n` for both shell blocks, and rendered `docker compose config --format json` on the installed plugin. Assertions confirmed exactly three services, the expected sibling worktree build context and bind source, loopback port publication, and project-scoped PostgreSQL volume naming. This proves configuration parsing, not image building, dependency installation, application correctness, or runtime lifecycle behavior.

```sh
git clone https://github.com/zoltanersek/berth "$run_root/berth-source"
git -C "$run_root/berth-source" checkout --detach 3b93287584dcc5c7c26298a62379d8ce001400ff
cargo install --locked --path "$run_root/berth-source" --root "$run_root/berth-install"
export PATH="$run_root/berth-install/bin:$PATH"
berth --version
```

The pinned application references agreed by the research team are Python `3.13.16`, PostgreSQL `17.11`, Redis `8.10.2`, and uv `0.12.23`. Their manifest availability was checked by the DevPod researcher, not by this researcher. None was pulled or executed here.

## Native behavior that matters

| Task | Supported path and boundary |
| --- | --- |
| Create two environments | `berth up NAME` creates sibling worktree `<repo>-NAME`, branch `NAME`, generated env, and Compose project `berth-NAME`. Use names unique across the entire Docker daemon, since project names contain no repository identity. |
| Python and dependencies | User-authored Dockerfile plus the shared application uv.lock. Berth has no runtime/package resolver or package lock of its own. |
| Service isolation | Compose project-scoped networks and volumes. Avoid external resources, `container_name`, fixed resource `name:`, and host networking. |
| Readiness | `up` and `start` call `docker compose up -d`, without `--wait`. Compose healthy dependencies can delay the app's creation; the final app health must still be checked. Use the shared app's `wait` command and Docker health inspection, labeled scripted. |
| Repeated application entry | No Berth `exec`, `run`, or persistent-shell command exists. Use Compose `exec -T --interactive=false app ...` with the exact generated env/project/config. Label entry as Compose delegated through benchmark glue. |
| Stop/restart | `berth stop NAME` calls Compose `stop`; `berth start NAME` calls Compose `up -d`. Worktree, containers and named volumes remain unless Compose recreates changed containers. There is a dashboard restart action, but no CLI `berth restart`. |
| Destroy | `berth down NAME` calls Compose `down -v`, then removes generated env, worktree and branch. It refuses dirty or unmerged work before touching Docker. This is destruction, not a persistence restart. |
| Data snapshots | Native `snapshot save`, `reset`, and `up --seed`; local `.berth/snapshots` tar archives use a temporary `busybox` container. Snapshotting stops containers first. Source/config/dependency reproduction is a separate task. |
| Status | `ls` considers a project running if at least one container is running. It does not establish all-service readiness or application correctness. No native wrong-instance rejection was found. |

Source evidence: [creation and Compose root](https://github.com/zoltanersek/berth/blob/3b93287584dcc5c7c26298a62379d8ce001400ff/src/up.rs#L41-L80), [worktree creation/removal](https://github.com/zoltanersek/berth/blob/3b93287584dcc5c7c26298a62379d8ce001400ff/src/worktree.rs), [Compose verbs](https://github.com/zoltanersek/berth/blob/3b93287584dcc5c7c26298a62379d8ce001400ff/src/docker.rs#L11-L78), [start/stop](https://github.com/zoltanersek/berth/blob/3b93287584dcc5c7c26298a62379d8ce001400ff/src/agent.rs#L57-L77), [safe destruction](https://github.com/zoltanersek/berth/blob/3b93287584dcc5c7c26298a62379d8ce001400ff/src/down.rs), [snapshots](https://github.com/zoltanersek/berth/blob/3b93287584dcc5c7c26298a62379d8ce001400ff/src/snapshot.rs).

Two source details need explicit treatment. First, `up` builds `compose = root.join(config.compose)` and passes `root` to Docker after creating the worktree. Ordinary `build: .` or `.:/workspace` therefore uses the original repository. The recipe below deliberately targets the sibling worktree with `${BERTH_NAME}`. This is application Compose wiring, not automatic Berth rebasing. Second, `berth agent NAME -- CMD` creates and tears down an environment for each invocation, launches CMD on the host without loading the generated service env, and returns success even if CMD exits nonzero. It is unsuitable for the retained-environment command benchmark. [Launcher implementation](https://github.com/zoltanersek/berth/blob/3b93287584dcc5c7c26298a62379d8ce001400ff/src/agent.rs#L15-L54).

## Concrete application fixture

Prepare a disposable Git repository named exactly `app` containing the shared `bench/fixtures/app` files, the Dockerfile and Compose config below, and `berth.yml`. Its parent must permit sibling worktrees. Commit the fixture before calling Berth, since it requires a valid HEAD and Git worktrees contain committed files. Add `.berth/`, `.venv/`, `.pytest_cache/`, `__pycache__/`, and `.rwb-source-marker` to the fixture's committed `.gitignore`. This keeps generated state and owned marker probes from blocking safe teardown.

`berth.yml`:

```yaml
version: 1
compose: compose.yaml
ports:
  postgres:
    env: PGPORT
  redis:
    env: REDIS_PORT
snapshot:
  volumes: [pgdata, redisdata]
```

`Dockerfile`:

```dockerfile
FROM ghcr.io/astral-sh/uv:0.12.23@sha256:61d393e44e249f2e4b526b6c7ddcecce245946826e608e11c93ad4f5bba55b21 AS uv
FROM python:3.13.16-slim-bookworm@sha256:a1165e272e578941b84abc79e4ab38a0305cd12803a5c4247979ac7655f4d641
COPY --from=uv /uv /uvx /usr/local/bin/
ENV UV_PROJECT_ENVIRONMENT=/opt/venv UV_PYTHON_DOWNLOADS=never PYTHONDONTWRITEBYTECODE=1
WORKDIR /workspace
COPY . .
RUN uv sync --frozen --python /usr/local/bin/python3.13
ENV PATH=/opt/venv/bin:$PATH
CMD ["python", "-c", "import time; time.sleep(1000000000)"]
```

`compose.yaml`:

```yaml
services:
  app:
    build:
      context: ../app-${BERTH_NAME:?Berth name required}
    volumes:
      - type: bind
        source: ../app-${BERTH_NAME:?Berth name required}
        target: /workspace
    environment:
      DATABASE_URL: postgresql://bench:bench@postgres:5432/bench
      REDIS_URL: redis://redis:6379/0
      RWB_CHECKOUT: ${BERTH_NAME}
    depends_on:
      postgres:
        condition: service_healthy
      redis:
        condition: service_healthy
    healthcheck:
      test: [CMD, python, -m, rwbapp, identity]
      interval: 1s
      timeout: 12s
      retries: 60
  postgres:
    image: postgres:17.11-alpine@sha256:b0f9560a2de083e2cc7382e75f808c7381a32852a7ec49117deedb300e552b24
    environment:
      POSTGRES_USER: bench
      POSTGRES_PASSWORD: bench
      POSTGRES_DB: bench
    ports: ["127.0.0.1:${PGPORT}:5432"]
    volumes: [pgdata:/var/lib/postgresql/data]
    healthcheck:
      test: [CMD-SHELL, "PGPASSWORD=$$POSTGRES_PASSWORD psql -h 127.0.0.1 -U $$POSTGRES_USER -d $$POSTGRES_DB -Atc 'select 1' | grep -qx 1"]
      interval: 1s
      timeout: 4s
      retries: 60
  redis:
    image: redis:8.10.2-alpine@sha256:3811787313eba226a2ef38658c6ccb91cd5e110edc89c37767de373120a0e5a0
    command: [redis-server, --appendonly, "yes", --appendfsync, always]
    ports: ["127.0.0.1:${REDIS_PORT}:6379"]
    volumes: [redisdata:/data]
    healthcheck:
      test: [CMD-SHELL, "redis-cli ping | grep -qx PONG"]
      interval: 1s
      timeout: 3s
      retries: 60
volumes:
  pgdata: {}
  redisdata: {}
```

Using a venv at `/opt/venv` keeps dependencies visible when the worktree bind mount replaces `/workspace`. The long-running app container is a normal Compose development service; the fixture's app is a CLI. Do not describe its sleep process as an HTTP application server.

## Noninteractive commands for the implementer

These are proposed commands, not executed lifecycle evidence. `run_root` is an owned absolute directory and `$run_root/app` is the committed fixture repository. Generate `name_a` and `name_b` as globally unique lowercase alphanumeric names, since the shared app marker rejects punctuation. Run this on the Docker host or a Desktop-shared host directory.

```sh
set -eu
repo="$run_root/app"
name_a="rwb${run_id}a"
name_b="rwb${run_id}b"
dc() {
  name="$1"; shift
  docker compose -f "$repo/compose.yaml" --env-file "$repo/.berth/$name.env" -p "berth-$name" "$@"
}
app() {
  name="$1"; shift
  dc "$name" exec -T --interactive=false app uv run --frozen "$@" </dev/null
}

berth --dir "$repo" validate
berth --dir "$repo" up "$name_a" </dev/null
app "$name_a" python -m rwbapp wait --timeout 120
app "$name_a" python -m rwbapp migrate
app "$name_a" python -m rwbapp mark --checkout "$name_a"
app "$name_a" python -m rwbapp crud --checkout "$name_a"
app "$name_a" python -m rwbapp cache --checkout "$name_a"
app "$name_a" pytest -q

berth --dir "$repo" up "$name_b" </dev/null
app "$name_b" python -m rwbapp wait --timeout 120
app "$name_b" python -m rwbapp migrate
app "$name_b" python -m rwbapp mark --checkout "$name_b"
app "$name_a" python -m rwbapp check --checkout "$name_a" --forbid "$name_b"
app "$name_b" python -m rwbapp check --checkout "$name_b" --forbid "$name_a"

# Prove the running app reads its own sibling worktree, then remove owned probes.
for name in "$name_a" "$name_b"; do
  printf '%s\n' "$name" > "$run_root/app-$name/.rwb-source-marker"
  app "$name" python -c 'import pathlib,os; p=pathlib.Path(".rwb-source-marker"); assert p.read_text().strip()==os.environ["RWB_CHECKOUT"]; print(p.read_text().strip())'
  rm "$run_root/app-$name/.rwb-source-marker"
done

# Repeat each independently through the recorder; retain its return code and JSON.
app "$name_a" python -m rwbapp read --checkout "$name_a"
app "$name_a" python -m rwbapp persist --checkout "$name_a"
berth --dir "$repo" stop "$name_a" </dev/null
app "$name_b" python -m rwbapp check --checkout "$name_b" --forbid "$name_a"
berth --dir "$repo" start "$name_a" </dev/null
app "$name_a" python -m rwbapp wait --timeout 120
app "$name_a" python -m rwbapp persisted --checkout "$name_a"
app "$name_a" python -m rwbapp check --checkout "$name_a" --forbid "$name_b"

# Destruction is a separate final task; these benchmark branches remain clean/merged.
berth --dir "$repo" down "$name_a" </dev/null
app "$name_b" python -m rwbapp check --checkout "$name_b" --forbid "$name_a"
berth --dir "$repo" down "$name_b" </dev/null
```

The adapter should execute subprocess argv directly rather than source generated shell files. Its environment must clear inherited `BERTH_NAME`, `COMPOSE_PROJECT_NAME`, `PGPORT`, and `REDIS_PORT`, because ambient shell variables override Compose env-file values. Capture generated env and `.berth/state.json` as evidence, but do not copy them as portable configuration. [Generated env](https://github.com/zoltanersek/berth/blob/3b93287584dcc5c7c26298a62379d8ce001400ff/src/env.rs#L11-L45). Noninteractive `exec` uses the documented Compose options. [Compose exec](https://docs.docker.com/reference/cli/docker/compose/exec/).

## Verification, reproduction and failure boundaries

- Require each command's exit status and complete JSON. Check `ok`, expected migrations, CRUD steps, and cache sequence. The current fixture's `persisted` command can return exit zero with `redis_durable: false`; explicitly require that field to be true for the agreed durable-cache task.
- Require A and B markers to match their own names and forbid the other. Check distinct PostgreSQL system identifiers, Redis run IDs, project networks, named volume names and mount sources. Internal service data paths and ports can be equal across containers; their volume identities and backing instances must differ. Redis run ID changes on process restart, while the marker and durable key must survive without reseeding.
- Inspect every service's health and exact container image IDs, Python patch, package versions, PostgreSQL version, Redis version, and uv version. The app `wait` command proves service connectivity, not Docker health for every service. Save `docker compose ps --format json` and Docker inspection alongside the app identity.
- After stop, inspect all A containers as stopped and require A's published endpoints to refuse connections. Continue B's functional checks. After start, require persisted data and readiness. `start` may reuse containers or recreate them when config/images changed; compare IDs explicitly.
- After down, query project-labeled containers, networks and volumes successfully and require empty results. Also require the owned worktree, branch, env file and state entry to be gone. A failed Docker query is an error, never evidence of no leftovers. Images, build caches and saved snapshot archives are intentionally separate retained artifacts.
- Copy source, migrations, tests, uv.lock, pyproject.toml, Dockerfile, berth.yml and compose.yaml into a fresh committed `app` repository under a different owned parent. Use a fresh unique berth name, repeat source-marker/functional checks, and require common file/lock hashes unchanged. A new project name isolates resources. `.berth/state.json`, assigned ports and local snapshots are not a dependency lock.
- Count source tool build, image pulls, application build/uv installation, readiness and repeated commands separately. A warm repeated app command uses the retained container and uv environment. A copied checkout can share immutable Docker build/image cache without sharing database/cache state.
- Native `up` rejects an already-existing name. Use `start` for repeated bring-up of an existing berth; do not call repeated `up` an idempotence failure. Port selection binds `127.0.0.1:0` and releases reservations before Docker starts, leaving a race. Do not claim a guaranteed port-collision guard. [Port allocator](https://github.com/zoltanersek/berth/blob/3b93287584dcc5c7c26298a62379d8ce001400ff/src/ports.rs#L4-L32).
- Keep bad startup in its own disposable repository/project. On failed `docker::up`, the source sets `rollback.docker_started` only after success, so partial Compose resources might remain while env/worktree rollback succeeds. The recorder must inspect and clean only its own project even if Berth has no state entry. This is a source inference awaiting execution, not an observed failure. [Rollback](https://github.com/zoltanersek/berth/blob/3b93287584dcc5c7c26298a62379d8ce001400ff/src/up.rs#L21-L37).

Use Docker Desktop with native ARM64 images on macOS, or Linux Docker Engine and Compose. Berth itself requires Git, Compose, daemon connectivity and writable sibling directories. A runner inside another container needs its worktree mount paths visible to the daemon host; mounting the Docker socket alone does not provide that. A remote daemon needs matching path sharing, or an explicitly separate build-only application lane. This bind-mount recipe is not evidence of remote-daemon portability. Berth's state uses filesystem locking, but source review does not establish every concurrent lifecycle operation as atomic.

The existing `eval/` files contain no Berth adapter or Berth results. The old database-only Compose `true` timings are historical context and cannot count as Berth application evidence. Berth is a direct local worktree/service lifecycle competitor. Give it its native Compose isolation, start/stop and snapshot behavior; record Python provisioning, application command entry, readiness verification and worktree-to-Compose wiring at their actual delegated or scripted boundaries.
