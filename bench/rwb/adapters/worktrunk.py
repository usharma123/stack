"""Worktrunk (max-sixty/worktrunk) 0.80.0 release binary.

Native: `wt switch --create` makes the worktree; the committed project config
(.config/wt.toml) holds hooks and aliases that are invoked explicitly through `wt`:
`wt hook pre-start` (dependency install), `wt -C <worktree> -y <alias>` (services, ports,
app entry), and `wt remove --foreground`, whose blocking pre-remove hook destroys the
worktree's services and volumes. Ports come from the native `hash_port` template filter.

Scripted: the commands inside those hooks/aliases (uv, Docker Compose) and the Compose file.
A private user config (`--config`) keeps ambient user hooks/aliases out; no shell
integration is installed. Worktree creation skips hooks (`--no-hooks`) because the
per-checkout source token must exist before dependencies are installed; the pre-start hook
is then run explicitly by the deps step.
"""
from .base import q
from .worktree_common import WorktreeHostAdapter

WT_VERSION = "0.80.0"
WT_URL = (f"https://github.com/max-sixty/worktrunk/releases/download/v{WT_VERSION}/"
          "worktrunk-aarch64-apple-darwin.tar.xz")
WT_SHA256 = "8a2bb053c4bc80dea7d9ce6c221ff038d10a3d2dca2dc8f60d1b1a094fa783a9"
# Private user config: worktrees as siblings of the fixture repo, <repo>.<branch>.
USER_CONFIG = 'worktree-path = "{{ repo_path }}/../{{ repo }}.{{ branch | sanitize }}"'
WT_BINARY_SHA256 = "0708ca37fc39f9fa48edc1af500a2ff3664ec0155f63909425b02994f29f0fd1"


class WorktrunkAdapter(WorktreeHostAdapter):
    name = "worktrunk"
    title = "Worktrunk 0.80.0 + uv + Docker Compose"
    repo_name = "rwbt"
    repo_files = {"wt.toml": ".config/wt.toml", "compose.yaml": "compose.yaml",
                  "python-version": ".python-version", "gitignore": ".gitignore"}
    features = dict(
        lockfile="scripted",            # uv.lock through the external uv provider
        frozen_setup="scripted",        # uv sync --frozen in the pre-start hook
        services="scripted",            # Compose commands inside wt aliases/hooks
        detached_services="scripted",
        readiness="scripted",           # benchmark healthchecks + `up --wait`
        per_checkout_ports="native",    # hash_port filter: deterministic, not reserved
        per_checkout_data="scripted",   # Compose volumes per branch-named project
        stop_confirmation="scripted",
        structured_status="scripted",   # `wt list --format=json` covers worktrees only
        wrong_instance_guard="unsupported")

    def __init__(self, options=None, variant=None, run_id="dryrun"):
        super().__init__(options, variant, run_id)
        self.pins.update(worktrunk=WT_VERSION, worktrunk_archive_sha256=WT_SHA256,
                         worktrunk_binary_sha256=WT_BINARY_SHA256)

    def user_config(self):
        return f"{self.tools()}/wt-user.toml"

    def checkout(self, name, index, token=""):
        # Matches the private user config's worktree-path template below.
        co = super().checkout(name, index, token)
        co.path = f"{self.realroot()}/{self.repo_name}.{self.branch(name)}"
        return co

    def wt(self, path, args):
        return f"wt --config {q(self.user_config())} -C {q(path)} -y {args}"

    def provision_tool(self):
        tools = self.tools()
        return [("provision-worktrunk", "\n".join([
            "set -euo pipefail",
            self.fetch(WT_URL, WT_SHA256, f"{tools}/dl/worktrunk.tar.xz"),
            f"tar -xJf {q(tools)}/dl/worktrunk.tar.xz -C {q(tools)}/dl",
            f'echo "{WT_BINARY_SHA256}  {tools}/dl/worktrunk-aarch64-apple-darwin/wt" | shasum -a 256 -c -',
            f"install -m 755 {q(tools)}/dl/worktrunk-aarch64-apple-darwin/wt {q(tools)}/bin/wt",
            f'wt --version | grep -qx "wt v{WT_VERSION}"',
            f"printf '%s\\n' {q(USER_CONFIG)} > {q(self.user_config())}"]), None)]

    def versions(self):
        return f'wt --version; shasum -a 256 "$(command -v wt)"; {self.common_versions()}'

    # ---- worktrees -------------------------------------------------------------------
    def prepare(self, co, lock_from=None):
        return "\n".join([
            "set -euo pipefail",
            self.ensure_repo(),
            self.wt(self.repo(), f"switch --create {q(self.branch(co.name))} --base main --no-cd "
                                 "--no-hooks --format=json"),
            *self.token_and_lock(co, lock_from)])

    def enter(self, co, body):
        # The `cmd` alias runs in the worktree wt resolves for -C, with this branch's URLs.
        return self.wt(co.path, f"cmd bash -c {q(body)}")

    def setup(self, co):
        # wt parses and lists the committed project hooks (malformed config fails here),
        # then the scripted provider resolves the pinned Python and checks the lock.
        return "\n".join(["set -euo pipefail", self.wt(co.path, "hook show >&2"),
                          self.uv_provider_check(co)])

    def deps(self, co):
        return self.wt(co.path, "hook pre-start")

    def frozen_setup(self, co):
        return "\n".join([self.setup(co), self.deps(co)])

    def tool_versions(self, co):
        return self.enter(co, "set -e; .venv/bin/python -VV; uv --version; "
                              "uv pip list --format freeze --exclude-editable; grep -E '^ +image:' compose.yaml")

    # ---- services: native aliases with scripted Compose bodies -----------------------
    def start(self, co):
        return self.wt(co.path, "up")

    def ports(self, co):
        """Shell lines setting PG/REDIS/PROJECT from the native hash_port alias."""
        return "\n".join([
            f"ports=$({self.wt(co.path, 'ports')})",
            'read -r PG REDIS PROJECT <<< "$ports"',
            f'test -n "$REDIS" && test "$PROJECT" = {q(self.project(co))} || '
            '{ echo "unexpected wt ports alias output: $ports" >&2; exit 1; }'])

    def stop(self, co):
        return "\n".join(["set -euo pipefail", self.wt(co.path, "stop-services"),
                          self.no_running_containers(self.project(co))])

    def status(self, co):
        return "\n".join(["set -euo pipefail", self.wt(self.repo(), "list --format=json"),
                          self.wt(co.path, "services-status")])

    def planned_pg_port(self, co):
        return "\n".join(["set -euo pipefail", self.ports(co), 'echo "$PG"'])

    def stopped_probe(self, co, identity):
        # The app reports container-internal ports; probe the hash_port host ports.
        return "\n".join(["set -euo pipefail", self.ports(co),
                          self.no_running_containers(self.project(co)),
                          self.ports_refuse('"$PG"', '"$REDIS"')])

    def cleanup(self, co):
        # Native removal: the blocking pre-remove hook runs `compose down --volumes` (data is
        # destroyed), then the worktree is removed in the foreground. Verified afterwards.
        project = self.project(co)
        return "\n".join([
            "set -euo pipefail",
            self.wt(self.repo(), f"remove {q(self.branch(co.name))} --foreground --force --force-delete"),
            f"test ! -e {q(co.path)} || {{ echo 'worktree {co.path} still exists' >&2; exit 1; }}",
            self.verify_project_gone(project)])
