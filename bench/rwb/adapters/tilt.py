"""Tilt on its local Docker Compose backend (no Kubernetes). See bench/research/tilt.md.

Host transport: Tilt and its Compose backend drive the host Docker daemon. Each checkout is
one Compose project, `rwb-<run id>-<checkout>`, with its own network and named volumes
(container boundary; in-container ports and paths repeat). `tilt ci` is the bounded startup
gate; `docker_compose(..., wait=True)` makes resources ready only after their healthchecks
pass. Tilt has no exec command for Compose services, so app commands use the same Compose
binary Tilt is pinned to (`compose exec`), into a toolchain container that bind-mounts the
checkout at its own absolute path.
"""
import os
import posixpath

from .base import Adapter, q

TILT_VERSION = "0.37.8"
TILT_ARCHIVE_SHA256 = "2d396b13c479f74deb19cb2161a3f0858f6e03f26f15f8d3b3be982d09ff4484"  # mac.arm64.tar.gz
TILT_BINARY_SHA256 = "190255a6e64023b4cfe7a2bbecb34b41113d8c5db7d74f74555b8827e38cfb79"
COMPOSE_VERSION = "5.6.0"
COMPOSE_BINARY_SHA256 = "bd714a42b46e51757fc1121085b3096b9064e3f80d3ca5a991e33f44df4f43c9"  # darwin-aarch64
IMAGES = dict(
    python="python:3.13.16-slim-bookworm@sha256:a1165e272e578941b84abc79e4ab38a0305cd12803a5c4247979ac7655f4d641",
    uv="ghcr.io/astral-sh/uv:0.12.23@sha256:61d393e44e249f2e4b526b6c7ddcecce245946826e608e11c93ad4f5bba55b21",
    postgres="postgres:17.6-alpine@sha256:ef257d85f76e48da1c64832459b59fcaba1a4dac97bf5d7450c77753542eee94",
    redis="redis:8.10.2-alpine@sha256:3811787313eba226a2ef38658c6ccb91cd5e110edc89c37767de373120a0e5a0")


def darwin_arm64_preflight(*commands):
    """Host preflight. Pinned binaries are darwin-arm64 and the Docker daemon must answer.
    A missing prerequisite exits 77 with `RWB-BLOCKED: <reason>` (recorded as blocked, not
    as a tool failure) after printing the facts it checked."""
    blocked = lambda reason: f'{{ echo "RWB-BLOCKED: {reason}" >&2; exit 77; }}'
    return "\n".join([
        'echo "host: $(uname -s) $(uname -m) $(sw_vers -productVersion 2>/dev/null)"',
        'test "$(uname -s)-$(uname -m)" = Darwin-arm64 || ' + blocked("pinned binaries are darwin-arm64; host is $(uname -s)-$(uname -m)"),
        "command -v docker >/dev/null || " + blocked("docker CLI not on PATH"),
        'echo "docker context: $(docker context show 2>&1)"',
        "docker version --format 'docker client {{.Client.Version}}, daemon {{.Server.Version}} {{.Server.Os}}/{{.Server.Arch}}' || "
        + blocked("Docker daemon not reachable"),
        *commands,
    ])


def remove_owned(listing):
    """Host cleanup: remove exactly what `listing` (lines `kind name [id]`) reports, then
    print it. Untagged images are removed by ID. Empty input runs nothing."""
    return "\n".join([
        "set -eo pipefail",
        f"{{\n{listing}\n}} > owned.txt",
        "awk '$1 == \"container\" {print $2}' owned.txt | xargs -r docker rm -f -v >/dev/null",
        "awk '$1 == \"network\" {print $2}' owned.txt | xargs -r docker network rm >/dev/null",
        "awk '$1 == \"volume\" {print $2}' owned.txt | xargs -r docker volume rm >/dev/null",
        "awk '$1 == \"image\" {print ($2 ~ /:<none>$/) ? $3 : $2}' owned.txt | xargs -r docker image rm >/dev/null",
        "cat owned.txt",
    ])


