"""Dev Containers CLI, plus the shared base for the container family (DevPod, DDEV, Lando).

Container tools run on the host Docker daemon (`transport = "host"`). Each checkout is its
own Compose project named with the run id; the app runs inside the tool's app container and
reaches PostgreSQL/Redis by project-scoped service DNS on their internal ports, so URL ports
equal server ports without any published-port hop. Same in-container paths (/workspace,
/var/lib/postgresql/data, 5432, 6379) in two checkouts are expected; isolation is proven by
the `container` boundary: different PG system_identifier, Redis run_id, markers, and the
container/volume receipts from bench/adapters/devcontainers/compose_receipt.py.

Research: bench/research/devcontainers.md. Handoff: bench/CONTAINER-ADAPTERS.md.
"""
import hashlib
from pathlib import Path

from .base import BENCH, Adapter, q

RECEIPT_REL = "adapters/devcontainers/compose_receipt.py"
RECEIPT_SHA256 = hashlib.sha256((BENCH / RECEIPT_REL).read_bytes()).hexdigest()

# Workload pins shared by the container family (anonymous registry index digests,
# re-verified 2026-10-06; they match bench/research/compose.md where the image is shared).
IMAGES = {
    "python": "python:3.13.16-slim-bookworm@sha256:a1165e272e578941b84abc79e4ab38a0305cd12803a5c4247979ac7655f4d641",
    "uv": "ghcr.io/astral-sh/uv:0.12.23@sha256:61d393e44e249f2e4b526b6c7ddcecce245946826e608e11c93ad4f5bba55b21",
    "postgres": "postgres:17.6-alpine@sha256:ef257d85f76e48da1c64832459b59fcaba1a4dac97bf5d7450c77753542eee94",
    "redis": "redis:8.10.2-alpine@sha256:3811787313eba226a2ef38658c6ccb91cd5e110edc89c37767de373120a0e5a0",
}

PLATFORM = ('case "$(uname -s)/$(uname -m)" in '
            'Darwin/arm64) rwb_platform=darwin-arm64;; Darwin/x86_64) rwb_platform=darwin-amd64;; '
            'Linux/aarch64|Linux/arm64) rwb_platform=linux-arm64;; Linux/x86_64) rwb_platform=linux-amd64;; '
            '*) echo "unsupported host platform $(uname -s)/$(uname -m)" >&2; exit 1;; esac')

# Verifies a file's SHA-256 with Python so the body works on macOS and Linux alike.
SHA_CHECK = ("python3 -I -c 'import hashlib,sys; d=hashlib.sha256(open(sys.argv[1],\"rb\").read()).hexdigest(); "
             "print(d, sys.argv[1]); sys.exit(0 if d == sys.argv[2] else 1)'")



def binary_hash(path):
    """Shell line printing the SHA-256 and real file name of an installed binary."""
    return ("python3 -I -c 'import hashlib,os,sys; p=os.path.realpath(sys.argv[1]); "
            "print(hashlib.sha256(open(p,\"rb\").read()).hexdigest(), os.path.basename(p))' " + q(path))


