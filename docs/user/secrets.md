# Secret grants

A task or command can be granted named secrets from the project's
[fnox](https://github.com/jdx/fnox) configuration. Stack resolves only the granted names,
through the fnox release `stack.lock` pins, after services verify, and keeps the values out of
everything it writes. With `--json` and over MCP, granted values are replaced in the captured
output.

A grant decides what stack injects and what it redacts. It is not access control: any command
stack runs can call fnox (or the provider behind it) itself and read every secret your user and
that provider can, granted or not, and a value read that way is not redacted. See
[Boundary](#boundary).

```toml
# stack.toml
[tools]
fnox = "1.39.0"

[tasks.deploy]
run = "./deploy.sh"
services = ["postgres"]
secrets = ["DEPLOY_KEY", "SENTRY_DSN"]
```

```sh
stack run deploy                                   # granted DEPLOY_KEY and SENTRY_DSN
stack exec --secret DEPLOY_KEY -- ./release.sh     # one command, repeat --secret for more
stack --json exec --secret DEPLOY_KEY -- ./check.sh
```

Over MCP, `stack_exec` takes `secrets: ["DEPLOY_KEY"]`. `stack run` and `stack_run` grant exactly
the task's list and accept no additions. `secrets` lists names, never values, so bundles may
declare it; two layers defining one task must agree on the whole task, as for any other field.

## Rules for names

Checked when the stack compiles and, for `--secret`, before any provider call. Every problem is
`invalid_secret`.

- A name matches `[A-Z_][A-Z0-9_]*` and is listed once.
- The stack must list `fnox` in its `[tools]` at a release. Stack adds nothing implicitly.
- A grant may not name a variable stack controls: `PATH`; anything starting `STACK_`, `MISE_`,
  `__MISE` or `PG`; a key of `[env]`; `UV_PYTHON` and `UV_PYTHON_PREFERENCE` when Python is
  pinned; and every endpoint variable of every service
  (`DATABASE_URL`, `REDIS_URL`, and `<NAME>_HOST`, `<NAME>_PORT`, `<NAME>_URL`).

## How values are resolved

Only for a command with at least one grant, after services verify:

1. The `fnox` on the command's `PATH` must be the release `stack.lock` pins. Otherwise the
   command does not run (`secret_unavailable`). A pinned fnox that is not installed stops
   `exec` earlier, with `tools_not_installed`; run `stack install`.
2. Stack runs `fnox env --json --describe`, which reads no values, then `fnox env --json --keys
   <names>`, both non-interactive and bounded by 30 seconds. File secrets, leases and keys that
   cannot be injected into an environment are refused before any value is requested.
3. Only the requested keys are set. If fnox asks to set a protected variable, nothing is
   applied (`invalid_secret`). If it asks to remove one, the variable is kept and the result
   says so in `warnings`.

Nothing fnox prints reaches you through stack, because its output can quote configuration lines
and therefore values. Errors carry stack's own message, key names, fnox's error kind, the exit
status and whether the deadline passed. Run `fnox env --json --describe` yourself to see
fnox's own diagnostic.

| Code | When |
|---|---|
| `invalid_secret` | malformed, duplicate or protected name; fnox missing from the tools; fnox returned a protected key |
| `secret_missing` | fnox does not know a key, or could not resolve it |
| `secret_unavailable` | fnox is not locked, not installed or not the pinned release, failed, timed out, or answered outside its protocol |
| `secret_unsupported` | file secret, lease, key not injectable into an environment, or a captured value stack cannot redact safely (see below) |

## Redaction in captured output

With `exec --json`, `run --json`, MCP `stack_exec` and `stack_run`, every granted value is
replaced by `[redacted:KEY]` in stdout and stderr as the output streams in, before it is bounded
to 64 KiB. A value split across reads, repeated, or overlapping another value is still replaced,
and timeout results are redacted the same way.

In captured mode, values shorter than 8 bytes are refused: replacing every occurrence of a short
string would mangle output. This is a usability limit, not a security property. Values that
could be confused with stack's own markers, such as `redacted` itself, are refused too.

Results list granted names under `secrets`. Key names are not secret; choose values that are not
also key names.

## Boundary

- **Grants are not access control.** Stack does not sandbox the command. It, or anything it
  starts, can run `fnox get`, `fnox exec` or the provider's own CLI and read any secret the
  user and provider allow, including ones never granted. Only granted values are redacted:
  an ungranted value the command reads itself appears in captured output as it was printed.
- **Terminal output is not redacted.** Without `--json` the command owns the terminal and a
  command that prints a value prints it.
- **Transformed values are not caught.** A value the program encodes, hashes or splits no longer
  matches literally.
- **Inherited variables pass through.** Variables your shell already had reach the command as
  before. A grant adds the granted keys; it is not a sandbox, and children inherit what the
  command received.
- **fnox chooses its own configuration.** fnox reads `fnox.toml` from the project upward, its
  global configuration, and `FNOX_PROFILE` and other `FNOX_*` settings. Stack only binds which
  fnox release runs.
- **Non-interactive is best effort.** fnox's prompts and browser logins are disabled, but a
  provider can still show an operating-system dialog. The deadline bounds the wait.
- **fnox's own state is fnox's.** Stack writes no value to `stack.lock`, generated
  configuration, `.stack/`, session records or logs. fnox and its providers may keep their own
  caches.

For file secrets, leases, interactive logins or a whole profile, `stack exec -- fnox exec --
<command>` still works, with fnox's own rules and no redaction by stack.

## doctor

When the stack lists `fnox`, `stack doctor` reports whether the pinned release is locked and
installed and checks, without resolving any value, that every declared name is known and
injectable.

[All docs](../README.md)
