---
name: verify-stack-ports-mcp
description: Model-check Stack's machine-wide port registry across checkouts (compile, compile/up --reassign-ports, pins, the ephemeral range, down --purge, gc, checkouts out of reach or deleted, interleaved compiles) and the stdio MCP server's admission (worker and waiting bounds, the reader's bounded queue, per-checkout order, independent work, cancellation, batches, signals against admission, input ending), with Rust state machines for log cursors, --follow and CLI parsing. Use when changing src/ports.rs, src/project.rs port requests, up's port check or --reassign-ports, src/mcp/server.rs, src/logs.rs, src/supervisor.rs log reading, or src/main.rs's run and --project parsing.
---

From the repository root, run `.cursor/skills/verify-stack-ports-mcp/check.sh`, or `verification/check.sh ports-mcp`. It needs bash, Java 11+, git, curl, sha256sum or shasum, and Cargo. The runner downloads the TLA+ v1.7.4 jar to `target/verification/tla2tools.jar` and checks its SHA-256 on every run, a cached copy included. It checks `PortAndMcpContract.tla` under every config in `expected.tsv` with one worker (about two minutes, the baseline nearly all of it), then runs the Rust tests in `rust-tests.txt`: library tests by path, integration tests as `tests/<crate>.rs <name>`, the binary's own as `src/main.rs <name>`. Set `VERIFY_SKIP_RUST=1` to check the model only, `CARGO="cargo +1.93.1"` to choose the toolchain and `CARGO_BUILD_JOBS` to bound the build. Evidence (each TLC log, `summary.tsv`, `rust.log`, `source.log`) stays under the printed `target/verification/ports-mcp.*` directory. Read every TLC result, a pass or a counterexample, with pstack `/statechart`.

## Shape

One module, two machines that share no variable, checked as a disjoint union: `Init` picks `part` ("ports" or "mcp") and only that machine's actions run. Every config therefore checks both machines; the distinct-state counts are the two spaces added, not multiplied. A behavior ends (`Terminated`) when the port machine has used `Steps` steps or the server has exited; any other state without a successor would be reported as a deadlock.

## What it promises

| Invariant | Promise | Harm if broken |
|---|---|---|
| `NoSharedReservation` | The registry never reserves one port twice, nor two ports for one service of a checkout. | Two checkouts' services are configured on one port; one cannot start, or a command reaches the other's. |
| `GivenIsReserved` | A checkout not shown to be gone (present, or out of reach) keeps every port it was given until its own compile, reassignment or purge changes it, whatever other checkouts do. | Another checkout takes a port this one's configuration and running services use. |
| `AssignedOutsideEphemeral` | An assigned (unpinned) port is never one outgoing connections may take. | A stopped service's port is held by a connection or its TIME_WAIT; it cannot start though nothing listens. |
| `ReassignMoves` | `compile --reassign-ports` and `up --reassign-ports` never give a service back a port its checkout gave up. | The reassign is a no-op; the next `up` hits the same conflict. |
| `OwnPinNeverConflicts` | A pinned port fails only for another checkout's (or another pin's) reservation, never for a port the same compile chose for one of the checkout's own services. | Every compile of that checkout fails with a `port_conflict` naming itself. |
| `WorkerBound`, `WaitingBound`, `PendingBound` | At most MaxRunning calls hold a worker, MaxWaiting wait (one more is refused at once, retryable), MaxPending lines wait for the loop (the client's writes block past it). | A client makes the server create threads or hold memory without limit. |
| `OneCallPerCheckout` | One call at a time per checkout. | Two lifecycle calls race on one checkout. |
| `PerCheckoutOrder` | Calls on one checkout start in arrival order (items of a batch in their order), or never run (cancelled or dropped). | `up` then `exec` run as `exec` then `up`. |
| `WorkConserving` | While a worker is free and no signal came, every waiting call waits for its own busy checkout, never behind another checkout's. | One busy checkout stalls independent work on others. |
| `CancelledNeverRuns`, `CancelledNeverAnswered` | A cancel that finds its call waiting, or given a worker but before its tool, keeps the tool from running; a cancelled call gets no response. | A cancelled `up` starts services; a client gets a response it said it no longer wants. |
| `NoWorkerAfterSignal`, `NoToolAfterSignal` | Once a signal that ends the server came, no call gets a worker, and no call's tool begins (settled at the worker's admission point). | A call the server will not wait for starts services. |
| `BatchSettles` | A batch the loop read goes out exactly once by the time the server exits (dropped and cancelled items owe nothing). | A client waits forever for a batch response. |
| `AcceptedAnswered` | Once input ends without a signal, every accepted call that was not cancelled runs and is answered before the server exits. | `echo calls | stack mcp` loses answers. |

