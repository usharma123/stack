<img src="https://raw.githubusercontent.com/usharma123/stack/main/assets/icon.svg" width="64" height="64" alt="">

# Stack

Stack gives every checkout of a project its own pinned tools and its own running services:
Postgres, Redis, or anything you can start from a command. Each checkout gets its own ports
and data, and Stack checks every service before a command runs. Run five agents in five
worktrees and none of them will touch another's database.

Define the stack once in `stack.toml`, or share it as a bundle from Git, a local path or an
OCI registry. Stack installs tools with [mise](https://mise.jdx.dev) and runs services with
Pitchfork.

## Installation

```sh
npm install -g @ushawarma/stack
stack setup
```

Needs Node.js 22.14+, on macOS 13+ or Linux (x64 or arm64). Intel Macs run tools-only stacks:
Pitchfork, which runs services, has no Intel macOS build. `stack setup` uses the mise on your
`PATH`, or downloads the release Stack is tested against.

## Quick start

```toml
# stack.toml
[tools]
python = "3.13"

[services.postgres]
preset = "postgres"
version = "17"

[tasks.test]
run = "python -m unittest"
services = ["postgres"]
```

```sh
stack compile          # lock exact versions in stack.lock, assign this checkout's ports
stack up --ttl 30m     # install, start and verify the services
stack run test         # run a task once every service checks out
stack exec -- python -c 'import os; print(os.environ["DATABASE_URL"])'
stack down             # stop everything and confirm it is gone
```

Commit `stack.toml` and `stack.lock`. Connection details reach your app through the
environment (`DATABASE_URL`, `REDIS_URL`, `<NAME>_PORT`), so nothing hardcodes a port.

## For coding agents

Run `stack mcp` to start a stdio MCP server with the same operations, or pass `--json` to any
command for one structured result with a stable error `code` and `hint`. If a service is down
or belongs to another checkout, Stack withholds its address (or, with `--require`, refuses to
run) rather than let the command reach the wrong server. See the [agent guide](https://github.com/usharma123/stack/blob/main/docs/user/agents.md).

## Some notes

Stack is early. Expect rough edges, and pin a version in CI.

Bundles are code you run: review them like any other dependency. Sessions with a `--ttl` are
cleaned up by `stack gc` or the next `stack up`. Nothing runs in the background unless you run
`stack gc --watch` under a supervisor.

## Documentation

Full docs live in [docs/](https://github.com/usharma123/stack/tree/main/docs).

- [Install and first run](https://github.com/usharma123/stack/blob/main/docs/user/install.md)
- [Coding agents: JSON output and MCP](https://github.com/usharma123/stack/blob/main/docs/user/agents.md)
- [Bundles and `stack.toml`](https://github.com/usharma123/stack/blob/main/docs/user/bundles.md)
- [Command reference](https://github.com/usharma123/stack/blob/main/docs/user/commands.md)
- [Worktrees and sandboxes](https://github.com/usharma123/stack/blob/main/docs/user/worktrees.md)
- [Secrets](https://github.com/usharma123/stack/blob/main/docs/user/secrets.md)
- [What Stack guarantees](https://github.com/usharma123/stack/blob/main/docs/user/guarantees.md)

Building from source? Start at [docs/operations/development.md](https://github.com/usharma123/stack/blob/main/docs/operations/development.md).
