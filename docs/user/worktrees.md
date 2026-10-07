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
  hooks and `--execute`. If it fails (for example `port_conflict` or `lock_outdated`), the
  worktree exists but nothing started; fix the cause and run the commands by hand.
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

[All docs](../README.md)
