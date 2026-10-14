# Install and first run

```sh
npm install -g @ushawarma/stack
stack setup
stack doctor
```

The package carries prebuilt binaries for macOS 13+ and Linux (static, any distribution) on
x64 and arm64, and needs Node.js 22.14+. You also need `git` and `tar` on `PATH`.

Stack installs everything through [mise](https://mise.jdx.dev). `stack setup` uses the mise on
your `PATH`; if there is none, it downloads the release Stack is tested against (checked by
SHA-256) into `~/.local/share/stack/bin`, where Stack finds it without a `PATH` change.
`stack doctor` checks the machine and, inside a project, that it compiles.

### Intel Macs

Pitchfork, which runs services, has no Intel macOS build. Tools-only stacks work. A stack with
services fails with `services_unsupported` before anything is installed.

## First run

In an empty directory, create `stack.toml`:

```toml
[tools]
python = "3.13"

[services.redis]
preset = "redis"
version = "7"
```

Then:

```sh
stack compile
stack up --ttl 30m
stack exec --require redis -- python -c 'import os; print(os.environ["REDIS_URL"])'
stack status
stack down
```

- `compile` writes the exact versions to `stack.lock` and gives this checkout its own ports.
- `up` installs Python and Redis, starts Redis and checks that it is this checkout's server.
- `exec` runs a command with the stack's tools and environment. `--require redis` makes it
  refuse to run if Redis does not check out.
- `down` stops Redis and returns once the process is gone.

`--ttl 30m` puts a lease on the session. Once it expires, `stack gc` or the next `stack up`
stops the services for you.

## What to commit

Commit `stack.toml` and `stack.lock`. Everything else Stack writes is specific to the
machine: `.stack/`, and the `.config/mise/` files it generates. In a Git checkout Stack adds a
small `.gitignore` next to each of them, so they never show up in `git status`. Stack never
edits a `.gitignore` of yours, and leaves your other files in `.config/` alone.

Next, [reuse a bundle](bundles.md), or set Stack up for [coding agents](agents.md).

[All docs](../README.md)
