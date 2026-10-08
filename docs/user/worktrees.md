# Worktree and agent-sandbox integration

Stack manages a checkout that already exists; it creates none and deletes none. Tools that
create and remove checkouts (Git worktrees, agent sandboxes) call Stack from their hooks:
`stack compile --locked && stack up` when a checkout appears, `stack down` before it goes.
Stopping before removal matters: Stack refuses to stop a deleted project's daemons by name
(see [guarantees](guarantees.md)), so a checkout removed while its services run leaves them
running until someone stops the recorded supervisor daemon explicitly.

## Worktrunk

Verified against Worktrunk 0.80.0 (`wt`). Project hooks live in `.config/wt.toml`; both of
these block, and a failing hook aborts the operation:

```toml
# .config/wt.toml
[pre-start]
stack = "stack compile --locked && stack up"

[pre-remove]
stack = "stack down"
```

- `pre-start` runs in the new worktree after it is created and before background creation
  hooks and `--execute`. If it fails before launch (`lock_outdated`, `port_conflict`,
  `install_failed`), the worktree exists and nothing started. If it fails during start or
  verification (`start_failed`, `not_ready`), services may be running under a launch record:
  check `stack status` and run `stack down` before abandoning the worktree. Either way, fix
  the cause and run the two commands by hand.
- `pre-remove` runs in the worktree about to be removed. A failing `stack down`
  (`stop_unconfirmed`, a supervisor query failure) aborts the removal, so services are never
  orphaned by `wt remove`. `--no-hooks` skips both hooks; then stop services yourself first.
- Hooks run at the worktree root with the ambient environment; Worktrunk passes context on
  stdin and through templates, not as exported variables, so the commands need no arguments.

Each worktree is a separate checkout to Stack: its own ports, data directories, identity
tokens and session. A shared `stack.lock` in the repository keeps every worktree on the same
exact versions, and `compile --locked` fails rather than drift.

## Other tools

The same two commands fit any tool with blocking create and remove hooks. Run `stack down`
from a hook that can still fail the removal; a hook that only runs after deletion is too late
for Stack to act on. Recipes for workz and GitGrove have not been verified.

## Isolated runs

An evaluation, a sandboxed agent or a throwaway check can run Stack without touching the
user's installation by pointing every store at one disposable directory. Keep `HOME`, so Git
and SSH configuration still work. Set all three Pitchfork directories: Pitchfork reads its
config from `$HOME/.config/pitchfork` whatever `XDG_CONFIG_HOME` says, registers every project
Stack starts there, and on macOS also keeps state under `~/.local/state`.

Save this as a file and run it with bash, for example
`bash isolated-stack.sh 0.1.18 ~/src/app stack exec -- pytest`. Pasted into an interactive
shell, the first failure's `exit` would close that shell. It needs `node`, `npm` and
`python3`; Python runs each step so that an interrupt reaches it.

