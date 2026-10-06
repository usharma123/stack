# Astra Tilt process review, round 1

Verdict: REQUEST CHANGES. One verified P2 ownership finding.

Read-only review of `/Users/utsavsharma/.t3/projects/stack`, scoped to `bench/rwb/adapters/tilt.py`, new `bench/adapters/tilt/owned_processes.py`, and `bench/tests/test_tilt_process_cleanup.py`. No repository edits, commits, delegation, network, real Docker, services, or builds. Offline tests use fake Docker commands and test-owned sleep processes. Fresh live Tilt validation remains pending.

## Finding

P2: Match the effective Compose project, not project-looking container arguments.

Location: `bench/adapters/tilt/owned_processes.py:79-88`, especially the unconditional scan and return on any matching value.

With a supported shared tools directory, an unrelated Compose client can be classified as this run's process:

```text
/shared/tools/docker-compose --project-name rwb-other-a exec -T app echo --project-name rwb-20261006t000000-abc123-a
```

I passed this row to `Matcher('/shared/tools', 'rwb-20261006t000000-abc123-', False)`. `kind(row)` returned `compose`. The actual Compose project is `rwb-other-a`; the later flag is an argument to `echo` inside the container. The same issue applies to `run` command payloads and conflicting project options. The termination loop consequently admits the unrelated PID and sends TERM if its identity remains stable. PID/start/argv rechecks do not correct an ownership misclassification.

Suggested fix: parse only the Compose option region relevant to the harness's generated commands, require an unambiguous effective project, and reject conflicting or ambiguous project declarations. Never scan container-command payloads for ownership. Add negative regressions for another project's exec/run command containing this run's project flag, and conflicting project declarations; assert no signals via the existing fake-table seam. The observed Tilt events invocation has its project flag before the subcommand, so a conservative matcher can retain the required leak coverage.

## Verified behavior

- The exact original macOS PPID-1 event-reader row parses and is owned. A fresh full macOS process-table parse also succeeded, with 913 rows at the time of inspection.
- The 10 new tests pass on Darwin 24.6.0. They exercise real macOS ps, a failed fake ci leaving an orphan, real cleanup through HostTransport and teardown, and survival of unrelated test processes.
- The 22 existing additional-adapter tests pass. These include generated shell syntax, fake scenario execution, owned Docker cleanup, and Tilt/Vagrant error receipts.
- Additional offline shell probes replaced process and Docker operations with controlled statuses. Process=7/Docker=0 returns 7; Process=0/Docker=9 returns 9; Process=7/Docker=9 returns 9. The Docker phase still executes after process failure. The plain subshell retains remove_owned's errexit behavior. Shared remove_owned has no changes in the reviewed diff.
- `host_resources()` includes the process listing; teardown records either nonempty leftovers or listing failure. Current run.py removes the work directory only when cleanup_problems is empty. The live-leak regression exercises teardown's refusal to report clean; work-directory deletion gating was checked in source, not by running the full main lifecycle.
- Every TERM/KILL uses a fresh per-PID start-time and displayed-argv comparison. The recycled-PID test passes. Final process inventory is required even after signal attempts; permission failures or surviving owned processes cannot produce clean termination.

## Limits

Shared-tools Tilt itself is deliberately unattributable and excluded. Shared-tools Compose is intended to be attributable by project, but the finding above must be fixed for that boundary to hold.

The ps output is a displayed command string, not lossless argv, and lstart has second resolution. The separate identity-check and kill calls retain the acknowledged narrow signal race. This is not an atomic PID-reuse guarantee; the helper's statement that a recycled PID is never signalled overstates that guarantee. No additional blocking finding is asserted for the acknowledged race.

The new tests emit ResourceWarning for unclosed Recorder steps.jsonl files but pass. Full-suite results reported by the implementation agent were not reverified. The parent must rerun the full suite after concurrent run/base/scenario edits settle, then perform the pending fresh live Tilt diagnostic. This review grants no live-runtime pass.

## Tests

```text
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest bench/tests/test_tilt_process_cleanup.py -v
10 tests, 1.267s, OK

PYTHONDONTWRITEBYTECODE=1 python3 -m unittest bench/tests/test_additional_adapters.py -v
22 tests, 4.979s, OK
```

The ownership counterexample above was evaluated directly against the imported helper without launching Compose or sending any signal.

## Fingerprints

Observed HEAD: `9dd1c20cc03c069fe937a0435291492663c93ab7`.
The three scoped files had identical SHA-256 values before and after verification:

```text
ff6e546a728ceb86c796ce74acc686cd115b4a8934d7674d813321e38cdb9514  bench/rwb/adapters/tilt.py
0f01e08c867de6e107338035faa0fa6afcf9ecc98d5e398dfad4f07d3f429019  bench/adapters/tilt/owned_processes.py
ef1f4b813d07ac5bbefa054684d385a975bcf343d960f5b9eefae66c7ba43138  bench/tests/test_tilt_process_cleanup.py
```

End-of-review dependency fingerprints, not an approval of those out-of-scope changes:

```text
67d04e016691c9361ca52ce86196ac3ede4cab55d6cc3ead13b5a827c28c8d32  bench/run.py
b4462a9b7ac54425d2d877ee8d2679ef963951d6797704e19bf0a04db4c5775e  bench/rwb/adapters/base.py
a4e2c00016164775dc9064e412723f95995c88817daa73c1b44b2d95878508c3  bench/rwb/scenario.py
15e690cc72e65a566364f1d9f1b4c26ed6f99806e11cfa1c270dc24b6fb5b528  bench/rwb/transport.py
```