## Action table: ports (src/ at the sync commit)

| Model action | Rust |
|---|---|
| `CompileRead(c, r)` | `project::compile` (write mode) → `ports::assign(state, root, requests, reassign_ports)` → `assign_outside`: `FileLock::acquire(ports.lock)`, `read_json(ports.json)` |
| `CompileWrite(c)` | the rest of `assign_outside`: `retain(!checkout_gone())` (`Pruned`), dropping the checkout's own when reassigning, then per request in order (`Order`): a pin checked against every other reservation (`port_conflict`), else a reservation outside the ephemeral range reused, else `find_free` (in range, not ephemeral, not reserved, not `excluded` = the checkout's pins and, reassigning, the ports it gives up, and one `TcpListener::bind` accepts: `Listening`); `write_json` only on success; the lock released. The checkout renders what it got (`given`: `mise::render`) |
| `Up(c)` | `session::up_within`'s port step: each assigned port that accepts connections without this checkout's service behind it is a `port_conflict` (nothing starts); else every service starts on its rendered port |
| `UpReassign(c)` | `session::prepare::up_reassigning` on a `port_conflict` at `ports` naming no pinned port → `ports::reassign(state, root, services)` (old reservation kept while `find_free` chooses) → `up_within` again |
| `Down(c)`, `Purge(c)` | `down` (reservations kept), `down --purge` → `ports::release` |
| `Gc` | `gc` → `ports::release_gone(state, keep)`, `keep` = a session still recorded (`run[c] # NoPorts`) |
| `Unmount`, `Remount`, `Delete`, `Reap` | `checkout::presence` = `Unknown` (lookup refused, volume unmounted), `Present` again, `Absent`; the deleted checkout's supervisor confirming its services gone |
| `ForeignListen(p)`, `ForeignExit` | another program |

`Gone(c)` is `checkout::gone` (`presence == Absent`). The registry lock is `FileLock` on `ports.lock` (flock): every registry write (`assign`, `reassign`, `release`, `release_gone`, `rekey`) takes it; `lookup` and `all` read without it (reports only; writes replace the file atomically).

## Action table: MCP (src/mcp/server.rs at the sync commit)

