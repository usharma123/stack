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
Stack starts there, and on macOS also keeps state under `~/.local/state`. In bash:

```sh
iso=$(mktemp -d /tmp/stack-iso.XXXXXX)   # short: the supervisor socket must fit 104 bytes on macOS
global_receipt() {
  for f in ~/.config/pitchfork/config.toml ~/.local/state/pitchfork/state.toml ~/.config/mise/config.toml; do
    shasum -a 256 "$f" 2>/dev/null || echo "absent $f"
  done
}
global_receipt >"$iso/global-before"
for v in $(compgen -e); do
  case $v in MISE_*|__MISE*|PITCHFORK_*|STACK_*) unset "$v" ;; esac
done
export STACK_STATE_DIR=$iso/stack/state STACK_DATA_DIR=$iso/stack/data STACK_CACHE_DIR=$iso/stack/cache
export MISE_DATA_DIR=$iso/mise/data MISE_CACHE_DIR=$iso/mise/cache MISE_STATE_DIR=$iso/mise/state \
  MISE_CONFIG_DIR=$iso/mise/config MISE_GLOBAL_CONFIG_FILE=$iso/mise/config/config.toml
export PITCHFORK_CONFIG_DIR=$iso/pf/config PITCHFORK_STATE_DIR=$iso/pf/state PITCHFORK_LOGS_DIR=$iso/pf/logs
export XDG_CONFIG_HOME=$iso/xdg/config XDG_DATA_HOME=$iso/xdg/data XDG_CACHE_HOME=$iso/xdg/cache \
  XDG_STATE_HOME=$iso/xdg/state UV_CACHE_DIR=$iso/uv-cache
export npm_config_cache=$iso/npm/cache npm_config_userconfig=$iso/npm/npmrc npm_config_prefix=$iso/npm/prefix
```

Then install Stack into the directory rather than globally, work, and clean up:

```sh
npm install --prefix "$iso/cli" --save-exact @ushawarma/stack@<version>
export PATH="$iso/cli/node_modules/.bin:$PATH"
stack setup
cd <checkout>
stack doctor                           # pitchfork_socket shows the socket path fits
stack compile && stack up --ttl 30m    # ... and the work
stack down                             # in every checkout you started
stack exec -- pitchfork supervisor stop   # the isolated supervisor, from a compiled checkout
stack gc
global_receipt | diff "$iso/global-before" - && rm -rf "$iso"
```

- A `mise` already on `PATH` is used as a binary with the isolated stores. Leave it off `PATH`
  and `stack setup` downloads the pinned release into `$STACK_DATA_DIR/bin` instead.
- The empty npm user config drops registry credentials from `~/.npmrc`; add only what the run
  needs.
- On Linux, Pitchfork's default state follows `XDG_STATE_HOME`: add that real path to
  `global_receipt` if it is set.
- Keep the receipts and the directory of a run that changed one; it was not isolated.

Verified on macOS arm64 with Stack 0.1.18 and Pitchfork 2.29.0.

[All docs](../README.md)
