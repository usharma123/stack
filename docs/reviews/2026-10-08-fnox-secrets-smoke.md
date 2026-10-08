# fnox secret grants: real-tool receipts

Route 3 of [the jdx expansion design](../design/jdx-expansion.md), run on 2026-10-08 on macOS
arm64 with mise 2026.10.3 (`/opt/homebrew/bin/mise`), fnox 1.39.0 (packslip, installed by mise)
and a debug build of this branch (`target/debug/stack`, 0.1.19), then a release build for the
E2E scenario. Fake-provider tests are in `src/secrets.rs`, `src/process.rs` and
`tests/runtime.rs`; these receipts are what only the real tools show.

Isolation for the manual runs: `HOME`, `XDG_CONFIG_HOME`, `FNOX_CONFIG_DIR` and `FNOX_STATE_DIR`
in a fresh `/tmp/stack-fnox-smoke.*` directory (so no user fnox configuration or state was
read), `MISE_STATE_DIR` and `MISE_CACHE_DIR` there too, `STACK_STATE_DIR` and `STACK_CACHE_DIR`
there, and only `MISE_DATA_DIR` pointing at the user's tool store so the installed fnox 1.39.0
was found. The project's `fnox.toml` used the `plain` provider with sentinel values
(`smoke-sentinel-*`, and `abc1234` as a 7-byte value). No real secret was read.

| # | Command | Result | Boundary |
|---|---|---|---|
| 1 | `stack --json compile` (`[tools] fnox = "1.39.0"`, `[tasks.deploy] secrets = ["DEPLOY_KEY"]`) | `ok`; fnox `1.39.0` pinned; `tasks.deploy.value.secrets == ["DEPLOY_KEY"]` | generated `conf.d/stack.toml` has no `secrets` key (mise's native task secrets are not emitted) |
| 2 | `stack --json install` | `ok` | |
| 3 | `stack --json run deploy` | exit 0; `stdout` `task got [redacted:DEPLOY_KEY]`, `stderr` `err [redacted:DEPLOY_KEY]`; `OTHER_KEY` absent from the task's env; `secrets: ["DEPLOY_KEY"]`, `warnings: []` | through `mise run --skip-deps`; mise's own `[deploy] $ <run line>` echo on stderr names no value |
| 4 | `stack --json exec --secret DEPLOY_KEY -- sh -c 'echo "x$DEPLOY_KEY"; echo "${OTHER_KEY-unset} ${HIDDEN-unset}"'` | `x[redacted:DEPLOY_KEY]`, `unset unset` | only the requested key is set; fnox's `remove` (`HIDDEN`, an `env = false` key, plus fnox's own provider variables) applied |
| 5 | `HIDDEN=inherited stack exec --secret DEPLOY_KEY -- sh -c 'echo "raw:$DEPLOY_KEY hidden:${HIDDEN-removed}"'` | `raw:smoke-sentinel-deploy-7f3a hidden:removed` | terminal mode: the value is printed unredacted, as documented; an inherited variable fnox lists in `remove` is removed |
| 6 | `stack --json exec --secret SHORT_KEY -- true` / `stack exec --secret SHORT_KEY -- sh -c 'echo "short:$SHORT_KEY"'` | `secret_unsupported` (`the value is shorter than 8 bytes ...`) / `short:abc1234` | 8-byte minimum only when captured |
| 7 | `stack --json exec --secret NOPE_KEY -- true` | `secret_missing`, `details: [{ key: NOPE_KEY, reason: unknown }]` | refused at describe, before any value was requested |
| 8 | `stack --json exec --secret HIDDEN -- true` | `secret_unsupported` (not injectable; `env = false`) | |
| 9 | `stack --json exec --secret PATH -- true` | `invalid_secret`, `operation: declare` | before any provider call |
| 10 | `stack mcp`: `stack_exec` with `secrets: ["DEPLOY_KEY"]`; `stack_run deploy`; `stack_run` with `secrets`; `stack_exec` with `timeout_secs: 1` printing the value then sleeping | `mcp:[redacted:DEPLOY_KEY]`; task output redacted; `usage`; `timed_out` with `details[0].stdout == "[redacted:DEPLOY_KEY]\n"` | |
| 11 | `stack --json doctor` | `fnox` check ok: `fnox 1.39.0 at <store>/fnox/1.39.0/fnox; 4 key(s) described, 1 declared by tasks and injectable` | describe only; no `--keys` call |
| 12 | malformed `fnox.toml` (unclosed inline table on a line holding `smoke-sentinel-malformed-0c4d`): `stack --json exec --secret DEPLOY_KEY -- true`, the same without `--json`, `stack --json run deploy`, `stack --json doctor` | `secret_unavailable`, `details: [{ step: describe, kind: config, exit_code: 1, timed_out: false }]`; human form the same; doctor's `fnox` check fails with stack's message | the sentinel appears in none of stack's outputs; fnox's own stderr on the same file does quote the line (`grep -c` = 1), which is why it is never forwarded |
| 13 | `pgrep -fl fnox` before and after all runs | no fnox processes either time | `--no-daemon`: no resolution daemon was started |
| 14 | `grep -rlI smoke-sentinel .stack .config stack.lock $STACK_STATE_DIR $STACK_CACHE_DIR` | nothing | `<cache>/secrets/` empty afterwards (scratch roots removed) |
| 15 | `STACK_TIMINGS=1 stack --json exec --secret DEPLOY_KEY -- true` | phases `lock compile trust env secrets reserve total`; no sentinel | |
| 16 | `tests/e2e/native.sh target/release/stack 10-secrets` | passed | isolated HOME and mise data directory; `stack install` downloaded and installed fnox 1.39.0 itself; covers `run`, `exec --json`, terminal `exec`, short/unknown/protected keys, MCP `stack_exec`, malformed `fnox.toml`, doctor, and a sweep of everything stack wrote |

Upstream facts this implementation relies on were checked against fnox v1.39.0 and mise
v2026.10.3 source by a separate research task (no edits) and partly reproduced above:
`fnox env --json` prints one JSON line with `schema: 1`; error kinds are `config`,
`invalid_keys` and `resolution`, each exit 1; the structured `message` can quote configuration
content; describe omits optional fields (`as_file`, `env`, `lease`) when unset; fnox's daemon
detaches with `setsid()` and caches values in memory (default idle timeout 8 hours), so Stack
passes `--no-daemon`; `--non-interactive` does not stop operating-system dialogs or configured
commands; `mise ls --json fnox` returns an array whose `install_path` is the release directory,
and packslip puts `.mise-bins` first on `PATH` with a link into that directory.

Not run here: a real non-plain provider (1Password, keychain, cloud secret managers), fnox
leases and file secrets with real backends (refused by design; covered with fakes), a fnox that
hangs for 30 seconds (covered by a unit test with a short deadline and a process-group check),
Linux, and the Docker E2E runner.
