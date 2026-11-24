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
  nothing is copied or replaced. After a restore the record names the new directory, and the
  old data is removed as `gc --reclaim-data` removes data: never across a mount inside it (it
  is left where it was set aside, `.<name>.stack-old` or `.<name>.stack-restore`, instead).
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
- **Restoring replaces atomically, every service or none.** Each service's snapshot data is
  copied to a sibling of its data directory before any is replaced (a copy that fails removes
  the copies: nothing changed). A journal (`$STACK_STATE_DIR/init/<checkout>.restore.json`)
  then records each swap, and each data directory, held open and checked as `up` recorded it
  and on its parent's mount, is exchanged with its copy in one step
  (`renameatx_np`/`renameat2`; two renames where the filesystem has no exchange). The old data
  is kept until every service's copy is in place; only then, once the checkout is shown to be
  the one that restored and each copy and record to be as the restore left them, are the init
  markers written (in one write), the record bound to the new directories and the old data
  removed. Otherwise the journal and the old data stay (`restore_incomplete`, the services that
  no longer match listed as `manual_recovery`). The snapshot is read again under the store's
  lock once its services are stopped: one replaced meanwhile (a `save --force` from another
  checkout) is refused with `snapshot_failed` before anything is replaced.
- **A failed restore says what it changed.** A swap that fails puts back every service it
  replaced, judged by which directory is where: the error (`snapshot_failed`) lists each
  service as `unchanged` or `rolled_back`, and nothing is recorded as restored. A service that
  cannot be put back keeps its old data where it is: the error is `restore_incomplete`, listing
  that service as `manual_recovery` with where its old data (`old_data`) and the restored copy
  (`restored_copy`) are, and the journal. Its services are left stopped (`left_stopped`), never
  started on that data, and `up` refuses to start them until the restore is finished: move the
  old data back as details say and remove the journal.
- **An interrupted restore is finished before anything else touches the data.** The next
  `snapshot save`, `snapshot restore` or `up` of the checkout (its `recover_restore` step)
  undoes a restore interrupted before every copy was in place, and records one interrupted
  after, provided every service its journal names is confirmed stopped, whichever services
  the command named: `snapshot save` and `restore` stop those too, and a journaled service
  `stack.toml` no longer defines must already be shown not to run (`stop_unconfirmed`
  otherwise). `restart` refuses with `restore_incomplete` while such a journal exists.
- **Saving publishes whole.** `save` builds the snapshot in a staging directory of the store
  and writes its `meta.json` last. `save --force` then exchanges it with the published
  snapshot in one step where the filesystem can (`renameatx_np`/`renameat2`; the step reports
  `publish: exchange`), so the name always holds a complete snapshot. Elsewhere the old one is
  renamed aside (`.old-<name>-…`) first (`publish: rename`). If Stack is interrupted between the
  two renames, `snapshot list`, `restore` and `up --from-snapshot` read the generation set
  aside, and the next snapshot operation moves it back before it removes any staging directory.
  A generation counts only when its `meta.json` parses and names it, so damaged metadata at the
  name never hides, or has removed, a valid generation set aside; a read or rename that fails
  is reported, with what was set aside kept. A staging directory whose owning process still
  runs is never removed.
- **Compatibility.** A snapshot records each service's preset and the exact release
  `stack.lock` pinned. Restoring into a different preset or major version fails with
  `snapshot_incompatible` before anything stops.
- **Init: restored data replays what its source would have.** A snapshot records the init state
  of each service's data when it was saved, and a restore gives the restored data that state,
  never more:
  - saved with init `done` (or `stale`): restored data counts as initialised for the init list
    and task definitions it ran: `done` if they match the checkout's current ones, `stale`
    otherwise. No init task runs (until `up --reinit`).
  - saved with init `failed` after completing some tasks (`init_resume` in the snapshot's
    metadata): restored data is `failed`, and the next `up` resumes from the failed task,
    skipping the completed ones whose definitions have not changed, exactly as it would have
    in the checkout it was saved from.
  - saved `pending`, or failed before completing a task: restored data has no init marker, and
    the next `up` runs the whole init list against it.

  The marker names the restored directory, so a data directory recreated later runs init again.
  A restore that fails or is rolled back leaves every marker as it was.

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

