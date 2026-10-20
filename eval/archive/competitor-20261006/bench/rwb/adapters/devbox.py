"""Devbox 0.18.4 with its PostgreSQL/Redis plugins (bench/research/devbox.md)."""
from .base import SHA256_FN, Adapter, q
from .common import wait_nix_daemon


class DevboxAdapter(Adapter):
    name = "devbox"
    title = "Devbox"
    image = "ev-devbox"
    features = dict(
        lockfile="native", frozen_setup="unsupported", services="native", detached_services="native",
        readiness="scripted", per_checkout_ports="scripted", per_checkout_data="native",
        stop_confirmation="scripted", structured_status="unsupported", wrong_instance_guard="unsupported")
    config_files = ("devbox.json", "bench-devbox/initdb.sh")
    lock_files = ("devbox.lock",)
    pins = dict(devbox="0.18.4", python="3.13.15", uv="0.12.22", postgresql="17.10", redis="8.10.2")
    timeouts = dict(setup=2400, start=300, step=300, ready=120)
    setup_scope = "`devbox install`: resolves/locks (A) and realises packages; plugin init and initdb run at start"
    cache_note = "ev-devbox image: its Nix store may already hold some packages (preseeded at image build)"

    def env(self):
        return "export NO_COLOR=1 DEVBOX_NO_PROMPT=1"

    def provision(self):
        # The image has no non-root setup for the CLI's executable bit in some builds.
        return [wait_nix_daemon(), ("provision-devbox-perms", "chmod a+rx /usr/local/bin/devbox && devbox version", "root")]

    def versions(self):
        return f"{self.env()}; devbox version; nix --version; {SHA256_FN}; rwb_sha256 /usr/local/bin/devbox"

    def local_env(self, co):
        return self.bench_local_env(co)

    def _in(self, co, body):
        return f"{self.env()}; cd {q(co.path)} && {body}"

    def break_config(self, co):
        return f"sed -i 's/postgresql@17.10/postgresql@99.99.99/' {q(co.path)}/devbox.json"

    def setup(self, co):
        return self._in(co, "devbox install")

    def frozen_setup(self, co):
        # No refuse-to-change mode; the harness compares lock bytes (mode reported n/a).
        return self.setup(co)

    def enter(self, co, body):
        return self._in(co, f"devbox run -- bash -c {q(body)}")

    def start(self, co):
        # `up -b` refuses an existing manager; the supported operation then is `services start`.
        return self._in(co, "devbox run initdb && "
                            "{ devbox services up -b || devbox services start postgresql redis; }")

    def conflict_logs(self, co):
        # Devbox's own service view plus its process/service logs written during this start.
        return self._in(co, "devbox services ls; find .devbox -maxdepth 4 -name '*.log' -newer bench.local.env "
                            "-print -exec tail -n 80 {} \\;")

    def artifacts(self, co):
        return (".devbox/compose.log", ".devbox/virtenv/postgresql/data/log")

    def status(self, co):
        return self._in(co, "devbox services ls")

    def prepare(self, co, lock_from=None):
        # devbox.d holds the plugins' generated config (e.g. redis.conf). Devbox expects it to be
        # committed with devbox.json, so a teammate's checkout receives A's copy with the lock.
        body = super().prepare(co, lock_from)
        if lock_from is not None:
            body += (f"\nif [ -d {q(lock_from.path)}/devbox.d ]; then "
                     f"cp -R {q(lock_from.path)}/devbox.d {q(co.path)}/devbox.d; fi")
        return body

    def stop(self, co):
        # `devbox services stop` returns before the servers have exited (observed: Redis still
        # listed right after it). Scripted confirmation: wait until THIS checkout's Redis (by its
        # port) and PostgreSQL (postmaster.pid in its PGDATA) are gone.
        return self._in(co, "devbox services stop && . ./bench.local.env && "
                            "for i in $(seq 1 150); do busy=0; "
                            "ps -eo args= | grep -Eq \"^redis-server 127\\.0\\.0\\.1:$REDIS_PORT( |$)\" && busy=1; "
                            "test -f .devbox/virtenv/postgresql/data/postmaster.pid && busy=1; "
                            "[ \"$busy\" = 0 ] && exit 0; sleep 0.2; done; "
                            "echo 'devbox services still running after stop' >&2; exit 1")
