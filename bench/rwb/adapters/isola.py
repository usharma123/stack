"""isola 0.4.1 (release): per-worktree databases and logical Redis DBs on shared servers.

isola clones one PostgreSQL database per git worktree from a template and claims one
logical Redis DB per worktree (owner marker `__isola_owner__`), both on servers that must
already exist. It has no exec command; its documented interface is the env file it
generates (`.env.isola`), which `uv run --env-file` loads for every app command. The
shared servers are benchmark-owned, run inside the run's own container and are declared
`scripted`; isola's own work is the per-worktree data boundary (`database`).

Layout inside the container: checkout A is the main worktree (branch `a`); B, D and E are
linked worktrees (`git worktree add`), the native isola scenario.
"""
import hashlib

from .agent_env_common import CANONICAL, GIT_ID, compact, init_repo
from .base import Adapter, q
from .common import MISE_VERSION, install_mise

ISOLA_VERSION = "0.4.1"
ISOLA_COMMIT = "af852ae57c6d107e09daaacf8d744cc09e18fbd0"
# From the release's checksums.txt (fetched 2026-10-06), linux arm64 archive.
ISOLA_LINUX_ARM64_SHA256 = "c2494bcf45405d74a79e6e74d7283e106b77dbbb41477955c3776b95d4fea92d"
ISOLA_URL = (f"https://github.com/cyucelen/isola/releases/download/v{ISOLA_VERSION}/"
             f"isola_{ISOLA_VERSION}_linux_arm64.tar.gz")
# Shared servers listen inside the run's own container, away from the checkout ports.
SHARED_PG_PORT = 25440
SHARED_REDIS_PORT = 26390


