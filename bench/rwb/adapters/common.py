"""Provisioning helpers shared by adapters. Provisioning is recorded but never timed as setup."""
from .base import sha256_check

# Pinned current releases (2026-10-06 research). The installed version is still recorded
# from the binary itself; a mismatch fails provisioning instead of being silently relabelled.
MISE_VERSION = "2026.10.3"
MISE_LINUX_ARM64_SHA256 = "357260e28904569a6e7124d33b65cef6d043846c07bb8f4906ddf22cc353d61d"


def install_mise(user_bin="$HOME/.local/bin"):
    """Download the pinned mise release into the container user's bin and verify its hash."""
    url = f"https://github.com/jdx/mise/releases/download/v{MISE_VERSION}/mise-v{MISE_VERSION}-linux-arm64"
    return "\n".join([
        "set -eu",
        f'test "$(uname -m)" = aarch64 || {{ echo "pinned mise hash is for linux-arm64" >&2; exit 1; }}',
        f'mkdir -p "{user_bin}"',
        f'curl -fsSL -o "{user_bin}/mise.download" {url}',
        sha256_check(MISE_LINUX_ARM64_SHA256, f'"{user_bin}/mise.download"'),
        f'chmod 755 "{user_bin}/mise.download" && mv "{user_bin}/mise.download" "{user_bin}/mise"',
        f'"{user_bin}/mise" --version | grep -q "^{MISE_VERSION} "',
    ])


def wait_nix_daemon(timeout_s=60):
    """Provisioning step: the ev-nix entrypoint starts nix-daemon in the background, so the
    first `docker exec` can race it. Wait for the socket and a store query (not timed)."""
    return ("wait-nix-daemon", "\n".join([
        "set -eu",
        f"for i in $(seq 1 {timeout_s * 10}); do [ -S /nix/var/nix/daemon-socket/socket ] && break; sleep 0.1; done",
        "test -S /nix/var/nix/daemon-socket/socket || { echo 'nix-daemon socket missing' >&2; exit 1; }",
        "nix store ping --store daemon >/dev/null 2>&1 || nix store info --store daemon",
    ]), None)


def sh_env(**values):
    return " ".join(f"{k}={v}" for k, v in values.items())
