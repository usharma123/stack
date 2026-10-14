# Worktrees and sandboxes

Each Git worktree is its own checkout to Stack, with its own ports, data, identity tokens and
session. A shared `stack.lock` keeps every worktree on the same exact versions.

Stack never creates or deletes checkouts. Tools that do (worktree managers, agent sandboxes)
should call it from their hooks:

- when a checkout appears: `stack compile --locked && stack up`
- before it is removed: `stack down`

Stopping first matters. Stack will not stop a deleted checkout's services by name, so a
checkout removed while its services run leaves them running until you stop them yourself
(see [`gc`](commands.md#gc)).

## Worktrunk

For [Worktrunk](https://github.com/max-sixty/worktrunk) (`wt`, tested with 0.80.0), add both
hooks to `.config/wt.toml`. Both block, and a failing hook aborts the operation:

```toml
# .config/wt.toml
[pre-start]
stack = "stack compile --locked && stack up"

[pre-remove]
stack = "stack down"
```

- If `pre-start` fails before anything starts (`lock_outdated`, `port_conflict`,
  `install_failed`), the worktree exists with nothing running. If it fails while starting
  (`start_failed`, `not_ready`), services may be running: check `stack status`, and run
  `stack down` before abandoning the worktree.
- A failing `stack down` aborts `wt remove`, so services are never orphaned. `--no-hooks`
  skips both hooks, so stop services yourself first.

## Other tools

The same two commands fit any tool with blocking create and remove hooks. Run `stack down`
from a hook that can still stop the removal. A hook that runs after deletion is too late.

## Isolated runs

To run Stack without touching your own installation (an evaluation, a sandboxed agent, a
throwaway check), point every store at one disposable directory.
[`examples/isolated-stack.sh`](../../examples/isolated-stack.sh) does this:

```sh
bash examples/isolated-stack.sh 0.1.21 ~/src/app stack exec -- pytest
```

It installs the given Stack version from npm into a fresh `/tmp/stack-iso.*` root, moves the
Stack, mise, Pitchfork, XDG, uv and npm stores there, runs `stack up` and your command, then
stops everything. It keeps `HOME`, so your Git and SSH configuration still work. It needs
`node`, `npm` and `python3`.

- The root is deleted only when every service is confirmed stopped, the supervisor is down,
  your global mise and Pitchfork files are unchanged, and the command exited 0. Otherwise it is
  kept, and its path is printed on the first line.
- To clean up a kept root, run `stack down` from the checkout with that root's environment,
  then stop its supervisor. Delete only that exact path. Never delete by pattern: another run's
  root looks the same.
- Ctrl-C, SIGTERM or SIGHUP cancels the running step and still runs cleanup. The script exits
  128 plus the signal number.
- For several checkouts, run `stack down` in each before stopping the supervisor.
- On Linux, if `XDG_STATE_HOME` is set, add its Pitchfork path to the script's receipt check.

[All docs](../README.md)
