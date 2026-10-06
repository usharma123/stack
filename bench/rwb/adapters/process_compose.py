"""Process Compose 1.122.0 over a Nix-pinned toolchain (bench/research/process-compose.md).

Lane name: "Nix-pinned toolchain + Process Compose 1.122.0 + project configuration".
Process Compose supervises, gates, probes and reports; it installs nothing. Nix (flake in
`toolchain/`) supplies Python/uv/PostgreSQL/Redis; ports, data paths and the control socket
are project configuration. This module also holds the small Nix-flake base shared by the
native-extra Nix lanes (services-flake, dnvr).
"""
import hashlib

from .base import Adapter, q

# nixpkgs revision shared with the plain Nix lane (research/nix.md). Inspected package sources
# at this revision: Python 3.13.15, PostgreSQL 17.11, Redis 8.10.2, uv 0.12.22; the run
# records the realized versions.
NIXPKGS_REV = "151fa4e8ddfdd8dd25d945ad94ed54a13de9f6e4"
NIX_PINS = dict(nixpkgs=NIXPKGS_REV, python="3.13.15", uv="0.12.22", postgresql="17.11", redis="8.10.2")

PC_VERSION = "1.122.0"
# Official release archive digest (research/process-compose.md, release checksum file).
PC_LINUX_ARM64_TGZ_SHA256 = "52fa7d5a2d5e0db470faec5976204fc215ed7e3d13689e930cf522becfb63778"


def short_runtime_dir(prefix, run_id):
    """Short, run-owned /tmp directory for Unix sockets (well under the 108-byte limit)."""
    return f"/tmp/{prefix}-{hashlib.sha1(run_id.encode()).hexdigest()[:10]}"


def install_process_compose(user_bin="$HOME/.local/bin"):
    url = (f"https://github.com/F1bonacc1/process-compose/releases/download/v{PC_VERSION}/"
           "process-compose_linux_arm64.tar.gz")
    return "\n".join([
        "set -eu",
        'test "$(uname -m)" = aarch64 || { echo "pinned process-compose digest is for linux-arm64" >&2; exit 1; }',
        f'mkdir -p "{user_bin}"',
        'tmp=$(mktemp -d)',
        f'curl -fsSL -o "$tmp/pc.tgz" {url}',
        f'echo "{PC_LINUX_ARM64_TGZ_SHA256}  $tmp/pc.tgz" | sha256sum -c -',
        'tar -xzf "$tmp/pc.tgz" -C "$tmp" process-compose',
        f'install -m 755 "$tmp/process-compose" "{user_bin}/process-compose"',
        'rm -rf "$tmp"',
        f'"{user_bin}/process-compose" version | grep -q "v\\?{PC_VERSION}"',
    ])


class NixFlakeAdapter(Adapter):
    """A flake kept in its own checkout subdirectory and entered as a path flake, so the
    copied store source never includes .venv or live service state (sockets cannot be
    copied into the store at all)."""
    image = "ev-nix"
    flake_dir = "toolchain"
    shell_attr = "default"
    timeouts = dict(setup=2400, start=300, step=300, ready=120)

    @property
    def lock_files(self):
        return (f"{self.flake_dir}/flake.lock",)

    def nix_env(self):
        return ("export NIX_CONFIG='experimental-features = nix-command flakes' "
                'PATH="$HOME/.local/bin:$PATH" NO_COLOR=1')

    def flake_ref(self, co, attr=None):
        ref = f"path:{co.path}/{self.flake_dir}"
        return f"{ref}#{attr}" if attr else ref

    def _in(self, co, body):
        return f"{self.nix_env()}; cd {q(co.path)} && {body}"

    def develop(self, co, body):
        return (f"nix develop {q(self.flake_ref(co, self.shell_attr))} --no-update-lock-file "
                f"--command bash -c {q(body)}")

    def setup(self, co):
        # Cold A writes the lock; B/E arrive with A's lock and resolve nothing new.
        return self._in(co, f"nix flake lock {q(self.flake_ref(co))} && " + self.develop(co, "true"))

    def frozen_setup(self, co):
        # --no-update-lock-file: evaluation fails instead of writing a changed lock.
        return self._in(co, self.develop(co, "true"))

    def break_config(self, co):
        return f"sed -i 's/postgresql_17/postgresql_99/g' {q(co.path)}/{self.flake_dir}/flake.nix"


class ProcessComposeAdapter(NixFlakeAdapter):
    name = "process-compose"
    title = f"Process Compose {PC_VERSION} (Nix toolchain)"
    features = dict(
        lockfile="native",            # Nix flake.lock (toolchain, not Process Compose)
        frozen_setup="native",        # nix --no-update-lock-file
        services="native",            # Process Compose supervision
        detached_services="native",   # up --detached
        readiness="native",           # readiness_probe + project is-ready --wait
        per_checkout_ports="scripted",  # bench.local.env ports and a per-checkout socket
        per_checkout_data="scripted",   # checkout-relative PGDATA/REDIS_DATA via rwb-env.sh
        stop_confirmation="scripted",   # down returns early; rwb-pc.sh waits for PIDs/API
        structured_status="native",   # process list -o json
        wrong_instance_guard="unsupported")
    config_files = ("toolchain/flake.nix", "process-compose.yaml", "rwb-pc.sh")
    shared_files = ("rwb-env.sh",)
    pins = dict(NIX_PINS, process_compose=PC_VERSION, process_compose_tgz_sha256=PC_LINUX_ARM64_TGZ_SHA256)

    def socket(self, co):
        return f"{short_runtime_dir('rwb-pc', self.run_id)}/{co.name}.sock"

    def provision(self):
        return [("provision-process-compose", install_process_compose(), None)]

    def versions(self):
        return (f"{self.nix_env()}; set -e; process-compose version; nix --version; "
                'sha256sum "$HOME/.local/bin/process-compose"')

    def local_env(self, co):
        return self.bench_local_env(co) + [
            f"printf 'RWB_PC_SOCKET=%s\\n' {q(self.socket(co))} > {q(co.path)}/pc.local.env"]

    def enter(self, co, body):
        return self._in(co, "source ./rwb-env.sh && " + self.develop(co, body))

    def start(self, co):
        # The detached manager inherits the toolchain shell's PATH and endpoint variables.
        return self._in(co, "source ./rwb-env.sh && " + self.develop(co, "bash ./rwb-pc.sh up"))

    def ready(self, co):
        return self._in(co, "bash ./rwb-pc.sh ready")

    def status(self, co):
        return self._in(co, "bash ./rwb-pc.sh status")

    def stop(self, co):
        return self._in(co, "bash ./rwb-pc.sh down")

    def cleanup(self, co):
        return self._in(co, "test ! -f pc.local.env || bash ./rwb-pc.sh down")
