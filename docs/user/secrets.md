# Secret grants

A task or command can be granted named secrets from the project's
[fnox](https://github.com/jdx/fnox) configuration. Stack resolves only the granted names,
through the fnox release `stack.lock` pins, after services verify, and keeps the values out of
everything it writes. With `--json` and over MCP, granted values are replaced in the captured
output.

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
the task's list and accept no additions (`stack_run` with `secrets` is a `usage` error).
`secrets` lists names, never values, so bundles may declare it; two layers defining one task must
agree on the whole task, as for any other field.

## Rules for names

Checked when the stack compiles (`compile`, `inspect`, `doctor`) and, for `--secret`, before any
provider call. Every problem is reported as `invalid_secret` with
`details: [{ key, operation: "declare", reason, task? }]`.

- A name matches `[A-Z_][A-Z0-9_]*` and is listed once.
- The stack must list `fnox` in its `[tools]` at a release (not `system` or `path:`). Stack adds
  nothing implicitly.
- A grant may not name a variable stack controls: `PATH`; anything starting `STACK_`, `MISE_`,
  `__MISE` or `PG`; a key of `[env]`; `UV_PYTHON` and `UV_PYTHON_PREFERENCE` when Python is
  pinned; and every endpoint variable of every service, used by the task or not
  (`DATABASE_URL` for a postgres preset, `REDIS_URL` for redis, and `<NAME>_HOST`, `<NAME>_PORT`,
  `<NAME>_URL` for each service).

When a command runs, the protected set also includes every variable the provider environment
sets and every variable stack withholds or removes for that command.

## How values are resolved

Only for a command that has at least one grant, after services are verified and unverified
endpoints withheld:

1. In a scratch provider root of its own under stack's cache (configured with fnox at its pinned
   release and nothing else, removed afterwards), stack asks `mise ls --json fnox` where that
   release is installed. Not installed is `secret_unavailable` (`kind: "not_installed"`; run
   `stack install`).
2. The `fnox` found on the command's `PATH` must lie in that directory, and after resolving links
   must be a regular file still inside it. Another release, another executable, or a link out of
   the directory is `secret_unavailable` (`kind: "not_pinned"`). Stack runs the resolved file.
3. `fnox --non-interactive --no-daemon env --json --describe` (value-free), then
   `fnox --non-interactive --no-daemon env --json --keys <names>`, each in the project directory
   with the command's environment, at most 30 seconds (less if an operation deadline is
   closer), at most 64 KiB of output, and its process group killed at the deadline. Describe
   refuses, before any value is requested, a key fnox does not know (`secret_missing`), a file
   secret, a key that cannot be injected into a command's environment (`env = false`), and a key
   supplied by a lease (`secret_unsupported`). `--no-daemon` keeps fnox from starting or using
   its resolution daemon, which would outlive the deadline and keep values cached in memory.
4. The whole answer is validated before anything is applied. Only the requested keys are set;
   anything else fnox returns is dropped. A protected variable in fnox's `set` is
   `invalid_secret` (`operation: "set"`) and nothing is applied. fnox's `remove` list is
   applied, except for protected variables, which are kept and reported in `warnings`
   (`"fnox asked to remove DATABASE_URL; kept"`; on the terminal, on stderr).
5. With `--json` and over MCP, a value shorter than 8 bytes is refused (`secret_unsupported`):
   replacing every occurrence of a short string would mangle output, and leaving it would leak
   it. This is a usability limit, not a confidentiality property. On the terminal there is no
   minimum.

Results of granted commands list the names (never values) under `secrets` and anything declined
under `warnings`. `inspect --json` lists each task's `secrets` names. Results of commands without
grants keep their previous shape.

Nothing fnox prints reaches you through stack: not its stdout, not its stderr, and not the
`message` inside its JSON protocol, all of which can quote configuration lines and therefore
values. Errors carry stack's own text, key names, fnox's error kind, the exit status and whether
the deadline passed. Run `fnox env --json --describe` in the project yourself to see fnox's own
diagnostic.

