#!/usr/bin/env bash
# Guix provisioning inside the run's own disposable container (never the host).
# Benchmark-owned; recorded but not timed as Guix setup.
#
#   provision.sh preflight   (root)  prerequisites; never changes anything
#   provision.sh install     (root)  official binary tarball -> /gnu,/var/guix; build users;
#                                    guix-daemon in this container; substitute keys
#   provision.sh canary      (agent) one substitute fetch and one local derivation build
#
# Exit 77 with a line `RWB-BLOCKED: <reason>` means a prerequisite of the environment is
# missing (no verified tarball digest, unreachable download, daemon build isolation denied).
# That is an environment blocker, not a Guix result. Any other nonzero exit is a failure.
#
# Inputs (environment): RWB_GUIX_URL, RWB_GUIX_SHA256 (required, lowercase hex, obtained and
# verified out of band), RWB_GUIX_DAEMON_FLAGS (default empty = the documented sandboxed
# daemon; any value, e.g. --disable-chroot, is a separately labelled condition).
set -euo pipefail

blocked() { echo "RWB-BLOCKED: $*"; echo "RWB-BLOCKED: $*" >&2; exit 77; }
probe() {  # probe <label> <command...>: record whether a kernel facility is available
  if "${@:2}" >/dev/null 2>&1; then echo "probe $1: ok"; else echo "probe $1: denied (exit $?)"; fi
}

preflight() {
  echo "arch: $(uname -m) kernel: $(uname -r) user: $(id -un)"
  test "$(uname -m)" = aarch64 || blocked "tarball pin is for aarch64-linux, host is $(uname -m)"
  # Facilities guix-daemon's default build sandbox and `guix shell --container` use. Recorded
  # as evidence only; the canary build below decides.
  probe userns unshare --user --map-root-user true
  probe mount+pid+net+ipc+uts-ns unshare --mount --pid --fork --net --ipc --uts true
  if command -v guix >/dev/null 2>&1 || test -e /gnu/store; then
    echo "existing Guix installation found in this container"; guix --version | head -1 || true
  fi
  [[ "${RWB_GUIX_SHA256:-}" =~ ^[0-9a-f]{64}$ ]] ||
    blocked "no verified SHA-256 for ${RWB_GUIX_URL:-<unset url>} (pass adapter option guix_binary_sha256)"
  code=0
  curl -fsSIL --max-time 60 -o /dev/null "$RWB_GUIX_URL" || code=$?
  [ "$code" = 0 ] || blocked "binary tarball unreachable from container: curl exit $code for $RWB_GUIX_URL"
  code=0
  curl -fsSL --max-time 60 -o /dev/null "https://codeberg.org/guix/guix.git/info/refs?service=git-upload-pack" || code=$?
  [ "$code" = 0 ] || blocked "channel repository unreachable (time-machine needs it): curl exit $code"
}

install_guix() {
  [[ "${RWB_GUIX_SHA256:-}" =~ ^[0-9a-f]{64}$ ]] || blocked "no verified SHA-256 (run preflight first)"
  tmp=$(mktemp -d /var/tmp/rwb-guix.XXXXXX)
  curl -fsSL --retry 3 -o "$tmp/guix.tar.xz" "$RWB_GUIX_URL"
  echo "$RWB_GUIX_SHA256  $tmp/guix.tar.xz" | sha256sum -c -
  tar --warning=no-timestamp -xf "$tmp/guix.tar.xz" -C "$tmp"
  test ! -e /gnu && test ! -e /var/guix || { echo "refusing to overwrite an existing /gnu or /var/guix" >&2; exit 1; }
  mv "$tmp/gnu" /gnu
  mv "$tmp/var/guix" /var/guix
  rm -rf "$tmp"
  root_guix=/var/guix/profiles/per-user/root/current-guix
  mkdir -p /root/.config/guix
  ln -sfn "$root_guix" /root/.config/guix/current
  ln -sf "$root_guix/bin/guix" /usr/local/bin/guix
  getent group guixbuild >/dev/null || groupadd --system guixbuild
  for i in $(seq -w 1 10); do
    id "guixbuilder$i" >/dev/null 2>&1 ||
      useradd -g guixbuild -G guixbuild -d /var/empty -s "$(command -v nologin)" \
        -c "Guix build user $i" --system "guixbuilder$i"
  done
  # Daemon of THIS container only, started directly (no init system in the container).
  # shellcheck disable=SC2086
  setsid "$root_guix/bin/guix-daemon" --build-users-group=guixbuild ${RWB_GUIX_DAEMON_FLAGS:-} \
    > /var/log/guix-daemon.log 2>&1 < /dev/null &
  for _ in $(seq 1 100); do test -S /var/guix/daemon-socket/socket && break; sleep 0.1; done
  test -S /var/guix/daemon-socket/socket || { cat /var/log/guix-daemon.log >&2; exit 1; }
  for key in "$root_guix"/share/guix/{ci.guix.gnu.org,bordeaux.guix.gnu.org}.pub; do
    guix archive --authorize < "$key"
  done
  guix --version | head -1
  echo "daemon flags: ${RWB_GUIX_DAEMON_FLAGS:-<none: default build sandbox>}"
}

canary() {
  log=$(mktemp /tmp/rwb-guix-canary.XXXXXX)
  if guix build --no-grafts -e '(@ (gnu packages base) hello)' > "$log" 2>&1 &&
     guix build --no-grafts -e '(begin (use-modules (guix gexp))
                                       (computed-file "rwb-canary" #~(mkdir #$output)))' >> "$log" 2>&1; then
    cat "$log"; return 0
  fi
  cat "$log" >&2
  if grep -qiE 'cannot (create|set up).*namespace|unshare|clone.*Operation not permitted|cannot change root|chroot' "$log"; then
    blocked "guix-daemon could not set up its build sandbox in this container (see log above); --disable-chroot was not used"
  fi
  if grep -qiE 'could not resolve host|connection (refused|timed out)|unable to download|substitute.*(unavailable|failed)' "$log"; then
    blocked "substitute download failed from this container (see log above)"
  fi
  exit 1
}

case "${1:-}" in
preflight) preflight ;;
install) install_guix ;;
canary) canary ;;
*) echo "usage: provision.sh preflight|install|canary" >&2; exit 2 ;;
esac
