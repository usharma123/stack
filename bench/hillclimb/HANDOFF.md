# Hill-climb handoff

Updated 2026-11-18, after step 3's review findings were fixed and approved. Everything is
committed and pushed on `fix/exercise-findings` (no pull request opened).

## What this branch does

Three steps, each implemented by one agent, reviewed by a two-reviewer council until both agreed,
then measured and committed.

| Step | Commits | Review state |
|---|---|---|
| Earlier fixes: exit watcher, lock backoff, port registry, git exclude lookup, settled `up` | `8678fd3`..`28ffb43` | Reviewed once before this round; findings fixed |
| 1. Supervisor built into Stack, replacing Pitchfork | `9afdcfd`, `2fe9681` | **Approved by both reviewers** |
| 2. Exec hot path: kept mise answers, wire probes, unreadable-record recovery | `dae9b94`, `8190bfc`, `2fe46e3` | **Approved by both reviewers** |
| 3. Fresh worktree: Postgres template clone, plain fsync, first-up trims | `692b572`, `d768ab5`, `45d3ff4`, plus the review fixes after `5588514` | **Approved** after six review rounds (see below) |

## Measurements

`python3 bench/hillclimb/perf.py <binary> "<label>"` (postgres 17 + redis 8, six worktrees) appends
to `results.jsonl`; `python3 bench/hillclimb/chart.py` renders `hillclimb.html`. Milliseconds, one
run per row on a machine that was not always quiet, so treat differences under about 20% as noise.

| | 0.1.23 released | step 1 | step 2 | step 3 | step 3, reviewed |
|---|---|---|---|---|---|
| `up`, two services, existing data | 2280 | 220 | 138 | 89 | 83 |
| `down` | 2296 | 98 | 94 | 148 | 108 |
| `restart` | 1968 | 187 | 256 | 219 | 167 |
| six fresh worktrees `up` at once | 4262 | 1431 | 1440 | 315 | 256 |
| `exec -- true` | 110 | 44 | 12 | 13 | 12 |
| `status` | 58 | 37 | 8 | 8 | 8 |
| `up`, already running | 600 | 101 | 13 | 36 | 13 |
| 24 execs across six worktrees | 790 | 305 | 82 | 80 | 69 |

Two numbers in the step 3 column are not regressions according to the implementer's interleaved
runs, and nobody else has checked that:

- `down` 148 ms: Redis acts on SIGTERM at its next 100 ms tick; with a random pause before `down`
  both builds averaged the same.
- `up`, already running, 36 ms: load noise; interleaved runs gave about 14 ms.

## State of the tree

- Last full check, on the committed tree: release build clean, `cargo clippy --locked
  --all-targets -- -D warnings` clean, `cargo test --locked --all-targets` 732 passed, 0 failed,
  15 ignored.
- Build on this machine needs `export DEVELOPER_DIR=/Library/Developer/CommandLineTools`.
- Some tests in `tests/runtime.rs` and one in `src/oci.rs` are timing-sensitive and fail under
  heavy load; they pass alone.
- Nothing from this work is left running.

## Open items, most important first

1. **Step 3's review findings are fixed and approved.** A template build or a published
   template the identity edit refuses never becomes a checkout's data directory; the checkout
   gets a plain `initdb`, and Stack tries to remove the template under its lock and logs what
   happened (removed, lock not taken, already replaced, or the error). Damage to a copy alone
   keeps a healthy template. The install fingerprint covers every share directory holding
   `postgres.bki` and every C library and musl loader, and gives no key on any listing or
   metadata error. The template format is now `stack-pg-template-3`; earlier templates are not
   cloned and `gc` collects them. Reviewed over six rounds by two reviewers (the last round by
   one), with release-binary reproductions on Postgres 17.12 and 18.7. Left open as follow-ups,
   judged not to block: the "cannot check it" branch of `retire` has no test; a transient
   error reading a template counts as "changed since it was built" and retires it; a template
   directory swapped for a symlink under a paused start is not detected.
2. **Nothing has ever run on Linux.** Every step's Linux code (`PR_SET_PDEATHSIG`, pidfd
   signalling, `/proc` paths, `FICLONE` reflink and copy, the C library key) was type-checked with
   `cargo clippy --target x86_64-unknown-linux-gnu` only. The release targets are musl, which was
   not type-checked.
3. **macOS copy fallback has no automated test.** When `clonefile` is refused, Stack runs
   `initdb`; this was exercised by hand with an HFS+ disk image only.
4. **`stack down` ignores commands still running through `stack exec`.** It stops services and
   reports "confirmed" regardless. Found during the step 2 review; both reviewers judged it
   separate from this work. Not changed.