| Code | When | Details |
|---|---|---|
| `invalid_secret` | malformed, duplicate or protected name; fnox missing from tools or unpinned; fnox returned a protected key in `set` | `[{ key, operation: "declare" \| "set", reason, task? }]` |
| `secret_missing` | fnox does not know a key, or could not resolve it | `[{ key, reason: "unknown" \| "unresolved" }]` |
| `secret_unavailable` | fnox not locked, not installed, not on `PATH`, not the pinned release; protocol violation, oversized answer, error reply, nonzero exit, timeout | `[{ step, kind, exit_code?, timed_out }]` |
| `secret_unsupported` | file secret, lease, key not injectable into an environment, value under 8 bytes when captured, value containing a NUL byte | `[{ key, reason }]` |

`kind` for `secret_unavailable` is one of `not_locked`, `not_installed`, `not_on_path`,
`not_pinned`, `provider`, `spawn`, `timed_out`, `oversized`, `protocol`, or fnox's own error kind
(`config`, `resolution`; `unknown` when fnox names a kind stack will not echo).

Resolution runs while stack holds the checkout's project lock, like the rest of planning a
command, so other stack commands in the same checkout wait for it: at most about 90 seconds
(the release query and the two fnox calls, 30 seconds each), usually well under one.

## Redaction in captured output

With `exec --json`, `run --json`, MCP `stack_exec` and `stack_run`, every granted value is
replaced by `[redacted:KEY]` in stdout and stderr as the output streams in, before it is bounded
to 64 KiB: a value split across reads, written twice, overlapping another value, or straddling
the start of the retained tail is still replaced. Overlapping values become one run naming each
key. Timeout results and error details built from the output are redacted the same way.

## Boundary

What a grant does and does not promise:

- **Terminal output is not redacted.** Without `--json` the command owns the terminal: nothing is
  captured, so stack cannot redact anything, and a command that prints a value prints it. There
  is no result to say otherwise; the `--secret` help says so.
- **Transformed values are not caught.** A value the program base64-encodes, URL-encodes,
  hashes, splits or re-encodes no longer matches literally and is not replaced.
- **Inherited variables pass through.** Variables your shell already had are inherited by the
  command byte for byte, as before, including one with the same name as a secret fnox did not
  grant. A grant adds the granted keys and applies fnox's removals; it is not a sandbox.
- **Children inherit what the command received.** Stack does not confine the granted values to
  the command's own process.
- **fnox chooses its own configuration.** fnox reads `fnox.toml` and its profile and local
  variants from the project directory upward (until a `root = true` file or the filesystem
  root), its global configuration (`FNOX_CONFIG_DIR`, else `$XDG_CONFIG_HOME/fnox`, else
  `~/.config/fnox`), and the inherited `FNOX_PROFILE` and other `FNOX_*` settings. Stack does not
  isolate that; it only binds which fnox release runs.
- **Non-interactive is best effort.** `--non-interactive` disables fnox's own prompts and browser
  logins, but a provider can still show an operating-system dialog or run a configured command.
  The 30-second deadline bounds the wait; it does not prevent the prompt.
- **fnox's own state is fnox's.** Stack never writes values to `stack.lock`, the generated
  configuration, `.stack/`, session records, the machine index, timing output, `inspect`,
  `doctor` or any log of its own. fnox and its providers may keep their own state (lease ledgers,
  keychains, provider caches); grants for leases are refused, but other providers' persistence
  is outside stack's control.
- **mise's native task secrets are not used.** Stack never writes `secrets` into the generated
  mise configuration; `stack run` passes the values it resolved through the environment of
  `mise run`.

For what grants do not support (file secrets, leases, interactive logins, every key of a
profile), `stack exec -- fnox exec -- <command>` still works: fnox then injects its profile into
the command itself, with fnox's own rules and no redaction by stack.

## doctor

When the stack lists `fnox`, `stack doctor` reports whether the pinned release is locked and
installed, and runs `fnox --non-interactive --no-daemon env --json --describe` (value-free) to
check that every name the tasks declare is known and injectable. It never resolves a value. Not
yet installed is reported, not a failure; a fnox error or an unknown declared key fails the
`fnox` check with stack's own message.

[All docs](../README.md)