class TiltAdapter(Adapter):
    name = "tilt"
    title = "Tilt (Docker Compose backend)"
    transport = "host"
    isolation_boundary = "container"
    start_waits_ready = True  # `tilt ci` returns once Compose --wait reports every service healthy
    features = dict(
        lockfile="unsupported",          # no Tilt lock; digest-pinned images + the fixture's uv.lock
        frozen_setup="unsupported",
        services="native",               # docker_compose() resources
        detached_services="native",      # Compose containers outlive `tilt ci`
        readiness="native",              # wait=True + Compose healthchecks
        per_checkout_ports="native",     # project-private networks; no host ports at all
        per_checkout_data="native",      # project-scoped named volumes
        stop_confirmation="native",      # `tilt down` returns after Compose removed the containers
        structured_status="scripted",    # `tilt get` needs a live `tilt up` server; Compose ps JSON instead
        wrong_instance_guard="unsupported")
    not_applicable = {
        "occupied_port": "services publish no host ports; each project reaches postgres:5432 and "
                         "redis:6379 on its own Compose network",
    }
    config_files = ("Tiltfile", "compose.yaml", "Dockerfile", ".dockerignore")
    timeouts = dict(setup=600, start=1200, step=300, ready=120)
    setup_scope = ("`tilt alpha tiltfile-result` only (Tiltfile + Compose model evaluation); image pulls, "
                   "the toolchain image build and container start happen in `tilt ci` (start)")
    cache_note = ("host Docker image/build cache not cleared; uv cache is run-owned and shared by the "
                  "run's checkouts (A fills it, B is warm)")
    pins = dict(tilt=TILT_VERSION, tilt_sha256=TILT_BINARY_SHA256, tilt_archive_sha256=TILT_ARCHIVE_SHA256,
                docker_compose=COMPOSE_VERSION, docker_compose_sha256=COMPOSE_BINARY_SHA256,
                images=IMAGES, uv_lock="fixtures/app/uv.lock (uv sync --frozen)")

    # ---- private tools (never installed globally) --------------------------------------
    def tools(self):
        """Pinned binaries live in the run-owned work dir unless a pre-provisioned dir is given
        (its binaries are still hash-checked)."""
        return self.options.get("tools_dir") or posixpath.join(posixpath.dirname(self.workdir()), "tools")

    def host_env(self, workdir):
        tools = self.options.get("tools_dir") or str(workdir / "tools")
        return {**os.environ, "PATH": f"{tools}:{os.environ.get('PATH', '/usr/bin:/bin')}",
                "TILT_DOCKER_COMPOSE_CMD": f"{tools}/docker-compose",
                "TILT_DEV_DIR": str(workdir / "tilt-dev"), "TILT_DISABLE_ANALYTICS": "1"}

    def provision(self):
        t = q(self.tools())
        base = f"https://github.com/tilt-dev/tilt/releases/download/v{TILT_VERSION}"
        compose = f"https://github.com/docker/compose/releases/download/v{COMPOSE_VERSION}/docker-compose-darwin-aarch64"
        download = [
            f"mkdir -p {t}/dl",
            f"curl -fsSL -o {t}/dl/tilt.tgz {base}/tilt.{TILT_VERSION}.mac.arm64.tar.gz",
            f'echo "{TILT_ARCHIVE_SHA256}  {t}/dl/tilt.tgz" | shasum -a 256 -c -',
            f"tar -xzf {t}/dl/tilt.tgz -C {t} tilt",
            f"curl -fsSL -o {t}/docker-compose {compose}",
            f"chmod 755 {t}/docker-compose",
        ] if not self.options.get("tools_dir") else []
        return [("provision-tilt", darwin_arm64_preflight(
            "set -eu", *download,
            f'echo "{TILT_BINARY_SHA256}  {t}/tilt" | shasum -a 256 -c -',
            f'echo "{COMPOSE_BINARY_SHA256}  {t}/docker-compose" | shasum -a 256 -c -',
            f'{t}/tilt version | grep -q "^v{TILT_VERSION},"',
            f'test "$({t}/docker-compose version --short)" = {COMPOSE_VERSION}'), None)]

    def versions(self):
        t = q(self.tools())
        return (f"set -e; {t}/tilt version; {t}/docker-compose version; "
                "docker version --format 'docker {{.Client.Version}} / daemon {{.Server.Version}} {{.Server.Os}}/{{.Server.Arch}}'; "
                f"shasum -a 256 {t}/tilt {t}/docker-compose")

    # ---- checkout ------------------------------------------------------------------------
    def project(self, co):
        return f"rwb-{self.run_id}-{co.name}"

    def uv_cache(self):
        return posixpath.join(self.workdir(), ".uv-cache")  # run-owned, shared by A..E (warm B)

    def local_env(self, co):
        env = dict(RWB_PROJECT=self.project(co), RWB_RUN=self.run_id, RWB_CHECKOUT=co.name,
                   RWB_SRC=co.path, RWB_UV_CACHE=self.uv_cache())
        lines = "".join(f"export {k}={q(v)}\n" for k, v in env.items())
        return [f"mkdir -p {q(self.uv_cache())}",
                f"printf '%s' {q(lines)} > {q(co.path)}/tilt.local.env"]

    def _in(self, co, body):
        return f"set -e; cd {q(co.path)} && . ./tilt.local.env && {body}"

    def compose(self, args):
        return f'"$TILT_DOCKER_COMPOSE_CMD" -p "$RWB_PROJECT" -f compose.yaml {args}'

    def break_config(self, co):
        # Request a PostgreSQL image version that does not exist.
        path = f"{q(co.path)}/compose.yaml"
        return (f"perl -pi -e 's{{postgres:17\\.6-alpine\\@sha256:[0-9a-f]+}}{{postgres:99.99.99-alpine}}' {path}\n"
                f"grep -q 'image: postgres:99.99.99-alpine$' {path}")

    # ---- the tool's own operations -----------------------------------------------------
    def setup(self, co):
        # Tilt has no install step: it evaluates the Tiltfile (and Compose model) here, and
        # pulls/builds images inside `tilt ci` (cold in A's start, cached for B).
        return self._in(co, "tilt alpha tiltfile-result")

    def start(self, co):
        return self._in(co, "tilt ci --port 0 --timeout 900s </dev/null")

    def ready(self, co):
        # Explicit health receipt: the Docker health state Compose --wait gated on.
        checks = " && ".join(
            f'id=$({self.compose("ps -q " + s)}) && test -n "$id" && '
            f"h=$(docker inspect -f '{{{{.State.Health.Status}}}}' \"$id\") && "
            f'printf \'{{"service":"{s}","container":"%s","health":"%s"}}\\n\' "$id" "$h" && test "$h" = healthy'
            for s in ("postgres", "redis"))
        return self._in(co, checks)

    def status(self, co):
        return self._in(co, self.compose("ps --all --format json"))

    def stop(self, co):
        return self._in(co, "tilt down")  # Compose down without --volumes: data volumes kept

    def cleanup(self, co):
        return self._in(co, "tilt down --delete-volumes")

    def enter(self, co, body):
        return self._in(co, self.compose(f"exec -T app bash -c {q(body)}"))

    def tool_versions(self, co):
        return self._in(co, " && ".join([
            self.compose("exec -T app bash -c 'command -v python3 uv; python3 --version; uv --version'"),
            self.compose("exec -T postgres postgres --version"),
            self.compose("exec -T redis redis-server --version")]))

    def instance_identity(self, co):
        # Every raw Docker query is its own guarded assignment (a failed substitution inside
        # printf would still exit 0); empty required IDs fail; JSON is printed last.
        p = self.project(co)
        lbl = f"--filter label=com.docker.compose.project={p}"
        return "\n".join([
            "set -u",
            f"pg=$(docker ps -q --no-trunc {lbl} --filter label=com.docker.compose.service=postgres) || exit 1",
            f"rd=$(docker ps -q --no-trunc {lbl} --filter label=com.docker.compose.service=redis) || exit 1",
            f"vols=$(docker volume ls -q {lbl}) || exit 1",
            f"net=$(docker network ls -q --no-trunc {lbl}) || exit 1",
            'for v in "$pg" "$rd" "$vols" "$net"; do [ -n "$v" ] || { echo "identity: empty Docker result" >&2; exit 1; }; done',
            "vols=$(printf '%s\\n' \"$vols\" | sort | tr '\\n' ' ')",
            f"printf '{{\"project\":\"%s\",\"postgres\":\"%s\",\"redis\":\"%s\",\"volumes\":\"%s\",\"network\":\"%s\"}}\\n' "
            f'{p} "$pg" "$rd" "$vols" "$net"'])

    def stopped_probe(self, co, identity):
        p = self.project(co)
        # The query's own status is checked: a failed `docker ps` is never read as "no containers".
        return (f"for i in $(seq 1 150); do left=$(docker ps -aq --filter label=com.docker.compose.project={p}) "
                f"|| exit 1; test -z \"$left\" && exit 0; sleep 0.2; done; echo 'containers of {p} remain' >&2; exit 1")

    # ---- leftovers and host cleanup (only this run's labels/names) ---------------------
    def service_processes(self):
        return (f"docker ps --filter label=rwb.run={self.run_id} --filter label=rwb.service=1 "
                "--format '{{.ID}} {{.Names}} {{.Status}}'")

    def supervisor_processes(self):
        return f"set -o pipefail; ps -axo pid=,stat=,args= | awk -v t={q(self.tools() + '/tilt')} 'index($0, t) && !/awk/'"

    def host_resources(self):
        prefix = f"rwb-{self.run_id}-"
        return "\n".join([
            "set -eo pipefail",
            f"docker ps -a --filter label=rwb.run={self.run_id} --format 'container {{{{.Names}}}}'",
            f"docker volume ls --format '{{{{.Name}}}}' | awk 'index($0, \"{prefix}\") == 1 {{print \"volume \" $0}}'",
            f"docker network ls --format '{{{{.Name}}}}' | awk 'index($0, \"{prefix}\") == 1 {{print \"network \" $0}}'",
            f"docker images --format '{{{{.Repository}}}}:{{{{.Tag}}}} {{{{.ID}}}}' | awk 'index($0, \"{prefix}\") == 1 {{print \"image \" $0}}'",
        ])

    def cleanup_host(self):
        return remove_owned(self.host_resources())