5. **Two services on one preset cannot both verify**, because both export the same variable names
   (`DATABASE_URL`, `PG*`). Documented in `docs/user/bundles.md`, not fixed.
6. **Power loss is not claimed anywhere.** Session records have a recovery rule (`stack down`);
   `ports.json`, the identity registry and `data.json` fail with `state_invalid` if unreadable and
   must be deleted by hand.
7. **Commit granularity.** Step 2's environment cache and session-record handling share too much
   of `src/session.rs` to compile separately, so they are one commit (`8190bfc`). Step 3's
   template clone and first-up trims are likewise one commit (`d768ab5`).

## User-visible changes to call out in release notes

- Pitchfork is no longer installed, pinned or started. A `stack.lock` that still pins it is
  accepted; the next `compile` drops the pin, after which 0.1.23 rejects that lock.
- Sessions started under Pitchfork by an older Stack are still stopped, and `up` moves them to the
  new supervisor on their existing data. `restart` on such a session refuses with `session_stale`.
- Error codes removed: `services_unsupported`, `socket_path_too_long`, `supervisor_unknown`,
  `supervisor_changed`, `supervisor_unbound`, `mv_data_unplaced`. New status `orphaned`; new
  refusal `stop_unconfirmed` when a killed supervisor leaves processes Stack cannot show are the
  checkout's (on macOS this includes Apple-shipped binaries such as `sh`, whose environment cannot
  be read).
- New state under `$STACK_STATE_DIR`: `supervisors/<key>/`, `data/<key>/`,
  `projects/<key>.provider-env.json`, `mise-version.json`, `templates/postgres/<key>/` (about
  39 MB per pinned Postgres release, shared copy-on-write with its clones).
- cockroachdb, nats and spicedb now get per-checkout secondary ports.
- Probe failure reasons read `postgres at 127.0.0.1:N: ...` instead of `psql failed: ...`.

## Review records

The council's briefs, reviews and reconciliations are in `bench/hillclimb/council/` on the machine
this was written on. They are not committed (the directory's files match the `step*` ignore
pattern and contain local paths). The binaries used for each measurement (`released-0.1.23`,
`step1-native-supervisor`, `step2-hot-path`, `step3-fresh-worktree`) are in `bench/hillclimb/`
there too, also ignored.

## The climb is closed (2026-11-18)

Step 4 profiled what was left and found it at or near the floor set by Postgres, Redis, `initdb`
and mise. Medians of 10 to 15 interleaved runs:

| operation | now | where the time goes |
|---|---|---|
| `up`, existing data | about 85-105 ms | Postgres needs 48 ms to accept connections; a 24 ms scan for leftovers of a previous session; 14-18 ms supervisor spawn |
| `down` | about 110-130 ms | 95 ms waiting for Redis, which acts on SIGTERM only at its next 100 ms tick; 20 ms process scans |
| `restart` | about 170 ms | the same Redis tick, then the Postgres start |
| six fresh worktrees `up` | about 260 ms | `mise env` for each new checkout; six Postgres starts competing for CPU |
| first `up` in a new state directory | about 690 ms | `initdb` is 448 ms of a 491 ms template build |

Tried and not kept:

- **Asking Redis to `SHUTDOWN` over its port after SIGTERM** (`down` 131 to 79 ms). Rejected in
  review, with reproductions on Redis 8.10.3: the request could reach a different Redis on the
  same port (and, by reading, the next generation after `restart`); `SHUTDOWN` without
  arguments skipped a save that `shutdown-on-sigterm save` would have made; a `SHUTDOWN` whose
  save failed cleared the pending SIGTERM, so Redis was killed at 10 s. Not worth 50 ms.
- Redis `--hz 100`: the same gain, but more idle CPU in every worktree for the whole session.
- Stop-wait polling capped at 5 ms instead of 20 ms: 13% on `down`.
- Cheaper process scans and dropping `down`'s second scan: about 10 ms.
- Overlapping stop and start in `restart`: a few ms.

Found on the way, not fixed: `STACK_TIMINGS=1` with a closed stderr makes the timing writer
panic (`src/timing.rs`), which can abort a command.

Reopen this only on evidence: a Linux run with a different profile, agent transcripts where
Stack's latency is a visible share of task time, fleets well beyond six where `up` grows worse
than linearly, or a regression in `perf.py`'s numbers.

## Where to pick up

1. Step 3 is done. Reviews now go to one standing reviewer session, not a two-reviewer council.
2. Run the test suite and `perf.py` on Linux (item 2).
3. Decide item 4.
4. Speed work is closed (see above). Next is the isolation claim itself: item 4, a fleet
   isolation test, and Linux.
