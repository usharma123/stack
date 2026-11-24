---
name: verify-stack-session
description: Model-check Stack's session lifecycle (up, restart, down, down --purge, gc, gc --reclaim-data) against crashes, unmounted or deleted checkouts, lease owners that cannot be told, unreadable supervisor records and mounts inside data. Use when changing src/session.rs, src/session/gc_pass.rs, src/lease.rs, src/checkout.rs, src/supervisor.rs record reads, or src/datadirs.rs removal.
---

From the repository root, run `.cursor/skills/verify-stack-session/check.sh`, or `verification/check.sh session`. It needs bash, Java 11+, git, curl, sha256sum or shasum, and Cargo. The runner downloads the TLA+ v1.7.4 jar to `target/verification/tla2tools.jar` and checks its SHA-256 on every run, a cached copy included. It checks `SessionLifecycle.tla` under every config in `expected.tsv` with one worker, then runs the Rust tests in `rust-tests.txt`. It takes about 90 seconds for the models. Set `VERIFY_SKIP_RUST=1` to check the models only, and `CARGO="cargo +1.93.1"` to choose the toolchain. Evidence (each TLC log, `summary.tsv`, `rust.log`, `source.log`) stays under the printed `target/verification/session.*` directory. Read every TLC result, a pass or a counterexample, with pstack `/statechart`.

## What it promises

One checkout with one service, seen by every Stack command that acts on it and by a machine that changes things underneath. Each safety property, and the harm it prevents:

| Invariant | Promise | Harm if broken |
|---|---|---|
| `NoReclaimOfExistingCheckout` | Records, ports and data are dropped as a gone checkout's only when it is deleted or replaced. | An offline volume or bind mount loses its session, ports and data. |
| `ExpiryOnlyForExitedOwner` | A session is stopped for its owner's exit only when the owner has exited. | A running agent's services are stopped under it. |
| `OwnerReportedAliveOnlyIfAlive` | `status` never says an exited owner runs. | An exited owner's session looks alive and is kept forever. |
| `EveryProcessRecorded` | Every live service process is named by the supervisor record on disk, which Stack never writes over or forgets while it cannot read it. | An orphan nobody can find or stop. |
| `ConfirmedMeansStopped` | A down or gc that says everything stopped is right. | A "confirmed" down leaves services holding ports and data. |
| `DataRemovedOnlyWhenStopped` | Data is never removed while a service process runs. | A live database's files are deleted under it. |
| `NoRemovalAcrossMount` | Nothing another mount holds is removed. | A mounted volume's contents are deleted. |
| `MountSeenAtPreflightKeepsData` | A removal whose preflight ran with a mount in the data removes nothing. | Data is half removed and then refused. |

## Action table (src/ at the sync commit)

| Model action | Rust |
|---|---|
| `UpBegin` | `session::up_within` under `project_lock`: `supervisor::readable` before `stop_previous`, ports from the compile, `checkout::location` or the previous session's location (adoption) |
| `UpRecord` | `up_within` saving the `launching: true` record (`record_launch`) before any start |
| `Start` | `start_in_order` → `supervisor::start` → `apply`: strict reads of spec and state, then with no live supervisor `recover` (`scan`: recorded pid+start+boot, then the token by `owned`), refusing (`unconfirmed`) while anything is left, then `spawn`. The supervisor itself refuses to start over a state it cannot read |
| `Verify` | `verify_launch`, then the final `save_with` of `launching: false` |
| `RestartBegin`, `RestartStop`, `RestartRecord` | `session::restart_within`: `stop_and_wait` → `supervisor::stop` + `wait_gone` (strict reads), then the `launching` save, then `Start`/`Verify` |
| `DownBegin`, `DownWait`, `DownConfirm` | `down_inner` → `down_unforgetting` → `down_stopping`: `supervisor::shutdown` (strict `read_spec`/`read_state`, `apply`, `wait_gone_with`), strays by port and carriers of `checkout::TOKEN_VAR`, then `remove_if_exists` of both records. With `--purge`, `purge_locked` |
| `GcBegin` | `gc_pass::gc_with` → `readable_session`, `checkout_state` (`checkout::presence_with`), `candidate`, `gc_locked` re-judging under `project_lock`; `Lease::expired` → `lease::owner_state_with`. The fault `RefreshAdoptsMountPoint` changes `checkout::Refresh::observe_with` |
| `GcGone` | `reclaim_gone`: `supervisor::shutdown`, then `reconcile_gone` (`state_lossy`, then `supervisor::unresolved`), then removing the index |
| `GcExpire` | `gc_locked` → `down_locked` |
| `GcDeleted` | `reclaim_deleted`: `ports::release_gone`, `datadirs::refresh_checkouts`, `Entry::checkout_gone` under `project_lock` |
| `RmDone` | the end of `purge_locked` (`ports::release`), or `reclaim_deleted`'s `supervisor::orphaned_dirs` → `shutdown` + `supervisor::forget` |
| `RmCheck` | `reclaim_data_with`: `datadirs::hold`, the marker, `service_stopped` (`state_lossy` + `unresolved`), `procinfo::carrying` |
| `RmPreflight` | `datadirs::Held::remove_within`: the directory's own mount check, then `walk(Walk::Check)` (every entry's mount via `Boundary::mount`/`entry_mount`, a failed listing refusing) |
| `RmRemove` | `walk(Walk::Remove)`: each entry checked again before `unlinkat` |
| `Status` | `lease::LeaseSummary::at` → `Lease::owner_state` |
| `OwnerExits`, `ReusePid`, `Reboot`, `CommandKilled`, `ServiceDies`, `SupervisorDies`, `RecordUnreadable`, `Unmount`, `Remount`, `Delete`, `Replace`, `MountInside` | the machine |

