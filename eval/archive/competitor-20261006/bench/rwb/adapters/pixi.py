"""Pixi 0.81.0 (bench/research/pixi.md), pinned release binary.

Native: conda-forge + PyPI resolution into one `pixi.lock`, `install --locked`, tasks.
Scripted: PostgreSQL/Redis lifecycle (`_shared/rwb-services.sh` via Pixi tasks), endpoint
variables (`_shared/rwb-env.sh`) and per-checkout ports (uncommitted bench.local.env).
App dependencies come from Pixi's own PyPI resolution, not the fixture's uv.lock: the
transitive set is recorded per run and may differ from the uv lane.
"""
from .base import Adapter, q, sha256_check

PIXI_VERSION = "0.81.0"
# Release asset checksum (pixi-aarch64-unknown-linux-musl.tar.gz.sha256, fetched 2026-10-06).
PIXI_TGZ_SHA256 = "9f8d2113fe9dc01788a65f5c2acec34fa56b1193461a5c3e9a775d6d2d621bcb"
PIXI_URL = (f"https://github.com/prefix-dev/pixi/releases/download/v{PIXI_VERSION}/"
            "pixi-aarch64-unknown-linux-musl.tar.gz")
MANIFEST = "--manifest-path pixi.toml"


class PixiAdapter(Adapter):
    name = "pixi"
    title = "Pixi + project scripts"
    image = "ev-base"
    features = dict(
        lockfile="native", frozen_setup="native", services="scripted", detached_services="scripted",
        readiness="scripted", per_checkout_ports="scripted", per_checkout_data="scripted",
        stop_confirmation="scripted", structured_status="scripted", wrong_instance_guard="unsupported")
    config_files = ("pixi.toml", "rwb-pixi-run.sh")
    shared_files = ("rwb-env.sh", "rwb-services.sh")
    lock_files = ("pixi.lock",)
    app_python = "python"
    start_waits_ready = True  # rwb-services.sh up waits for pg_isready + redis PING (scripted)
    pins = dict(pixi=PIXI_VERSION, pixi_tgz_sha256=PIXI_TGZ_SHA256, python="3.13.15", postgresql="17.11",
                redis="8.10.2", psycopg="3.3.6", redis_py="8.1.0", pytest="9.1.1",
                deviation="Python 3.13.15 (conda-forge has no 3.13.16 for linux-aarch64); PyPI set "
                          "resolved by Pixi, not fixtures/app/uv.lock")
    timeouts = dict(setup=2400, start=300, step=300, ready=120)
    setup_scope = ("`pixi lock` (A only) + `pixi install --locked`: solves and installs Python, PostgreSQL, Redis "
                   "and the PyPI dependencies; initdb happens at the first scripted start")
    cache_note = "fresh ev-base container: empty Pixi/rattler cache; A downloads, B reuses (same container)"

    def env(self):
        return 'export PATH="$HOME/.local/bin:$PATH" NO_COLOR=1 PIXI_NO_PROGRESS=true'

    def provision(self):
        tgz = "/tmp/rwb-pixi.tar.gz"
        return [("provision-pixi", "\n".join([
            "set -eu",
            'test "$(uname -m)" = aarch64 || { echo "pinned pixi hash is for linux-aarch64" >&2; exit 1; }',
            f"curl -fsSL --retry 3 -o {tgz} {PIXI_URL}",
            sha256_check(PIXI_TGZ_SHA256, tgz),
            'mkdir -p "$HOME/.local/bin"',
            f'tar -xzf {tgz} -C "$HOME/.local/bin" pixi && rm -f {tgz}',
            f'"$HOME/.local/bin/pixi" --version | grep -qx "pixi {PIXI_VERSION}"',
        ]), None)]

    def versions(self):
        return f"{self.env()}; pixi --version; command -v pixi"

    def local_env(self, co):
        return self.bench_local_env(co)

    def _in(self, co, body):
        return f"{self.env()}; cd {q(co.path)} && {body}"

    def break_config(self, co):
        return f"sed -i 's/^postgresql = \"17.11.\\*\"$/postgresql = \"99.99.99.*\"/' {q(co.path)}/pixi.toml && " \
               f"grep -q '^postgresql = \"99.99.99' {q(co.path)}/pixi.toml"

    def setup(self, co):
        # A solves and writes the lock; B/C/E install from A's committed lock.
        return self._in(co, f"{{ test -f pixi.lock || pixi lock {MANIFEST}; }} && pixi install --locked {MANIFEST}")

    def frozen_setup(self, co):
        # --locked: abort if the lock does not satisfy the manifest instead of updating it.
        return self._in(co, f"test -f pixi.lock && pixi install --locked {MANIFEST}")

    def enter(self, co, body):
        return self._in(co, f"RWB_BODY={q(body)} pixi run --locked {MANIFEST} rwb")

    def deps(self, co):
        # PyPI dependencies are installed by `pixi install` (setup); verify them in the env.
        return self.enter(co, "python -c 'import psycopg, redis, pytest; "
                              "print(psycopg.__version__, redis.__version__, pytest.__version__)'")

    def tool_versions(self, co):
        return self.enter(co, "set -e; command -v python3 postgres redis-server; python3 --version; "
                              "postgres --version; redis-server --version; python -c 'import importlib.metadata as m; "
                              "print(*(d + \"==\" + m.version(d) for d in (\"psycopg\", \"psycopg-binary\", \"redis\", \"pytest\")))'")

    def start(self, co):
        return self._in(co, f"pixi run --locked {MANIFEST} up")

    def status(self, co):
        return self._in(co, f"pixi run --locked {MANIFEST} services-status")

    def stop(self, co):
        return self._in(co, f"pixi run --locked {MANIFEST} down")

    def artifacts(self, co):
        return (".rwb-state/logs",)
