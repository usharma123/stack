"""pkgx 2.11.0 + dev 1.8.1 (bench/research/pkgx.md).

pkgx provides the tools; `dev` turns the checkout's pkgx.yaml (plus files it sniffs, such as
uv.lock) into an environment, entered non-interactively with the documented
`eval "$(pkgx dev)"` route. pkgx has no lockfile and no service manager: PostgreSQL/Redis
lifecycle is the shared benchmark script (adapters/_shared/rwb-services.sh).

Resolved-version deviations from the Nix lanes are declared in adapters/pkgx/pkgx.yaml and
recorded from the binaries at run time (PostgreSQL 17.2.0, Redis 8.10.0).
"""
from .base import Adapter, q

PKGX_VERSION = "2.11.0"
PKGX_LINUX_ARM64_TXZ_SHA256 = "fb4b9c2beb7264027e0cf99d9b60464425c84005951a10f63b38529b763b40d4"
DEV_VERSION = "1.8.1"
PANTRY_REV = "2df061bd184985428bc17aba4a8a8c1e2fd39781"
PANTRY_DIR = "$HOME/.rwb-pkgx/pantry"
EXPECTED = dict(python="3.13.15", postgresql="17.2", redis="8.10.0", uv="0.12.22")


def install_pkgx(user_bin="$HOME/.local/bin"):
    url = (f"https://github.com/pkgxdev/pkgx/releases/download/v{PKGX_VERSION}/"
           f"pkgx-{PKGX_VERSION}%2Blinux%2Baarch64.tar.xz")
    return "\n".join([
        "set -eu",
        'test "$(uname -m)" = aarch64 || { echo "pinned pkgx digest is for linux-aarch64" >&2; exit 1; }',
        f'mkdir -p "{user_bin}"',
        'tmp=$(mktemp -d)',
        f'curl -fsSL -o "$tmp/pkgx.tar.xz" "{url}"',
        f'echo "{PKGX_LINUX_ARM64_TXZ_SHA256}  $tmp/pkgx.tar.xz" | sha256sum -c -',
        'tar -xJf "$tmp/pkgx.tar.xz" -C "$tmp" pkgx',
        f'install -m 755 "$tmp/pkgx" "{user_bin}/pkgx"',
        'rm -rf "$tmp"',
        f'"{user_bin}/pkgx" --version | grep -qx "pkgx {PKGX_VERSION}"',
    ])


def freeze_pantry():
    """Pin package metadata: a Pantry clone at a recorded commit, used via PKGX_PANTRY_DIR."""
    return "\n".join([
        "set -eu",
        f'rm -rf "{PANTRY_DIR}" && mkdir -p "{PANTRY_DIR}" && cd "{PANTRY_DIR}"',
        "git init -q",
        "git remote add origin https://github.com/pkgxdev/pantry.git",
        f"git fetch -q --depth 1 origin {PANTRY_REV}",
        "git checkout -q FETCH_HEAD",
        f'test "$(git rev-parse HEAD)" = {PANTRY_REV}',
        "test -d projects",
    ])


class PkgxAdapter(Adapter):
    name = "pkgx"
    title = f"pkgx {PKGX_VERSION} + dev {DEV_VERSION}"
    image = "ev-base"
    features = dict(
        lockfile="unsupported", frozen_setup="unsupported",
        services="scripted", detached_services="scripted", readiness="scripted",
        per_checkout_ports="scripted", per_checkout_data="scripted",
        stop_confirmation="scripted", structured_status="scripted",
        wrong_instance_guard="unsupported")
    config_files = ("pkgx.yaml",)
    shared_files = ("rwb-env.sh", "rwb-services.sh")
    lock_files = ()
    pins = dict(pkgx=PKGX_VERSION, pkgx_txz_sha256=PKGX_LINUX_ARM64_TXZ_SHA256, dev=DEV_VERSION,
                pantry=PANTRY_REV, **EXPECTED)
    timeouts = dict(setup=2400, start=300, step=300, ready=120)

    def env(self):
        return f'export PATH="$HOME/.local/bin:$PATH" PKGX_PANTRY_DIR="{PANTRY_DIR}" NO_COLOR=1'

    def provision(self):
        return [("provision-pkgx", install_pkgx(), None), ("provision-pkgx-pantry", freeze_pantry(), None)]

    def versions(self):
        return (f"{self.env()}; set -e; pkgx --version; sha256sum \"$HOME/.local/bin/pkgx\"; "
                f"git -C \"{PANTRY_DIR}\" rev-parse HEAD; pkgx --quiet +pkgx.sh/dev={DEV_VERSION} -- dev --version")

    def local_env(self, co):
        return self.bench_local_env(co)

    def _activate(self, co):
        # dev's dump does not check pkgx's exit status (research), so capture first and then
        # require the activated tools before running anything.
        return (f"{self.env()}; cd {q(co.path)} && source ./rwb-env.sh && "
                f'dev_env="$(pkgx --quiet +pkgx.sh/dev={DEV_VERSION} -- dev)" && eval "$dev_env" && '
                "command -v python3 uv postgres redis-server >/dev/null")

    def enter(self, co, body):
        return f"{self._activate(co)} && bash -c {q(body)}"

    def setup(self, co):
        # Resolves and downloads on first use; the version checks make a silent partial
        # environment (dev ignores pkgx failures) a setup failure.
        checks = [
            f"python3 --version | grep -qx 'Python {EXPECTED['python']}'",
            f"postgres --version | grep -q ' {EXPECTED['postgresql']}$'",
            f"redis-server --version | grep -q 'v={EXPECTED['redis']} '",
            f"uv --version | grep -q '^uv {EXPECTED['uv']}'",
        ]
        return self.enter(co, "set -e; " + "; ".join(checks))

    def break_config(self, co):
        return f"sed -i \"s/'=17.2.0'/'=99.99.99'/\" {q(co.path)}/pkgx.yaml"

    def start(self, co):
        return self.enter(co, "bash ./rwb-services.sh up")

    def status(self, co):
        return self.enter(co, "bash ./rwb-services.sh status")

    def stop(self, co):
        return self.enter(co, "bash ./rwb-services.sh down")
