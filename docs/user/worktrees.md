# Worktrees and sandboxes

Each Git worktree is its own checkout to Stack, with its own ports, data, identity tokens and
session. A shared `stack.lock` keeps every worktree on the same exact versions.

Stack never creates or deletes checkouts. Tools that do (worktree managers, agent sandboxes)
should call it from their hooks. Ready-made recipes follow for [Worktrunk](#worktrunk),
[Claude Code](#claude-code) and [plain `git worktree`](#plain-git-worktree):

- when a checkout appears: `stack compile --locked && stack up`
- before it is removed: `stack down`
- after it is removed, from anywhere: `stack gc --reclaim-data` releases its ports (plain
  `stack gc` does that too) and removes its databases. Without the flag the data is kept and
  reported, so a checkout removed by mistake loses nothing until you ask.

To give a new checkout a ready database instead of an empty one, declare init tasks
(`init = ["migrate", "seed"]`), which `up` runs once per new database, or save a snapshot in
one checkout and start the others from it: `stack compile --locked && stack up --from-snapshot
seeded`. See [service data](data.md).

Stopping first matters. Stack will not stop a deleted checkout's services by name, so a
checkout removed while its services run leaves them running until you stop them yourself, or
until `stack gc --stop-orphans` confirms each one is still the recorded process and stops it
(see [`gc`](commands.md#gc) and [stopping orphans](commands.md#stopping-orphans)).

Checkouts do not wait on each other: the cleanup pass at the start of `stack up` skips any
other checkout's session that another command holds, and never fails because of one. Run
`stack gc --dry-run` to see what a pass would reclaim.

`stack down` keeps the checkout's service data, in case the checkout comes back. To remove it
in the same step, use `stack down --purge` in the remove hook (see [`down`](commands.md#down)).

## Moving a checkout

A moved or renamed worktree keeps its ports and data once you run `stack mv <old> <new>` with
its services stopped (see [`mv`](commands.md#mv)). Stop them first, then move:

```sh
stack -C ../feature down
git worktree move ../feature ../feature-2
stack mv ../feature ../feature-2
```

`stack ps` lists every checkout Stack has records for, and how much data each holds.

## Worktrunk

For [Worktrunk](https://github.com/max-sixty/worktrunk) (`wt`, tested with 0.80.0), add both
hooks to `.config/wt.toml`. Both block, and a failing hook aborts the operation:

```toml
# .config/wt.toml
[pre-start]
stack = "stack compile --locked && stack up"
# or, from a snapshot saved on main (see data.md):
# stack = "stack compile --locked && stack up --from-snapshot seeded"

[pre-remove]
stack = "stack down"
```

- If `pre-start` fails before anything starts (`lock_outdated`, `port_conflict`,
  `install_failed`), the worktree exists with nothing running. If it fails while starting
  (`start_failed`, `not_ready`), services may be running: check `stack status`, and run
  `stack down` before abandoning the worktree.
- A failing `stack down` aborts `wt remove`, so services are never orphaned. `--no-hooks`
  skips both hooks, so stop services yourself first.

## Claude Code

Start the stack when a session starts, with a lease so it stops on its own later. In
`.claude/settings.json`:

```json
{
  "hooks": {
    "SessionStart": [
      {
        "matcher": "startup",
        "hooks": [
          { "type": "command", "command": "stack -C \"$CLAUDE_PROJECT_DIR\" up --ttl 2h --quiet" }
        ]
      }
    ]
  }
}
```

- Claude Code adds a `SessionStart` hook's stdout to the session's context, so Claude sees each
  service's port and the session id. Errors go to stderr; a failing hook does not stop the
  session, so Claude can still run `stack status`.
- `up` is safe to repeat. Add a second entry with `"matcher": "resume"` to check the stack
  again when a session is resumed.
- `stack exec` and `stack run` renew the lease. If the agent mostly runs commands without
  Stack, use a longer `--ttl` or have it run `stack renew`.
- A session started with `claude --worktree` starts in the new worktree, so the same hook
  starts that worktree's own services. No worktree hook is needed for that.
- Stopping: `SessionEnd` hooks share a 1.5-second budget, too short for `stack down`, so rely
  on the lease (and [`setup --gc-agent`](commands.md#doctor-and-setup) to collect it).
- `WorktreeCreate` hooks replace Claude Code's own `git worktree add`: the hook must create
  the worktree itself and print only its path on stdout. Add one only if you need custom
  creation anyway, and send Stack's output to stderr (`stack -C "$path" up --ttl 2h >&2`).
  `WorktreeRemove` runs only for worktrees such a hook created. Run `stack -C "$path" down`
  there: a non-zero exit stops the removal while the directory still exists, so services are
  not orphaned. Check the [hooks reference](https://code.claude.com/docs/en/hooks) for the
  JSON each hook receives.

## Plain `git worktree`

Git runs `post-checkout` in a new worktree after `git worktree add`, with an all-zero previous
HEAD. Hooks live in the shared `.git/hooks`, so one hook covers every worktree:

```sh
#!/bin/sh
# .git/hooks/post-checkout (make it executable)
[ "$1" = 0000000000000000000000000000000000000000 ] || exit 0   # only new checkouts
[ -f stack.toml ] || exit 0
stack compile --locked && stack up --ttl 4h
```

A failing `post-checkout` does not undo the checkout; the worktree exists either way.

Git has no hook that runs before a worktree is removed, so nothing can stop its services for
you. Stop them first, for example with a shell function:

```sh
wt-rm() { stack -C "$1" down && git worktree remove "$1"; }
```

A worktree removed with services running keeps them running until you stop them, or until
`stack gc --stop-orphans` does (see [stopping orphans](commands.md#stopping-orphans)).

## Other tools

The same two commands fit any tool with blocking create and remove hooks. Run `stack down`
from a hook that can still stop the removal. A hook that runs after deletion is too late.

## Your shell and direnv

`stack exec -- <cmd>` checks every service each time it runs. To get the same environment in
an interactive shell instead, load it once:

```sh
eval "$(stack env)"                 # bash or zsh
stack env --shell fish | source     # fish
```

With [direnv](https://direnv.net), put the snippet `stack hook direnv` prints in `.envrc`:

```sh
stack hook direnv > .envrc
direnv allow
```

It loads `stack env` on entering the checkout, and reloads when `stack.toml`, `stack.lock` or
the session changes.

Both are snapshots. Services that do not verify when the environment is loaded have their
addresses withheld, as `exec` withholds them, and a comment names them. But a shell keeps the
values it loaded: if a service stops later, or another checkout takes its port, the shell
still points at that address. Re-run `stack env` (or `direnv reload`) after `up`, `restart`
or `down`, and use `stack exec` when you need the check every time.

mise's own shell activation (`mise activate`, or running `mise` in the checkout) also reads
the configuration Stack generates, so it sets the same `DATABASE_URL` and `<NAME>_PORT`
values. It does no verification: you get those addresses whether or not the service runs, or
is this checkout's.

## Isolated runs

To run Stack without touching your own installation (an evaluation, a sandboxed agent, a
throwaway check), point every store at one disposable directory.
[`examples/isolated-stack.sh`](../../examples/isolated-stack.sh) does this:

```sh
bash examples/isolated-stack.sh <version> ~/src/app stack exec -- pytest   # e.g. latest
```

Pick the release to evaluate; npm's `latest` is the newest published one. Release 0.1.21 gives
`exec` and `stack run` tasks a custom service's proxy URL instead of its loopback URL, and
0.1.22 fixed `exec` only, so evaluate the task loop with a newer release. It installs the given Stack
version from npm into a fresh `/tmp/stack-iso.*` root, moves the
Stack, mise, Pitchfork, XDG, uv and npm stores there, runs `stack up` and your command, then
stops everything. It keeps `HOME`, so your Git and SSH configuration still work. It needs
`node`, `npm` and `python3`.

- The root is deleted only when every service is confirmed stopped, the supervisor is down,
  your global mise and Pitchfork files are unchanged, and the command exited 0. Otherwise it is
  kept, and its path is printed on the first line.
- To clean up a kept root, run `stack down` from the checkout with that root's environment: it
  also stops the root's supervisor when Stack started it and nothing else uses it, and says why
  not otherwise (a Stack older than this default needs `stack down --stop-supervisor`, or
  `stack down` and then `stack exec -- pitchfork supervisor stop`). Delete only that exact path.
  Never delete by pattern: another run's root looks the same.
- Ctrl-C, SIGTERM or SIGHUP cancels the running step and still runs cleanup. The script exits
  128 plus the signal number.
- For several checkouts, run `stack down` in each: the supervisor is kept while the others'
  sessions use it, and the last one stops it.
- On Linux, if `XDG_STATE_HOME` is set, add its Pitchfork path to the script's receipt check.

[All docs](../README.md)
