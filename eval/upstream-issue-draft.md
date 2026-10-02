<!-- DRAFT — not posted. Target: jdx/mise -->
# Allow arbitrary commands to require managed daemons before receiving their connection environment

## Context
`[tasks.x] daemons = [...]` starts required daemons and waits for readiness before the task runs. `mise exec`
is documented as running a command with configured tools and environment, and exports preset connection
variables (`DATABASE_URL`, `PGPORT`, `REDIS_URL`) derived from configuration, whether or not the daemon runs.

## Reproduction (mise 2026.9.18, pitchfork 2.29.0, Ubuntu 24.04 aarch64, non-root)
1. Project with `[daemons.postgres] preset="postgres" port="auto"` and `[daemons.redis] preset="redis" port="auto"`.
2. Start an unrelated Postgres on 127.0.0.1:5432 and Redis on 6379.
3. `mise run test` (task with `daemons = ["postgres","redis"]`) → fails fast, names the PID holding the port. Good.
4. `mise exec -- pytest` → passes, connected to the unrelated instance (`show data_directory` = foreign dir).
5. Same with a plain copy of the project while the original runs: `exec` reaches the original's database.

The two execution paths behave differently in a way that is easy to hit from scripts and coding agents, which
often run commands directly rather than through tasks.

## Request (opt-in)
A way for a project or invocation to declare required managed daemons for arbitrary commands, e.g.
`mise exec --daemons postgres,redis -- pytest` or a setting, such that mise starts or verifies those daemons
(as tasks do) and refuses to run, or withholds their connection variables, when they are unavailable.

Not requested: validating every daemon on every `exec` (`python --version` shouldn't need Postgres), or
preventing intentionally external databases.