Stack runs the presets itself and owns their data. A new checkout's data directory is
`$STACK_STATE_DIR/data/<checkout key>/<service>` (by default under `~/.local/state/stack`),
keyed by the checkout's path. The supervisor creates it on the service's first start (for
Postgres, a copy of what `initdb -U postgres --auth=trust` writes under the pinned `C.UTF-8`
locale, put in a sibling directory renamed into place; see *Postgres templates* below) and
records the preset and major version in it
(`.mise-daemon-version`, the file mise's presets wrote); it reuses the directory afterwards only
while the major version matches, and refuses it otherwise, kept as it is. It also refuses to
initialise data with a server whose major version is not the one stack.lock pins (one found on
PATH because the pinned release is not installed). A checkout an older Stack ran keeps the
directory mise's preset made for it (`$MISE_STATE_DIR/daemons/<hash>/data/<service>`): Stack
finds it through its own data record, the session's record, or mise's state for that path, and
never initialises a new one in its place. Stack records the directory for the checkout when `up`
claims it. Snapshots copy and replace only that recorded
directory. `stack down` and `stack gc` keep data directories unless asked to remove them
(`down --purge`, `gc --reclaim-data`).

### Postgres templates

`initdb` takes about half a second per new database, more when several checkouts start at
once. Stack runs it once for each Postgres release, locale and time zone and keeps the result
as a template in `$STACK_STATE_DIR/templates/postgres/<key>/` (about 40 MB each); a new data
directory is a copy of it, made in milliseconds. On APFS, Btrfs and XFS the copy shares the
template's blocks until either is written, so a new database takes almost no space at first
(`stack ps` still counts each one in full). On macOS a data directory on another volume than
Stack's state directory, or on a filesystem that cannot clone, is made by `initdb` as before.

- The first start that needs a template builds it; checkouts starting at the same time wait for
  that one build instead of each running `initdb`. Reinstalling or upgrading the release,
  changing a file it installs, the locale or the time zone, or (on Linux) the system C
  library, keys another template.
- A copy is the cluster `initdb` would have made, with its own system identifier and
  authentication nonce: Postgres stamps each cluster with a random identifier, which
  replication, `pg_rewind`, WAL archives and backup tools use to tell clusters apart, so Stack
  gives each copy a new one. It does this only for Postgres releases whose file layout it has
  checked (17 and 18); for others every new database is made by `initdb`. A template build it
  cannot do this for (a damaged control file, say) is never used or kept, and the database is
  made by `initdb` instead. A published template it cannot do this for is never used either:
  each new database that copies it first tries to remove it, waiting up to 5 seconds for the
  template's lock, and the service's log says what came of that (removed, or not and why).
  File times and the time of the cluster's first checkpoint come from when the template was
  built.
- A template is kept only when `initdb` was not held back by the machine (on a machine short of
  shared memory it lowers `max_connections`): such a build is used for that one database and
  the next new database tries again. A template is flushed to stable storage before it is
  used; one that could not be is used only until the machine restarts.
- Stack never runs a server on a template, and checks after every copy that nothing changed it
  since it was built; one that changed is never used, `initdb` runs instead, and Stack tries to
  remove the template the same way. Anything else that goes wrong with a template, including
  a full disk where templates are kept, also makes the database by `initdb`; the service's log
  (`stack logs postgres`) says which way a database was made.
- `stack gc` removes templates whose release was reinstalled or removed, and ones unused for 14
  days; `stack gc --dry-run` lists the ones a pass would try to remove. Deleting the directory
  by hand is safe while no `stack up` is starting a new database: the next one builds what it
  needs.

## Errors

| Code | When |
|---|---|
| `init_invalid` | An init list names an unknown task, repeats one, or names a task whose `services` exclude the service; or `--reinit` names a service without init tasks |
| `init_failed` | An init task exited non-zero or could not start. Services stay running; fix it and run `stack up` |
| `snapshot_not_found` | No snapshot of that name for this project; the hint lists the ones there are |
| `snapshot_exists` | `save` to a name in use; pick another, `rm` it, or `--force` |
| `snapshot_incompatible` | The snapshot's preset or major version differs from this checkout's |
| `snapshot_unsupported` | A service that is not a postgres or redis preset, or data holding links or special files |
| `snapshot_failed` | The supervisor reports no data directory, Stack has no record of it for this checkout, it is not the recorded directory or lacks this checkout's marker, or a service has no data yet (run `stack up`); or a restore failed and every service was put back (details list each as `unchanged` or `rolled_back`) |
| `restore_incomplete` | A restore could not put every service back, could not be recorded, or was interrupted and cannot be undone; details list each service (`manual_recovery` with `old_data`), its services stay stopped, and `up` and `restart` refuse until it is resolved |
| `stop_unconfirmed` | A service could not be confirmed stopped; nothing was copied |

[All docs](../README.md)
