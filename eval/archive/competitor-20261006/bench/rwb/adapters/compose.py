"""Docker Compose v5.6.0 (bench/research/compose.md), host transport.

The pinned standalone `docker-compose` release binary (darwin-aarch64, release checksum) is
downloaded into the run-owned directory and talks to the host Docker daemon; the host's own
Compose plugin is neither used nor changed. Each checkout is one project
`rwb-<run id>-<checkout>` with its own network and named volumes. The app container is built
from the checkout (its code and SOURCE_TOKEN baked into the image), so the source gate checks
that the running container holds this checkout's code. Isolation boundary: `container`.
"""
import os
import posixpath

from .base import Adapter, q, sha256_check

COMPOSE_VERSION = "5.6.0"
# Release checksums.txt (v5.6.0, fetched 2026-10-06).
COMPOSE_DARWIN_ARM64_SHA256 = "bd714a42b46e51757fc1121085b3096b9064e3f80d3ca5a991e33f44df4f43c9"
COMPOSE_URL = (f"https://github.com/docker/compose/releases/download/v{COMPOSE_VERSION}/"
               "docker-compose-darwin-aarch64")
LOCK = "compose.images.lock.yaml"


class ComposeAdapter(Adapter):
    name = "compose"
    title = "Docker Compose"
    transport = "host"
    isolation_boundary = "container"
    features = dict(
        lockfile="native", frozen_setup="unsupported", services="native", detached_services="native",
        readiness="native", per_checkout_ports="native", per_checkout_data="native",
        stop_confirmation="native", structured_status="native", wrong_instance_guard="unsupported")
    not_applicable = {
        "occupied_port": "no host ports are published: services listen inside each project's own "
                         "network, so a squatted host port tests nothing Compose owns",
    }
    config_files = ("compose.yaml", "Dockerfile", ".dockerignore")
    lock_files = (LOCK,)
    start_waits_ready = True  # `up --wait`: postgres/redis healthchecks + the app's identity check
    pins = dict(compose=COMPOSE_VERSION, compose_sha256=COMPOSE_DARWIN_ARM64_SHA256, python="3.13.16",
                uv="0.12.23", postgres="17.11-alpine", redis="8.10.2-alpine",
                note="host Docker Desktop daemon; images digest-pinned in compose.yaml/Dockerfile")
    timeouts = dict(setup=2400, start=300, step=300)
    setup_scope = ("`config --lock-image-digests` (A only) + `pull postgres redis` + `build app` (Python deps via "
                   "`uv sync --frozen` in the image); containers are created at `up`")
    cache_note = ("host Docker image/build cache not cleared (base images may already be present); "
                  "B reuses A's layers on the same daemon")

    def __init__(self, options=None, variant=None, run_id="dryrun"):
        super().__init__(options, variant, run_id)
        self.pins = dict(self.pins, projects=[self.project(c) for c in "abcde"])

    # ---- host plumbing ------------------------------------------------------------------
    def tools(self):
        return posixpath.join(posixpath.dirname(self.root or "/tmp/rwb-dry-run/w"), "tools")

    def project(self, name):
        return f"rwb-{self.run_id}-{name}"

    def host_env(self, workdir):
        env = {k: v for k, v in os.environ.items() if not k.startswith("COMPOSE_")}
        env["PATH"] = f"{workdir}/tools:" + env.get("PATH", "/usr/bin:/bin")
        # Run-owned client config (see provision): same daemon context, no credential helper.
        env["DOCKER_CONFIG"] = f"{workdir}/docker-config"
        env["NO_COLOR"] = "1"
        return env

    def dc(self, co, lock=True):
        files = f"-f {q(co.path)}/compose.yaml"
        if lock:
            files += f' $(test -f {q(co.path)}/{LOCK} && printf -- "-f %s" {q(co.path + "/" + LOCK)})'
        return (f"{q(self.tools())}/docker-compose --ansi never --progress plain "
                f"--project-directory {q(co.path)} {files} -p {q(self.project(co.name))}")

    def provision(self):
        binary = f"{self.tools()}/docker-compose"
        return [("provision-compose", "\n".join([
            "set -eu",
            'test "$(uname -s)/$(uname -m)" = Darwin/arm64 || { echo "pinned compose hash is for darwin-arm64" >&2; exit 1; }',
            f"mkdir -p {q(self.tools())}",
            f"curl -fsSL --retry 3 -o {q(binary)}.part {COMPOSE_URL}",
            sha256_check(COMPOSE_DARWIN_ARM64_SHA256, f"{q(binary)}.part"),
            f"chmod 755 {q(binary)}.part && mv {q(binary)}.part {q(binary)}",
            f'{q(binary)} version | grep -q "v{COMPOSE_VERSION}"',
            # The host's Docker Desktop credential helper fails non-interactively (exit 1, no
            # output) and no registry auths are stored, so the run uses a private client config:
            # the user's current context (contexts copied), no credsStore, anonymous public pulls.
            # ~/.docker is only read.
            'src="${RWB_USER_DOCKER_CONFIG:-$HOME/.docker}"; dst="$DOCKER_CONFIG"; mkdir -p "$dst"',
            'if [ -d "$src/contexts" ]; then cp -R "$src/contexts" "$dst/"; fi',
            'ctx=$(sed -n \'s/.*"currentContext"[[:space:]]*:[[:space:]]*"\\([^"]*\\)".*/\\1/p\' "$src/config.json" 2>/dev/null | head -n 1)',
            'if [ -n "$ctx" ]; then printf \'{"currentContext": "%s"}\\n\' "$ctx" > "$dst/config.json"; else echo "{}" > "$dst/config.json"; fi',
            "docker info --format 'daemon {{.ServerVersion}} {{.OSType}}/{{.Architecture}}'",
        ]), None)]

    def versions(self):
        return (f"{q(self.tools())}/docker-compose version; docker version --format "
                "'client {{.Client.Version}} server {{.Server.Version}} {{.Server.Os}}/{{.Server.Arch}}'; "
                "docker compose version 2>/dev/null | sed 's/^/host plugin (unused): /' || true")

    # ---- operations ---------------------------------------------------------------------
    def app_dir(self, co):
        return "/app"

    def app_source_path(self, co):
        return "/app"

    def enter(self, co, body):
        return f"{self.dc(co)} exec -T app bash -c {q(body)}"

    def deps(self, co):
        # Installed at image build from uv.lock; verified unchanged in the running container.
        return self.enter(co, "cd /app && uv sync --frozen --check")

    def tool_versions(self, co):
        one_off = f"{self.dc(co)} run --rm --no-deps -T --entrypoint"
        return (f"set -e; {one_off} python app --version; {one_off} uv app --version; "
                f"{one_off} postgres postgres --version; {one_off} redis-server redis --version")

    def break_config(self, co):
        return (f"sed -i '' -e 's#image: postgres:17.11-alpine@sha256:[0-9a-f]*#image: postgres:99.99.99-alpine#' "
                f"{q(co.path)}/compose.yaml && grep -q 'postgres:99.99.99-alpine' {q(co.path)}/compose.yaml")

    def setup(self, co):
        # A writes the image-digest lock (native); B/C/E receive it as committed.
        return (f"set -e; test -f {q(co.path)}/{LOCK} || {self.dc(co, lock=False)} config --lock-image-digests "
                f"-o {q(co.path)}/{LOCK}; {self.dc(co)} pull postgres redis; {self.dc(co)} build app")

    def frozen_setup(self, co):
        # No refuse-to-change mode; lock bytes and versions are compared (mode n/a).
        return self.setup(co)

    def start(self, co):
        return f"{self.dc(co)} up -d --wait --wait-timeout 120"

    def status(self, co):
        return f"{self.dc(co)} ps --format json"

    def stop(self, co):
        # Removes containers and the network; named volumes (data) are retained.
        return f"{self.dc(co)} down"

    def stopped_probe(self, co, identity):
        label = f"label=com.docker.compose.project={self.project(co.name)}"
        return (f"for i in $(seq 1 150); do out=$(docker ps -q --filter {q(label)}) || exit 1; "
                f"test -z \"$out\" && exit 0; sleep 0.2; done; echo \"still running: $out\" >&2; exit 1")

    def instance_identity(self, co):
        label = f"label=com.docker.compose.project={self.project(co.name)}"
        return (f"set -e; pg=$({self.dc(co)} ps -q postgres); rd=$({self.dc(co)} ps -q redis); "
                f"test -n \"$pg\" && test -n \"$rd\"; vols=$(docker volume ls -q --filter {q(label)} | sort | tr '\\n' ' '); "
                f"printf '{{\"project\":\"%s\",\"postgres\":\"%s\",\"redis\":\"%s\",\"volumes\":\"%s\"}}\\n' "
                f"{q(self.project(co.name))} \"$pg\" \"$rd\" \"$vols\"")

    def service_processes(self):
        # Running containers of this run's projects. A failed query prints a line: never "clean".
        lines = [f"docker ps --filter label=com.docker.compose.project={self.project(c)} "
                 f"--format '{{{{.ID}}}} {{{{.Names}}}} {{{{.Status}}}}' || echo 'docker ps failed for {self.project(c)}'"
                 for c in "abcde"]
        return "; ".join(lines)

    def supervisor_processes(self):
        return "true"

    def cleanup_host(self):
        steps = []
        for name in "abcde":
            path = f"{self.root}/{name}"
            steps.append(f"if test -f {q(path)}/compose.yaml; then {self.dc(self.checkout(name, 0))} "
                         f"down -v --remove-orphans --rmi local || rc=1; fi")
        return "rc=0; " + "; ".join(steps) + "; exit $rc"

    def host_resources(self):
        out = []
        for name in "abcde":
            label = f"label=com.docker.compose.project={self.project(name)}"
            for kind in ("ps -a", "volume ls", "network ls"):
                out.append(f"docker {kind} -q --filter {q(label)} || echo 'docker {kind} failed'")
        out.append(f"docker images -q --filter reference={q(f'rwb-{self.run_id}-*')} || echo 'docker images failed'")
        return "; ".join(out)
