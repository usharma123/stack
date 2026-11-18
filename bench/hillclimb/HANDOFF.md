# Hill-climb handoff

Written 2026-11-18. Work stopped here at the owner's request. Everything is committed and pushed
on `fix/exercise-findings` (17 commits ahead of `main`, no pull request opened).

## What this branch does

Three steps, each implemented by one agent, reviewed by a two-reviewer council until both agreed,
then measured and committed.

| Step | Commits | Review state |
|---|---|---|
| Earlier fixes: exit watcher, lock backoff, port registry, git exclude lookup, settled `up` | `8678fd3`..`28ffb43` | Reviewed once before this round; findings fixed |
| 1. Supervisor built into Stack, replacing Pitchfork | `9afdcfd`, `2fe9681` | **Approved by both reviewers** |
| 2. Exec hot path: kept mise answers, wire probes, unreadable-record recovery | `dae9b94`, `8190bfc`, `2fe46e3` | **Approved by both reviewers** |
| 3. Fresh worktree: Postgres template clone, plain fsync, first-up trims | `692b572`, `d768ab5`, `45d3ff4` | **Fixes not re-verified** (see below) |

## Measurements

`python3 bench/hillclimb/perf.py <binary> "<label>"` (postgres 17 + redis 8, six worktrees) appends
to `results.jsonl`; `python3 bench/hillclimb/chart.py` renders `hillclimb.html`. Milliseconds, one
run per row on a machine that was not always quiet, so treat differences under about 20% as noise.

| | 0.1.23 released | step 1 | step 2 | step 3 |
|---|---|---|---|---|
| `up`, two services, existing data | 2280 | 220 | 138 | 89 |
| `down` | 2296 | 98 | 94 | 148 |
| `restart` | 1968 | 187 | 256 | 219 |
| six fresh worktrees `up` at once | 4262 | 1431 | 1440 | 315 |
| `exec -- true` | 110 | 44 | 12 | 13 |
| `status` | 58 | 37 | 8 | 8 |
| `up`, already running | 600 | 101 | 13 | 36 |
| 24 execs across six worktrees | 790 | 305 | 82 | 80 |

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

1. **Step 3's review fixes were never re-verified.** The council's first review requested changes
   (two must-fix, seven should-fix). The implementer reports all fixed except as noted in items 2
   and 3, and the tree passes its tests, but neither reviewer has checked the fixes. Before
   merging, have someone verify at least:
   - the `pg_control` edit uses the verified layout table (17: nonce at 256, CRC at 288; 18: nonce
     at 257, CRC at 292) and falls back to `initdb` for any other version;
   - the install fingerprint covers the directory holding `postgres.bki`, its `extension`,
     `timezonesets` and `tsearch_data` entries, and followed links;
   - a template built with reduced `max_connections`/`shared_buffers` is not published;
   - `gc` re-checks a template under its lock before deleting it.
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

## Where to pick up

1. Verify step 3's fixes (item 1), ideally with the two-reviewer process used for steps 1 and 2.
2. Run the test suite and `perf.py` on Linux (item 2).
3. Decide item 4.
4. Next speed targets, from the last profiles: `restart` (about 220 ms, mostly Postgres stop and
   start), `down` (bounded by Redis's 100 ms shutdown tick), and a first `up` that must build the
   template (about 0.7-0.9 s, once per pinned release).