| Model action | Rust |
|---|---|
| `SendCall`, `SendBatch`, `SendCancel`, `Close` with `Room` | the client writing a line; `read_frames` takes `Credit` (at most `MAX_PENDING_FRAMES` lines read and not handled) before reading each, then `Event::End` after the last |
| `HandleLine` (call) | `Server::line` → `message` → `request("tools/call")`: refused with `BUSY` (-32000, retryable) when `waiting.len() + deferred.len() >= MAX_WAITING`, else `accept` (checkout fixed now: `project_key`) and `admit` |
| `HandleLine` (batch) | `Server::batch`: a `Batch` owing one hold, then each item `expect`ed and dispatched in order (`ping` answered at once), then the hold released (`skip`); sent once nothing is owed and a response is in (`Batch::add`) |
| `HandleLine` (cancel) | `notifications/cancelled` → `Server::cancel`: an in-flight call's `Cancel` fires; a waiting one is removed and `skip`ped |
| `Admit` / `Scan` | `Server::admit`: once `stopping()`, `drop_waiting`; else oldest first while `running < MAX_RUNNING`, passing over a call whose checkout is `busy` (it keeps its place); `start` gives a worker (`running += 1`, `busy.insert`) |
| `Admission(i)` | the worker's admission point in `start`: the tool runs only if `!cancel.is_cancelled() && !signalled()`; else no response, the batch owed nothing, `Ended` sends `Done` |
| `ToolEnd(i)` | the tool returns: `reply.ok`, or `reply.skip` when its `Cancel` fired; `Ended` sends `Done` |
| `Done(i)` | `Event::Done` → `Server::done` (`running -= 1`, `busy.remove`) → `admit`; also in `finish` |
| `HandleEnd` | `Event::End` → the loop breaks → `finish`: while `running > 0`, handle `Done` (admitting waiting calls as workers free) |
| `Signal`, `SeeSignal` | a signal passed on to a command (`process::passed_on`); the loop's check at its top and in `finish` → `drop_waiting` |
| `Exit` | `finish` returns once `running == 0`; `serve` returns |

`Done` events and lines arrive on one channel from different threads, so their relative order is arbitrary (the model lets the loop take either); lines keep their order and `End` comes after every line.

## Bounds and case distinctions