class ContainerAdapter(Adapter):
    """Shared behaviour for tools that create their own Compose projects on the host daemon."""
    transport = "host"
    isolation_boundary = "container"
    workspace = "/workspace"          # where the app container sees the checkout
    version = ""
    lock_files = ("uv.lock",)
    timeouts = dict(setup=1800, start=900, step=300, ready=120)
    not_applicable = {
        "occupied_port": "no host port is published for PostgreSQL/Redis; each checkout uses "
                         "internal ports on its own Compose network, so a host listener cannot collide",
    }
    # The tool's own command entry resumes a stopped project (DDEV `exec`). Requested core
    # hook: record `stop.a` from the stopped probe and report the resumed entry separately.
    entry_auto_resumes = False
    cache_note = ("host Docker image cache and the pinned CLI download are reused across runs; "
                  "A pulls/builds only images missing from the host cache, B reuses A's")
    # Shared infrastructure (kind:name) the tool may create on first use; snapshotted at
    # provision so cleanup removes it only if this run created it and nothing uses it.
    shared_infra = ()

    # ---- naming and environment ---------------------------------------------------------
    @property
    def tools(self):
        """Benchmark-owned install directory for the pinned CLI (override: tools_dir=...)."""
        default = Path.home() / ".cache" / "rwb-bench-tools" / f"{self.name}-{self.version}"
        return str(Path(self.options.get("tools_dir", default)).expanduser())

    @property
    def state(self):
        """Run-owned tool state (global config, CLI data). Never the user's own state."""
        return f"{self.workdir()}/_state"

    def project(self, name):
        """Run-owned per-checkout name, used for the Compose project."""
        return f"rwb-{self.run_id}-{name}"

    def selectors(self, name):
        """compose_receipt selectors owned by checkout `name`."""
        return [self.project(name)]

    def all_selectors(self):
        return [s for name in "abcde" for s in self.selectors(name)]

    def env(self):
        """Export lines prefixed to every body, so each recorded body is self-contained."""
        return (f"export PATH={q(self.tools)}/bin:\"$PATH\" DOCKER_CLI_HINTS=false; "
                "unset COMPOSE_PROJECT_NAME COMPOSE_FILE COMPOSE_PROFILES; ")

    def receipt(self, *args):
        return f"python3 -I {q(self.src + '/' + RECEIPT_REL)} " + " ".join(q(str(a)) for a in args)

    def in_checkout(self, co, body):
        return f"{self.env()}cd {q(co.path)} && {body}"

    # ---- validity and pins --------------------------------------------------------------
    @property
    def pins(self):
        return dict(version=self.version, images=self.images(), receipt_sha256=RECEIPT_SHA256,
                    validity=self.validity(), entry_auto_resumes=self.entry_auto_resumes,
                    **self.extra_pins())

    def images(self):
        return dict(IMAGES)

    def extra_pins(self):
        return {}

    def validity(self):
        """Machine-readable conditions under which this adapter's results are valid. The
        preflight provision step enforces the host ones and fails closed."""
        return dict(
            host_docker_daemon="docker info must succeed; checkouts must be visible to the daemon (Docker Desktop file sharing covers the temp dir)",
            private_state=f"{self.state} only; user tool state is never read or written",
            owned_names=f"every project/volume/image contains run id {self.run_id}",
            core_hooks_required=self.core_hooks_required(),
        )

    def core_hooks_required(self):
        hooks = ["frozen_copy: add C to Scenario.started (setup/start create containers); host cleanup_host covers it until then"]
        if self.entry_auto_resumes:
            hooks.append("stop.a: honour entry_auto_resumes; the after-stop app call through the tool entry restarts the project")
        return hooks

    def preflight(self):
        lines = ["set -eu", PLATFORM, 'echo "platform=$rwb_platform"',
                 "docker info --format 'docker server {{.ServerVersion}} {{.OperatingSystem}} {{.Architecture}}'",
                 "docker compose version", f"mkdir -p {q(self.state)} {q(self.tools)}/bin"]
        if self.shared_infra:
            lines.append(self.receipt("shared-snapshot", f"{self.state}/shared-infra.json", *self.shared_infra))
        return "\n".join(lines)

    def provision(self):
        return [("preflight", self.env() + self.preflight(), None)] + self.install()

    def install(self):
        raise NotImplementedError

    def version_commands(self):
        raise NotImplementedError

    def versions(self):
        """Tool version (+ installed artifact hash), host Docker engine and Compose versions."""
        return self.env() + "\n".join(["set -e", *self.version_commands(),
                                        "docker version --format 'client {{.Client.Version}} server {{.Server.Version}} {{.Server.Os}}/{{.Server.Arch}}'",
                                        "docker compose version"])

    def fetch_binary(self, base_url, assets, dest):
        """Download the release asset for this platform into self.tools and check its hash.
        assets: {platform: (asset name, sha256)}. Re-verifies an existing download."""
        cases = " ".join(f"{p}) asset={q(a)}; sum={s};;" for p, (a, s) in sorted(assets.items()))
        return "\n".join([
            "set -eu", PLATFORM, f"case $rwb_platform in {cases} *) echo \"no pinned asset for $rwb_platform\" >&2; exit 1;; esac",
            f"cd {q(self.tools)}",
            f'if [ ! -f "$asset" ] || ! {SHA_CHECK} "$asset" "$sum" >/dev/null; then '
            f'curl -fsSL -o "$asset.part" {q(base_url)}/"$asset" && mv "$asset.part" "$asset"; fi',
            f'{SHA_CHECK} "$asset" "$sum"',
        ] + ([f'chmod 755 "$asset" && ln -sf "../$asset" bin/{q(dest)}'] if dest else []))

    # ---- checkout ------------------------------------------------------------------------
    def app_dir(self, co):
        return self.workspace

    def app_source_path(self, co):
        return self.workspace

    def edit(self, path, old, new):
        """Shell line replacing text in a checkout file, failing if the text is absent."""
        return ("python3 -I -c 'import sys; p,o,n=sys.argv[1:]; s=open(p).read(); "
                "assert o in s, \"pattern not found in \"+p; open(p,\"w\").write(s.replace(o,n))' "
                f"{q(path)} {q(old)} {q(new)}")

    # ---- common application/verification bodies ----------------------------------------
    def tool_versions(self, co):
        inside = self.enter(co, "set -e; python3 --version; uv --version")
        return f"set -e\n{inside}\n{self.server_versions(co)}"

    def server_versions(self, co):
        raise NotImplementedError

    def frozen_setup(self, co):
        """Fresh copy C: build from committed config, start, then let uv refuse lock changes."""
        return "\n".join(["set -e", self.setup(co), self.start(co),
                          self.enter(co, f'cd {q(self.workspace)} && uv sync --locked --python "$(command -v python3)"')])

    def instance_identity(self, co):
        return self.env() + self.receipt("identity", self.selectors(co.name)[0])

    def stopped_probe(self, co, identity):
        return self.env() + self.receipt("stopped", self.selectors(co.name)[0], 45)

    def service_processes(self):
        """Running containers of this run's projects (the services are containers here)."""
        return self.env() + self.receipt("running", self.run_id, *self.all_selectors())

    def supervisor_processes(self):
        """Host processes started from this adapter's private tool directory."""
        return f"ps -axo pid=,args= | grep -F -- {q(self.tools)} | grep -v -e 'grep -F' || true"

    def host_resources(self):
        return self.env() + self.receipt("resources", self.run_id, *self.all_selectors())

    def native_teardown(self, name):
        """Best-effort native teardown for checkout `name` if it was prepared (shell line)."""
        return ""

    def cleanup_host(self):
        lines = []
        for name in "abcde":
            line = self.native_teardown(name)
            if line:
                lines.append(f"( {line} ) || echo 'native teardown of {name} failed; removing owned resources' >&2")
        lines.append(self.receipt("remove", self.run_id, *self.all_selectors()))
        if self.shared_infra:
            lines.append(self.receipt("shared-cleanup", f"{self.state}/shared-infra.json"))
        return self.env() + "\n".join(lines)


