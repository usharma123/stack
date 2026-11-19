## Running it in this worktree

Tools are pinned in `mise.toml`. `dev.py` gives this worktree its own PostgreSQL, Redis, API and worker,
with ports chosen for this worktree and data under `.dev/`. `python3 dev.py --help` lists every command;
`status` prints JSON.

- `python3 dev.py up` installs the pinned tools, starts postgres, redis, api and worker, and checks that
  the api answers. Before the api starts, it applies pending migrations (`migrate.py`). A new database is
  seeded once (`seed.py`). Safe to repeat.
- `python3 dev.py status` shows this worktree's services, their ports and process IDs, and whether each
  one is running.
- `python3 dev.py exec <command>` runs any command with the pinned tools and this worktree's service
  addresses (`DATABASE_URL`, `REDIS_URL`, `API_URL`, `API_PORT`). `eval "$(python3 dev.py env)"` loads the
  same variables into your shell.
- `python3 dev.py test` runs the test suite (pytest) against this worktree's services; `python3 dev.py
  smoke` runs the smoke test. `python3 dev.py migrate` and
  `python3 dev.py seed` apply migrations or load the demo data now.
- `python3 dev.py restart api worker` restarts the app processes after you change their code (they do not
  reload on their own).
- `python3 dev.py logs <service>` shows a service's recent output.
- `python3 dev.py down` stops everything this worktree started and confirms it is gone. The data is
  kept; `python3 dev.py down --purge` also deletes this worktree's data.
