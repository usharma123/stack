---
name: verify-stack-snapshot
description: Model-check Stack's snapshot store publication (save --force by exchange or two renames, recovery of an interrupted save) and multi-service restore (copies, journal, swaps, rollback, commit checks, init markers and data records, replay by the next command). Use when changing src/snapshot.rs, session::with_stopped, init_tasks markers, or datadirs swap, rebind or remove_tree.
---

From the repository root, run `.cursor/skills/verify-stack-snapshot/check.sh`, or `verification/check.sh snapshot`. It needs bash, Java 11+, git, curl, sha256sum or shasum, and Cargo. The runner downloads the TLA+ v1.7.4 jar to `target/verification/tla2tools.jar` and checks its SHA-256 on every run, a cached copy included. It checks `SnapshotAtomicity.tla` under every config in `expected.tsv` with one worker (a few seconds), then runs the Rust tests in `rust-tests.txt`: library tests by path, integration tests as `tests/<crate>.rs <name>`. Set `VERIFY_SKIP_RUST=1` to check the models only, `CARGO="cargo +1.93.1"` to choose the toolchain and `CARGO_BUILD_JOBS` to bound the build. Evidence stays under the printed `target/verification/snapshot.*` directory. Read every TLC result with pstack `/statechart`.

## What it promises

| Invariant | Promise | Harm if broken |
|---|---|---|
| `ReaderFindsGeneration` | A reader without the lock always finds a generation of the snapshot, reading one file or listing at a time while a `save --force` runs. | `snapshot_not_found` after a crashed `save --force`, or during one without an exchange. |
| `ValidGenerationNeverLost` | A valid generation is always published or set aside. | The snapshot is gone. |
| `RestoresChosenGeneration` | A restore copies only the generation it chose before stopping the services. | Data from another checkout's newer save, for services it never stopped. |
| `AllOrNothing` | With no journal pending, every service holds its old data, or every one its restored copy. | Services start on a mix of old and restored data. |
| `OldDataKeptUntilRecorded` | Old data is removed only once every service holds its copy and its marker and record describe it. | No way back from a restore that went wrong. |
| `MarkersDescribeData` | With no journal pending, each init marker and data record describes the data in place. | Init skipped or rerun on the wrong data, or data reclaimed by a stale record. |
| `NeverTouchesRunningService` | No restore step moves a running service's data. | A database's files are swapped under it. |
| `NoStartOnPendingRestore` | No service starts on data a restore has still to finish or undo. | A service runs on half-restored data. |

## Action table (src/snapshot.rs at the sync commit unless named)