`OwnerStates` is `lease::owner_state_with`: the boot, then the namespace (Linux), then start times, then `procinfo::liveness`. `Judge` is `checkout_state` over `presence_with`, `same_mount` and `on_mount`. `Found` is what `wait_gone`/`recover` (record) and the token scans (`carrying`, `scan`) can find.

## Bounds and case distinctions

- `Commands = 4` Stack commands and `Events = 3` machine events per behavior, for one checkout, one service, and at most two service processes (`Inst`). There are 32 initial states: Linux or macOS, location or device-only evidence (records Stack 0.1.24 or earlier kept), an owner or none, the owner's namespace, and whether its start time was readable.
- The checkout can be present, offline (an unmounted volume on another device, or a bind mount of the same device on Linux), deleted, or replaced by another directory on the same mount. Any lookup can be refused (`Judgements` adds unknown).
- The supervisor's `state.json` can be readable, a newer Stack's (`schema_version` above 1), or damaged, and it changes underneath commands between any two of their steps. Service processes may have an unreadable environment (`hidden`). A stop may miss its deadline.
- Data can hold a mount inside, one bind-mounted file (Linux), or be a mount point itself, and a mount can appear between the preflight and the removal.

## Atomicity assumptions and what is outside

- One action is one step of the code, including the following. The supervisor spawns a process and records it in one step (the token scan covers the gap in the code). An entry's mount check and its `unlinkat` form one step, because unlinking a mount point fails with EBUSY. A strict read of `state.json` together with what is decided from it is one step.
- Every service process either is recorded by pid, start time and boot, or carries the checkout token. A process that is neither, and whose environment cannot be read, is never seen by Stack. That is a stated limit of the code (`TOOLS_ONLY_CHECKED`), not modeled.
- Outside the model: TTL expiry, which is policy and can stop a live owner's session by design. Also outside: unreadable session records (`recover_unreadable`, covered by `tests/runtime.rs`), legacy Pitchfork sessions, the session's own pid list, `active_executions`, multiple services and dependencies, labels and fleet selection, `stack mv`, ports beyond reserved or released, identities, `pgtemplate`, legacy records with no device at all, and the snapshot journal (see `verify-stack-snapshot`).

## Faults (historical bugs) and expected results

`FALSE.cfg` passes every invariant (4,472,046 distinct states, depth 25). Each `TRUE-*.cfg` puts one bug back and must end with TLC exit 12 and the named violation. `expected.tsv` pins every config's exit code, invariant and distinct-state count, and the runner fails on any difference.

