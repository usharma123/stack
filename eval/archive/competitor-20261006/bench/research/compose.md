# Docker Compose research for a real application benchmark

Research date: 2026-10-06. Owner: Compose researcher. This is a recipe and source review for the benchmark implementer, Opus 5.5. It contains no new timing results. No containers were started, restarted, or removed during research.

## Version and evidence boundary

The latest official release observed was [v5.6.0, published October 2, 2026](https://github.com/docker/compose/releases/tag/v5.6.0). Its source commit is `42f48072bbf92ee9b0e43f9fdf2008d03546e7ca`. A shallow clone at `/tmp/stack-bench-sources/compose` first resolved main to `524a36d2cf2deaf9eed1c3671d7cb93f07671b48`, then fetched and checked out the release tag for the implementation review below. Use the release commit when reproducing this research.

Executed, read-only observations on this macOS ARM64 host:

| Component | Observed identity |
| --- | --- |
| Docker CLI | `28.1.1`, build `4eba377327`, `/opt/homebrew/bin/docker` |
| Installed Compose plugin | `v2.40.3-desktop.1` |
| Docker daemon | Engine `29.1.3`, Linux ARM64, API `1.52` |
| Docker Desktop | `4.55.0`, build `213807` |
| Cached PostgreSQL | `postgres:17.6-alpine`, metadata `PG_VERSION=17.6` |
| Cached Redis | `redis:8-alpine`, metadata `REDIS_VERSION=8.10.2` |
| Cached Python application base | The image inventory contained no `python:*` image |

`docker compose --help` and command help confirmed `up --wait --wait-timeout`, `exec -T`, `run --rm --no-deps`, and both config digest flags on the installed plugin. Registry manifest reads, using anonymous Docker Hub token requests, confirmed the exact image references below and Linux ARM64/AMD64 support. Those reads did not pull images. The usual `docker buildx imagetools inspect` attempt failed in the host credential helper with `error getting credentials`, exit status 1, and empty helper output; record that as a preflight issue if it affects the eventual run. Anonymous registry reads are not proof that Docker's configured credentials can pull/build successfully.

Static validation extracted the fixture into a disposable directory, parsed both Python files and the lock-generator Python, passed `bash -n` for the shell recipes, and passed the installed plugin's `config` validation. Its rendered JSON contained exactly `app`, `postgres`, and `redis`, the expected project-scoped network and volumes, required healthy dependency conditions, and no published host ports. This validates configuration parsing on v2.40.3, not v5.6.0 runtime behavior, dependency installation, application correctness, or lifecycle success.

Use v5.6.0 for a current-release comparison, or clearly label a run on the installed v2.40.3 plugin. A Compose plugin version is not an application container image version. Docker client, daemon, VM, BuildKit, CPU architecture, and resource allocation all belong in the run manifest. Do not replace the installed plugin globally for this study. A versioned release binary can live in the benchmark's private tools directory, with its release checksum verified, and be invoked directly as `docker-compose` against the same daemon. [Official installation instructions](https://github.com/docker/compose#where-to-get-docker-compose).

## What Compose supports natively

Compose is a strong direct baseline for an application with containerized dependencies. Its normal workflow builds an application image, starts the application and services together, executes commands in a retained container, and removes project resources explicitly. [Official quickstart](https://docs.docker.com/compose/gettingstarted/).

| Requirement | Native behavior | Work the benchmark must supply |
| --- | --- | --- |
| Pinned Python and Python dependencies | Dockerfile build using a digest-pinned Python base | The dependency installer and hash-pinned lock are application choices; Compose is not a Python package solver |
| PostgreSQL and Redis | Declare services with image references | Schema, cache semantics, markers, and functional integration tests |
| Independent checkouts | Explicit `-p` project names, separate default networks, project-scoped named volumes | Unique project names per run and checkout; do not reuse one name across both |
| Readiness | Docker healthchecks, `depends_on: condition: service_healthy`, `up --wait` | Useful service probes and application readiness probe |
| Repeated work | `exec -T` in the running application container | Workload assertions and timing collection |
| Fresh command container | `run --rm -T` | Report separately from retained-container execution |
| Persistent data | Named volumes retained by ordinary `down` | Redis persistence configuration and proof that stored values survive recreation |
| Teardown | `down`, optional `--volumes --remove-orphans` | Check project labels after cleanup, preserve receipts on failure |
| Copyable configuration | Compose files, Dockerfile, source, lock; digest override generation | Version control and image distribution/export for a built application image |
| Shared configuration | Native local/Git/OCI `include` and explicit overrides | Select immutable Git revisions or OCI digests and test conflicts according to Compose's rules |

Compose's [include workflow](https://docs.docker.com/compose/how-tos/multiple-compose-files/include/) is a real composition capability. Included resources are copied into the parent model; duplicate resources require the supported override route. Evaluate that if reusable project definitions matter. Do not award Stack an exclusive sharing capability merely because Compose uses YAML rather than bundles.

## Source implementation checked

These links point to the release commit, not a moving branch. Claims here come from reading code; runtime behavior still needs the benchmark execution.

- [Project loading and container labels, `loader.go`](https://github.com/docker/compose/blob/42f48072bbf92ee9b0e43f9fdf2008d03546e7ca/pkg/compose/loader.go#L105). The model loads environment/config/name options, then adds project, service, Compose version, working-directory, and config-file labels. [Project-name precedence](https://docs.docker.com/reference/cli/docker/compose/#use--p-to-specify-a-project-name) puts `-p` ahead of environment, top-level name, and directory basename.
- [Network and volume preparation, `create.go`](https://github.com/docker/compose/blob/42f48072bbf92ee9b0e43f9fdf2008d03546e7ca/pkg/compose/create.go#L144). Compose adds project ownership labels to managed networks and named volumes. [Network endpoint creation](https://github.com/docker/compose/blob/42f48072bbf92ee9b0e43f9fdf2008d03546e7ca/pkg/compose/create.go#L477) supplies service aliases and the model's resolved network name. [Network documentation](https://docs.docker.com/compose/how-tos/networking/) describes project networks and service-name DNS.
- [Startup wait, `start.go`](https://github.com/docker/compose/blob/42f48072bbf92ee9b0e43f9fdf2008d03546e7ca/pkg/compose/start.go#L82) builds a required condition for each service and waits with the requested timeout. [Dependency polling](https://github.com/docker/compose/blob/42f48072bbf92ee9b0e43f9fdf2008d03546e7ca/pkg/compose/service_containers.go#L162) checks health conditions every 500 ms. [Health evaluation](https://github.com/docker/compose/blob/42f48072bbf92ee9b0e43f9fdf2008d03546e7ca/pkg/compose/service_containers.go#L531) can fall back to running for a service without a healthcheck. `--wait` is therefore only an application readiness check when the app has a useful healthcheck. [Official startup-order guide](https://docs.docker.com/compose/how-tos/startup-order/).
- [Retained-container execution, `exec.go`](https://github.com/docker/compose/blob/42f48072bbf92ee9b0e43f9fdf2008d03546e7ca/pkg/compose/exec.go#L30) selects a project/service container and invokes Docker exec, propagating command failure. It does not call `waitDependencies` before every exec. Match this to a benchmark where all tools start and verify services before the repeated-work phase, and record separate per-command identity guarantees where relevant.
- [One-off execution, `run.go`](https://github.com/docker/compose/blob/42f48072bbf92ee9b0e43f9fdf2008d03546e7ca/pkg/compose/run.go#L122) starts dependencies, chooses a one-off container name, and waits on dependency conditions unless `NoDeps` is set. This path includes container creation work; it is not interchangeable with `exec` latency.
- [Teardown, `down.go`](https://github.com/docker/compose/blob/42f48072bbf92ee9b0e43f9fdf2008d03546e7ca/pkg/compose/down.go#L112) removes containers and managed networks; named volume removal is conditional on `options.Volumes`. [Official down reference](https://docs.docker.com/reference/cli/docker/compose/down/) explains ordinary versus volume-removing teardown and external-resource exclusions.
- [Config command setup, `config.go`](https://github.com/docker/compose/blob/42f48072bbf92ee9b0e43f9fdf2008d03546e7ca/cmd/compose/config.go#L113) enables image resolution automatically for `--lock-image-digests`. [The rendering path](https://github.com/docker/compose/blob/42f48072bbf92ee9b0e43f9fdf2008d03546e7ca/cmd/compose/config.go#L208) resolves image references and reduces the result to an image override. `--resolve-image-digests` produces a full rendered model instead. [Official config reference](https://docs.docker.com/reference/cli/docker/compose/config/).
- [Volume reconciliation, `reconcile.go`](https://github.com/docker/compose/blob/42f48072bbf92ee9b0e43f9fdf2008d03546e7ca/pkg/compose/reconcile.go#L419) handles missing volumes and configuration drift. An accepted recreation can discard data. The routine persistence task should keep the volume definition unchanged and recreate containers, not deliberately change the volume's driver/options.

## Concrete application fixture

The following fixture is proposed implementation input, not an executed integration result. Freeze it once and use the same Python application, assertions, dependency versions, and business workload for every comparable tool. It is a small invoice application: commit a total to PostgreSQL, cache it in Redis, and recover it from PostgreSQL after a cache miss. The retained app container serves a health endpoint and is also the place where integration tests execute.

Use `python:3.12.12-slim-bookworm` at the manifest digest below as a deliberately pinned common Python version. This is not a latest-Python claim. The PostgreSQL 17.6 image matches the existing cached baseline; Redis 8.10.2 is the exact version observed behind the cached Redis 8 tag. If the study chooses other common versions, change all tools together before measurement. These manifest-index references were verified via Docker Hub registry reads on the research date:

```text
python:3.12.12-slim-bookworm@sha256:593bd06efe90efa80dc4eee3948be7c0fde4134606dd40d8dd8dbcade98e669c
postgres:17.6-alpine@sha256:ef257d85f76e48da1c64832459b59fcaba1a4dac97bf5d7450c77753542eee94
redis:8.10.2-alpine@sha256:3811787313eba226a2ef38658c6ccb91cd5e110edc89c37767de373120a0e5a0
```

`compose.yaml`:

```yaml
services:
  app:
    build:
      context: .
    environment:
      DATABASE_URL: postgresql://bench:bench@postgres:5432/bench
      REDIS_URL: redis://redis:6379/0
      BENCH_MARKER: ${BENCH_MARKER:?supply the checkout marker}
    command: [python, task.py, serve]
    depends_on:
      postgres:
        condition: service_healthy
      redis:
        condition: service_healthy
    healthcheck:
      test: [CMD, python, -c, "import urllib.request; urllib.request.urlopen('http://127.0.0.1:8000/healthz', timeout=2).read()"]
      interval: 1s
      timeout: 4s
      retries: 60
      start_period: 2s
  postgres:
    image: postgres:17.6-alpine@sha256:ef257d85f76e48da1c64832459b59fcaba1a4dac97bf5d7450c77753542eee94
    environment:
      POSTGRES_USER: bench
      POSTGRES_PASSWORD: bench
      POSTGRES_DB: bench
    volumes:
      - pgdata:/var/lib/postgresql/data
    healthcheck:
      test: [CMD-SHELL, "PGPASSWORD=$$POSTGRES_PASSWORD psql -h 127.0.0.1 -U $$POSTGRES_USER -d $$POSTGRES_DB -Atc 'select 1' | grep -qx 1"]
      interval: 1s
      timeout: 4s
      retries: 60
      start_period: 2s
  redis:
    image: redis:8.10.2-alpine@sha256:3811787313eba226a2ef38658c6ccb91cd5e110edc89c37767de373120a0e5a0
    command: [redis-server, --appendonly, "yes", --appendfsync, always]
    volumes:
      - redisdata:/data
    healthcheck:
      test: [CMD-SHELL, "redis-cli ping | grep -qx PONG"]
      interval: 1s
      timeout: 3s
      retries: 60
volumes:
  pgdata: {}
  redisdata: {}
```

No host ports are necessary for app-to-service work. Both checkouts use `postgres:5432` and `redis:6379` inside their own networks. Do not use `localhost` for these URLs inside the app. Avoid `container_name`, `network_mode: host`, `external: true`, or a fixed `name:` on a network/volume in this isolation fixture. A named volume with an explicit custom name is not automatically scoped by the project. [Network attributes](https://docs.docker.com/reference/compose-file/networks/), [volume attributes](https://docs.docker.com/reference/compose-file/volumes/).

`Dockerfile`:

```dockerfile
FROM python:3.12.12-slim-bookworm@sha256:593bd06efe90efa80dc4eee3948be7c0fde4134606dd40d8dd8dbcade98e669c
ENV PYTHONUNBUFFERED=1 PYTHONDONTWRITEBYTECODE=1 PIP_DISABLE_PIP_VERSION_CHECK=1
WORKDIR /workspace
COPY requirements.lock .
RUN python -m pip install --only-binary=:all: --require-hashes -r requirements.lock
COPY task.py test_application.py ./
CMD ["python", "task.py", "serve"]
```

`requirements.lock` should be checked into the common fixture. For this recipe, generate it once before the benchmark, using the following noninteractive host preparation command. PyPI version metadata was read during research to verify that these exact versions exist and that this is the full dependency set for CPython 3.12 on Linux. Principal metadata sources are [pytest 8.4.2](https://pypi.org/pypi/pytest/8.4.2/json), [psycopg 3.2.10](https://pypi.org/pypi/psycopg/3.2.10/json), and [redis 6.4.0](https://pypi.org/pypi/redis/6.4.0/json). The hash generator permits the published wheels for each pinned version so both ARM64 and AMD64 can use the same file. Generation is network preparation, not a measured Compose command.

```sh
python3 - <<'PY'
import json
from pathlib import Path
from urllib.request import urlopen
versions = {
    "pytest": "8.4.2", "iniconfig": "2.1.0", "packaging": "25.0",
    "pluggy": "1.6.0", "pygments": "2.19.2", "psycopg": "3.2.10",
    "psycopg-binary": "3.2.10", "redis": "6.4.0",
    "typing-extensions": "4.15.0",
}
lines = []
for package, version in sorted(versions.items()):
    with urlopen(f"https://pypi.org/pypi/{package}/{version}/json", timeout=30) as r:
        metadata = json.load(r)
    hashes = sorted({x["digests"]["sha256"] for x in metadata["urls"]
                     if x["packagetype"] == "bdist_wheel" and not x["yanked"]})
    if not hashes:
        raise RuntimeError(f"No non-yanked wheels for {package}=={version}")
    lines.append(f"{package}=={version} " + " ".join(f"--hash=sha256:{h}" for h in hashes))
Path("requirements.lock").write_text("\n".join(lines) + "\n")
PY
```

`task.py`:

```python
import json
import os
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer

import psycopg
import redis

def database():
    return psycopg.connect(os.environ["DATABASE_URL"], connect_timeout=3)

def cache():
    return redis.Redis.from_url(os.environ["REDIS_URL"], decode_responses=True,
                               socket_connect_timeout=3, socket_timeout=3)

def health():
    with database() as db:
        if db.execute("select 1").fetchone() != (1,):
            raise RuntimeError("PostgreSQL probe mismatch")
    if not cache().ping():
        raise RuntimeError("Redis probe mismatch")

def seed():
    marker = os.environ["BENCH_MARKER"]
    with database() as db:
        db.execute("create table if not exists instance_marker (slot text primary key, value text not null)")
        db.execute("create table if not exists invoices (id text primary key, total integer not null)")
        db.execute("insert into instance_marker values ('owner', %s) on conflict do nothing", (marker,))
    existing = cache().get("bench:owner")
    if existing is None:
        cache().set("bench:owner", marker, nx=True)
    verify()

def verify():
    expected = os.environ["BENCH_MARKER"]
    with database() as db:
        rows = db.execute("select value from instance_marker where slot='owner'").fetchall()
    actual = cache().get("bench:owner")
    if rows != [(expected,)] or actual != expected:
        raise RuntimeError(f"Wrong checkout data: postgres={rows!r}, redis={actual!r}, expected={expected!r}")
    return {"postgres_marker": rows[0][0], "redis_marker": actual}

def add_invoice(invoice_id, amounts):
    total = sum(amounts)
    with database() as db:
        db.execute("insert into invoices values (%s, %s)", (invoice_id, total))
    cache().set("invoice:" + invoice_id, str(total))
    return total

def get_invoice(invoice_id):
    value = cache().get("invoice:" + invoice_id)
    if value is not None:
        return int(value), "cache"
    with database() as db:
        row = db.execute("select total from invoices where id=%s", (invoice_id,)).fetchone()
    if row is None:
        raise KeyError(invoice_id)
    cache().set("invoice:" + invoice_id, str(row[0]))
    return row[0], "postgres"

class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path != "/healthz":
            self.send_error(404)
            return
        try:
            health()
            body = b"ready\n"
            self.send_response(200)
        except Exception as error:
            body = str(error).encode()
            self.send_response(503)
        self.end_headers()
        self.wfile.write(body)

if __name__ == "__main__":
    action = sys.argv[1]
    if action == "serve":
        HTTPServer(("0.0.0.0", 8000), Handler).serve_forever()
    elif action == "health":
        health()
        print("ready")
    elif action == "seed":
        seed()
        print(json.dumps(verify(), sort_keys=True))
    elif action == "verify":
        print(json.dumps(verify(), sort_keys=True))
    else:
        raise SystemExit("unknown action")
```

`test_application.py`:

```python
import importlib.metadata
import json
import os
import sys
import uuid
from urllib.request import urlopen

import task

def test_runtime():
    assert sys.version_info[:3] == (3, 12, 12)
    for package, version in {"psycopg": "3.2.10", "redis": "6.4.0", "pytest": "8.4.2"}.items():
        assert importlib.metadata.version(package) == version

def test_application_ready():
    with urlopen("http://127.0.0.1:8000/healthz", timeout=3) as response:
        assert response.status == 200
        assert response.read() == b"ready\n"

def test_checkout_identity():
    assert task.verify() == {"postgres_marker": os.environ["BENCH_MARKER"],
                             "redis_marker": os.environ["BENCH_MARKER"]}

def test_invoice_commit_cache_and_cache_miss():
    invoice_id = str(uuid.uuid4())
    key = "invoice:" + invoice_id
    try:
        assert task.add_invoice(invoice_id, [125, 250, 375]) == 750
        assert task.get_invoice(invoice_id) == (750, "cache")
        with task.database() as db:
            assert db.execute("select total from invoices where id=%s", (invoice_id,)).fetchone() == (750,)
        assert task.cache().delete(key) == 1
        assert task.get_invoice(invoice_id) == (750, "postgres")
        assert task.get_invoice(invoice_id) == (750, "cache")
    finally:
        task.cache().delete(key)
        with task.database() as db:
            db.execute("delete from invoices where id=%s", (invoice_id,))
```

Use pytest's normal assertion rewriting; do not run the checks under Python optimization. Outside pytest, the harness must use explicit failure checks rather than `assert`, and require both zero exit and the expected receipt content.

## Noninteractive lifecycle recipe

Assume the files above and the generated lock exist in a frozen fixture directory. Run these commands under `bash` with stdin closed. They create only uniquely named benchmark resources. The script is a suggested flow for the eventual runner; research did not execute it.

```sh
set -euo pipefail
: "${COMPOSE_FIXTURE:?absolute path to the frozen fixture directory}"
run_root=$(mktemp -d "${TMPDIR:-/tmp}/compose-application.XXXXXX")
run_id=$(python3 -c 'import uuid; print(uuid.uuid4().hex[:12])')
project_a="bench-compose-${run_id}-a"
project_b="bench-compose-${run_id}-b"
checkout_a="$run_root/checkout-a"
checkout_b="$run_root/checkout-b"
mkdir "$checkout_a" "$checkout_b"
cp "$COMPOSE_FIXTURE"/{compose.yaml,Dockerfile,requirements.lock,task.py,test_application.py} "$checkout_a/"
cp "$COMPOSE_FIXTURE"/{compose.yaml,Dockerfile,requirements.lock,task.py,test_application.py} "$checkout_b/"
printf 'BENCH_MARKER=%s\n' "$project_a" > "$checkout_a/bench.env"
printf 'BENCH_MARKER=%s\n' "$project_b" > "$checkout_b/bench.env"

# Explicit paths prevent accidental parent-directory or ambient .env selection.
ca() {
  docker compose --project-directory "$checkout_a" --env-file "$checkout_a/bench.env" \
    -f "$checkout_a/compose.yaml" -p "$project_a" "$@"
}
cb() {
  docker compose --project-directory "$checkout_b" --env-file "$checkout_b/bench.env" \
    -f "$checkout_b/compose.yaml" -p "$project_b" "$@"
}

# The real harness must capture cleanup errors and fail the run if cleanup fails.
# This trap is a fallback, not the final cleanup receipt.
trap 'ca down --volumes --remove-orphans </dev/null || true; cb down --volumes --remove-orphans </dev/null || true' EXIT

ca config --quiet </dev/null
cb config --quiet </dev/null
ca config --format json > "$run_root/a.config.json"
cb config --format json > "$run_root/b.config.json"
docker version --format '{{json .}}' > "$run_root/docker-version.json"
docker compose version > "$run_root/compose-version.txt"

# Cold preparation: pulls, dependency installation, and builds are measured
# separately from warm startup and repeated application work.
ca pull postgres redis </dev/null
ca build app </dev/null
cb build app </dev/null
ca up -d --wait --wait-timeout 120 </dev/null
cb up -d --wait --wait-timeout 120 </dev/null
ca exec -T app python task.py seed </dev/null
cb exec -T app python task.py seed </dev/null
ca exec -T app python task.py verify </dev/null > "$run_root/a.identity.json"
cb exec -T app python task.py verify </dev/null > "$run_root/b.identity.json"
ca exec -T app python -m pytest -q test_application.py </dev/null
cb exec -T app python -m pytest -q test_application.py </dev/null

# The measured warm command is a real integration suite in the retained app.
# Run each command through the timer/receipt collector; do not time this loop
# as though it were one sample, and never overlap competitors' timing runs.
for i in $(seq 1 20); do
  ca exec -T app python -m pytest -q test_application.py </dev/null
done

# Repeated up should preserve unchanged container identities and data.
ca ps -q > "$run_root/a.containers.before"
ca up -d --wait --wait-timeout 120 </dev/null
ca ps -q > "$run_root/a.containers.after"
ca exec -T app python task.py verify </dev/null

# Full container removal, while named data volumes remain.
ca down </dev/null
cb exec -T app python task.py verify </dev/null
ca up -d --wait --wait-timeout 120 </dev/null
ca exec -T app python task.py verify </dev/null
ca exec -T app python -m pytest -q test_application.py </dev/null

# Reset A's data; B must still return its own marker and pass its tests.
ca down --volumes --remove-orphans </dev/null
cb exec -T app python task.py verify </dev/null
cb exec -T app python -m pytest -q test_application.py </dev/null
cb down --volumes --remove-orphans </dev/null

for project in "$project_a" "$project_b"; do
  test -z "$(docker ps -aq --filter "label=com.docker.compose.project=$project")"
  test -z "$(docker network ls -q --filter "label=com.docker.compose.project=$project")"
  test -z "$(docker volume ls -q --filter "label=com.docker.compose.project=$project")"
done
trap - EXIT
```

The final Python harness should replace shell command substitutions in cleanup checks with captured subprocess results and explicit return-code checks. An empty string from a failed Docker command must never count as successful cleanup. The same applies to marker/identity reads. Retain `run_root` receipts until the report is verified; never perform a global Docker prune.

For a one-off application command, use `ca run --rm -T --no-deps app python task.py verify </dev/null` after the project has passed readiness. The verification CLI does not need to reach the retained application's HTTP listener. For fresh-container pytest, the current readiness test points to localhost and would need to address the retained `app` service by DNS, or run the server in the fresh test container. Make that change explicit. Report the one-off command as a separate task, not as the warm retained `exec` path.

For an additional restart task, run `ca stop`, then `ca start --wait --wait-timeout 120`, then verify and test. Record this separately from `down`/`up` and from an abrupt process-failure scenario. Named PostgreSQL volumes preserve committed transactions. Redis's mounted `/data` plus AOF makes the marker durable; `appendfsync always` deliberately sets a stronger durability policy. Apply the same Redis persistence policy across all competitors, or report policy differences rather than compare different guarantees.

## Exact receipts and checks for the implementer

1. Save every command's argv, cwd, sanitized environment, start/end timestamps, stdout, stderr, and exit code. Include pull/build logs and failed attempts. Separate install, resolve/download, build/dependency installation, warm readiness, warm functional command, lifecycle, and teardown phases. Apply a whole-command subprocess deadline as well as `--wait-timeout`; the latter is not a general pull/build/process deadline.
2. Parse both identity JSON receipts and require exact equality to their respective unique project marker. A's PostgreSQL marker and Redis marker must differ from B's. Validate at startup, after repeated commands, after A teardown while B survives, and after A down/up. Do not reseed between down and up; that would hide lost data.
3. `ps --format json` must show all three services running and healthy after `up --wait`. Parse JSON robustly; supported CLI versions can produce an array or newline-delimited objects. Also inspect container labels, image IDs, mounts, network IDs, and Docker health state. A and B's app/postgres/redis containers must belong to their respective project labels. [Official up behavior](https://docs.docker.com/reference/cli/docker/compose/up/).
4. Capture PostgreSQL system identity with `exec -T postgres psql -U bench -d bench -Atc 'select system_identifier::text from pg_control_system()'`. Capture Redis identity with `exec -T redis redis-cli --raw INFO server`, parse `run_id`, and record `redis_version`. Require distinct system identifiers/run IDs across A and B while both are running. PostgreSQL system identity remains with its data volume after container recreation; Redis run ID changes when its process restarts, so require persistent marker equality rather than Redis run-ID stability after restart.
5. Inspect named volume mounts and network IDs through Docker inspection. Expected logical names are A/B's `<project>_pgdata`, `<project>_redisdata`, and `<project>_default`. Require different resource IDs/names and correct project labels. Structural isolation plus application markers is better evidence than testing only container-name differences. A default bridge network is project isolation for normal service discovery; it is not a security boundary against a Docker administrator or a malicious process with daemon access.
6. Save Python `--version`, package versions, PostgreSQL `show server_version`, Redis `INFO server`, container image IDs, and registry digests. A mutable tag, or merely inspecting the host's Python, is insufficient. The application test requires exact Python patch and package versions; add equivalent service version checks to the harness.
7. Idempotent `up` should retain container IDs for this unchanged configuration and fixed image set. `down`/`up` should replace container IDs while preserving both durable markers. Compare these states and require explicit success; do not infer persistence from a zero `up` exit.
8. After `down --volumes --remove-orphans`, require successful Docker queries that show no project-labeled containers, managed networks, or declared volumes. Cached images/build layers are deliberately retained. Compare cleanup semantics accordingly, rather than treat an image cache as an orphaned service.

## Reproduction and copied-checkout task

Compose's config renderer is not a complete dependency lock. The useful reproduction artifact includes `compose.yaml`, Dockerfile, Python source, tests, `requirements.lock`, immutable service/base-image references, and source control identity. Copy those exact files into checkout B, then provide B's separate local marker/project identity. Record byte hashes of the common files and keep markers outside the portable application definition.

For a tag-based Compose file, a native service-image override can be prepared before measurement with:

```sh
ca config --lock-image-digests postgres redis > "$checkout_a/compose.images.lock.yaml"
```

The reviewed v5.6.0 CLI enables digest resolution automatically for this flag; adding `--resolve-image-digests` is redundant. A build-only `app` without `image:` has no published application image to resolve; use this override for the registry-image services. The Dockerfile's `FROM` digest and Python lock remain necessary. If the app is distributed instead of rebuilt, give it an immutable registry digest and copy that reference too. The lock/render operation may contact the registry and belongs in preparation. Avoid copying a fully rendered configuration containing A's absolute build paths, project resource names, or local environment into B. [Config command documentation](https://docs.docker.com/reference/cli/docker/compose/config/).

For an offline reproduction lane, build the application image once, tag it per the benchmark fixture identity, export that image and dependency images with `docker image save`, then import them in a clean daemon and run with immutable recorded image IDs/digests and builds disabled. This is a Docker image distribution task. It is different from copying a source checkout and rebuilding Python wheels from network sources. Report those as separate tasks and preserve hashes of the archive and fixture files.

## Fair comparison and platform constraints

Use two principal lanes. In the task-completion lane, every tool completes the same invoice integration suite, checkout-isolation checks, and persistence/cleanup tasks using its normal supported workflow. Compose should get its retained application container and native healthchecks. In the command-entry lane, describe the actual execution path: Compose CLI plus daemon transport plus container exec, versus a host process launcher for host-oriented tools. The difference is relevant, but a single `true` measurement is not application productivity or a universal tool win.

Cold app setup includes downloading the Python base and service images, Dockerfile dependency installation, and app build. Warm setup can reuse image layers and the dependency cache. Measure both; do not give Compose only a cold rebuild while the other tools get preinstalled dependencies, or give it a warm cache while other tools reinstall. For copied checkouts, report how much cache is shared by the same daemon. That shared immutable cache does not imply shared database/cache data.

macOS uses Linux containers through Docker Desktop's VM. Native ARM64 images avoid forced AMD64 emulation. Linux can use Docker Engine directly. A container running the benchmark needs a reachable Docker daemon and usable Compose CLI; mounting the host socket uses the host daemon and does not create an isolated daemon. This recipe uses image `COPY` rather than runtime source bind mounts, so a runner inside another container does not require its checkout path to exist on the daemon host. Remote-daemon and Docker-in-Docker cases need separate environment labels and cold-start accounting. Windows Desktop is adjacent platform support; these POSIX commands are not a tested PowerShell recipe.

Docker daemon availability and image build/pull are preflight requirements, not Compose installation success. Capture credential-helper failures, permission errors, available disk space, and daemon/VM resource configuration as environment facts. The read-only checks here confirmed daemon connectivity and cached image metadata but did not validate a full pull or build. Docker Compose installation alone does not provision Docker Engine on every platform.

## Historical evaluation limitations

The existing [`eval/harness/compose-benchmark.py`](../../eval/harness/compose-benchmark.py) creates two uniquely named database-only projects, waits for health, writes PostgreSQL/Redis markers, and repeatedly executes `true` inside PostgreSQL. It includes useful marker-isolation and teardown checks. It has no Python application container, no Python dependency setup, no functional application integration suite, and no named-volume persistence task. Redis persistence is not explicitly enabled there. Its old timings cannot populate the new application-task report.

[`eval/fixture/tests/test_stack.py`](../../eval/fixture/tests/test_stack.py) checks Python major/minor, a PostgreSQL `select 1`, and Redis ping. Reuse it only as historical smoke-test context. The stronger fixture above checks actual invoice commit, cache hit, database fallback, and checkout-specific data. [`eval/PERFORMANCE.md`](../../eval/PERFORMANCE.md) reports an October 5 host-only released/candidate Stack comparison, not a fresh Compose result.

## Common mistakes to reject

- Reusing directory basenames as project names for two checkouts named `app`, or setting the same top-level `name:` without explicit `-p`.
- Publishing both projects on the same fixed host port when the task only needs container-internal communication.
- Checking only process-running status, using bare `depends_on`, or equating `docker compose wait` with readiness. `wait` waits for containers to stop; `up --wait` waits for running/healthy startup.
- Running `exec` before the app exists, or measuring a one-off `run` container as though it were retained `exec`.
- Omitting `-T`/closed stdin in unattended execution, losing exit status in shell pipelines, or accepting missing receipts as success.
- Connecting the app to the host's database/cache by default URLs, or using shared external networks/explicit volume names without declaring that sharing is intentional.
- Treating `down --volumes` data loss as a persistence failure, treating Redis memory-only mode as durable storage, or reseeding markers after restart.
- Using floating `redis:8-alpine` as an exact-version pin, calling the installed older plugin the latest release, or claiming a digest override also locks Dockerfile/dependency builds.
- Reusing the historical database-only harness as evidence that a Python application worked.
- Calling Docker VM/daemon resource limits a property of Compose without recording the host setup, or running timing tests in parallel with competitor installation/build work.

The expected result is a supported Compose workflow with useful native isolation and lifecycle behavior, plus explicit application code and reproducibility files. The benchmark must execute it before reporting task success, latency, durability, or cleanup outcomes.
