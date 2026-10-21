"""dnvr (dialohq/dnvr, untagged commit a66c2bb) with its default tmux runner (bench/research/dnvr.md).

`dnvr up` always attaches a terminal, so start runs it on a recorded PTY and detaches with
dnvr's own Ctrl-G binding (adapters/dnvr/rwb-dnvr.sh). Driver time is part of start.
"""
import json

from .base import q
from .process_compose import NIX_PINS, NixFlakeAdapter

DNVR_REV = "a66c2bbabb67293812a5c39855ab0ecf6af21d41"


class DnvrAdapter(NixFlakeAdapter):
    name = "dnvr"
    start_scope = "start-to-ready: PTY driver (`script` + FIFO), `dnvr up`, wait for pg.url/redis.url, detach"
    title = f"dnvr {DNVR_REV[:7]} (tmux runner)"
    flake_dir = "dnvr"
    shell_attr = "rwb"
    features = dict(
        lockfile="native", frozen_setup="native",
        services="native",            # process modules + postgres preset, run by dnvr's runner
        detached_services="native",   # the tmux session persists after detach
        readiness="scripted",         # PG preset's `url` key is native; Redis publisher + PTY wait are ours
        per_checkout_ports="scripted",  # local.json port pair (dnvr's pick-port is unused)
        per_checkout_data="native",   # .dnvr under each checkout root
        stop_confirmation="scripted",   # no down command; Ctrl-C panes, wait for pid locks
        structured_status="unsupported",  # `dnvr ps` is a text table (kept as observed evidence)
        wrong_instance_guard="unsupported")
    # start() returns only after pg.url and redis.url are published by live producers.
    start_waits_ready = True
    config_files = ("dnvr/flake.nix", "rwb-dnvr.sh")
    pins = dict(NIX_PINS, dnvr=DNVR_REV, dnvr_own_nixpkgs="062346a6d85bc4b49dfaa61c986e9c5be21217d1 (overridden by follows)")

    def versions(self):
        return f"{self.nix_env()}; nix --version; echo dnvr {DNVR_REV}"

    def local_env(self, co):
        local = json.dumps(dict(pg=co.pg_port, redis=co.redis_port, instance=co.instance))
        return [f"printf '%s\\n' {q(local)} > {q(co.path)}/dnvr/local.json"]

    def enter(self, co, body):
        return self._in(co, self.develop(co, body))

    def tool_versions(self, co):
        return self.enter(co, "set -e; command -v python3 uv postgres redis-server tmux dnvr; python3 --version; "
                              "uv --version; postgres --version; redis-server --version; tmux -V")

    def start(self, co):
        return self.enter(co, "bash ./rwb-dnvr.sh up")

    def status(self, co):
        return self.enter(co, "bash ./rwb-dnvr.sh status")

    def stop(self, co):
        return self.enter(co, "bash ./rwb-dnvr.sh down")

    def cleanup(self, co):
        return self._in(co, "test ! -f dnvr/flake.lock || " + self.develop(co, "bash ./rwb-dnvr.sh down"))

    def artifacts(self, co):
        # PG preset jsonlog, runner pane logs and the PTY transcripts ($DNVR_STATE/logs).
        return (".dnvr/logs",)

    def diagnostics(self, co):
        return [("dnvr-logs", self.enter(co, "bash ./rwb-dnvr.sh logs"))]

    def conflict_logs(self, co):
        return self.enter(co, "bash ./rwb-dnvr.sh logs")

    def supervisor_processes(self):
        return ("ps -eo pid=,user=,stat=,args= | awk '$2==\"agent\" && $3 !~ /^Z/' | "
                "grep -E 'tmux|dnvr-tmux-sidebar|rwb-redis|-pg( |$)' | grep -v -E 'grep|awk' || true")
