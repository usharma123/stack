# Install and first run

Install the CLI with:

```sh
npm install -g @ushawarma/stack
stack --version
```

Prebuilt binaries cover macOS 13+ and Linux (static, any distribution or libc), on x64 and arm64.
Services need [mise](https://mise.jdx.dev) on PATH; stack installs everything else, including
Pitchfork, through it. Run `stack doctor` to check a machine.
Node.js 22.14+ is required. See [release guide](../RELEASING.md) for CI checks,
trusted publishing setup, release tags, and recovery.

## First run

Install [mise](https://mise.jdx.dev/getting-started.html) and make sure `mise`, `git`, and `tar` are on `PATH`.

In an empty directory, create `stack.toml`:

```toml
[tools]
python = "3.13"

[services.redis]
preset = "redis"
version = "7"
```

Then run:

```sh
stack doctor
stack compile
stack up --ttl 30m
stack exec --require redis -- python -c 'import os; print(os.environ["REDIS_URL"])'
stack status
stack down
```

`compile` records exact versions in `stack.lock` and assigns this checkout its own ports. `up` installs the tools and starts Redis. `exec` verifies Redis before exposing its connection URL. `down` confirms the service has stopped.

Commit `stack.toml` and `stack.lock`. Keep generated machine-specific files out of version control, including `.stack/` and `.config/mise/conf.d/stack.toml`. Stack writes `.stack/.gitignore` itself; a `.stack/session.json` copied in from another directory (committed by an earlier release, or a duplicated checkout) is ignored.

> Stack is an early prototype. Review bundles before using them: their commands run as trusted code. A TTL is reclaimed by `stack gc` or a later `stack up`; unattended cleanup requires running `stack gc --watch` under a supervisor.

Next, [reuse a bundle](bundles.md) or read the [command reference](commands.md).

[All docs](../README.md)
