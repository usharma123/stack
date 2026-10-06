"""Plain Nix flake dev shell + project scripts (bench/research/nix.md).

Nix pins and realises the toolchain (python313, uv, postgresql_17, redis from nixpkgs
151fa4e8, shared with the native-extra Nix lanes). It has no service lifecycle: the
benchmark's own `_shared/rwb-services.sh` (pg_ctl + daemonized redis-server, explicit ports
from the uncommitted bench.local.env, state in .rwb-state/) runs inside the shell and every
service capability is declared `scripted`. Row name: "Nix + project scripts".
"""
from .base import Adapter, q
from .common import wait_nix_daemon
from .process_compose import NIX_PINS

FLAKE = "./nix"
NIX_FLAGS = "--no-update-lock-file --no-warn-dirty"


class NixAdapter(Adapter):
    name = "nix"
    title = "Nix (flake dev shell) + project scripts"
    image = "ev-nix"
    features = dict(
        lockfile="native", frozen_setup="native", services="scripted", detached_services="scripted",
        readiness="scripted", per_checkout_ports="scripted", per_checkout_data="scripted",
        stop_confirmation="scripted", structured_status="scripted", wrong_instance_guard="unsupported")
    config_files = ("nix/flake.nix",)
    shared_files = ("rwb-env.sh", "rwb-services.sh")
    lock_files = ("nix/flake.lock",)
    start_waits_ready = True  # rwb-services.sh up waits for pg_isready + redis PING (scripted)
    pins = dict(NIX_PINS)
    timeouts = dict(setup=2400, start=300, step=300, ready=120)
    setup_scope = ("`nix flake lock` (A only) + realising the dev shell (`nix develop -c true`): downloads/builds "
                   "every package; initdb happens at the first scripted start")
    cache_note = ("ev-nix image (Determinate Nix 3.23.0 / Nix 2.35.2): store as built, may hold some paths; "
                  "A's substitutes are reused by B (same store)")

    def env(self):
        return "export NO_COLOR=1 NIX_CONFIG='experimental-features = nix-command flakes'"

    def provision(self):
        return [wait_nix_daemon()]

    def versions(self):
        return f"{self.env()}; nix --version; command -v nix"

    def local_env(self, co):
        return self.bench_local_env(co)

    def _in(self, co, body):
        return f"{self.env()}; cd {q(co.path)} && {body}"

    def _shell(self, co, body):
        # The shared env script exports DATABASE_URL/REDIS_URL/PGDATA/REDIS_DATA (scripted wiring).
        return self._in(co, f"nix develop {FLAKE} {NIX_FLAGS} --command bash -c {q('source ./rwb-env.sh && ' + body)}")

    def break_config(self, co):
        return f"sed -i 's/pkgs.postgresql_17/pkgs.postgresql_99/' {q(co.path)}/nix/flake.nix"

    bad_config_pattern = r"postgresql_99"

    def setup(self, co):
        # A generates the lock; B/C/E receive A's committed flake.lock and must not change it.
        return self._in(co, f"{{ test -f nix/flake.lock || nix flake lock {FLAKE}; }} && "
                            f"nix develop {FLAKE} {NIX_FLAGS} --command true")

    def frozen_setup(self, co):
        # --no-update-lock-file: evaluation fails instead of rewriting a stale/incomplete lock.
        return self._in(co, f"test -f nix/flake.lock && nix develop {FLAKE} {NIX_FLAGS} --command true")

    def enter(self, co, body):
        return self._shell(co, body)

    def start(self, co):
        return self._shell(co, "bash ./rwb-services.sh up")

    def status(self, co):
        return self._shell(co, "bash ./rwb-services.sh status")

    def stop(self, co):
        return self._shell(co, "bash ./rwb-services.sh down")

    def artifacts(self, co):
        return (".rwb-state/logs",)
