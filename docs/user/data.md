# Service data: init tasks and snapshots

A new checkout starts with empty databases. Two things get it to a usable one:

- **Init tasks** run your migrations and seeds, once for each new database, as part of `stack up`.
- **Snapshots** copy a migrated, seeded database from one checkout so other checkouts of the
  same project can start from it instead of running init again.

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
stack snapshot save seeded     # stops postgres, copies its data, starts and verifies it again
```

In a new worktree:

```sh
git worktree add ../feature && cd ../feature
stack compile --locked
stack up --from-snapshot seeded   # copies the snapshot in before the first start; no init runs
stack status                      # postgres: ready, verified instance; init done
```

The worktree has its own copy of the data, on its own port, verified as its own instance.
Changes there never reach `main`'s database.

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

## Snapshots

```sh
stack snapshot save <name> [--service S]... [--force]
stack snapshot restore <name>
stack snapshot list
stack snapshot rm <name>
```

The MCP tools are `stack_snapshot_save`, `stack_snapshot_restore`, `stack_snapshot_list` and
`stack_snapshot_rm`, and `stack_up` takes `from_snapshot`, `no_init` and `reinit`.

- **What is copied.** The data directories of the project's `postgres` and `redis` preset
  services (`--service` narrows it). Other services cannot be snapshotted
  (`snapshot_unsupported`), and neither can a data directory holding a symbolic link, such as a
  Postgres tablespace outside it.
- **Only the recorded directory.** The data directory is the one `up` recorded for this
  checkout (the record `stack gc --reclaim-data` and `stack mv` use): the supervisor must name
  it, it must still be the same directory, reached with no link followed, and carry this
  checkout's marker. Anything else fails with `snapshot_failed` or `snapshot_unsupported`, and
  nothing is copied or replaced. After a restore the record names the new directory.
- **Only while stopped.** `save` and `restore` take the project lock, stop the affected
  services if they run, and the services that `depends_on` them, and confirm they are gone, as
  `stack restart` does. They check that the supervisor agrees, copy, then start them again (each
  service's `setup` first, dependencies before dependents) and verify each one, reporting every
  step. Services that were not running are left stopped. A stale or half-finished session is refused with
  `session_stale` (run `stack up` first), and running commands with `session_busy`.
- **Copy-on-write** where the filesystem supports it: `clonefile` on APFS, and reflinks through
  `copy_file_range` on Btrfs and XFS. Elsewhere it is a plain copy, so expect the time and space
  of the data's size. Files describing one run of a server (`postmaster.pid`,
  `postmaster.opts`) are left out.
- **Restoring replaces atomically.** The snapshot is copied to a sibling of the data directory,
  then exchanged with it in one step (`renamex_np`/`renameat2`; two renames where the platform
  has no exchange). An interrupted restore leaves the old data or the new, never a mix.
- **Compatibility.** A snapshot records each service's preset and the exact release
  `stack.lock` pinned. Restoring into a different preset or major version fails with
  `snapshot_incompatible` before anything stops.
- **Init.** A snapshot records whether init had completed and against which init list and task
  definitions. Restored data counts as initialised for that digest: `done` if it matches the
  checkout's current one, `stale` otherwise (never run again silently). Data saved before init
  completed is `pending` after a restore.

### `up --from-snapshot`

`stack up --from-snapshot <name>` restores each snapshotted service whose data directory does
not exist yet, before the service first starts, and then starts and verifies as usual. A
service that already has data keeps it (the `restore_snapshot` step lists it under `skipped`),
so the same command can run every time a checkout is created. A missing or incompatible
snapshot fails before anything starts (`snapshot_not_found`, `snapshot_incompatible`).

### Which checkouts share snapshots

Snapshots live in Stack's machine state (`$STACK_STATE_DIR/snapshots/<project id>/`), keyed by
project rather than by checkout path:

- In Git, the project is the repository's common directory (shared by a clone and all its
  worktrees) plus the path of `stack.toml` inside the checkout, so two Stack projects in one
  repository keep separate snapshots. Separate clones of the same remote are separate projects.
- Outside Git, the project is the checkout directory itself.

`stack snapshot list` shows the project identity it used.

### Identity after a restore

Stack verifies Postgres and Redis by asking the server for its data directory and comparing it
with the one the supervisor reports for this checkout. Neither server stores that path in its
data: Postgres takes it from `-D` and Redis from `--dir` when they start. Restored data lives
at the restoring checkout's own path, so the server reports that path and verifies as this
checkout's instance, not the one that saved it. Identity tokens of probed services are kept in
Stack's machine state per checkout, never in a data directory.

## Where data directories live

mise's presets own the data. Each checkout's data directory is
`$MISE_STATE_DIR/daemons/<checkout hash>/data/<service>` (by default under
`~/.local/state/mise`), keyed by the checkout's path. mise creates it on the service's first
start (`initdb` for Postgres) and reuses it afterwards as long as the major version matches.
Stack reads the path from `mise daemons --json`, which reports it before the first start, and
records it for the checkout when `up` claims it. Snapshots copy and replace only that recorded
directory. `stack down` and `stack gc` keep data directories unless asked to remove them
(`down --purge`, `gc --reclaim-data`).

## Errors

| Code | When |
|---|---|
| `init_invalid` | An init list names an unknown task, repeats one, or names a task whose `services` exclude the service; or `--reinit` names a service without init tasks |
| `init_failed` | An init task exited non-zero or could not start. Services stay running; fix it and run `stack up` |
| `snapshot_not_found` | No snapshot of that name for this project; the hint lists the ones there are |
| `snapshot_exists` | `save` to a name in use; pick another, `rm` it, or `--force` |
| `snapshot_incompatible` | The snapshot's preset or major version differs from this checkout's |
| `snapshot_unsupported` | A service that is not a postgres or redis preset, or data holding links or special files |
| `snapshot_failed` | The supervisor reports no data directory, Stack has no record of it for this checkout, it is not the recorded directory or lacks this checkout's marker, or a service has no data yet (run `stack up`) |
| `stop_unconfirmed` | A service could not be confirmed stopped; nothing was copied |

[All docs](../README.md)
