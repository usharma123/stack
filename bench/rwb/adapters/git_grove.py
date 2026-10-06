"""GitGrove (@gitgrove/cli) 0.1.0-alpha.1.8, the newest version published to npm. Source tag
alpha.1.11 / main differ (env regeneration on every start, .env.example bootstrap) and are
NOT used.

Native: `grove start <branch> --json` attaches to the worktree, writes .env.worktree once
(COMPOSE_PROJECT_NAME from the naming template, DB_PORT/REDIS_PORT from its probe-based
allocator, pinned image references passed through by the strict env contract) and runs the
configured custom-shell provider; `grove stop` runs the provider's stop script; `grove
status --json`; `grove delete --yes` removes the worktree.

Scripted (bench/adapters/git-grove/): the provider scripts, Compose project and Dockerfile.
The worktree's own source is baked into its app image, and the app runs inside the Compose
network (`docker compose exec`), so its source token proves which checkout was built.
Worktrees are created with `git worktree add` (Grove then attaches by path) because
`grove start --new` would build before the per-checkout source token exists. Grove's
destructive `docker teardown` is interactive-only, so data destruction is scripted.
"""
from .base import q
from .worktree_common import NEXT_FREE_PY, WorktreeHostAdapter

GROVE_VERSION = "0.1.0-alpha.1.8"
GROVE_INTEGRITY = ("sha512-szaFvkSzi+a8JK8R1Ui+Yvv9u/6xnAAEw1Kvk61z+qRtOr+zlZD57Nr6yLzc+R21pZOh"
                   "Ym3X2WyD00TEaGC0mQ==")
GROVE_CLI_SHA256 = "b8cd71e6d268fc6578da93d8da293728085900e80978dd316e5e53654a426b12"