| Model action | Rust |
|---|---|
| `ReadGen` | `lookup` when nothing moves between its reads: under the store lock (`RestoreLock`), and a restore's own first read, taken as one step (`generation`, `set_aside_read`, `meta_at`: a generation counts only when its meta.json parses and names it) |
| `ReaderStart`, `ReadName`, `ReadAside`, `ReadFinal` | `lookup` without the lock (`read_generation` for restore's first read and `up --from-snapshot`'s check, `list`), one read per step: the name's meta.json, the generations set aside (keeping the metadata read with the directory chosen), round again while what it saw set aside changed, then the name a last time. `names` lists the published names again after the generations set aside for the same reason |
| `ReadAgain` | (`RereadChosen` only) the reader before `bd9d2fd`: `read_generation` read the chosen directory's meta.json a second time |
| `RecoverStore` | `recover_store`: put back the newest valid `.old-*` when the name holds no valid generation (`recover_stray`, `recover_old`), then remove staging nobody owns |
| `SaverLock` … `SaverDone` | `save`'s store part under `store_lock`: `recover_store`, the `.tmp-*` built (meta.json last), `publish`: `store_rename` onto nothing, `exchange` and `remove_tree`, or `aside` then `publish` then `remove_tree(old)` |
| `RestoreBegin` | `restore`: `read_generation` before `session::with_stopped`, which stops every service named and every service `journaled` names. A first restore, and one retry while none committed |
| `SaveBegin` | `save` → `with_stopped` → `recover_restore(confirmed)` (its publication is the saver's) |
| `StopRefused` (in both) | `with_stopped`'s `stop_unconfirmed`: a journaled service `stack.toml` no longer defines that may still run, a service the supervisor still reports running once stopped, or `supervisor::unresolved`; it returns before the operation, touching no data and starting nothing it stopped |
| `RestartB` | `session::restart_within` → `refuse_pending` |
| `UpBegin`, `UpStart` | `up_within` → `recover_before_start` (refuses while a journaled service may run, or `supervisor::unresolved`), then the starts |
| `Recover` | `recover_restore`: `stop_unconfirmed` unless every journaled service is in `stopped`, then `commit` (committed) or `undo_all` (swapping) |
| `RestoreLock` | `restore` under `store_lock`: `recover_store`, `read_generation` again, `check_same` |
| `RestoreCopy` | `restore_services` step 1: `copy_tree` into `.<dir>.stack-restore-*` for every service, `discard` on failure |
| `RestoreJournal` | step 2: `write_json(journal_path, phase: "swapping")` |
| `RestoreSwap`, `RestoreSwap2` | step 3: `swap_in` → `datadirs::Held::swap_in` (`exchange_at`, else `renameat` to aside, then the copy in, put back on failure); `FAIL_SWAP`/`CRASH_AFTER` in tests |
| `RestoreCommit` | step 4: the journal rewritten `committed`; a failed write goes to `roll_back` |
| `CommitCheck` | `check_committed`: token, `dir_at(target) == new`, `Rebind::matches` |
| `CommitMarkers` | `init_tasks::set_markers` (one write) |
| `CommitLedger` | `datadirs::rebind` (one write, all or nothing) |
| `CommitClean`, `CommitUnjournal` | `commit`'s `datadirs::remove_tree(at, old)` per service, then `fs::remove_file(journal)` |
| `UndoStep` | `undo` (by `dir_at` of target, fresh and aside) in `undo_all`; `FAIL_UNDO` in tests |
| `Finish` | the end of `with_stopped`: start what it stopped, except after `restore_incomplete` (`left_stopped`) |
| `CommandKilled`, `SaverKilled`, `DamageMeta`, `EvidenceChanges` | the machine and other programs |

## Bounds and case distinctions

- Two services (`a`, `b`), two generations (`g1` published, `g2` from another checkout's single `save --force`). `Commands = 4` commands of this checkout and `Events = 3` environment events per behavior, with a `restore` and one retry while none has committed (a retry after a refusal or an interrupted attempt, which recovers that attempt's journal first). Data is labelled old and new relative to the restore that commits; later commands replay it. One lock-free reader runs once, at any point, against that one save. There are two initial states: a filesystem with an exchange, and one without, which uses two renames for both publication and swaps.
- Distinguished cases: a save published by exchange and by two renames, a crash between those renames, and a published generation whose metadata is damaged while the old one is set aside. A restore can fail or be killed at each copy, journal write, swap, rename, commit write, marker write, record write, cleanup and undo, and each undo can fail. While a committed restore is unrecorded, the restored copy can be replaced, the data record rebound, or the checkout replaced (another token).

## Atomicity assumptions and what is outside

- One action is one rename, exchange, synced file write (`write_json` fsyncs before its rename) or directory removal. `recover_store`'s two renames are one step: between them, readers read the generation set aside. Each `undo` of one service is one step: `undo` judges only by where each directory is, and its test covers every position. An exchange's internal check is part of the exchange.
- The lock-free reader takes one read per step against one save. `lookup` is exact for that: a reader that misses the name, then misses a generation set aside because the save removed it, reads the name after the save published there. Only a second save completed within one reader's few reads can hide the snapshot from it again; `lookup` then goes round while what it saw set aside changes, at most 100 times, and that churn is outside the model (one save per behavior). The restore's own first read is one step (`ReadGen`); whatever generation it could find, `RestoreLock` compares under the lock.
- `DamageMeta` damages the published generation only after a killed save left the old one aside. Damage to the only valid generation, or during a running publication, cannot be recovered by design and is not modeled.
- Evidence while a command holds the locks. `EvidenceChanges` (another directory at a restored copy's place, a data record rebound, the checkout replaced) happens only between commands. While a command replays a committed restore under the project lock, between `check_committed` and the marker, record and cleanup writes, Stack assumes no process of the same user changes that evidence: such a process takes none of Stack's locks, and `datadirs::rebind` and `remove_tree` act on the directories they hold, so a change in that window is not detected. A same-user process can change any file, so this is a trust assumption, not a guarantee.
- Crashes, not power loss. `CommandKilled` and `SaverKilled` are a process ending between two steps, with every step done before it durable. `write_json` syncs a file before its rename, but no directory is synced after a rename or removal, and copies are not synced (`state::sync_file`: Stack does not claim durability across power loss). After power loss a rename, an exchange or a removal can be undone or reordered, which the model does not explore.
- Outside the model: `up --from-snapshot` into data directories that do not exist yet (`Swap.old = None`, `restore_before_start`), copy integrity (`copy_tree`, `check_tree`), `snapshot rm`, compatibility checks, the staging owner's liveness (`live_elsewhere`), mounts inside old data (`remove_tree` keeps it; see `verify-stack-session` for removal), and a service run by something other than Stack.

## Faults (historical bugs) and expected results

`FALSE.cfg` passes every invariant (4,235,172 distinct states). Each `TRUE-*.cfg` must end with TLC exit 12 and the named violation. Each `WITNESS-*.cfg` checks the opposite of something that must be reachable, and must violate it: `WITNESS-StopUnconfirmed` (`NoUnconfirmedRefusal`), `with_stopped` refusing with a service unconfirmed, which the baseline otherwise never exercised (`recover_restore`'s own `stop_unconfirmed` stays reachable only under `ReplayUnconfirmed`, since `with_stopped` confirms every journaled service first); `WITNESS-RetryRecovers` (`RetryNeverCommits`), a restore retried after an interrupted one that recovers it and commits. `expected.tsv` pins every config's exit code, invariant and distinct-state count.

| Config | Bug put back | Violation |
|---|---|---|
| `TRUE-NoAsideRecovery` | fbd8580: two renames only, nothing read or recovered from `.old-*` | `ReaderFindsGeneration` |
| `TRUE-InvalidPublishedHidesAside` | e2b22c0: damaged metadata at the name counts as a generation, and the valid one set aside is removed | `ValidGenerationNeverLost` |
| `TRUE-NoGenerationRecheck` | 91a579b: no `check_same` under the store lock | `RestoresChosenGeneration` |
| `TRUE-OneAtATime` | 5af8555: services replaced one at a time, old data removed at once, no journal | `AllOrNothing` |
| `TRUE-CommitWithoutChecks-records` | 474434a: a committed restore recorded without `check_committed` | `MarkersDescribeData` |
| `TRUE-CommitWithoutChecks-old` | the same, removing old data | `OldDataKeptUntilRecorded` |
| `TRUE-RestartIgnoresJournal` | 91a579b: `restart` starts services while a journal is pending | `NoStartOnPendingRestore` |
| `TRUE-ReplayUnconfirmed` | 91a579b: recovery with only the requested services confirmed stopped (with `RestartIgnoresJournal`, which alone keeps this invariant) | `NeverTouchesRunningService` |
| `TRUE-RereadChosen` | before `bd9d2fd`: a lock-free reader read the chosen directory's meta.json again, after a save without an exchange could have moved or removed it (found by this model's review, and by `readers_racing_forced_saves_without_an_exchange_always_find_a_generation`: 270 of 7,469 read rounds missed during 2,281 saves) | `ReaderFindsGeneration` |

## Rust regressions

`rust-tests.txt` lists the `snapshot::tests` that run the real functions:

- For the store: `a_forced_save_replaces_the_published_generation_whole`, `a_save_interrupted_between_its_renames_…`, `recovery_takes_the_newest_valid_generation_…`, `a_published_generation_whose_metadata_is_malformed_…` and the failed-rename tests.
- For the lock-free reader: `readers_racing_forced_saves_without_an_exchange_always_find_a_generation` (a saver thread publishing by two renames against `read_meta`, `names` and `generation` for three seconds) and `a_reader_that_finds_neither_the_name_nor_a_generation_set_aside_reads_the_name_again` (the interleaving one step at a time, through the `BETWEEN_READS` hook).
- For `RestoreLock`: `a_restore_goes_ahead_only_with_the_generation_whose_services_were_stopped`.
- For the restore itself: `a_restore_replaces_every_service_…`, `a_swap_failing_midway_…`, `a_data_directory_replaced_after_its_check_…`, `a_service_that_cannot_be_put_back_…`, `undoing_a_swap_reads_where_each_directory_is`, `a_restore_killed_between_the_two_renames_of_a_swap_…`, `an_interrupted_restore_is_undone_before_it_commits_and_finished_after`, `a_committed_restore_is_recorded_only_while_…`, `recording_a_committed_restore_can_be_interrupted_anywhere_…`, `an_interrupted_restore_is_finished_only_with_every_service_it_names_confirmed_stopped` (which also asserts `refuse_pending`), and `restored_data_carries_the_init_state_…`. `an_interrupted_restore_is_undone_before_it_commits_and_finished_after` is also the retry `WITNESS-RetryRecovers` reaches: interrupted, recovered, run again.
- `tests/data_lifecycle.rs` `an_interrupted_restore_is_finished_only_with_every_service_it_covers_stopped` for `StopRefused` (a journaled service `stack.toml` no longer defines, still running: `stop_unconfirmed`, nothing stopped or touched) and for `SaveBegin`'s recovery. The other CLI-level tests there cover the same paths end to end; this script does not run them.

## Reading a result

Replay a counterexample against the action table and the real functions before calling it a bug. If a step cannot happen, fix the model, never the invariant. The modeling errors found while writing this model were these. A layout slip let an `ELSE` swallow a conjunct. `OldDataKeptUntilRecorded` first judged environment changes made after Stack's removal, so it now records Stack's removal step. A second restore conflated the old and new labels, which led to the one-restore bound.

## Coverage record

Covered files: `src/snapshot.rs` (store: `generation`, `set_aside`, `recover_store`, `publish`, `read_generation`, `check_same`; restore: `restore_services`, `swap_in`, `undo`, `undo_all`, `roll_back`, `check_committed`, `commit`, `recover_restore`, `journaled`, `refuse_pending`, `recover_before_start`, `save`, `restore`), `src/session.rs` (`with_stopped`, `restart_within`'s `refuse_pending`), `src/datadirs.rs` (`Held::swap_in`, `exchange`, `rebind`, `Rebind::matches`, `remove_tree`), `src/init_tasks.rs` (`set_markers`).

The model was confirmed against these files at `e32c089`. Commit `8e92201` added only tests. `bd9d2fd` changed the store's readers (`lookup`, `set_aside_read`, `read_generation`, `names`, `list`); the reader actor, `StopRefused` and the restore retry were added with it, confirmed against `src/snapshot.rs` at `bd9d2fd` and `src/session.rs` (`with_stopped`) unchanged since `e32c089`. If any covered function changes, recheck the action table, the bounds, `expected.tsv` and the Rust list. Move the sync commit forward only after confirming that the model describes the new code and rerunning the check. Use `/maintain-formal-verification-skill` to reconcile intervening changes.
