"""Flox 1.17.0 (bench/research/flox.md). Services live only while an activation is held, so
the benchmark holds one (scripted) per checkout; stop is `flox services stop` inside it and
restart is `flox activate -- flox services start`; cleanup releases the activation."""
from .base import Adapter, q
from .common import wait_nix_daemon

HOLDERS = ".rwb-holders"


class FloxAdapter(Adapter):
    name = "flox"
    title = "Flox"
    image = "ev-flox"
    features = dict(
        lockfile="native", frozen_setup="unsupported", services="native", detached_services="scripted",
        readiness="scripted", per_checkout_ports="scripted", per_checkout_data="native",
        stop_confirmation="native", structured_status="native", wrong_instance_guard="unsupported")
    config_files = (".flox/env/manifest.toml",)
    lock_files = (".flox/env/manifest.lock",)
    pins = dict(flox="1.17.0")
    timeouts = dict(setup=2400, start=300, step=300, ready=120)
    setup_scope = "first `flox activate -- true`: locks the manifest (A) and realises the environment"
    cache_note = "ev-flox image: its Nix store may already hold some packages (preseeded at image build)"

    def env(self):
        return "export FLOX_DISABLE_METRICS=true NO_COLOR=1"

    def provision(self):
        return [wait_nix_daemon()]

    def versions(self):
        return f"{self.env()}; flox --version; nix --version"

    def prepare(self, co, lock_from=None):
        # Path environment pointer created natively; the committed manifest then replaces it.
        init = f"{self.env()}; flox init --no-auto-setup --bare -d {q(co.path)} >/dev/null"
        base = super().prepare(co, lock_from)
        first, rest = base.split("\n", 2)[:2], base.split("\n", 2)[2]
        return "\n".join(first + [init, rest])

    def local_env(self, co):
        return self.bench_local_env(co)

    def _in(self, co, body):
        return f"{self.env()}; cd {q(co.path)} && {body}"

    def break_config(self, co):
        return f"sed -i 's/postgresql_17/postgresql_99/' {q(co.path)}/.flox/env/manifest.toml"

    def setup(self, co):
        return self._in(co, "flox activate -d . -- true")

    def frozen_setup(self, co):
        # No refuse-to-change mode; the harness compares lock bytes (mode reported n/a).
        return self.setup(co)

    def enter(self, co, body):
        return self._in(co, f"flox activate -d . -- bash -c {q(body)}")

    def _alive_holder(self):
        return (f'alive=""; for p in $(cat {HOLDERS}/pids 2>/dev/null); do '
                f'kill -0 "$p" 2>/dev/null && alive=$p; done')

    def start(self, co):
        # Services live only while an activation is held (scripted holder). Supported lifecycle
        # (research/flox.md): one held activation per checkout; restart = `flox activate -- flox
        # services start` inside it. A repeated start with running services is a no-op.
        return self._in(co, f"mkdir -p {HOLDERS} && {self._alive_holder()}; "
                            f'if [ -z "$alive" ]; then '
                            f"setsid nohup flox activate -d . --start-services -- sleep 86400 "
                            f"> {HOLDERS}/holder.$$.log 2>&1 < /dev/null & echo $! >> {q(co.path)}/{HOLDERS}/pids; "
                            f"sleep 1; kill -0 $!; "
                            f"elif flox services status -d . --json | jq -e 'length > 0 and all(.status == \"Running\")' >/dev/null; then "
                            f'echo "services already running in held activation $alive"; '
                            f"else flox activate -d . -- flox services start; fi")

    def status(self, co):
        return self._in(co, "flox services status -d . --json")

    def stop(self, co):
        # Stop the services; the held activation stays (its release is cleanup, below).
        return self._in(co, "flox services stop -d .")

    def cleanup(self, co):
        # Stop services, then release this checkout's holders (pids recorded at start) and wait
        # for them to exit; the last activation's release ends Flox's service manager.
        return self._in(co, f"flox services stop -d . ; rc=$?; "
                            f"for p in $(cat {HOLDERS}/pids 2>/dev/null); do kill $p 2>/dev/null; done; "
                            f"for p in $(cat {HOLDERS}/pids 2>/dev/null); do "
                            f"for i in $(seq 1 150); do kill -0 $p 2>/dev/null || break; sleep 0.2; done; "
                            f"! kill -0 $p 2>/dev/null || rc=1; done; rm -f {HOLDERS}/pids; exit $rc")

    def conflict_logs(self, co):
        return self._in(co, "flox services status -d . --json; flox services logs -d . postgres; "
                            "flox services logs -d . redis")

    def artifacts(self, co):
        return (HOLDERS, ".flox/log")
