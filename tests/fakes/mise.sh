#!/bin/sh
echo "$*" >>"$REVIEW_FIXTURE/mise.log"
# The Python the test found on its own PATH before narrowing stack's (see tests/support/python.rs).
py=${REVIEW_PYTHON:-python3}
# Which file ran, however PATH named it: a relative entry is found from the working directory.
case $0 in */*) self=${0%/*} ;; *) self=. ;; esac
echo "$(cd "$self" && pwd -P)/${0##*/} $1 $2" >>"$REVIEW_FIXTURE/mise-self.log"
# A supervisor stand-in that runs until killed, started only when none runs. `$1` records who
# started it: `mise x` in stack's own session, or a request client in its killable group.
supervise() {
  if test -f "$REVIEW_FIXTURE/supervisor-process" && ! kill -0 "$(cut -d' ' -f1 "$REVIEW_FIXTURE/supervisor-pid" 2>/dev/null)" 2>/dev/null; then
    sleep 60 </dev/null >/dev/null 2>&1 &
    echo "$! $1" >"$REVIEW_FIXTURE/supervisor-pid"
  fi
}
case "$1 $2" in
  'latest '*)
    # Where resolution ran and the only configuration it could see.
    { echo "dir=$(pwd -P) trusted=${MISE_TRUSTED_CONFIG_PATHS-unset} no_config=${MISE_NO_CONFIG-unset}"; cat .config/mise/conf.d/stack.toml 2>/dev/null; } >>"$REVIEW_FIXTURE/latest.log"
    if test -f "$REVIEW_FIXTURE/latest-empty"; then exit 0; fi
    v=${2#*@}; if test "$v" = "$2"; then v=1.0.0; fi
    case "$2" in python@3.13) v=3.13.16 ;; postgres@17) v=17.11 ;; redis@8) v=8.2.1 ;; rust@1.93) v=1.93.1 ;; esac
    echo "$v" ;;
  'which pitchfork') echo "$REVIEW_FIXTURE/bin/pitchfork" ;;
  'ls --json')
    # Installed releases, asked in a scratch root: where it ran and the only configuration it saw.
    { echo "dir=$(pwd -P) args=$*"; cat .config/mise/conf.d/stack.toml 2>/dev/null; } >>"$REVIEW_FIXTURE/ls.log"
    if test "${3-}" = fnox; then
      { echo "dir=$(pwd -P) args=$*"; cat .config/mise/conf.d/stack.toml 2>/dev/null; } >>"$REVIEW_FIXTURE/fnox-ls.log"
      if test -f "$REVIEW_FIXTURE/ls.json"; then cat "$REVIEW_FIXTURE/ls.json"; else echo '[]'; fi
    else
      # Every release the configuration names, installed when an install recorded it (or a
      # test listed it) in `installed`; `ls.json` stands for fnox's rows when a test gives them.
      "$py" -c '
import json, os, sys, tomllib
fixture = sys.argv[1]
try:
    tools = tomllib.load(open(".config/mise/conf.d/stack.toml", "rb")).get("tools", {})
except FileNotFoundError:
    tools = {}
try:
    installed = set(open(os.path.join(fixture, "installed")).read().split())
except FileNotFoundError:
    installed = set()
out = {}
for tool, value in tools.items():
    for v in value if isinstance(value, list) else [value]:
        v = v["version"] if isinstance(v, dict) else v
        out.setdefault(tool, []).append({"version": v, "requested_version": v, "install_path": os.path.join(fixture, "installs", tool, v), "installed": f"{tool}@{v}" in installed, "active": False})
if os.path.exists(os.path.join(fixture, "ls.json")):
    out["fnox"] = json.load(open(os.path.join(fixture, "ls.json")))
print(json.dumps(out))' "$REVIEW_FIXTURE"
    fi ;;
  'skills ls') echo '[]' ;;
  'lock --platform')
    # Artifact locking in a scratch root: what it was given, then the lock a test supplies.
    { echo "dir=$(pwd -P) args=$*"; cat .config/mise/conf.d/stack.toml; echo '--- seed'; cat .config/mise/mise.lock; } >>"$REVIEW_FIXTURE/lock.log"
    if test -f "$REVIEW_FIXTURE/mise-lock.toml"; then cp "$REVIEW_FIXTURE/mise-lock.toml" .config/mise/mise.lock; fi ;;
  'install --locked'|'install --yes')
    # The rendered lock as the install saw it.
    if test -f .config/mise/mise.lock; then cp .config/mise/mise.lock "$REVIEW_FIXTURE/rendered-at-install"; fi
    if test "$2" = --locked && test -f "$REVIEW_FIXTURE/install-locked-fail"; then cat "$REVIEW_FIXTURE/install-locked-fail" >&2; exit 1; fi
    # Record what was installed: the named tools (every one when none is named), each at every
    # version the configuration gives it.
    shift 1
    "$py" -c '