class GitGroveAdapter(WorktreeHostAdapter):
    name = "git-grove"
    title = "GitGrove 0.1.0-alpha.1.8 + Docker Compose"
    repo_name = "rwbg"
    extra_host_programs = ("node", "npm")
    repo_files = {"grove-config.json": ".grove/config.json", "compose.yaml": "compose.yaml",
                  "Dockerfile": "Dockerfile", "dockerignore": ".dockerignore",
                  "env.example": ".env.example", "bin/start.sh": "bin/start.sh",
                  "bin/stop.sh": "bin/stop.sh", "gitignore": ".gitignore"}
    tool_files = ("tool/package.json", "tool/package-lock.json")
    app_python = "/app/.venv/bin/python"
    features = dict(
        lockfile="scripted",            # uv.lock, installed with uv sync --locked in the image
        frozen_setup="scripted",
        services="scripted",            # Grove runs the custom-shell scripts; Compose runs services
        detached_services="scripted",
        readiness="scripted",           # benchmark healthchecks + `up --wait` in start.sh
        per_checkout_ports="native",    # Grove's probe-based DB_PORT/REDIS_PORT allocation
        per_checkout_data="scripted",   # Compose volumes under Grove's project name
        stop_confirmation="scripted",
        structured_status="scripted",   # `grove status --json` reports discovery, not health
        wrong_instance_guard="unsupported")

    def __init__(self, options=None, variant=None, run_id="dryrun"):
        super().__init__(options, variant, run_id)
        self.pins.update(git_grove=GROVE_VERSION, git_grove_integrity=GROVE_INTEGRITY,
                         git_grove_cli_sha256=GROVE_CLI_SHA256, git_grove_track="npm published")

    def trees(self):
        return f"{self.realroot()}/trees"

    def checkout(self, name, index, token=""):
        # Grove attaches at $GROVE_WORKTREE_ROOT/<branch with / replaced by ->.
        co = super().checkout(name, index, token)
        co.path = f"{self.trees()}/{self.branch(name)}"
        return co

    def host_env(self, workdir):
        env = super().host_env(workdir)
        env["GROVE_WORKTREE_ROOT"] = self.trees()
        self.pins["host_env"] = dict(env)
        return env

    def grove(self, args):
        return f"(cd {q(self.repo())} && GROVE_WORKTREE_ROOT={q(self.trees())} grove {args})"

    def image(self, co):
        return f"{self.project(co)}-app"

    def provision_tool(self):
        tools, home = self.tools(), self.private_home()
        copies = [f"cp {q(str(self.config_dir()))}/{q(rel)} {q(tools)}/grove/" for rel in self.tool_files]
        return [("provision-git-grove", "\n".join([
            "set -euo pipefail",
            f"mkdir -p {q(tools)}/grove", *copies,
            # npm ci installs exactly the committed lock and verifies every integrity hash.
            f"(cd {q(tools)}/grove && npm ci --ignore-scripts --no-audit --no-fund --cache {q(home)}/.npm)",
            f'echo "{GROVE_CLI_SHA256}  {tools}/grove/node_modules/@gitgrove/cli/dist/cli.js" | shasum -a 256 -c -',
            f"ln -sfn {q(tools)}/grove/node_modules/.bin/grove {q(tools)}/bin/grove",
            f'grove --version | grep -q "v{GROVE_VERSION}$"']), None)]

    def versions(self):
        return (f"grove --version | grep -E 'v[0-9]'; node --version; "
                f"shasum -a 256 {q(self.tools())}/grove/node_modules/@gitgrove/cli/dist/cli.js; "
                f"{self.common_versions()}")

    # ---- worktrees -------------------------------------------------------------------
    def prepare(self, co, lock_from=None):
        return "\n".join([
            "set -euo pipefail",
            self.ensure_repo(),
            f"mkdir -p {q(self.trees())}",
            f"git -C {q(self.repo())} worktree add -q -b {q(self.branch(co.name))} {q(co.path)} main",
            *self.token_and_lock(co, lock_from)])

    def break_config(self, co):
        # Provider pin: a CPython image tag that does not exist.
        return (f"sed -i '' 's|^RWB_PYTHON_IMAGE=.*|RWB_PYTHON_IMAGE=python:3.13.99-slim-bookworm|' "
                f"{q(co.path)}/.env.example")

    def setup(self, co):
        # Build co's app image from co's worktree: pinned Python/uv images, `uv sync --locked`.
        return "\n".join([
            "set -euo pipefail",
            f"cd {q(co.path)}",
            "set -a; . ./.env.example; set +a",
            f"docker build --build-arg RWB_PYTHON_IMAGE --build-arg RWB_UV_IMAGE "
            f"--label rwb.run={q(self.run_id)} -t {q(self.image(co))} ."])

    def frozen_setup(self, co):
        return self.setup(co)

    def compose(self, co, args):
        return (f"docker compose -p {q(self.project(co))} --project-directory {q(co.path)} "
                f"--env-file {q(co.path)}/.env.worktree -f {q(co.path)}/compose.yaml {args}")

    def enter(self, co, body):
        # The app process runs in co's app container (WORKDIR /app); no harness cd.
        return self.compose(co, f"exec -T app bash -c {q(body)}")

    def app_source_path(self, co):
        return "/app"

    def port_map(self, co):
        # No NAT on the app path: it reaches postgres:5432/redis:6379 on the project network,
        # so the URL port must equal the server port with no mapping receipt.
        return None

    def deps(self, co):
        # Dependencies were installed into the image at setup; this re-checks them in the
        # running container against the committed lock without changing it.
        return self.enter(co, "uv sync --locked --no-python-downloads")

    def tool_versions(self, co):
        return "\n".join([
            "set -euo pipefail",
            f"docker run --rm --network none {q(self.image(co))} sh -c "
            "'python -VV; uv --version; uv pip list --python /app/.venv/bin/python --format freeze --exclude-editable'",
            f"grep -E '^RWB_' {q(co.path)}/.env.example"])

    # ---- services: native grove lifecycle running the scripted provider -------------
    def env_value(self, co, key):
        return f"$(sed -n 's/^{key}=//p' {q(co.path)}/.env.worktree)"

    def start(self, co):
        return "\n".join([
            "set -euo pipefail",
            self.grove(f"start {q(self.branch(co.name))} --json"),
            # Grove must have derived the project this adapter tracks for cleanup.
            f'test "{self.env_value(co, "COMPOSE_PROJECT_NAME")}" = {q(self.project(co))} || '
            '{ echo "grove wrote an unexpected COMPOSE_PROJECT_NAME" >&2; exit 1; }'])

    def stop(self, co):
        return "\n".join(["set -euo pipefail", self.grove(f"stop {q(self.branch(co.name))}"),
                          self.no_running_containers(self.project(co))])

    def status(self, co):
        return "\n".join(["set -euo pipefail", self.grove(f"status --json {q(self.branch(co.name))}"),
                          self.compose(co, "ps --all --format json")])

    def planned_pg_port(self, co):
        # Grove allocates DB_PORT inside `grove start`, so no tool command reports it in
        # advance. The benchmark predicts Grove's rule (first port >= 5432 that binds and is
        # not Docker-published); the squatter then tests whether Grove's probe skips it.
        return f"python3 -I - 5432 <<'PY'\n{NEXT_FREE_PY.strip()}\nPY"

    def stopped_probe(self, co, identity):
        # The app reports in-network ports (5432/6379); probe the published host ports.
        return "\n".join([
            "set -euo pipefail",
            f'DB_PORT={self.env_value(co, "DB_PORT")}; REDIS_PORT={self.env_value(co, "REDIS_PORT")}',
            'test -n "$DB_PORT" && test -n "$REDIS_PORT"',
            self.no_running_containers(self.project(co)),
            self.ports_refuse('"$DB_PORT"', '"$REDIS_PORT"')])

    def cleanup(self, co):
        # Destroys co's containers and data volumes (scripted: Grove's teardown needs a
        # terminal), then Grove removes the worktree and branch; the image goes last.
        project = self.project(co)
        return "\n".join([
            "set -euo pipefail",
            self.compose_down_volumes(project),
            self.grove(f"delete {q(self.branch(co.name))} --yes --delete-branch"),
            f"test ! -e {q(co.path)} || {{ echo 'worktree {co.path} still exists' >&2; exit 1; }}",
            self.verify_project_gone(project),
            f"if docker image inspect {q(self.image(co))} >/dev/null 2>&1; then docker image rm {q(self.image(co))} >/dev/null; fi"])
