"""BranchBox 0.13.4 (release): feature worktrees plus its native Dev Container runtime.

`feature start --runtime container` only creates and configures the worktree (its container
provider's start is a no-op, and `feature exec` runs on the host), so the application lane
uses BranchBox's own `devcontainer build/up/exec/down`, which drive Docker Compose directly
with project = workspace basename. No @devcontainers/cli, editor, daemon or account.

Release quirks handled explicitly (research/branchbox.md): creation-hook failures are not
reported by `up`, so dependencies are installed and checked through `devcontainer exec`;
`down` keeps volumes and `down --volumes` destroys them.
"""
import hashlib
from pathlib import Path

from .agent_env_common import (CANONICAL, IMAGES, ContainerApp, compact, compose_receipt, compose_remove,
                               compose_resources, git, host_env, images_left, init_repo, project_stopped,
                               require_owned)
from .base import Adapter, q

BRANCHBOX_VERSION = "0.13.4"
BRANCHBOX_COMMIT = "a00b3ee9a0acce000ee8f478dc0d6a8a24b3c7df"
ARCHIVES = {  # from the release's checksums.txt
    "arm64": ("branchbox-0.13.4-aarch64-apple-darwin",
              "3446e7462b9a42724034695e3cb3163d263aa792a3d97acac95d155aa1c22392"),
    "x86_64": ("branchbox-0.13.4-x86_64-apple-darwin",
               "9d5550c0763a1e44940eaca8efd379279a36bdde3944ebec0e416b545d310ce8"),
}
RELEASE = f"https://github.com/branchbox/branchbox/releases/download/v{BRANCHBOX_VERSION}"

# Checks the `feature start --json` receipt names the expected worktree.
START_RECEIPT_PY = r'''
import json, os, sys
receipt, expected = json.load(open(sys.argv[1])), sys.argv[2]
if os.path.realpath(receipt["worktree_path"]) != os.path.realpath(expected):
    sys.exit(f"feature start created {receipt['worktree_path']}, expected {expected}")
print(json.dumps(dict(worktree=receipt["worktree_path"], branch=receipt["branch_name"],
                      runtime=receipt["runtime"], compose_project=receipt.get("compose_project_name"))))
'''


