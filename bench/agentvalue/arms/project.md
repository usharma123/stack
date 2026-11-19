# Invoice ledger

A small invoice API: Python 3.13, PostgreSQL 17 and Redis 7.

- `app.py` is the HTTP API (`GET/POST /invoices`, `GET/PATCH /invoices/<id>`, `GET /invoices/<id>/receipts`,
  `GET /health`). It reads invoices through a Redis cache (`ledger/cache.py`).
- `worker.py` runs background jobs that the API queues on a Redis list (`ledger/jobs.py`). When
  `WORKER_HEALTH_PORT` is set it also answers on that port, which the dev tooling uses to check that
  the worker is up; keep that endpoint if you change the worker.
- `ledger/db.py` holds the SQL. `ledger/config.py` reads every address from the environment
  (`DATABASE_URL`, `REDIS_URL`, `PORT`); nothing hardcodes a port.
- `migrate.py` applies the SQL files in `migrations/` in file-name order, each once, recording them in
  `schema_migrations`. Never edit a migration that exists; add a new file named
  `YYYYMMDD_description.sql` (for example `20261118_add_widgets.sql`).
- `seed.py` loads the demo data (three invoices) into a new database.
- `tests/` holds the pytest suite; it talks to the running API and database through `API_URL` and
  `DATABASE_URL`. `smoke.py` is a quick check that the API serves the demo data.

The dependency set is fixed (`pyproject.toml`, `uv.lock`); do not add packages.

## Working in parallel

Several people and coding agents work on this repository at the same time, each in their own git
worktree. Every worktree runs its own services with their own ports and data, as described below. Other
worktrees' services, and the main checkout's, may be running while you work: only start, stop or
change services from your own worktree, and never stop processes you did not start.
