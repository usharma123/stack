"""workz (rohansx/workz) stable 0.11.0, the latest published release. Source-head features
(named service ports, repo-keyed names, `run`, hook context, reaping) are NOT used.

Native: `workz start --isolated` creates the worktree and allocates a PORT range, DB_NAME and
COMPOSE_PROJECT_NAME in .env.local; `workz sync --isolated --json` re-applies it idempotently;
`workz switch <branch>` resolves the worktree directory (the same `__workz_cd:` contract its
shell integration uses); `workz done` removes the worktree and releases the allocation.

Scripted (bench/adapters/workz/): the uv/Python provider, the URL derivation from workz's
values (rwb-workz-env.sh), and the Compose services. `workz start --docker` is not used: it
runs once at creation, does not load .env.local, and downgrades Compose failure to a warning.
"""
from .base import q
from .worktree_common import WorktreeHostAdapter

WORKZ_VERSION = "0.11.0"
WORKZ_URL = (f"https://github.com/rohansx/workz/releases/download/v{WORKZ_VERSION}/"
             f"workz-v{WORKZ_VERSION}-aarch64-apple-darwin.tar.gz")
WORKZ_SHA256 = "a0a203b6d4f76dd00198d101163c644529553cb7eb33c261fa4fa15ae405d195"
WORKZ_BINARY_SHA256 = "72994c049c43989e4ec868dd3741389548f70aa15342acd34cd88349c5feef97"


class WorkzAdapter(WorktreeHostAdapter):
    name = "workz"
    prepare_scope = 'native `workz start --isolated --no-sync` worktree creation (excluded from first_task)'
    title = "workz 0.11.0 + uv + Docker Compose"
    repo_name = "rwbz"
    repo_files = {"workz.toml": ".workz.toml", "compose.yaml": "compose.yaml",
                  "python-version": ".python-version", "gitignore": ".gitignore",
                  "rwb-workz-env.sh": "rwb-workz-env.sh"}
    features = dict(
        lockfile="scripted",            # uv.lock through the external uv provider
        frozen_setup="scripted",        # uv sync --frozen
        services="scripted",            # Compose glue; workz declares no services
        detached_services="scripted",
        readiness="scripted",           # benchmark healthchecks + `up --wait`
        per_checkout_ports="native",    # workz port-range allocation (PORT, PORT+1)
        per_checkout_data="scripted",   # Compose volumes under workz's project name
        stop_confirmation="scripted",
        structured_status="scripted",   # `workz status` is text; Compose ps --format json
        wrong_instance_guard="unsupported")

    def __init__(self, options=None, variant=None, run_id="dryrun"):
        super().__init__(options, variant, run_id)
        self.pins.update(workz=WORKZ_VERSION, workz_archive_sha256=WORKZ_SHA256,
                         workz_binary_sha256=WORKZ_BINARY_SHA256, workz_track="published stable")

    def checkout(self, name, index, token=""):
        # workz's own layout: <parent>/<repo>--<branch with / replaced by ->.
        co = super().checkout(name, index, token)
        co.path = f"{self.realroot()}/{self.repo_name}--{self.branch(name)}"
        return co

    def project(self, co):
        # workz derives COMPOSE_PROJECT_NAME = branch slug (non-alphanumerics -> "_").
        return self.branch(co.name).replace("-", "_")

    def provision_tool(self):
        tools = self.tools()
        return [("provision-workz", "\n".join([
            "set -euo pipefail",
            self.fetch(WORKZ_URL, WORKZ_SHA256, f"{tools}/dl/workz.tar.gz"),
            f"mkdir -p {q(tools)}/dl/workz && tar -xzf {q(tools)}/dl/workz.tar.gz -C {q(tools)}/dl/workz",
            f'echo "{WORKZ_BINARY_SHA256}  {tools}/dl/workz/workz" | shasum -a 256 -c -',
            f"install -m 755 {q(tools)}/dl/workz/workz {q(tools)}/bin/workz",
            f'workz --version | grep -qx "workz {WORKZ_VERSION}"']), None)]

    def versions(self):
        return f'workz --version; shasum -a 256 "$(command -v workz)"; {self.common_versions()}'

    # ---- worktrees -------------------------------------------------------------------
    def prepare(self, co, lock_from=None):
        return "\n".join([
            "set -euo pipefail",
            self.ensure_repo(),
            f"cd {q(self.repo())}",
            # Creates ../rwbz--<branch>, allocates the port range, writes .env.local.
            f"workz start {q(self.branch(co.name))} --isolated --no-sync",
            *self.token_and_lock(co, lock_from),
            f"{q(co.path)}/rwb-workz-env.sh"])

    def nav(self, co):
        """cd into co's worktree as resolved by `workz switch <branch>` (not by the harness)."""
        return "\n".join([
            "set -euo pipefail",
            f"cd {q(self.repo())}",
            f"out=$(workz switch {q(self.branch(co.name))})",
            'case "$out" in __workz_cd:*) cd "${out#__workz_cd:}" ;;'
            ' *) echo "workz switch printed no directory: $out" >&2; exit 2 ;; esac'])

    def enter(self, co, body):
        return "\n".join([self.nav(co), "set -a; . ./.env; set +a", f"bash -c {q(body)}"])

    def setup(self, co):
        # workz re-applies its isolation (same allocation) and reports it as JSON; then the
        # scripted provider resolves the pinned Python and checks the committed lock.
        return "\n".join([
            self.nav(co),
            "workz sync --isolated --no-install --json",
            "./rwb-workz-env.sh",
            self.uv_provider_check(co)])

    def deps(self, co):
        return self.enter(co, "uv sync --frozen --no-python-downloads")

    def frozen_setup(self, co):
        return "\n".join([self.setup(co), self.deps(co)])

    def tool_versions(self, co):
        return self.enter(co, "set -e; .venv/bin/python -VV; uv --version; "
                              "uv pip list --format freeze --exclude-editable; grep -E '^ +image:' compose.yaml")

    # ---- services (scripted Compose, project and ports from workz) ------------------
    def compose(self, args):
        return f'docker compose -p "$COMPOSE_PROJECT_NAME" --env-file .env -f compose.yaml {args}'

    def start(self, co):
        return self.enter(co, self.compose("up -d --wait --wait-timeout 120"))

    def stop(self, co):
        return "\n".join([self.nav(co), "set -a; . ./.env; set +a",
                          self.compose("stop --timeout 30"),
                          self.no_running_containers(self.project(co))])

    def status(self, co):
        return "\n".join([self.nav(co), "set -a; . ./.env; set +a",
                          "workz status >&2", self.compose("ps --all --format json")])

    def planned_pg_port(self, co):
        return f"sed -n 's/^PORT=//p' {q(co.path)}/.env.local"

    def stopped_probe(self, co, identity):
        # The app reports container-internal ports; probe the host ports workz allocated.
        return "\n".join([
            "set -euo pipefail",
            f"set -a; . {q(co.path)}/.env; set +a",
            self.no_running_containers(self.project(co)),
            self.ports_refuse('"$PORT"', '"$REDIS_PORT"')])

    def cleanup(self, co):
        # Destroys co's containers and data volumes, then lets workz remove the worktree and
        # release its port allocation. --force: the untracked SOURCE_TOKEN makes it dirty.
        project = self.project(co)
        return "\n".join([
            "set -euo pipefail",
            self.compose_down_volumes(project),
            f"cd {q(self.repo())}",
            f"workz done {q(self.branch(co.name))} --force --delete-branch",
            f"test ! -e {q(co.path)} || {{ echo 'worktree {co.path} still exists' >&2; exit 1; }}",
            self.verify_project_gone(project)])
