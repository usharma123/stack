## Running it in this worktree

This repository uses Stack: `stack.toml` declares the pinned tools and the services, and `stack.lock`
locks them. `stack` is on PATH. Stack gives this worktree its own PostgreSQL, Redis, API and worker,
with ports and data of its own. `stack --help` lists every command; add `--json` to any command for a
single machine-readable result (`stack mcp` serves the same operations over MCP).

- `stack up` installs the pinned tools, starts postgres, redis, api and worker, and checks each one.
  Before the api starts, it applies pending migrations (`migrate.py`). A new database is seeded once
  (`seed.py`). Safe to repeat.
- `stack status` shows this worktree's services, their ports and whether each one checks out.
- `stack exec -- <command>` runs any command with the pinned tools and this worktree's service
  addresses (`DATABASE_URL`, `REDIS_URL`, `API_URL`, `API_PORT`). `eval "$(stack env)"` loads the same
  variables into your shell.
- `stack run test` runs the test suite (pytest) against this worktree's services; `stack run smoke` runs
  the smoke test. `stack run migrate` and `stack run seed` apply migrations or load the demo data now.
- `stack restart api worker` applies pending migrations and restarts the app processes after you change
  their code (they do not reload on their own; `stack status` tells you when a watched file changed).
- `stack logs <service>` shows a service's recent output.
- `stack down` stops everything this worktree started and confirms it is gone. The data is kept;
  `stack down --purge` also deletes this worktree's data.
