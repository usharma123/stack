"""devenv 2.4.0 (bench/research/devenv.md). PostgreSQL/Redis modules with native readiness
probes, detached native process manager and automatic per-checkout port allocation.

The image ships devenv 2.3.1; provisioning installs the released 2.4.0 CLI (source commit
b904dcb5) into a run-owned Nix profile in the container, never the image's default profile.
"""
from .base import Adapter, q
from .common import wait_nix_daemon

DEVENV_VERSION = "2.4.0"
DEVENV_REV = "b904dcb51fe48c30db250038241507f60752f222"
NIXPKGS_REV = "151fa4e8ddfdd8dd25d945ad94ed54a13de9f6e4"  # shared Nix-lane revision
CLI_PROFILE = "$HOME/rwb-devenv-cli"
NO_TUI = "--no-tui"


class DevenvAdapter(Adapter):
    name = "devenv"
    title = "devenv"
    image = "ev-devenv"
    features = dict(
        lockfile="native", frozen_setup="unsupported", services="native", detached_services="native",
        readiness="native", per_checkout_ports="native", per_checkout_data="native",
        stop_confirmation="native", structured_status="unsupported", wrong_instance_guard="unsupported")
    config_files = ("devenv.nix", "devenv.yaml")
    lock_files = ("devenv.lock",)
    pins = dict(devenv=DEVENV_VERSION, devenv_rev=DEVENV_REV, nixpkgs=NIXPKGS_REV,
                deviation="nixpkgs 151fa4e8 (shared Nix lanes) instead of the research recipe's addf7cf5 "
                          "(devenv's own lock: Python 3.13.9/PG 17.7/Redis 8.2.2) for version parity")
    timeouts = dict(setup=3600, start=300, step=300, ready=120)
    setup_scope = ("first `devenv shell -- true`: resolves inputs into devenv.lock (A) and builds the shell with "
                   "all packages; service init (initdb) happens at `devenv up`")
    cache_note = ("ev-devenv image (Determinate Nix 3.23.0, devenv 2.3.1 preinstalled): Nix store as built; the "
                  "2.4.0 CLI is provisioned untimed; A's substitutes are reused by B (same store)")

    def env(self):
        return f'export PATH="{CLI_PROFILE}/bin:$PATH" NO_COLOR=1'

    def provision(self):
        return [wait_nix_daemon(), ("provision-devenv-cli", "\n".join([
            "set -eu",
            f'nix profile add --profile "{CLI_PROFILE}" --accept-flake-config '
            f"github:cachix/devenv/{DEVENV_REV}#devenv",
            f'"{CLI_PROFILE}/bin/devenv" version | grep -q "devenv {DEVENV_VERSION}"',
        ]), None)]

    def versions(self):
        return f"{self.env()}; devenv version; command -v devenv; nix --version"

    def _in(self, co, body):
        return f"{self.env()}; cd {q(co.path)} && {body}"

    def break_config(self, co):
        return f"sed -i 's/pkgs.postgresql_17/pkgs.postgresql_99/' {q(co.path)}/devenv.nix"

    bad_config_pattern = r"postgresql_99"

    def setup(self, co):
        return self._in(co, f"devenv shell {NO_TUI} -- true")

    def frozen_setup(self, co):
        # No refuse-to-change mode was found in the 2.4.0 CLI; lock bytes are compared (mode n/a).
        return self.setup(co)

    def enter(self, co, body):
        return self._in(co, f"devenv shell {NO_TUI} -- bash -c {q(body)}")

    def start(self, co):
        return self._in(co, f"devenv up -d {NO_TUI}")

    def ready(self, co):
        return self._in(co, f"devenv processes wait {NO_TUI} --timeout 120")

    def status(self, co):
        return self._in(co, f"devenv processes list {NO_TUI}")

    def stop(self, co):
        return self._in(co, f"devenv down {NO_TUI}")

    def planned_pg_port(self, co):
        # Before `up`, devenv evaluates the requested base port; its allocator then takes the
        # first free port from there. Benchmark prediction of that rule: walk up from the base
        # until a port refuses connections (the squatter then forces a relocation).
        return self._in(co, f"set -o pipefail; base=$(devenv eval {NO_TUI} processes.postgres.ports.main.value "
                            "| tr -dc '0-9\\n' | grep -E '^[0-9]+$' | tail -n 1); test -n \"$base\"; p=$base; "
                            "while (exec 3<>/dev/tcp/127.0.0.1/$p) 2>/dev/null; do p=$((p+1)); done; echo $p")

    def artifacts(self, co):
        return (".devenv/processes.log", ".devenv/state/process-compose")
