# Install and first run

Install the CLI with:

```sh
npm install -g @ushawarma/stack
stack setup
```

Prebuilt binaries cover macOS 13+ and Linux (static, any distribution or libc), on x64 and arm64.
On Intel Macs stack runs tools-only stacks: Pitchfork, which supervises services, publishes no
Intel macOS build, so `install`, `up`, `exec` and `run` refuse a stack with services there
(`services_unsupported`) before installing anything.
Stack needs [mise](https://mise.jdx.dev); it installs everything else, including Pitchfork,
through it. `stack setup` uses a mise already on `PATH`; otherwise it downloads the pinned release
stack is tested against (checked by SHA-256) into `~/.local/share/stack/bin` (`$STACK_DATA_DIR/bin`
or `$XDG_DATA_HOME/stack/bin` when set). Stack finds it there without any `PATH` change. Run
`stack doctor` to check a machine.
Node.js 22.14+ is required. See [release guide](../RELEASING.md) for CI checks,
trusted publishing setup, release tags, and recovery.

## First run

Run `stack setup` once, and make sure `git` and `tar` are on `PATH`.

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

Commit `stack.toml` and `stack.lock`. The rest stack generates is machine-specific: `.stack/`, `.config/mise/conf.d/stack.toml`, `.config/mise/mise.lock`, `.config/mise/locks/` and the [skill links](skills.md#linking-skills-into-the-project) with their `.stack-skills.json`. In a git checkout, `compile` (and every command that compiles) names exactly those paths in a `.gitignore` of stack's own beside them: `.stack/.gitignore`, `.config/mise/.gitignore` and `<skills dir>/.gitignore`. Each ignores itself, so it never shows as untracked, and covers only the checkout it is in, so a path stack generates in one worktree is never hidden in another where it is yours. Your other files under `.config/` and your own skills stay visible, and a file you already track stays tracked. A `.gitignore` of yours in one of those directories is never changed; stack warns about the generated paths it leaves unignored. A directory reached through a symbolic link (a skills dir or `.config/mise` linked into another checkout, say) is left alone entirely, with a warning, by the same rule skill links follow. Earlier releases listed these paths in the shared `.git/info/exclude`; stack removes those blocks (taking git's `exclude.lock`, waiting up to five seconds for another writer, or less when the call has a shorter `--timeout`), leaving your own lines there. A `.stack/session.json` copied in from another directory (committed by an earlier release, or a duplicated checkout) is ignored.

> Stack is an early prototype. Review bundles before using them: their commands run as trusted code. A TTL is reclaimed by `stack gc` or a later `stack up`; unattended cleanup requires running `stack gc --watch` under a supervisor.

Next, [reuse a bundle](bundles.md) or read the [command reference](commands.md).

[All docs](../README.md)