| Config | Bug put back | Violation |
|---|---|---|
| `TRUE-UnknownOwnerExited` | e80afa3: a probe that says nothing (EPERM, hidepid) taken for an exit | `ExpiryOnlyForExitedOwner` |
| `TRUE-ForeignNamespaceTrusted` | 3e18893: an owner named from another PID namespace judged by the PID here | `ExpiryOnlyForExitedOwner` |
| `TRUE-PidOnlyOwner` | 3e18893: alive while any process has the PID | `OwnerReportedAliveOnlyIfAlive` |
| `TRUE-MissingPathIsDeleted` | 93b56b0: a path not found is a deleted checkout | `NoReclaimOfExistingCheckout` |
| `TRUE-DeviceJudgesBindMount` | 8717db4: the recorded device alone judges a missing checkout | `NoReclaimOfExistingCheckout` |
| `TRUE-RefreshAdoptsMountPoint` | 494f225: gc's observation records an empty mount point's mount | `NoReclaimOfExistingCheckout` |
| `TRUE-UnknownSchemaAsEmpty-records` | a68bfdb/62e4fab: an unreadable `state.json` read as nothing and written over | `EveryProcessRecorded` |
| `TRUE-UnknownSchemaAsEmpty-confirm` | the same, in down and gc | `ConfirmedMeansStopped` |
| `TRUE-UnknownSchemaAsEmpty-data` | the same, in the data-removal check | `DataRemovedOnlyWhenStopped` |
| `TRUE-NoRemovalPreflight` | 8549878: the removal walk descends into any directory, with no check first | `NoRemovalAcrossMount` |
| `TRUE-FileMountUnchecked` | 41eb061: a file mounted over is not checked | `MountSeenAtPreflightKeepsData` |

## Rust regressions

`rust-tests.txt` lists the library tests that run the real functions behind the key transitions:

- `lease::tests::*` for `OwnerStates`.
- `checkout::tests::*`, `a_checkout_out_of_reach_is_left_alone_and_a_deleted_one_is_reclaimed` and `a_mount_point_left_where_a_checkout_was_is_never_taken_for_it` for `Judge`, `GcBegin` and `GcDeleted`.
- `a_deleted_checkouts_session_is_kept_while_its_supervisor_record_cannot_be_read` for `GcGone`.
- `a_data_record_*` and `reclaimed_data_holding_a_mount_is_kept_with_its_record` for `RmCheck`.
- `supervisor::tests::*` for strict reads in `apply`, `wait_gone`, `forget` and `unresolved`.
- `datadirs::tests::*` for `RmPreflight` and `RmRemove`.
- `a_process_found_only_by_the_last_rescan_is_still_stopped` for `DownWait`.

The CLI-level tests `a_supervisor_record_of_a_newer_stack_is_refused_and_left_as_it_is` (`tests/runtime.rs`), for `UpBegin`, `DownBegin` and `RestartBegin`, and `a_supervisor_never_starts_over_a_state_a_newer_stack_wrote` (`tests/supervisor.rs`), for `Start`, also cover these transitions but are not run by this script.

## Reading a result

A counterexample is a real bug only if each step can happen in the code. Replay it step by step against the action table, and as a test of the real function where you can. If a step cannot happen, the model is wrong: fix the model, never weaken an invariant. The two modeling errors found while writing this model were strict reads the model first left out (`DownWait` and `RestartStop` must refuse an unreadable record, as `wait_gone` does since 62e4fab).

## Coverage record

Covered files: `src/session.rs` (`up_within`, `restart_within`, `down_*`, `gc_locked`, `reclaim_gone`, `reconcile_gone`, `reclaim_deleted`, `purge_locked`, `reclaim_data_with`, `service_stopped`, `checkout_state`), `src/session/gc_pass.rs`, `src/lease.rs`, `src/checkout.rs`, `src/supervisor.rs` (record reads, `shutdown`, `wait_gone_with`, `recover`, `scan`, `unresolved`, `forget`, `orphaned_dirs`), `src/datadirs.rs` (`hold`, `Held::remove_within`, `walk`), `src/ports.rs` (`release_gone`).

The model was confirmed against these files at `e32c089`. Commit `8e92201` added only tests. If any covered function changes, recheck the action table, the bounds, `expected.tsv` and the Rust list. Move the sync commit forward only after confirming that the model describes the new code and rerunning the check. Use `/maintain-formal-verification-skill` to reconcile intervening changes. If a config is intended to change, update its row in `expected.tsv` and say why in the commit.
