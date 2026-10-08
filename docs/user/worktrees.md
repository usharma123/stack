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
shell, the first failure's `exit` would close that shell.

```bash
#!/usr/bin/env bash
# usage: isolated-stack.sh <stack version> <checkout> <command> [args...]
set -u
[ $# -ge 3 ] || { echo "usage: $0 <stack version> <checkout> <command> [args...]" >&2; exit 2; }
version=$1 checkout=$2
shift 2
iso=$(mktemp -d /tmp/stack-iso.XXXXXX) || exit 1   # short: the supervisor socket must fit 104 bytes on macOS
echo "isolated root: $iso" >&2
keep() { echo "$1; keeping $iso" >&2; exit "${2:-1}"; }
trap 'echo "interrupted; cleaning up" >&2' INT TERM   # stop the command, not the cleanup

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

npm install --prefix "$iso/cli" --save-exact "@ushawarma/stack@$version" || keep "npm install failed"
export PATH="$iso/cli/node_modules/.bin:$PATH"
stack setup || keep "stack setup failed"
# Every step that needs the checkout runs in a subshell, so this shell never enters it.
(cd "$checkout" && stack doctor && stack compile) || keep "doctor or compile failed; nothing started"

# The command runs only after up verifies, but cleanup runs either way: a failed up
# (start_failed, not_ready) can leave services running.
(cd "$checkout" && stack up --ttl 30m && "$@")
status=$?

(cd "$checkout" && stack down) || keep "stack down failed; services may still run, see stack status"
(cd "$checkout" && stack exec -- pitchfork supervisor stop) || keep "supervisor stop failed"
(cd "$checkout" && stack exec -- pitchfork supervisor status --json) >"$iso/supervisor-after" ||
  keep "supervisor status failed"
node -e 'process.exit(JSON.parse(require("fs").readFileSync(process.argv[1], "utf8")).status === "down" ? 0 : 1)' \
  "$iso/supervisor-after" || keep "the supervisor is not confirmed down"
(cd "$checkout" && stack gc) || keep "stack gc failed"
global_receipt >"$iso/global-after" || keep "cannot read a global provider file"
cmp -s "$iso/global-before" "$iso/global-after" || keep "a global provider file changed; the run was not isolated"
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
- An interrupt stops the command, and cleanup still runs. Interrupting cleanup makes that step
  fail, so the root is kept.
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