import os, sys, tomllib
try:
    config = tomllib.load(open(".config/mise/conf.d/stack.toml", "rb"))
except FileNotFoundError:
    config = {}
tools = {k: v if isinstance(v, list) else [v] for k, v in config.get("tools", {}).items()}
# A preset service installs its tool at the version the daemon names.
for daemon in config.get("daemons", {}).values():
    if "preset" in daemon and "version" in daemon:
        tools.setdefault(daemon["preset"], []).append(daemon["version"])
named = [a for a in sys.argv[2:] if not a.startswith("-")] or list(tools)
with open(os.path.join(sys.argv[1], "installed"), "a") as f:
    for tool in named:
        for v in tools.get(tool, []):
            f.write("%s@%s\n" % (tool, v["version"] if isinstance(v, dict) else v))' "$REVIEW_FIXTURE" "$@" ;;
  'version ')
    echo "no_config=${MISE_NO_CONFIG-unset}" >>"$REVIEW_FIXTURE/version.log"
    echo 'mise WARN  mise version 2099.1.1 available' >&2
    if test -f "$REVIEW_FIXTURE/mise-version"; then cat "$REVIEW_FIXTURE/mise-version"; else echo '2026.10.3 macos-arm64 (2026-10-05)'; fi ;;
  'env --json')
    if test -f "$REVIEW_FIXTURE/fail-env-after-start" && test -f "$REVIEW_FIXTURE/started"; then exit 1; fi
    # The first lookup after a start takes `slow-env-after-start` seconds; later ones answer at once.
    if test -f "$REVIEW_FIXTURE/slow-env-after-start" && test -f "$REVIEW_FIXTURE/started" && ! test -f "$REVIEW_FIXTURE/env-slowed"; then
      touch "$REVIEW_FIXTURE/env-slowed"; sleep "$(cat "$REVIEW_FIXTURE/slow-env-after-start")"
    fi
    cat "$REVIEW_FIXTURE/env.json" ;;
  'daemons --json')
    if test -f "$REVIEW_FIXTURE/fail-query-after-one" && test -f "$REVIEW_FIXTURE/started"; then
      if test -f "$REVIEW_FIXTURE/query-observed"; then exit 1; fi
      touch "$REVIEW_FIXTURE/query-observed"
    fi
    if test -f "$REVIEW_FIXTURE/fail-query"; then echo 'supervisor unavailable' >&2; exit 1; fi
    # A supervised listener (see `daemons start`) is reported with its real PID while alive.
    if test -f "$REVIEW_FIXTURE/listen-port" && kill -0 "$(cat "$REVIEW_FIXTURE/pf-tracked-pid" 2>/dev/null)" 2>/dev/null; then
      "$py" -c 'import json,sys; pid=int(sys.argv[2]); d=json.load(open(sys.argv[1])); [e.__setitem__("pid", pid) for e in d if e.get("status") in ("running", "starting")]; print(json.dumps(d))' "$REVIEW_FIXTURE/daemons.json" "$(cat "$REVIEW_FIXTURE/pf-tracked-pid")"
    else
      cat "$REVIEW_FIXTURE/daemons.json"
    fi ;;
  'daemons start')
    supervise request
    # A request client that would launch a service later, after the request gave up. Like a
    # real client it keeps SIGINT's default action (a plain `&` job of sh would ignore it).
    if test -f "$REVIEW_FIXTURE/late-client"; then
      "$py" -c 'import signal,sys,time; signal.signal(signal.SIGINT, signal.SIG_DFL); open(sys.argv[2], "w").close(); time.sleep(2); open(sys.argv[1], "w")' "$REVIEW_FIXTURE/late-launch" "$REVIEW_FIXTURE/client-waiting" >/dev/null 2>&1 &
    fi
    if test -f "$REVIEW_FIXTURE/slow-start"; then sleep "$(cat "$REVIEW_FIXTURE/slow-start")"; fi
    # A supervised listener on the configured port, ended by `daemons stop` like a real daemon.
    if test -f "$REVIEW_FIXTURE/listen-port" && ! kill -0 "$(cat "$REVIEW_FIXTURE/pf-tracked-pid" 2>/dev/null)" 2>/dev/null; then
      "$py" -c 'import socket,sys,time; s=socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1); s.bind(("127.0.0.1", int(sys.argv[1]))); s.listen(); time.sleep(600)' "$(cat "$REVIEW_FIXTURE/listen-port")" </dev/null >/dev/null 2>&1 &
      echo $! >"$REVIEW_FIXTURE/pf-tracked-pid"
      sleep 0.3
    fi
    if test -f "$REVIEW_FIXTURE/daemons-started.json"; then
      cp "$REVIEW_FIXTURE/daemons-started.json" "$REVIEW_FIXTURE/daemons.json"
      touch "$REVIEW_FIXTURE/started"
    fi
    if test -f "$REVIEW_FIXTURE/fail-start"; then cat "$REVIEW_FIXTURE/fail-start" >&2; echo 'start failed' >&2; exit 1; fi ;;
  'run --skip-deps')
    # `mise run --skip-deps --no-timings <task> -- <args>`: echo what the task would receive.
    shift 3; task=$1; shift 2; printf '%s|' "$task" "$@"
    # Held by a test while it edits and compiles the project: after stack planned the task,
    # before mise reads its configuration. Bounded at 30 seconds.
    if test -f "$REVIEW_FIXTURE/run-gate"; then
      touch "$REVIEW_FIXTURE/run-waiting"; n=0
      while test -f "$REVIEW_FIXTURE/run-gate" && test $n -lt 600; do sleep 0.05; n=$((n + 1)); done
    fi
    # The configuration mise loads, as mise picks it: the project file the override names
    # (from the working directory, or absolute), else the global one.
    config=$MISE_OVERRIDE_CONFIG_FILENAMES
    test -f "$config" || config=$MISE_GLOBAL_CONFIG_FILE
    echo "config=$config root=${MISE_GLOBAL_CONFIG_ROOT-unset} dir=$(pwd -P)" >>"$REVIEW_FIXTURE/run.log"
    # The generated config the task ran from: its definition as stack planned it.
    cp "$config" "$REVIEW_FIXTURE/run-config" 2>/dev/null
    # The provider lock mise finds beside it, if any.
    rm -f "$REVIEW_FIXTURE/run-lock"
    cp "${config%/conf.d/*}/mise.lock" "$REVIEW_FIXTURE/run-lock" 2>/dev/null
    # Like mise, link the configuration it loaded under its state directory, when a test asks.
    if test -f "$REVIEW_FIXTURE/track-configs"; then
      mkdir -p "$MISE_STATE_DIR/tracked-configs"
      ln -sf "$config" "$MISE_STATE_DIR/tracked-configs/$(printf %s "$config" | cksum | cut -d' ' -f1)"
    fi
    if test "$task" = hang; then sleep 30; fi
    if test "$task" = fail; then exit 3; fi
    # A task whose tools mise could not install: what mise printed, as a test gives it.
    if test "$task" = refused; then cat "$REVIEW_FIXTURE/run-refusal" >&2; exit 1; fi
    # A task that shows the environment it was given, on both streams.
    if test "$task" = showenv; then echo; env | sort; env | sort >&2; fi ;;
  'x --')
    # The supervisor stack starts on its own: which mise it is told to run daemons with.
    echo "$*|${PITCHFORK_MISE_BIN-unset}" >>"$REVIEW_FIXTURE/supervisor-start.log"
    # A start that hangs for `x-hang` seconds (its PID recorded to end it) or fails, as asked.
    if test -f "$REVIEW_FIXTURE/x-hang"; then echo $$ >"$REVIEW_FIXTURE/x-hang-pid"; exec sleep "$(cat "$REVIEW_FIXTURE/x-hang")"; fi
    if test -f "$REVIEW_FIXTURE/x-fail"; then echo 'cannot start the supervisor' >&2; exit 1; fi
    supervise detached ;;
  'daemons logs')
    if test -f "$REVIEW_FIXTURE/logs.txt"; then cat "$REVIEW_FIXTURE/logs.txt"; else echo "Error: Daemon $4 not found" >&2; exit 1; fi ;;
  'daemons stop')
    if test -f "$REVIEW_FIXTURE/fail-stop"; then echo 'cannot stop' >&2; exit 1; fi
    if test -f "$REVIEW_FIXTURE/pf-tracked-pid"; then kill "$(cat "$REVIEW_FIXTURE/pf-tracked-pid")" 2>/dev/null; fi
    # A stopped supervised listener is reported stopped, like a real daemon.
    if test -f "$REVIEW_FIXTURE/listen-port"; then
      "$py" -c 'import json,sys; d=json.load(open(sys.argv[1])); [ (e.__setitem__("status", "stopped"), e.pop("pid", None)) for e in d ]; json.dump(d, open(sys.argv[1], "w"))' "$REVIEW_FIXTURE/daemons.json"
    fi ;;
esac
