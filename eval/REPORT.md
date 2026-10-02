# Competitor stress test — 2026-10-02

Fixture: Python 3.13 + uv + Postgres 17 + Redis, with 3 pytest tests that hit both services.
Sandbox: Ubuntu 24.04 container (aarch64), non-root `agent` user with sudo, no systemd, no init.
Every command runs in a fresh subprocess with stdin closed, the way an agent runs it.
Harness: `harness/*.sh`, configs: `configs/*`, raw results: `results/*.tsv`, logs: `results/logs/`.

Versions: Flox 1.17.0 · devbox 0.18.4 · devenv 2.3.1 (2.4.0 was out; not tested) · mise 2026.9.18

## Scorecard

| | Flox | devbox | devenv | mise |
|---|---|---|---|---|
| Cold setup (tools + services) | 24.9s | 93.1s | 48.0s | 21.7s |
| Warm setup, new project dir | 1.4s | 3.5s | 4.4s | 0.02s |
| Per-command exec overhead | 0.25s | **1.8s** | 0.12s | 0.01s |
| Exec without shell activation | yes | yes | yes | yes |
| Tool list as JSON | no | no | partial (`eval`) | yes |
| Background services from a subprocess | **no** (needs a held-open activation) | yes | yes | none (DIY) |
| Native readiness wait | no | health column (Redis wrongly "Not Ready") | yes (`processes wait`) | DIY |
| Service status as JSON | yes | no | no | none |
| `up` twice | ok (skips) | **error, exit 1** | ok | DIY failed |
| Second project, same ports | services die, start reports success | crash-loop, `up -b` reports success | auto-allocates port **but env still points at old port** | fails, "examine the log" |
| Unknown package | clear | clear (4s) | **1,209 lines with ANSI codes** | clear one-liner |
| Impossible version | clear; but `python@3.99` on installed pkg → **exit 0, no-op** | clear | requires adding a Nix input first | clear (404 URL) |
| Install SIGKILLed partway | clean | clean | clean | clean |
| Supervisor SIGKILLed | services die with it | **services orphaned; `stop` says success** | services die; status errors clearly | n/a (daemons live forever) |
| Output noise per command | login warning + shell-detect WARN | info lines | progress log + update nag | progress bars in non-TTY |
| Agent integration | — | — | MCP server | — |

## Confirmed critical failures

1. **devenv: allocated port not propagated.** Second project got Postgres on 5433, but `PGPORT`/`DATABASE_URL`
   stayed 5432. Its tests passed — against the *first* project's database (`data_directory=/home/agent/proj/...`).
2. **devbox: orphaned services.** After the supervisor died, postgres/redis kept running (reparented to PID 1),
   `devbox services ls` errored, and `devbox services stop` printed "stopped successfully" while 7 processes lived.
3. **False success on `up`.** Flox and devbox both return 0 when services immediately fail (port in use).
4. **Flox: hook failure doesn't fail activation.** As root, `initdb` errors inside the hook; `flox activate` exits 0.
5. **Flox: request silently ignored.** `flox install python@3.99` when python exists → "already installed", exit 0.
6. **Flox: services require a held-open activation.** `flox services start` from a fresh subprocess is refused.
7. **devbox: launcher installed root-only (0711)** — unusable by non-root user until chmod.

## Not tested

macOS native; Worktrunk/workz; concurrent racing installs into the same store; remote/cloud backends;
devenv 2.4.0; Windows. Readiness timeouts for mise and devenv-redis in the TSV are harness quoting bugs
(tests passed and native/`pg_ctl -w` readiness confirmed), not tool failures. Zombie processes are due to no
init in the container and were excluded from leftover counts.

## Addendum: mise experimental daemons (Pitchfork 2.29.0), same mise 2026.9.18

The original run missed this path; "mise has no service management" was wrong. Config: `configs/mise-daemons/mise.toml`
(`[daemons.postgres] preset="postgres" port="auto"`, same for redis, `[tasks.test] daemons=[...]`). Container ran with `--init`.