class BranchboxAdapter(ContainerApp, Adapter):
    name = "branchbox"
    prepare_scope = 'native `feature start` worktree/config creation (excluded from first_task)'
    title = "BranchBox"
    transport = "host"
    isolation_boundary = "container"
    features = dict(
        lockfile="unsupported",          # config sync only; images are digest-pinned config
        frozen_setup="unsupported",
        services="native",               # native runtime runs the Compose services
        detached_services="native",
        readiness="unsupported",         # no waitFor; the app's wait is used
        per_checkout_ports="native",     # no host ports: each project has its own network
        per_checkout_data="native",      # project-scoped named volumes
        stop_confirmation="native",      # `devcontainer down` removes the containers
        structured_status="unsupported",  # no status command for the native runtime
        wrong_instance_guard="unsupported")
    not_applicable = {
        "occupied_port": "the recipe publishes no host ports; services are reachable only inside "
                         "each workspace's own Compose network",
    }
    config_files = ("devcontainer/devcontainer.json", "devcontainer/Dockerfile",
                    "devcontainer/compose.yaml", "gitignore")
    timeouts = dict(setup=3600, start=600, step=300)

    def __init__(self, options=None, variant=None, run_id="dryrun"):
        super().__init__(options, variant, run_id)
        self.tag = compact(run_id)
        self.binary = self.options.get("branchbox_binary")
        self.pins = dict(branchbox=BRANCHBOX_VERSION, branchbox_commit=BRANCHBOX_COMMIT,
                         archives={k: v[1] for k, v in ARCHIVES.items()}, images=dict(IMAGES),
                         runtime=dict(CANONICAL))
        if self.binary and Path(self.binary).exists():
            self.pins["branchbox_sha256"] = hashlib.sha256(Path(self.binary).read_bytes()).hexdigest()

    # ---- names and paths --------------------------------------------------------------
    def tools(self):
        return f"{self.workdir()}/../tools"

    def main(self):
        return f"{self.workdir()}/main"

    def feature(self, co):
        return f"rwb-{self.tag}-{co.name}"

    def project(self, co):
        # The native runtime passes `-p <workspace basename>`.
        return require_owned(self.feature(co), self.run_id)

    def checkout(self, name, index, token=""):
        co = super().checkout(name, index, token)
        co.path = f"{self.workdir()}/{self.feature(co)}"
        return co

    def container_root(self, co):
        return f"/workspaces/{self.feature(co)}"

    def bb(self, workspace, *args):
        return f"RWB_WORKSPACE={q(workspace)} branchbox " + " ".join(args) + " </dev/null"

    # ---- host environment and provisioning --------------------------------------------
    def host_env(self, workdir):
        return host_env(workdir, [Path(workdir) / "tools" / "bin"])

    def provision(self):
        t = self.tools()
        if self.binary:
            body = [f"mkdir -p {t}/bin", f"cp {q(self.binary)} {t}/bin/branchbox"]
            if self.options.get("branchbox_sha256"):
                body.append(f'echo "{self.options["branchbox_sha256"]}  {t}/bin/branchbox" | shasum -a 256 -c -')
        else:
            cases = " ".join(f'{arch}) a={name}; s={sha};;' for arch, (name, sha) in ARCHIVES.items())
            body = [
                'test "$(uname -s)" = Darwin || { echo "pinned BranchBox archives are for macOS hosts" >&2; exit 1; }',
                f'case "$(uname -m)" in {cases} *) echo "unsupported arch" >&2; exit 1;; esac',
                f"mkdir -p {t}/bin {t}/dl",
                f'curl -fsSL -o {t}/dl/bb.tar.gz "{RELEASE}/$a.tar.gz"',
                f'echo "$s  {t}/dl/bb.tar.gz" | shasum -a 256 -c -',
                f'tar -xzf {t}/dl/bb.tar.gz -C {t}/dl "$a/branchbox"',
                f'install -m 755 "{t}/dl/$a/branchbox" {t}/bin/branchbox',
            ]
        body.append(f'{t}/bin/branchbox --version | grep -q "{BRANCHBOX_VERSION}"')
        return [("provision-branchbox", "set -euo pipefail\n" + "\n".join(body), None)]

    def versions(self):
        return ("set -e; branchbox --version; shasum -a 256 \"$(command -v branchbox)\"; docker version --format "
                "'{{.Server.Version}} {{.Server.Os}}/{{.Server.Arch}}'; docker compose version; git --version")

    # ---- checkouts ----------------------------------------------------------------------
    def prepare(self, co, lock_from=None):
        """Main repository (once), then a native feature worktree for co. `feature start` is
        configuration-only for the container runtime: it starts no services."""
        main, cfg = self.main(), f"{self.src}/adapters/{self.name}"
        receipts = f"{self.workdir()}/receipts"
        return "\n".join([
            "set -euo pipefail",
            f"mkdir -p {q(receipts)}",
            f"if [ ! -d {q(main)}/.git ]; then",
            f"  mkdir -p {q(main)}/.devcontainer",
            f"  cp -R {self.src}/fixtures/app/. {q(main)}/",
            f"  rm -rf {q(main)}/.venv {q(main)}/.pytest_cache",
            f"  cp {cfg}/devcontainer/devcontainer.json {cfg}/devcontainer/Dockerfile {cfg}/devcontainer/compose.yaml {q(main)}/.devcontainer/",
            f"  cp {cfg}/gitignore {q(main)}/.gitignore",
            *("  " + line for line in init_repo(main, "main")),
            f"  (cd {q(main)} && {self.bb(main, 'init', '-y', '--no-parent-structure', '--no-coding-agents', '--skip-env')})",
            # Commit BranchBox's own config mutations so worktrees branch from the real config.
            f"  {git(main, 'add', '-A', '.devcontainer', '.gitignore')}",
            f"  {git(main, 'diff', '--cached', '--quiet')} || "
            f"{git(main, 'commit', '-q', '-m', q('Record BranchBox workspace configuration'))}",
            "fi",
            self.bb(main, "feature", "start", q(self.feature(co)), "--repo", q(main), "--runtime", "container",
                    "--skip-module", "database", "--skip-module", "tunnel", "--skip-module", "specs",
                    "--json") + f" > {q(receipts)}/{self.feature(co)}.start.json",
            f"python3 -I -c {q(START_RECEIPT_PY)} {q(receipts)}/{self.feature(co)}.start.json {q(co.path)}",
            f"printf '%s\\n' {q(co.token)} > {q(co.path)}/rwbapp/SOURCE_TOKEN",
        ])

    def break_config(self, co):
        return (f"sed -i.orig 's#^    image: postgres:17.11-alpine@sha256:[0-9a-f]*$#    image: postgres:99.99.99-alpine#' "
                f"{q(co.path)}/.devcontainer/compose.yaml && grep -q 'postgres:99.99.99-alpine' {q(co.path)}/.devcontainer/compose.yaml")

    # ---- lifecycle ----------------------------------------------------------------------
    def setup(self, co):
        return self.bb(co.path, "devcontainer", "build", q(co.path), "--json")

    def start(self, co):
        return self.bb(co.path, "devcontainer", "up", q(co.path), "--json")

    def stop(self, co):
        # Native down removes the containers and keeps the named volumes.
        return self.bb(co.path, "devcontainer", "down", q(co.path), "--json")

    def enter(self, co, body):
        return self.bb(co.path, "devcontainer", "exec", "--workspace-folder", q(co.path), "--", "bash", "-c", q(body))

    def tool_versions(self, co):
        label = q(f"label=com.docker.compose.project={self.project(co)}")
        return "\n".join([
            "set -e",
            self.enter(co, "command -v python3 uv; python3 --version; uv --version; "
                           f"{self.app_python} -c 'import psycopg, redis; print(psycopg.__version__, redis.__version__)'"),
            f"for c in $(docker ps -q --filter {label}); do docker inspect --format "
            "'{{index .Config.Labels \"com.docker.compose.service\"}} {{.Config.Image}} {{.Image}}' \"$c\"; done",
            f"docker exec $(docker ps -q --filter {label} --filter label=com.docker.compose.service=postgres) postgres --version",
            f"docker exec $(docker ps -q --filter {label} --filter label=com.docker.compose.service=redis) redis-server --version",
        ])

    def instance_identity(self, co):
        return "set -euo pipefail\n" + self.code_check(co, co.path) + " && " + \
            compose_receipt(self.project(co), co.path, "/workspaces", digest_var="h")

    def stopped_probe(self, co, identity):
        label = q(f"label=com.docker.compose.project={self.project(co)}")
        return "\n".join([
            project_stopped(self.project(co)),
            f'v=$(docker volume ls -q --filter {label} | wc -l)',
            '[ "$v" -ge 2 ] || { echo "down removed volumes" >&2; exit 1; }',
        ])

    def cleanup(self, co):
        # Native scoped destruction while the worktree/config still exist; D's broken config
        # is restored first so Compose can render the project it must remove.
        restore = f"if [ -e {q(co.path)}/.devcontainer/compose.yaml.orig ]; then mv {q(co.path)}/.devcontainer/compose.yaml.orig {q(co.path)}/.devcontainer/compose.yaml; fi"
        return f"set -euo pipefail\n{restore}\n" + \
            self.bb(co.path, "devcontainer", "down", q(co.path), "--volumes", "--remove-orphans", "--json")

    def names(self):
        return [self.checkout(n, i) for i, n in enumerate("abcde")]

    def cleanup_host(self):
        return "\n".join(f"( {compose_remove(self.project(co), images=[f'{self.project(co)}-app'])} )"
                         for co in self.names())

    def host_resources(self):
        cos = self.names()
        return "set -euo pipefail\n" + "\n".join(f"( {compose_resources(self.project(co))} )" for co in cos) + \
            "\n" + images_left([f"{self.project(co)}-app" for co in cos])

    def service_processes(self):
        return "set -euo pipefail\n" + "\n".join(
            f"docker ps --filter {q('label=com.docker.compose.project=' + self.project(co))} "
            "--filter status=running --format '{{.ID}} {{.Names}}'" for co in self.names())

    def supervisor_processes(self):
        return "true"
