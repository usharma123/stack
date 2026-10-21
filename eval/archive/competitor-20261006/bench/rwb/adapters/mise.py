"""mise with Pitchfork daemons (bench/research/mise.md), pinned mise 2026.10.3."""
from .base import SHA256_FN, Adapter, q
from .common import MISE_VERSION, install_mise


class MiseAdapter(Adapter):
    name = "mise"
    title = "mise + Pitchfork"
    image = "ev-base"
    features = dict(
        lockfile="native", frozen_setup="native", services="native", detached_services="native",
        readiness="native", per_checkout_ports="scripted", per_checkout_data="native",
        stop_confirmation="native", structured_status="native", wrong_instance_guard="unsupported")
    config_files = ("mise.toml",)
    lock_files = ("mise.lock",)
    start_waits_ready = True  # Pitchfork waits for each preset's readiness probe
    setup_scope = "mise lock (A only) + `mise install --locked`: installs every tool and server binary"
    cache_note = "fresh ev-base container: no mise cache; A downloads, B reuses (same container)"
    pins = dict(mise=MISE_VERSION, pitchfork="2.29.0", python="3.13.16", uv="0.12.23",
                postgres="17.11", redis="8.10.2")

    def env(self):
        return 'export PATH="$HOME/.local/bin:$PATH" MISE_YES=1 NO_COLOR=1'

    def provision(self):
        return [("provision-mise", install_mise(), None)]

    def versions(self):
        return f'{self.env()}; mise --version; {SHA256_FN}; rwb_sha256 "$(command -v mise)"'

    def local_env(self, co):
        # Independent clones keep the base port with port="auto"; the documented remedy is a
        # per-checkout local override (scripted endpoint configuration).
        return [f"sed -e 's/@PGPORT@/{co.pg_port}/' -e 's/@REDISPORT@/{co.redis_port}/' "
                f"{self.src}/adapters/mise/mise.local.toml.in > {q(co.path)}/mise.local.toml"]

    def _in(self, co, body):
        return f"{self.env()}; cd {q(co.path)} && {body}"

    def break_config(self, co):
        return f"sed -i 's/\"17.11\"/\"99.99.99\"/g' {q(co.path)}/mise.toml {q(co.path)}/mise.local.toml"

    def setup(self, co):
        return self._in(co, "mise trust mise.toml && mise trust mise.local.toml && "
                            "{ test -f mise.lock || mise lock --platform linux-arm64; } && mise install --locked")

    def frozen_setup(self, co):
        return self._in(co, "mise trust mise.toml && mise trust mise.local.toml && MISE_LOCKED=1 mise install --locked")

    def enter(self, co, body):
        return self._in(co, f"mise exec -- bash -c {q(body)}")

    def start(self, co):
        return self._in(co, "mise daemons start postgres redis")

    def status(self, co):
        return self._in(co, "mise daemons ls --json")

    def stop(self, co):
        return self._in(co, "mise daemons stop postgres redis")