| Test | Result |
|---|---|
| Baseline `mise run test` | pass, 7.6s; starts daemons, waits, runs tests |
| Env propagation | `PGHOST/PGPORT/PGUSER/PGDATABASE/DATABASE_URL/REDIS_URL` exported to `mise exec` |
| Status JSON | rich: id, status, pid, port, data_dir, ownership, size |
| Foreign Postgres/Redis on default ports | `mise run test` **fails in 25ms, exit 1**, names the PID holding the port, suggests fix |
| …but direct `mise exec -- pytest` | **3 passed against the foreign instance**: env exported from config even though mise's daemon is stopped |
| Linked git worktree | own ports (5583/6530) propagated to env, tests hit own data dir |
| Plain copy (not a worktree) | `run test` fails with clear error; direct `exec -- pytest` **passes against the primary's database** |
| start/start/stop/stop/start | all exit 0 (idempotent) |
| Two concurrent cold starts | both exit 0, one instance per project, no duplicates |
| SIGKILL Pitchfork supervisor | services survive, new supervisor adopts them, status accurate, `stop` cleans all |
| Root | explicit refusal with fix; exec env still exports `DATABASE_URL` |
| Output | terminal escape sequences in non-TTY output |

Not tested: shared imports of `[daemons]` across projects, hash-offset collisions between many worktrees, lease/TTL behaviour.

## Bundle sharing test — mise 2026.9.18 + Pitchfork 2.29.0 vs Flox 1.17.0

Bundle: Python/uv + Postgres + Redis + connection wiring + a setup step that loads `fixtures/seed.sql`
+ an internal CLI. Second bundle (obs) adds jq and sets `LOG_LEVEL` (deliberate conflict). Third: a custom
internal service. Consumed by two independent git repos (not worktrees). mise bundles served from a local
smart-HTTP git server; Flox bundles as local environment directories. Nothing published externally.
Bundle files: `bundles/`. FloxHub remote includes and `flox publish` were not tested (need an account and
would publish externally).

| Requirement | mise | Flox |
|---|---|---|
| Complete reuse | **No.** `[daemons]` and `[settings]` rejected in includes; each consumer copies 14 lines (daemons, experimental flag, task wiring). Tools/env/tasks do share. | **Yes.** 3-line consumer; services, vars, hooks, packages all included. |
| Shared-service alternative | `project =` runs ONE instance; app gets no `DATABASE_URL`; stopping appA stops appB's database. | n/a |
| Independent instances | Only after copying daemons; second repo conflicts on 5432 until a gitignored `mise.local.toml` repeats the whole daemon with a new base. Then: separate data dirs, independent stop. | Yes. Per-consumer data dir; ports moved with a 2-line committed override; independent stop. |
| Supporting files | Task catalog carries the repo: `seed` resolved `../fixtures/seed.sql`. But `{{config_root}}` in an included `_.path` resolves to the **consumer**, so the bundled CLI is not on PATH. | No mechanism: no variable points at the bundle; hooks can't find bundle files. |
| Composition + overrides | Merges; consumer override works. Conflicting `LOG_LEVEL` **silently** resolved (later wins, no message). | Merges; every override **reported**. But a conflicting `services.postgres` is reported and accepted → `postgres = "sleep 1000"`, exit 0. |
| Pinned, reproducible updates | Tag pins hold only via cache: after retagging `v1`, a fresh machine got **v2** from the same config. Full SHA reproduces. `mise.lock` records **no** bundle commit. | Lock embeds included manifests. Consumers unchanged until `flox include upgrade`; fresh copy reproduced v1 with bundle at v2 and even with the bundle dir absent. Consumer manifest holds a machine path. |
| Open extension (custom service) | Custom daemons work locally but can't be shared (same `[daemons]` restriction). | Custom service bundle merged and ran with no core changes. |
| Open extension (internal CLI) | Not on PATH (config_root issue); workable as a task. | No file shipping without building/publishing a package. |
| Distribution | git (https/ssh smart protocol only; no file://, no dumb HTTP) and OCI. | Local dir or FloxHub (account). No git/OCI. |