- Ports: checkouts c1 (db, web) and c2 (db, web pinned to port 1), ports 1..5 with 5 in the ephemeral range, and in half the worlds a reservation c1.db = 5 from before the range moved. `Steps = 5` commands and events per behavior (a compile's read counts, its write does not). `find_free`'s hash-seeded scan is any eligible port (over-approximation).
- MCP: four calls, each on checkout A, B or none, the last two optionally in one batch with a ping, at most one cancel, a signal at any time; MaxRunning 2, MaxWaiting 2, MaxPending 1 for Rust's 16, 256, 16. Every rule compares a count with its bound, so these reach the same cases (at, below and past each bound); the Rust tests use the real constants. A and B are interchangeable: B is used only once A is.
- Symmetry and ordering cuts that lose no rule: calls are sent in id order; the batch holds the last two.

## Atomicity assumptions and what is outside

- A compile's read and write are separate steps (interleaving checked without the lock); the rest of a registry operation is one step under the lock. `find_free`'s bind check and the write are one step: another program binding a chosen port before the service starts makes `up` report `port_conflict` (`LIMIT-ReservedPortTaken`), never a shared port.
- `up`'s port check and its starts are one step: a program that binds the port in between makes the start fail (`start_failed`), not cross-talk.
- One registry per state directory: checkouts using different `STACK_STATE_DIR`s are not isolated from each other. `rekey` (`stack mv`) and reservations an earlier Stack made without a recorded mount (their presence is completed by observation, `checkout::Refresh`) are not modeled; `checkout::presence` itself is covered by its unit tests.
- MCP: the admission check and the tool's first step are not one atomic step (`LIMIT-SignalDuringTool`); a signal during a tool is passed on to the commands it runs. Roots deferral (`deferred`, `roots/list`) is not modeled: it holds calls ahead of `accept`, counts toward MAX_WAITING, and its cancel and drop paths are covered by the Rust tests. Duplicate request ids (a client protocol violation) are not modeled: a second call with an in-flight call's id would replace its `Cancel`. A tool that never returns keeps its checkout busy (fairness is only as good as tool deadlines).
- Not modeled, covered by Rust state machines: log cursors and `--follow` (below), `run`'s flag forwarding and `--project` spelling.

## Log cursors, --follow and CLI parsing (Rust state machines, not in the model)

`logs::tests::a_seeded_log_read_by_cursor_and_by_follow_returns_each_record_once_or_reports_its_gap` drives the real `fetch` (with the cursor passed back as the text a client keeps, `--tail` 1/2/3/7/100, `--grep`) and `--follow`'s per-poll read `logs::poll` against a seeded writer that lays records out as the supervisor does: repeated lines, equal stamps, a record written in two parts, rotations (one and two between reads), a record cut at the end of a rotated file, and truncation by hand. Each read returns exactly the whole records after its cursor, in order, each once with its stamp, none split; stops after `--tail` lines with `more` exactly when whole records are left; and a record gone before it was read is gone only with the `gap` the read reports. `supervisor::tests` cover exact byte and record boundaries, `tail`'s window at a record boundary, and an over-long record. Cursors naming several services, one with no log yet, and positions in replaced or cut files: `cursors_naming_several_services_read_each_on_and_positions_in_lost_files_say_so`; parsing and refusal: `cursors_round_trip_and_refuse_anything_else`.

What a cursor read guarantees, and its limits:
- A cursor names its file by device and inode. On a filesystem that reuses a freed inode for the next log file (ext4 does; APFS does not soon), a cursor two rotations behind can be taken for the new file: the read then reports `truncated` instead of `rotated`, or, once that file has grown past the cursor's offset, skips silently and may begin inside a record. The state machine keeps every rotated-away file linked so it checks only the rest.
- A file cut by hand and written past the cursor again cannot be told from one that grew; a cut while the cursor is in the rotated file is not seen. The state machine truncates only where a read can tell and reads at once.
- A cursor naming a service with no log yet reads that log's tail (at most `--tail` lines) once there is one; output before that tail is not returned.

CLI: `src/main.rs tests::run_gives_the_task_every_argument_after_its_name` (every argument after the task name is the task's, `--` right after it only a separator), `a_project_path_after_a_space_is_refused_with_the_spelling_that_works` and `a_directory_named_like_a_subcommand_is_never_taken_for_one` (`--project PATH` refused with its fix, never for a `-C` directory called like a subcommand); `tests/fleet.rs ps_status_logs_and_down_act_on_a_projects_worktrees_or_a_label` for `logs --project[=PATH]` across worktrees.

## Faults (historical bugs) and expected results

`FALSE.cfg` passes every invariant (2,788,007 distinct states: 218,989 of the port machine and 2,569,018 of the MCP server). Each `TRUE-*.cfg` puts one bug back and must end with TLC exit 12 and the named violation; each `LIMIT-*.cfg` has no fault and shows what is not promised. `expected.tsv` pins every config's exit code, invariant and distinct-state count, and the runner fails on any difference.

| Config | Bug put back | Violation |
|---|---|---|
| `TRUE-PruneUnknown` | 93b56b0: a checkout whose presence cannot be shown (lookup refused, volume unmounted) is pruned as deleted | `GivenIsReserved` |
| `TRUE-KeepEphemeral` | 2d97f39: a reservation inside the ephemeral range (made before the range moved) is reused | `AssignedOutsideEphemeral` |
| `TRUE-PinUnchecked` | design guard (tests/compile.rs `pinned_ports_cannot_collide_across_projects`): a pin is not checked against other checkouts' reservations | `NoSharedReservation` |
| `TRUE-NoRegistryLock` | design guard (`FileLock` on ports.lock since 65c8c13): compiles read and write the registry unlocked; one's write loses the other's | `GivenIsReserved` |
| `TRUE-ReassignForgetsOld` | eb73033 (found by this model): `compile --reassign-ports` chose again from the ports it gave up | `ReassignMoves` |
| `TRUE-AllocateOverOwnPin` | b664207 (found by the seeded registry state machine): an unpinned service before a pinned one could be given the pinned port | `OwnPinNeverConflicts` |
| `TRUE-ThreadPerCall` | fac91c6: every call got a thread at once | `WorkerBound` |
| `TRUE-UnboundedWaiting` | fac91c6: no limit on calls waiting | `WaitingBound` |
| `TRUE-UnboundedFrames` | 4fdfc4c: the reader read ahead without limit | `PendingBound` |
| `TRUE-HeadOfLineBlocking` | design guard (fac91c6): admission stops at the first call whose checkout is busy | `WorkConserving` |
| `TRUE-NewestFirst` | design guard (fac91c6): admission takes the newest waiting call first | `PerCheckoutOrder` |
| `TRUE-AdmitIgnoresSignal` | 4fdfc4c: only the loop's top checked for a signal; a call ending still started a waiting one | `NoWorkerAfterSignal` |
| `TRUE-WorkerSkipsCheck` | ad3b172: the worker called the tool without checking again, so a signal between `admit` and the tool still ran it | `NoToolAfterSignal` |
| `TRUE-DropLeavesBatchOwed` | 4fdfc4c: dropped calls were not settled in their batch, so it was never sent | `BatchSettles` |
| `TRUE-CancelMissesWaiting` | design guard (fac91c6): a waiting call cannot be cancelled | `CancelledNeverRuns` |
| `TRUE-EofDropsWaiting` | design guard (fac91c6): input ending drops calls still waiting | `AcceptedAnswered` |
| `LIMIT-ReservedPortTaken` | none: another program binds a reserved port (up refuses with `port_conflict`) | `ReservedPortsStayFree` |
| `LIMIT-SignalDuringTool` | none: a signal comes while a tool runs | `NoSignalWhileToolRuns` |

## Rust regressions

`rust-tests.txt` lists them, grouped: the port registry (the seeded state machine over every registry operation, with a checkout out of reach and one deleted; eight checkouts assigning at once through the real lock; the three fixes' tests; presence; the CLI's pin, deletion, conflict and `up --reassign-ports` paths), MCP admission (the bounds with the real constants, per-checkout turns and order, cancellation waiting and in flight, signals at each point including the worker seam, the reader's bound, input ending, batches, SIGTERM end to end), log cursors and `--follow`, and CLI parsing.

## Reading a result

A counterexample is a real bug only if each step can happen in the code. Replay it against the action tables, and as a test of the real function where you can. If a step cannot happen, the model is wrong: fix the model, never weaken an invariant. Real bugs found while writing this skill, each fixed with a failing-then-passing test in its own commit before the model: `compile --reassign-ports` returning the ports it gave up (`ReassignMoves`, eb73033); an unpinned service taking its checkout's pinned port (b664207, from the registry state machine); `more` reported for a record still being written (4142797, from the log state machine); a `-C` directory named like a subcommand taken for one by the `--project` check (479e1ab). Modeling errors found: `BatchSettles` first judged a batch the server never read (a signal stopped the loop first), so it now speaks of a batch the loop read.

## Coverage record

Covered files: `src/ports.rs` (`assign`, `assign_outside`, `find_free`, `reassign`, `release`, `release_gone`), `src/project.rs` (`compile`'s port requests), `src/session/prepare.rs` (`up_reassigning`), `src/session.rs` (up's port step), `src/checkout.rs` (`gone`, `presence` as an input), `src/mcp/server.rs` (`read_frames`, `Credit`, `Server::line`, `batch`, `message`, `request`, `cancel`, `drop_waiting`, `accept`, `admit`, `done`, `start`, `finish`, `Batch`, `Reply`), and, through the Rust state machines only, `src/logs.rs` (`Cursor`, `fetch`, `one`, `after`, `fresh`, `poll`, `follow`), `src/supervisor.rs` (`read_after`, `records_from`, `record_at`, `tail`), `src/main.rs` (`task_and_args`, `project_spelling`, `subcommand_in`).

The model was confirmed against these files at `8c6f27f` (the last commit changing a covered file: it added `logs::poll` and tests; eb73033, 479e1ab, b664207 and 4142797 before it are the fixes above). If any covered function changes, recheck the action tables, the bounds, `expected.tsv` and the Rust list. Move the sync commit forward only after confirming that the model describes the new code and rerunning the check. Use `/maintain-formal-verification-skill` to reconcile intervening changes. If a config is intended to change, update its row in `expected.tsv` and say why in the commit.
