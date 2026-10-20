# Astra report-selector review, round 2

REQUEST_CHANGES. The three specific round-1 reproductions are resolved. Two remaining P2 defects were reproduced offline.

Scope: current uncommitted `bench/report.py`, `bench/tests/test_reporting.py`, `bench/run.py`, and `bench/README.md` on `codex/realworld-competitor-bench`, against `bench/reviews/sol-measurement-handoff.md` and `bench/reviews/round-1-report.md`. No repository edits or commits.

## Prior findings

1. External referenced logs: resolved. `log_path` rejects absolute paths, parent traversal, and symlinks, and requires an existing file inside the hashed run tree. The original external-log probe now emits an evidence problem and zero metrics. Regression tests also cover traversal and symlinked files/directories.
2. Missing selected evidence counted as complete: resolved. Coverage uses an explicit `covered` boolean. All 26 selected attempts pointing to nonexistent directories now yield `coverage_complete=False`; the regression test checks `--require-complete` returns 1.
3. Invalid step durations: resolved for the reported step fields. Negative or noninteger outer durations and noninteger inner durations invalidate evidence. Negative inner samples withhold the inner distribution with a reason while preserving valid outer measurements. The original negative-inner probe now emits null percentiles and zero available inner samples. The related first-task metadata path remains unchecked, as described below.

## Remaining findings

1. P2, `bench/report.py:380-382`: validate first-task wall duration before publishing it. `first_task` validates the summed step durations but copies `wall_ns` and `prepare_ns` directly from metadata. A correctly hashed run with `first_task.a.wall_ns=-900000000` still reports all four metrics and renders `wall -0.9 s`. A float is accepted too. A string or null is also accepted into `reported_timings`, then `render` raises TypeError when dividing it by 1e9. The CLI writes report.json before rendering Markdown, so this can leave a partial final-report directory. Require `wall_ns` to be a nonnegative integer excluding bool, and validate optional `prepare_ns` similarly when present. Omit the affected metric with a reason on invalid metadata. Add negative, noninteger, missing/null wall-duration coverage.

2. P2, `bench/report.py:244-246`: verify the files named by successful artifact receipts exist. Artifact validation only checks whether `seq` exists. A run whose metadata records `artifacts=[{checkout: "a", path: "service.log", ok: true, seq: <existing step>}]` is accepted with no evidence problems and all four metrics even though `artifacts/a/service.log` is absent. Hashing the current directory cannot detect a file lost before the manifest hashes were taken. This violates the handoff's requirement for existing referenced raw files/artifacts and can label an incomplete evidence bundle reportable. For successful copy receipts, validate the expected `artifacts/<checkout>/<path>` file or directory, require containment in the hashed run tree, and reject missing artifacts. Preserve explicitly failed best-effort collection as diagnostic evidence rather than requiring unsuccessful copies to exist. Add a regression that deletes a successfully copied artifact before calculating manifest hashes.

## Verification

- `python3 -B -m unittest discover -s bench/tests -p test_reporting.py -v`: 28 tests passed in 5.040 seconds. Log: `/tmp/astra-report-r2-tests.log`.
- `python3 -B /tmp/astra-report-r1/probe.py`: reran the original three reproductions; external logs rejected, missing coverage false, negative inner statistics withheld.
- `TMPDIR=/tmp python3 -B /tmp/astra-report-r2-probe.py`: independent probes for the two remaining defects. Receipts: `/tmp/astra-report-r2-h0ot7_h7`. Every mutation occurred before `selected()` calculated manifest hashes; these failures do not depend on stale hashes.

Limitations: synthetic offline Scenario receipts only. No Docker, services, installation, network, benchmark measurements, or full-suite execution. Parent owns the full offline suite. Review/result references remain parent attestations, not authenticated approvals. Concurrent adapter and transport changes are outside this review. These results do not approve final benchmark measurements or a PR.

Verdict: REQUEST_CHANGES until both remaining evidence-validation defects are fixed.

Reviewed-file SHA256 snapshot:

```text
88f1d3929f7befcdceb42cada17dd4a701e546f2d65a44e922306f2627aefa48  bench/report.py
f389a3c6a7e067f626bb3594fafd09c8ea7571a5bfbb2a792482dceafb645382  bench/tests/test_reporting.py
040c5f51c6c8df2be9e57f9854366d687a04f6d194ac9c9b680e4e6142aa5deb  bench/run.py
85d5292feb58414010a1c259a15186d6f871c768d43ee3fe48cdc27f7a9f9f9a  bench/README.md
```

HEAD at final snapshot: `61ab8a121743496cf67032abbc985c0058a8e592`.