class IsolaAdapter(Adapter):
    name = "isola"
    title = "isola"
    image = "ev-base"
    isolation_boundary = "database"
    features = dict(
        lockfile="unsupported",          # .isola.toml declares no tool or package versions
        frozen_setup="unsupported",
        services="scripted",             # servers are benchmark-run; isola provisions data on them
        detached_services="scripted",
        readiness="unsupported",         # 400 ms process grace only; the app's wait is used
        per_checkout_ports="unsupported",  # shared servers: one port pair for every worktree
        per_checkout_data="native",      # database + logical DB per worktree
        stop_confirmation="native",      # `isola down` waits for the process group to exit
        structured_status="native",      # `isola ls --json`, `isola accessory ls --json`
        wrong_instance_guard="unsupported")
    not_applicable = {
        "occupied_port": "isola allocates ports only for port-bearing services; PostgreSQL/Redis "
                         "are shared servers, so a squatted checkout port tests nothing isola owns",
    }
    # Contract hook requested in AGENT-ENV-ADAPTERS.md: `isola down` stops the worktree's
    # service but its database stays reachable through the generated env file, by design.
    stop_keeps_data_endpoints = True
    config_files = ("isola.toml", "gitignore", "toolchain.toml")
    timeouts = dict(setup=3600, start=300, step=300)

    def __init__(self, options=None, variant=None, run_id="dryrun"):
        super().__init__(options, variant, run_id)
        self.project = f"rwb-isola-{compact(run_id)}"
        self.db_prefix = f"rwb_{compact(run_id)}"[:40]
        self.pins = dict(isola=ISOLA_VERSION, isola_commit=ISOLA_COMMIT,
                         isola_archive_sha256=ISOLA_LINUX_ARM64_SHA256, mise=MISE_VERSION,
                         toolchain=dict(CANONICAL), project=self.project,
                         shared=dict(pg_port=SHARED_PG_PORT, redis_port=SHARED_REDIS_PORT))
        config = (self.config_dir() / "isola.toml")
        if config.exists():
            self.pins["isola_toml_sha256"] = hashlib.sha256(config.read_bytes()).hexdigest()

    # ---- environment ----------------------------------------------------------------
    def env(self):
        tc = f"{self.home}/rwb-toolchain"
        return ("set -e; "
                f'export PATH="$HOME/.local/bin:$PATH" MISE_YES=1 NO_COLOR=1 MISE_TRUSTED_CONFIG_PATHS={tc} '
                f"RWB_SHARED={self.home}/rwb-shared RWB_RUN={q(self.run_id)} "
                f"RWB_PG_PORT={SHARED_PG_PORT} RWB_REDIS_PORT={SHARED_REDIS_PORT} UV_PYTHON_DOWNLOADS=never; "
                f'if [ -s {tc}/env.sh ]; then . {tc}/env.sh; fi')

    def script(self, name):
        return f"{self.src}/adapters/isola/{name}"

    def provision(self):
        tc = f"{self.home}/rwb-toolchain"
        isola = "\n".join([
            "set -eu",
            'test "$(uname -m)" = aarch64 || { echo "pinned isola hash is for linux-arm64" >&2; exit 1; }',
            'd=$(mktemp -d) && mkdir -p "$HOME/.local/bin"',
            f'curl -fsSL -o "$d/isola.tar.gz" {ISOLA_URL}',
            f'echo "{ISOLA_LINUX_ARM64_SHA256}  $d/isola.tar.gz" | sha256sum -c -',
            'tar -xzf "$d/isola.tar.gz" -C "$d" isola',
            'install -m 755 "$d/isola" "$HOME/.local/bin/isola" && rm -rf "$d"',
            f'"$HOME/.local/bin/isola" version | grep -q "{ISOLA_VERSION}"',
            f"mkdir -p {tc} && cp {self.src}/adapters/isola/toolchain.toml {tc}/mise.toml",
        ])
        return [("provision-mise", install_mise(), None), ("provision-isola", isola, None)]

    def versions(self):
        return (f'export PATH="$HOME/.local/bin:$PATH"; isola version; mise --version; '
                f'sha256sum "$(command -v isola)" "$(command -v mise)"')

    # ---- checkouts: main worktree A, linked worktrees for the others ----------------
    def render_config(self, co):
        return (f"sed -e {q('s/@PROJECT@/' + self.project + '/')} -e {q('s/@DB_PREFIX@/' + self.db_prefix + '/')} "
                f"-e 's/@PG_PORT@/{SHARED_PG_PORT}/' -e 's/@REDIS_PORT@/{SHARED_REDIS_PORT}/' "
                f"{self.src}/adapters/isola/isola.toml > {q(co.path)}/.isola.toml")

    def main(self):
        return f"{self.workdir()}/a"

    def prepare(self, co, lock_from=None):
        # isola has no lock to carry from A; every worktree shares A's committed uv.lock.
        lines = ["set -eu"]
        if co.name == "a":
            lines += [f"mkdir -p {q(co.path)}",
                      f"cp -R {self.src}/fixtures/app/. {q(co.path)}/",
                      f"rm -rf {q(co.path)}/.venv {q(co.path)}/.pytest_cache",
                      f"cp {self.src}/adapters/isola/gitignore {q(co.path)}/.gitignore",
                      self.render_config(co),
                      *init_repo(co.path, "a")]
        else:
            lines.append(f"git {GIT_ID} -C {q(self.main())} worktree add -q -b {q(co.name)} {q(co.path)}")
        lines.append(f"printf '%s\\n' {q(co.token)} > {q(co.path)}/rwbapp/SOURCE_TOKEN")
        return "\n".join(lines)

    def break_config(self, co):
        # isola declares no versions; its own invalid-configuration failure is an accessory
        # whose server is unreachable, which must keep the dependent service from starting.
        return f"sed -i 's#^server_url = \"postgres://bench@127.0.0.1:{SHARED_PG_PORT}/postgres\"$#server_url = \"postgres://bench@127.0.0.1:1/postgres\"#' {q(co.path)}/.isola.toml && grep -q '127.0.0.1:1/' {q(co.path)}/.isola.toml"

    # ---- lifecycle --------------------------------------------------------------------
    def _in(self, co, body):
        return f"{self.env()}; cd {q(co.path)} && {body}"

    def setup(self, co):
        # Benchmark toolchain (cold in A, already installed for the others). Its environment
        # is snapshotted once so repeated commands do not pay for mise; isola never sees mise.
        tc = f"{self.home}/rwb-toolchain"
        return (f"{self.env()}; mise -C {tc} install && mise -C {tc} env -s bash > {tc}/env.sh.new && "
                f"mv {tc}/env.sh.new {tc}/env.sh && . {tc}/env.sh && python3 --version && uv --version && "
                "command -v initdb pg_ctl psql redis-server redis-cli")

    def start(self, co):
        return self._in(co, f"bash {self.script('shared-servers.sh')} ensure && isola up && "
                            "test -s .env.isola && isola ls --json && isola accessory ls --json")

    def enter(self, co, body):
        return self._in(co, f"test -s .env.isola && uv run --no-project --env-file .env.isola -- bash -c {q(body)}")

    def status(self, co):
        return self._in(co, f"isola ls --json && isola accessory ls --json && bash {self.script('shared-servers.sh')} status")

    def stop(self, co):
        return self._in(co, "isola down")

    def stopped_probe(self, co, identity):
        # The shared servers keep serving B; the stop is of this worktree's service, and its
        # accessories must be retained (`down` is not `destroy`).
        return self._in(co, f"python3 -I {self.script('receipt.py')} stopped {q(co.name)}")

    def instance_identity(self, co):
        return self._in(co, f"python3 -I {self.script('receipt.py')} identity {q(co.name)} {q(self.project)}")

    def cleanup(self, co):
        # Restore the committed config (D's was broken) so destroy reaches the right servers,
        # drop this worktree's database and logical DB, and verify. A goes last and also stops
        # the run-owned shared servers.
        body = (f"{self.render_config(co)} && "
                f"python3 -I {self.script('receipt.py')} destroy {q(co.name)} {SHARED_PG_PORT} {SHARED_REDIS_PORT}")
        if co.name == "a":
            body += f" && bash {self.script('shared-servers.sh')} stop"
        return self._in(co, body)

    def supervisor_processes(self):
        return ("ps -eo pid=,user=,stat=,args= | awk '$2==\"agent\" && $3 !~ /^Z/' | "
                "grep -E 'rwb-isola-keeper|isola ' | grep -v -E 'grep|awk' || true")