```bash
#!/usr/bin/env bash
# usage: isolated-stack.sh <stack version> <checkout> <command> [args...]
set -u
[ $# -ge 3 ] || { echo "usage: $0 <stack version> <checkout> <command> [args...]" >&2; exit 2; }
for tool in node npm python3; do
  command -v "$tool" >/dev/null || { echo "$tool is required" >&2; exit 2; }
done
version=$1 checkout=$2
shift 2
grace=10   # seconds a cancelled step has to exit before SIGKILL
iso=$(mktemp -d /tmp/stack-iso.XXXXXX) || exit 1   # short: the supervisor socket must fit 104 bytes on macOS
echo "isolated root: $iso" >&2
mkdir "$iso/steps" || exit 1
interrupt='' late='' signalled='' phase=setup n=0
keep() { echo "$1; keeping $iso" >&2; exit "${interrupt:-${2:-1}}"; }

# This script signals no process itself. Its trap records the first signal and appends a
# cancel request for the running step, whose owner passes it on.
on_signal() {
  echo "interrupted by SIG$1" >&2
  interrupt=${interrupt:-$2} signalled=1
  [ "$phase" != cleanup ] || late=1
  echo "$1" >>"$iso/steps/$n.cancel"
}
trap 'on_signal INT 130' INT
trap 'on_signal TERM 143' TERM
trap 'on_signal HUP 129' HUP

# A step's owner leads a new session and process group, which the step joins, so the group ID
# is the owner's own PID and cannot name another process while the owner runs. On the first
# cancel request it sends that signal to its group; when the step exits, `grace` seconds pass
# or a second request arrives, SIGKILL ends whatever is left in the group, the owner included.
# A process that left the group, like a daemon, is never signalled. The step's exit status
# (128 + the signal for one a signal ended) goes to steps/<n>.status.
owner='
import os, signal, subprocess, sys, time
prefix, grace, cwd, argv = sys.argv[1], float(sys.argv[2]), sys.argv[3], sys.argv[4:]
script = os.getppid()
os.setsid()
group = os.getpgrp()

def requests():
    try:
        with open(prefix + ".cancel") as f:
            return f.read().split()
    except FileNotFoundError:
        return []

def finish(code, sweep):
    with open(prefix + ".status", "w") as f:
        f.write("%d\n" % code)
    if sweep:
        os.killpg(group, signal.SIGKILL)
    sys.exit(0)

if requests():
    finish(128 + getattr(signal, "SIG" + requests()[0]), False)
for s in (signal.SIGINT, signal.SIGQUIT):
    signal.signal(s, signal.SIG_DFL)  # bash starts background jobs with these ignored
try:
    step = subprocess.Popen(argv, cwd=cwd, close_fds=False)
except OSError as e:
    print("%s: %s" % (argv[0], e.strerror), file=sys.stderr)
    finish(127 if isinstance(e, FileNotFoundError) else 126, False)
for s in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP, signal.SIGQUIT):
    signal.signal(s, signal.SIG_IGN)  # the owner acts on requests, not signals
sent, kill_at = 0, None
while step.poll() is None:
    asked = requests() or (["TERM"] if os.getppid() != script else [])  # the script died
    if asked and not sent:
        os.killpg(group, getattr(signal, "SIG" + asked[0]))
        kill_at = time.monotonic() + grace
    if len(asked) > 1:
        kill_at = time.monotonic()
    sent = len(asked)
    if kill_at is not None and time.monotonic() >= kill_at:
        finish(128 + signal.SIGKILL, True)
    time.sleep(0.05)
finish(step.returncode if step.returncode >= 0 else 128 - step.returncode, sent > 0)
'
# usage: step <directory> <command> [args...]
step() {
  n=$((n + 1))
  if [ -n "$interrupt" ]; then
    case $phase in
      setup) keep "interrupted; nothing started" ;;
      run) return "$interrupt" ;;
      cleanup) [ -z "$late" ] || keep "cleanup was interrupted" ;;
    esac
  fi
  python3 -I -c "$owner" "$iso/steps/$n" "$grace" "$@" <&0 &
  local pid=$!
  # A trapped signal ends wait early; wait again until the owner has exited.
  until signalled=; wait "$pid" 2>/dev/null; [ -z "$signalled" ]; do :; done
  return "$(cat "$iso/steps/$n.status" 2>/dev/null || echo 1)"
}

# A missing file is recorded as absent. A file that exists but cannot be read fails the receipt.
global_receipt() {
  node -e '
    const fs = require("fs"), { createHash } = require("crypto");
    for (const f of process.argv.slice(1)) {
      let data;
      try { data = fs.readFileSync(f); } catch (e) {
        if (e.code !== "ENOENT") throw e;
        console.log(`absent ${f}`);
        continue;
      }
      console.log(`${createHash("sha256").update(data).digest("hex")}  ${f}`);
    }' ~/.config/pitchfork/config.toml ~/.local/state/pitchfork/state.toml ~/.config/mise/config.toml
}
global_receipt >"$iso/global-before" || keep "cannot read a global provider file"

for v in $(compgen -e); do
  case $v in MISE_*|__MISE*|PITCHFORK_*|STACK_*|npm_config_*|NPM_CONFIG_*) unset "$v" ;; esac
done
export STACK_STATE_DIR=$iso/stack/state STACK_DATA_DIR=$iso/stack/data STACK_CACHE_DIR=$iso/stack/cache
export MISE_DATA_DIR=$iso/mise/data MISE_CACHE_DIR=$iso/mise/cache MISE_STATE_DIR=$iso/mise/state \
  MISE_CONFIG_DIR=$iso/mise/config MISE_GLOBAL_CONFIG_FILE=$iso/mise/config/config.toml
export PITCHFORK_CONFIG_DIR=$iso/pf/config PITCHFORK_STATE_DIR=$iso/pf/state PITCHFORK_LOGS_DIR=$iso/pf/logs
export XDG_CONFIG_HOME=$iso/xdg/config XDG_DATA_HOME=$iso/xdg/data XDG_CACHE_HOME=$iso/xdg/cache \
  XDG_STATE_HOME=$iso/xdg/state UV_CACHE_DIR=$iso/uv-cache
export npm_config_cache=$iso/npm/cache npm_config_userconfig=$iso/npm/npmrc \
  npm_config_globalconfig=$iso/npm/global-npmrc npm_config_prefix=$iso/npm/prefix

step "$iso" npm install --prefix "$iso/cli" --save-exact "@ushawarma/stack@$version" || keep "npm install failed"
export PATH="$iso/cli/node_modules/.bin:$PATH"
step "$iso" stack setup || keep "stack setup failed"
{ step "$checkout" stack doctor && step "$checkout" stack compile; } || keep "doctor or compile failed; nothing started"
[ -z "$interrupt" ] || keep "interrupted; nothing started"

# The command runs only after up verifies, but cleanup runs either way: a failed or
# interrupted up (start_failed, not_ready) can leave services running.
phase=run
step "$checkout" stack up --ttl 30m && step "$checkout" "$@"
status=$?

phase=cleanup
step "$checkout" stack down || keep "stack down failed; services may still run, see stack status"
step "$checkout" stack exec -- pitchfork supervisor stop || keep "supervisor stop failed"
step "$checkout" stack exec -- pitchfork supervisor status --json >"$iso/supervisor-after" ||
  keep "supervisor status failed"
node -e 'process.exit(JSON.parse(require("fs").readFileSync(process.argv[1], "utf8")).status === "down" ? 0 : 1)' \
  "$iso/supervisor-after" || keep "the supervisor is not confirmed down"
step "$checkout" stack gc || keep "stack gc failed"
[ -z "$late" ] || keep "cleanup was interrupted"
global_receipt >"$iso/global-after" || keep "cannot read a global provider file"
cmp -s "$iso/global-before" "$iso/global-after" || keep "a global provider file changed; the run was not isolated"
[ -z "$interrupt" ] || keep "interrupted; everything it started is stopped"
[ "$status" = 0 ] || keep "the command exited $status; everything it started is stopped" "$status"
rm -rf -- "$iso"
```

