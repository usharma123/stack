# Independent Astra hands-on evaluation

Read REPORT.md for findings and coverage. This is an independent single-agent session, not a benchmark implementation. The fixtures are small real applications with intentionally failing starting versions and completed versions. No Stack product code was changed.

`run.json` identifies this run's evidence. Do not rerun a driver against that retained run. To create a fresh run, from this repository use:

```sh
python3 eval/agentdx-20261020/astra/harness.py new
python3 eval/agentdx-20261020/astra/harness.py run build cargo build --locked
python3 eval/agentdx-20261020/astra/bootstrap.py
python3 eval/agentdx-20261020/astra/ledger_work.py
python3 eval/agentdx-20261020/astra/mcp_client.py
python3 eval/agentdx-20261020/astra/fix_ledger.py
python3 eval/agentdx-20261020/astra/second_checkout.py
python3 eval/agentdx-20261020/astra/catalog_work.py
python3 eval/agentdx-20261020/astra/dependents.py
python3 eval/agentdx-20261020/astra/tools_bundle.py
python3 eval/agentdx-20261020/astra/secrets_work.py
python3 eval/agentdx-20261020/astra/lifecycle.py
python3 eval/agentdx-20261020/astra/failures.py
python3 eval/agentdx-20261020/astra/cancel_repro.py
python3 eval/agentdx-20261020/astra/cleanup.py
```

These are evidence drivers, not an unattended pass/fail suite. Inspect each result and stop on an unexpected prerequisite failure. Negative cases intentionally return failures. `cleanup.py` requires the cancellation and checkout drivers to have run; for a partial run, call `stack down` in every created project with `harness.env()`, then stop the isolated Pitchfork supervisor. Never substitute the user's normal environment.

The Linux scripts create one named Docker container using the existing `rust:1.93` image. `linux_prepare.py` retains the original failed login-shell build attempt; retry with `docker exec <name> bash -c 'cd /work/src && cargo build --locked'` as recorded in the report. Then use `linux_run.py`. The initial Linux app assertion exposes SQL_ASCII defaults. The encoding adjustment and root-user experiment were performed separately; their full argv receipts are in raw data. `linux_cleanup.py` covers both Linux users and expects that root experiment. It archives evidence before removing only its named container.

The cleanup driver stops services and the supervisor but intentionally retains tool state and failed evidence. This completed run subsequently archived its isolated root under the raw evidence directory, and removed its extra Git worktree after preserving the app. See `resources.json` and cleanup receipts.

## Tasks

- Invoice ledger: add an optional invoice note through a non-destructive SQL migration, and fix stale Redis responses after a PATCH. `app.py` is the broken baseline; `app.fixed.py` is the completed application. `test_api.py` checks API, PostgreSQL, Redis, and preservation of the seeded customer.
- Catalog: enforce case-insensitive, whitespace-trimmed SKU uniqueness and preserve storage on rejection. `server.mjs` is the broken baseline; `server.fixed.mjs` is the fix. `test.mjs` asserts a duplicate returns 409 and leaves the original list unchanged.
- Dependent worker: `server.dependent.mjs`, `worker.mjs`, and `stack.dependent.toml` demonstrate a worker that loads API state once. Restarting the API leaves the worker stale until explicitly restarted.

Both applications are deliberately small. They are not production-hardened, and completing these tasks does not establish a productivity advantage over another setup.
