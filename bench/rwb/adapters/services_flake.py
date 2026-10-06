"""services-flake (juspay, main 0ba7183) via process-compose-flake (bench/research/services-flake.md).

Not services-flake release 0.4.0: the current module sources are pinned by commit and the
bundled Process Compose comes from the shared nixpkgs revision.
"""
import json

from .base import q
from .process_compose import NIX_PINS, NixFlakeAdapter, short_runtime_dir

PINS = dict(NIX_PINS,
            services_flake="0ba7183cab54ffbd0be70cb95694f024701afd2b",
            process_compose_flake="464ff6880737f063c3f0d3d2c7781fda9190868f",
            flake_parts="024633cd702b10285db5cb19b40ad48d2399ba60",
            process_compose="1.122.0 (nixpkgs)")


class ServicesFlakeAdapter(NixFlakeAdapter):
    name = "services-flake"
    title = "services-flake (process-compose-flake)"
    flake_dir = "services"
    features = dict(
        lockfile="native", frozen_setup="native",
        services="native",            # postgres/redis service modules
        detached_services="native",   # up -D
        readiness="native",           # module probes + project is-ready --wait
        per_checkout_ports="scripted",  # local.json port pair; no allocator
        per_checkout_data="native",   # CWD-relative module dataDir
        stop_confirmation="scripted",   # down returns early; rwb-sf.sh waits
        structured_status="native",
        wrong_instance_guard="unsupported")
    config_files = ("services/flake.nix", "rwb-sf.sh")
    pins = PINS

    def socket(self, co):
        return f"{short_runtime_dir('rwb-sf', self.run_id)}/{co.name}.sock"

    def versions(self):
        return f"{self.nix_env()}; nix --version"

    def local_env(self, co):
        local = json.dumps(dict(pg=co.pg_port, redis=co.redis_port, instance=co.instance, socket=self.socket(co)))
        return [f"printf '%s\\n' {q(local)} > {q(co.path)}/services/local.json"]

    def enter(self, co, body):
        return self._in(co, self.develop(co, body))

    def tool_versions(self, co):
        return self.enter(co, "set -e; command -v python3 uv postgres redis-server process-compose; python3 --version; "
                              "uv --version; postgres --version; redis-server --version; process-compose version")

    def start(self, co):
        return self.enter(co, "bash ./rwb-sf.sh up")

    def ready(self, co):
        return self.enter(co, "bash ./rwb-sf.sh ready")

    def status(self, co):
        return self.enter(co, "bash ./rwb-sf.sh status")

    def stop(self, co):
        return self.enter(co, "bash ./rwb-sf.sh down")

    def cleanup(self, co):
        return self._in(co, "test ! -f services/flake.lock || " + self.develop(co, "bash ./rwb-sf.sh down"))