The script deletes its root only after `stack down` confirmed this checkout's services
stopped, Pitchfork reports the isolated supervisor `down`, `stack gc` succeeded, the global
receipts match and the command exited 0. Every other outcome exits nonzero and keeps the root,
with its receipts and supervisor logs, at the path printed on the first line. It checks each
step's exit status itself, because `set -e` ignores a failure in an `&&` list unless it is
the last command.

- Clean up only that exact path, after reading it. A kept root whose services may still run
  needs `stack down` from the checkout, then the supervisor stop, run with `iso` set to that
  path and the script's exports. Never delete by pattern, such as `/tmp/stack-iso.*`, and
  never decide a directory or supervisor is yours from `pgrep` or `ps`. Another run's looks
  the same.
- SIGINT, SIGTERM or SIGHUP, whether sent to the script alone or to its whole process group as
  Ctrl-C does, reaches the running step's group. If nothing has been started yet, no later
  step starts. From `stack up` on, the step is cancelled and cleanup still runs. A signal
  during cleanup cancels the remaining cleanup. Either way the script exits 128 plus the first
  signal's number and keeps the root, even if every later step succeeds.
- Each step runs in a session of its own, with the script's input and output but no
  controlling terminal, so it cannot prompt through `/dev/tty`. The signal and the SIGKILL
  reach only processes still in the step's process group. `stack exec --timeout` moves its
  command to a group of its own and passes signals on, but not SIGKILL: a command there that
  ignores the signal outlives the step.
- If the script itself dies, for example from SIGKILL, the running step gets SIGTERM and then
  SIGKILL, but cleanup does not run: stop the kept root's services as described above.
- For several checkouts, run `stack down` in each before stopping the supervisor.
- A `mise` already on `PATH` is used as a binary with the isolated stores. Leave it off `PATH`
  and `stack setup` downloads the pinned release into `$STACK_DATA_DIR/bin` instead.
- The empty npm user and global configs drop registry settings and credentials from
  `~/.npmrc` and npm's own `etc/npmrc`; add only what the run needs.
- On Linux, Pitchfork's default state follows `XDG_STATE_HOME`: add that real path to
  `global_receipt` if it is set.

The script's successful service run was verified on macOS arm64 with the published Stack
0.1.18 package and Pitchfork 2.29.0, including confirmed shutdown and unchanged global
receipts. Failure handling is tested with stand-in commands.

[All docs](../README.md)