DC_VERSION = "0.89.0"


class DevcontainersAdapter(ContainerAdapter):
    name = "devcontainers"
    title = "Dev Containers CLI"
    version = DC_VERSION
    config_files = (".devcontainer/devcontainer.json", ".devcontainer/compose.yaml", ".devcontainer/Dockerfile")
    lock_files = ("uv.lock", ".devcontainer/Dockerfile", ".devcontainer/compose.yaml")
    start_waits_ready = True
    setup_scope = "devcontainer build: builds the app image; base images pull and uv sync (postCreateCommand) run in `up`"
    bad_config_pattern = r"3\.13\.99"
    features = {
        # No environment lock: digest-pinned images + uv.lock. devcontainer-lock.json covers
        # Features only, and this config uses none.
        "lockfile": "scripted",
        "frozen_setup": "scripted",
        "services": "native",
        "detached_services": "native",
        # `up` starts the app only after Compose reports postgres/redis healthy (depends_on).
        "readiness": "native",
        "per_checkout_ports": "native",
        "per_checkout_data": "native",
        # The CLI has no stop/down; Compose with the recorded project name does it.
        "stop_confirmation": "scripted",
        "structured_status": "scripted",
        "wrong_instance_guard": "unsupported",
    }

    def extra_pins(self):
        lock = BENCH / "adapters" / self.name / "cli" / "package-lock.json"
        return dict(npm_package="@devcontainers/cli", npm_integrity="sha512-LzaoOGKQ/Zql6PsiZ4hVIYVZagzWkD65aG/1ou5/"
                    "Kly5Y1PtjLg1yn7qu+LZCzVoAl6DZZ/pbz8qOO4RLNlqMg==",
                    package_lock_sha256=hashlib.sha256(lock.read_bytes()).hexdigest(),
                    source_commit="5dc7533314b5ba7ec3875c30143dfe1aec644870")

    @property
    def bin(self):
        return f"{self.tools}/node_modules/.bin/devcontainer"

    def install(self):
        cli = f"{self.src}/adapters/{self.name}/cli"
        body = "\n".join([
            "set -eu",
            "node -e 'const m=+process.versions.node.split(\".\")[0]; if (m < 20) { console.error(\"Node >= 20 required, have \"+process.version); process.exit(1) }'",
            f"cp {q(cli)}/package.json {q(cli)}/package-lock.json {q(self.tools)}/",
            f"npm ci --prefix {q(self.tools)} --ignore-scripts --no-audit --no-fund --cache {q(self.tools)}/.npm-cache",
            f'test "$({q(self.bin)} --version)" = {DC_VERSION}',
            f"ln -sf {q(self.bin)} {q(self.tools)}/bin/devcontainer",
        ])
        return [("install-devcontainers-cli", self.env() + body, None)]

    def version_commands(self):
        return ["devcontainer --version", "node --version",
                f"python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))[\"packages\"]"
                f"[\"node_modules/@devcontainers/cli\"][\"integrity\"])' {q(self.tools)}/node_modules/.package-lock.json"]

    def local_env(self, co):
        # The CLI reads COMPOSE_PROJECT_NAME from `.env` in its working directory (the checkout):
        # a machine-local, per-checkout binding that survives every later `up`.
        return [f"printf 'COMPOSE_PROJECT_NAME=%s\\n' {q(self.project(co.name))} > {q(co.path)}/.env"]

    def dc(self, co):
        return f"devcontainer --workspace-folder {q(co.path)} --user-data-folder {q(self.state)}/devcontainers-{co.name}"

    def compose(self, co):
        return f"docker compose -p {q(self.project(co.name))} -f .devcontainer/compose.yaml"

    def setup(self, co):
        # Builds the app image for this checkout's project; base images are pulled by `up`.
        return self.in_checkout(co, f"devcontainer build --workspace-folder {q(co.path)}")

    def start(self, co):
        out = f"{self.state}/up-{co.name}.json"
        check = ("python3 -I -c 'import json,sys; r=json.loads(open(sys.argv[1]).read().strip().splitlines()[-1]); "
                 "print(json.dumps(r)); ok = r.get(\"outcome\") == \"success\" and r.get(\"composeProjectName\") == sys.argv[2] "
                 "and bool(r.get(\"containerId\")); sys.exit(0 if ok else 1)'")
        return self.in_checkout(co, f"mkdir -p {q(self.state)} && devcontainer up --workspace-folder {q(co.path)} "
                                    f"--user-data-folder {q(self.state)}/devcontainers-{co.name} --skip-post-attach "
                                    f"> {q(out)} < /dev/null && {check} {q(out)} {q(self.project(co.name))}")

    def enter(self, co, body):
        return self.in_checkout(co, f"devcontainer exec --workspace-folder {q(co.path)} "
                                    f"--user-data-folder {q(self.state)}/devcontainers-{co.name} bash -c {q(body)}")

    def server_versions(self, co):
        return self.in_checkout(co, f"{self.compose(co)} exec -T postgres postgres --version && "
                                    f"{self.compose(co)} exec -T redis redis-server --version")

    def status(self, co):
        return self.in_checkout(co, f"{self.compose(co)} ps --all --format json")

    def stop(self, co):
        return self.in_checkout(co, f"{self.compose(co)} stop")

    def cleanup(self, co):
        return self.in_checkout(co, f"{self.compose(co)} down --volumes --remove-orphans")

    def native_teardown(self, name):
        path = f"{self.workdir()}/{name}"
        return (f"if [ -f {q(path)}/.devcontainer/compose.yaml ]; then cd {q(path)} && docker compose -p "
                f"{q(self.project(name))} -f .devcontainer/compose.yaml down --volumes --remove-orphans; fi")

    def break_config(self, co):
        return self.edit(f"{co.path}/.devcontainer/Dockerfile", IMAGES["python"],
                         "python:3.13.99-slim-bookworm")
