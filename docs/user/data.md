# Service data: init tasks

A new checkout starts with empty databases. **Init tasks** get it to a usable one: they run
your migrations and seeds, once for each new database, as part of `stack up`.

## Worked example

```toml
# stack.toml
[services.postgres]
preset = "postgres"
version = "17"
init = ["migrate", "seed"]          # run in this order, once per database

[tasks.migrate]
run = "uv run alembic upgrade head"
services = ["postgres"]

[tasks.seed]
run = "uv run python -m app.seed"
services = ["postgres"]
```

On `main`:

```sh
stack compile
stack up                       # starts postgres, verifies it, then runs migrate and seed
stack up                       # init is done for this database: nothing runs
stack status                   # postgres: ready, verified instance; init done
```

A new worktree gets its own database, on its own port, and its first `stack up` runs init
there. Changes there never reach `main`'s database.

## Init tasks

Init tasks and a service's [`setup`](bundles.md) are different hooks. `setup` runs before
every start of the service, before it listens, so it must be safe to repeat (`uv sync`, an
idempotent migration run from the service that needs the database). Init tasks run once per
database, after the service is up and verified, so they can reach it: migrations and seed data
that must not run twice.

A service lists the names of `[tasks.*]` under `init`. After every service verifies, `up` runs
the init tasks of each service whose current data directory has not completed them, in the
order listed. Each task runs the way `stack run <task>` runs it: through `mise run`, with the
stack's environment, granted the secrets it declares (values under 8 bytes are refused, as for
captured output), its output captured. Init tasks count against `up`'s `--timeout`, and each
appears in `up`'s steps as `{"step": "init", "detail": {"service", "task", "exit_code"}}`.
`up`'s result reports each service under `init`: its `status`, the tasks it `ran`, and
`resumed_after` when it continued an earlier attempt.

- **Once per database.** Stack records completion per checkout and service, in its machine
  state (never inside the data directory). The record names the data directory as Stack's data
  record does (where it resolved and which directory it is: device and inode), and a digest of
  the init list and the definitions of its tasks. A data directory that is deleted and
  recreated is a new database, so init runs again. `up` against a database that completed init
  never runs it again.
- **Purge and move.** `stack down --purge` forgets the record of each service whose data it
  removed, so the next `up` initialises the new database; data it kept keeps its record.
  `stack gc --reclaim-data` does the same for a deleted checkout. `stack mv` carries the
  records to the new path with the data, so a moved database is not initialised twice.
- **Failures.** A failing task fails `up` with `init_failed`, naming the service and task, with
  its exit code and the end of its output. Services stay running and verified. The next `up`
  resumes from the failed task, skipping tasks before it that completed with unchanged
  definitions.
- **Changes.** Editing the init list or one of its tasks after init completed does nothing
  automatically: `status` reports the service as `stale` and `up` warns. Run
  `stack up --reinit <service>` to run the whole list again. Make migrations idempotent, as
  `--reinit` runs every task again.
- **Existing databases.** Adding `init` to a project whose checkouts already have data runs the
  list once on each of them at the next `up` (they have no record yet). Use `--no-init` to skip
  it there.
- `stack up --no-init` runs no init task. `--reinit <service>` (repeatable) runs that service's
  list again from the first task. They cannot be combined.
- **Services without a data directory** (a `run` command) are keyed by checkout and service:
  their init runs once per checkout, until `--reinit`.

`stack status` reports each service's `init`:

| State | Meaning |
|---|---|
| `done` | every task ran against this data directory, from the current definitions |
| `pending` | init has not completed against this data directory, or it does not exist yet |
| `failed` | a task failed; the next `up` resumes from it |
| `stale` | init completed, but the list or a task changed since; `up --reinit <service>` |
| `none` | the service declares no init tasks |

`status` stays `healthy` whatever the init state: it describes the running services.

### Validation and composition

`compile` (and every command that compiles) fails with `init_invalid` when an init list names a
task no layer defines, lists a task twice, or names a task whose `services` list leaves the
service out (a task with no `services` list is allowed), or when an init-only table names a
service no layer defines.

`init` is composed as its own key, apart from the rest of the service definition, so it never
restarts a running service:

- A bundle may declare `init` on its services, naming tasks it or another layer defines.
- A table that sets only `init` adds it to a service another layer defines:
  `[services.postgres]` with just `init = ["migrate"]` in `stack.toml`.
- Two layers that set different lists conflict (`conflict`, kind `init`). Resolve it with
  `[override.services.<name>] init = [...]`; an override table with only `init` keeps the
  service's definition, and `init = []` removes the list.

`stack inspect` lists each init list under `init`, with the layer it came from.

## Where data directories live

mise's presets own the data. Each checkout's data directory is
`$MISE_STATE_DIR/daemons/<checkout hash>/data/<service>` (by default under
`~/.local/state/mise`), keyed by the checkout's path. mise creates it on the service's first
start (`initdb` for Postgres) and reuses it afterwards as long as the major version matches.
Stack reads the path from `mise daemons --json`, which reports it before the first start, and
records it for the checkout when `up` claims it. `stack down` and `stack gc` keep data
directories unless asked to remove them (`down --purge`, `gc --reclaim-data`).

## Errors

| Code | When |
|---|---|
| `init_invalid` | An init list names an unknown task, repeats one, or names a task whose `services` exclude the service; or `--reinit` names a service without init tasks |
| `init_failed` | An init task exited non-zero or could not start. Services stay running; fix it and run `stack up` |

[All docs](../README.md)
