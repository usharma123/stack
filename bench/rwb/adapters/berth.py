"""Berth (source main 3b93287, no releases): a worktree plus its own Compose project per name.

`berth up NAME` creates sibling worktree `<repo>-NAME` on branch NAME, allocates host ports
into `.berth/NAME.env` and runs `docker compose up -d` with project `berth-NAME`. It runs
Compose from the ROOT repository, so compose.yaml names the created worktree explicitly
(${BERTH_NAME}); the source token and code digest prove each app runs its own worktree.
`stop`/`start` keep containers and volumes; `down` destroys them with the worktree.

Berth has no exec command. App commands use Compose `exec` against Berth's exact project,
compose file and generated env file (delegated entry). Host transport: Berth drives the
host Docker daemon; every resource is named with this run's id.
"""
import hashlib
from pathlib import Path

from .agent_env_common import (CANONICAL, IMAGES, ContainerApp, compact, compose_receipt, compose_remove,
                               compose_resources, host_env, images_left, init_repo, project_stopped,
                               require_owned)
from .base import Adapter, q

BERTH_REPO = "https://github.com/zoltanersek/berth"
BERTH_COMMIT = "3b93287584dcc5c7c26298a62379d8ce001400ff"
BERTH_VERSION = "0.1.0"


class BerthAdapter(ContainerApp, Adapter):
    name = "berth"
    prepare_scope = 'fixture repository only; Berth creates the worktree inside setup (`berth up`), so it IS in first_task'
    title = "Berth"
    transport = "host"
    isolation_boundary = "container"
    app_python = "/opt/venv/bin/python"
    features = dict(
        lockfile="unsupported",          # no runtime/package lock; images are digest-pinned config
        frozen_setup="unsupported",
        services="native",               # one Compose project per berth
        detached_services="native",
        readiness="unsupported",         # `compose up -d` without --wait; the app's wait is used
        per_checkout_ports="native",     # host ports allocated into .berth/NAME.env
        per_checkout_data="native",      # project-scoped named volumes
        stop_confirmation="native",      # `berth stop` = `compose stop` (synchronous)
        structured_status="unsupported",  # `berth ls` is a text table
        wrong_instance_guard="unsupported")
    not_applicable = {
        "occupied_port": "Berth binds 127.0.0.1:0 for a free port and starts Compose in the same `up`; "
                         "there is no planned port to occupy before start, only a race",
    }
    config_files = ("berth.yml", "compose.yaml", "Dockerfile", "gitignore")
    timeouts = dict(setup=3600, start=600, step=300)

    def __init__(self, options=None, variant=None, run_id="dryrun"):
        super().__init__(options, variant, run_id)
        self.tag = compact(run_id)
        self.binary = self.options.get("berth_binary")
        self.pins = dict(berth=BERTH_VERSION, berth_commit=BERTH_COMMIT, images=dict(IMAGES),
                         runtime=dict(CANONICAL))
        if self.binary and Path(self.binary).exists():
            self.pins["berth_sha256"] = hashlib.sha256(Path(self.binary).read_bytes()).hexdigest()
            expected = self.options.get("berth_sha256")
            if expected and expected != self.pins["berth_sha256"]:
                raise ValueError(f"berth binary hash {self.pins['berth_sha256']} != expected {expected}")

    # ---- names and paths --------------------------------------------------------------
    def tools(self):
        return f"{self.workdir()}/../tools"

    def bname(self, co):
        return f"rwb{self.tag}{co.name}"

    def project(self, co):
        return require_owned(f"berth-{self.bname(co)}", self.run_id)

    def repo(self, co):
        # D (bad config) gets its own disposable repository so its broken compose file never
        # touches A/B/E, which share the main repository like teammates' agents do.
        return f"{self.workdir()}/d-repo/app" if co.name == "d" else f"{self.workdir()}/main/app"

    def checkout(self, name, index, token=""):
        co = super().checkout(name, index, token)
        co.path = f"{self.repo(co)}-{self.bname(co)}"
        return co

    def container_root(self, co):
        return "/workspace"

    def env_file(self, co):
        return f"{self.repo(co)}/.berth/{self.bname(co)}.env"

    def berth(self, co, *args):
        return f"berth --dir {q(self.repo(co))} " + " ".join(args) + " </dev/null"

    def dc(self, co, *args):
        return (f"docker compose -f {q(self.repo(co))}/compose.yaml --env-file {q(self.env_file(co))} "
                f"-p {q(self.project(co))} " + " ".join(args))

    # ---- host environment and provisioning --------------------------------------------
    def host_env(self, workdir):
        return host_env(workdir, [Path(workdir) / "tools" / "bin"])

    def provision(self):
        t = self.tools()
        if self.binary:
            body = [f"mkdir -p {t}/bin", f"cp {q(self.binary)} {t}/bin/berth"]
            if self.pins.get("berth_sha256"):
                body.append(f'echo "{self.pins["berth_sha256"]}  {t}/bin/berth" | shasum -a 256 -c -')
        else:
            body = [
                f"git clone -q {BERTH_REPO} {t}/berth-src",
                f"git -C {t}/berth-src checkout -q --detach {BERTH_COMMIT}",
                f'test "$(git -C {t}/berth-src rev-parse HEAD)" = {BERTH_COMMIT}',
                f"CARGO_HOME={t}/cargo CARGO_TARGET_DIR={t}/target cargo build --locked --release "
                f"--manifest-path {t}/berth-src/Cargo.toml",
                f"mkdir -p {t}/bin && cp {t}/target/release/berth {t}/bin/berth",
            ]
        body.append(f"{t}/bin/berth --version")
        return [("provision-berth", "set -euo pipefail\n" + "\n".join(body), None)]

    def versions(self):
        return ("set -e; berth --version; shasum -a 256 \"$(command -v berth)\"; docker version --format "
                "'{{.Server.Version}} {{.Server.Os}}/{{.Server.Arch}}'; docker compose version; git --version")

    # ---- checkouts ----------------------------------------------------------------------
    def prepare(self, co, lock_from=None):
        """Ensure the committed fixture repository. Berth itself creates the worktree at `up`,
        so the source token is written right after it exists (setup)."""
        repo, cfg = self.repo(co), f"{self.src}/adapters/{self.name}"
        return "\n".join([
            "set -euo pipefail",
            f"if [ ! -d {q(repo)}/.git ]; then",
            f"  mkdir -p {q(repo)}",
            f"  cp -R {self.src}/fixtures/app/. {q(repo)}/",
            f"  rm -rf {q(repo)}/.venv {q(repo)}/.pytest_cache",
            f"  cp {cfg}/berth.yml {cfg}/compose.yaml {cfg}/Dockerfile {q(repo)}/",
            f"  cp {cfg}/gitignore {q(repo)}/.gitignore",
            *("  " + line for line in init_repo(repo, "main")),
            "fi",
            self.berth(co, "validate"),
        ])

    def break_config(self, co):
        # Unknown PostgreSQL version in D's own repository (Compose reads the root file).
        return (f"sed -i.orig 's#^    image: postgres:17.11-alpine@sha256:[0-9a-f]*$#    image: postgres:99.99.99-alpine#' "
                f"{q(self.repo(co))}/compose.yaml && grep -q 'postgres:99.99.99-alpine' {q(self.repo(co))}/compose.yaml")

    # ---- lifecycle ----------------------------------------------------------------------
    def setup(self, co):
        # `berth up` is one native step: worktree + branch + ports + env + image build +
        # `compose up -d`. Setup timing therefore includes the first service start.
        return "\n".join([
            "set -euo pipefail",
            self.berth(co, "up", q(self.bname(co))),
            f"test -d {q(co.path)}/.git -o -f {q(co.path)}/.git",
            f"printf '%s\\n' {q(co.token)} > {q(co.path)}/rwbapp/SOURCE_TOKEN",
            f"cat {q(self.env_file(co))}",
        ])

    def start(self, co):
        return self.berth(co, "start", q(self.bname(co)))

    def stop(self, co):
        return self.berth(co, "stop", q(self.bname(co)))

    def status(self, co):
        return f"set -e; {self.berth(co, 'ls')}; {self.dc(co, 'ps', '--all', '--format', 'json')}"

    def enter(self, co, body):
        return self.dc(co, "exec", "-T", "app", "bash", "-c", q(body)) + " </dev/null"

    def tool_versions(self, co):
        return "\n".join([
            "set -e",
            self.enter(co, "command -v python3 uv; python3 --version; uv --version; "
                           f"{self.app_python} -c 'import psycopg, redis; print(psycopg.__version__, redis.__version__)'"),
            self.dc(co, "exec", "-T", "postgres", "postgres", "--version") + " </dev/null",
            self.dc(co, "exec", "-T", "redis", "redis-server", "--version") + " </dev/null",
            self.dc(co, "images", "--format", "json"),
        ])

    def instance_identity(self, co):
        # Containers, volumes, networks and published ports of this berth's project, with the
        # app's /workspace bind source == this worktree and identical host/container code.
        return "set -euo pipefail\n" + self.code_check(co, co.path) + " && " + \
            compose_receipt(self.project(co), co.path, "/workspace", digest_var="h")

    def stopped_probe(self, co, identity):
        # Containers stopped (Docker state) and Berth's published host ports refuse.
        env = q(self.env_file(co))
        return "\n".join([
            project_stopped(self.project(co)),
            f"pg=$(sed -n 's/^PGPORT=//p' {env}); rp=$(sed -n 's/^REDIS_PORT=//p' {env})",
            'test -n "$pg" -a -n "$rp"',
            'for p in "$pg" "$rp"; do ok=0; for i in $(seq 1 150); do '
            '(exec 3<>"/dev/tcp/127.0.0.1/$p") 2>/dev/null || { ok=1; break; }; sleep 0.2; done; '
            '[ "$ok" = 1 ] || { echo "published port $p still accepting" >&2; exit 1; }; done',
            f'v=$(docker volume ls -q --filter label=com.docker.compose.project={self.project(co)} | wc -l)',
            '[ "$v" -ge 2 ] || { echo "stop removed volumes" >&2; exit 1; }',
        ])

    def cleanup(self, co):
        # Native destruction (`down` = compose down -v + worktree + branch). After a rolled-back
        # `up` (D) Berth holds no record, so the host cleanup removes any partial project.
        name = q(self.bname(co))
        return "\n".join([
            "set -uo pipefail",
            f"if [ -e {q(self.env_file(co))} ]; then",
            f"  {self.berth(co, 'down', name)} || {{ echo 'berth down refused; forcing' >&2; "
            f"{self.berth(co, 'down', '--force', name)}; exit 1; }}",
            "fi",
        ])

    def names(self):
        return [self.checkout(n, i) for i, n in enumerate("abcde")]

    def cleanup_host(self):
        bodies = [compose_remove(self.project(co), images=[f"{self.project(co)}-app"]) for co in self.names()]
        return "\n".join(f"( {b} )" for b in bodies)

    def host_resources(self):
        cos = self.names()
        return "set -euo pipefail\n" + "\n".join(f"( {compose_resources(self.project(co))} )" for co in cos) + \
            "\n" + images_left([f"{self.project(co)}-app" for co in cos])

    def service_processes(self):
        # Services run in Compose containers: list this run's still-running ones.
        return "set -euo pipefail\n" + "\n".join(
            f"docker ps --filter {q('label=com.docker.compose.project=' + self.project(co))} "
            "--filter status=running --format '{{.ID}} {{.Names}}'" for co in self.names())

    def supervisor_processes(self):
        return "true"
