<img src="https://raw.githubusercontent.com/usharma123/stack/main/assets/icon.svg" width="64" height="64" alt="">

# Stack

Reusable development stacks for projects and coding agents.

Define your tools and services once, publish them as a bundle, and reuse them across projects.
Each checkout gets pinned versions, its own ports, and service checks before commands run.
Stack uses [mise](https://mise.jdx.dev) to install tools and Pitchfork, through `mise daemons`,
to run services.

## Installation

```sh
npm install -g @ushawarma/stack
stack setup
```

Requires Node.js 22.14+ and supports macOS 13+ and Linux on x64 and arm64.
`stack setup` downloads the [mise](https://mise.jdx.dev) release stack is tested against, unless
mise is already on `PATH`. Stack installs Pitchfork through mise.

Start with [install and first run](https://github.com/usharma123/stack/blob/main/docs/user/install.md)
for a complete example.

## How it works

A project's `stack.toml` can define tools and services directly or import bundles from Git,
a local path, or an OCI registry:

```toml
[tools]
python = "3.13"

[services.redis]
preset = "redis"
version = "7"
```

```sh
stack compile                 # lock versions and assign this checkout's ports
stack up --ttl 30m             # start and verify services
stack exec --require redis -- python -c 'import os; print(os.environ["REDIS_URL"])'
stack down                    # stop services and confirm they are gone
```

Define `[tasks.<name>]` in `stack.toml` and run them with `stack run <name>`, which first
verifies the stack's services.

Commit `stack.toml` and `stack.lock` to share the configuration and resolved versions.
Use `--json` for structured command results, or `stack mcp` for the stdio MCP server.
Coding agents: see the [agent quickstart](https://github.com/usharma123/stack/blob/main/docs/user/agents.md).

## Status

Stack is an early prototype. Service and session scenarios are tested with real mise and
Pitchfork on Linux and macOS. See the [design](https://github.com/usharma123/stack/blob/main/docs/DESIGN.md)
for implementation scope and limits.

Bundles run trusted code. Review them before use. TTL cleanup runs during `stack gc` and
`stack up`; run `stack gc --watch` under a supervisor for unattended cleanup.

## Documentation

Full docs live in [docs/](https://github.com/usharma123/stack/tree/main/docs).

- [Install and first run](https://github.com/usharma123/stack/blob/main/docs/user/install.md)
- [Bundles and project configuration](https://github.com/usharma123/stack/blob/main/docs/user/bundles.md)
- [Command reference](https://github.com/usharma123/stack/blob/main/docs/user/commands.md)
- [Guarantees and limits](https://github.com/usharma123/stack/blob/main/docs/user/guarantees.md)
- [Service identity probes](https://github.com/usharma123/stack/blob/main/docs/user/identity-probes.md)
- [Secret grants](https://github.com/usharma123/stack/blob/main/docs/user/secrets.md)
- [Publishing bundles to OCI registries](https://github.com/usharma123/stack/blob/main/docs/user/registries.md)
- [JSON output and MCP](https://github.com/usharma123/stack/blob/main/docs/user/agents.md)
- [Agent skills](https://github.com/usharma123/stack/blob/main/docs/user/skills.md)

Building from source? Start with [development and validation](https://github.com/usharma123/stack/blob/main/docs/operations/development.md).
