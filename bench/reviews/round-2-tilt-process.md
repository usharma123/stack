# Astra Tilt process review, round 2

Verdict: REQUEST CHANGES. One verified P2 ownership finding remains.

Read-only review of the current dirty files in `/Users/utsavsharma/.t3/projects/stack`: `bench/rwb/adapters/tilt.py`, `bench/adapters/tilt/owned_processes.py`, and `bench/tests/test_tilt_process_cleanup.py`. Reviewed the round-1 report and Tilt section of `bench/reviews/sol-remaining-runtime-results.md`. No repository edits, commits, delegation, network, live Docker, services or builds. Only offline fake-process tests and test-owned sleep processes ran. Scratch files are under `/tmp`.

## Finding

P2: Whitespace splitting can still claim another Compose project's process.

Location: `bench/adapters/tilt/owned_processes.py:87`, and the premature subcommand return at lines 96-97.

`_rest()` splits the displayed ps command on whitespace. An option value containing spaces therefore becomes several tokens. The first non-option continuation is mistaken for the subcommand, preventing the parser from checking the remaining actual global options. For example, the following actual argument sequence has two project declarations and must be rejected under the new conservative contract:

```python
[
    '/shared/tools/docker-compose',
    '-p', 'rwb-20261006t000000-abc123-a',
    '-f', '/tmp/my project/compose.yaml',
    '-p', 'rwb-other-a',
    'events', '--json',
]
```

For its space-joined ps representation, `Matcher('/shared/tools', 'rwb-20261006t000000-abc123-', False).kind(row)` returns `compose`. `project/compose.yaml` is treated as the subcommand and the later conflicting project is ignored. The fake-table termination seam sends SIGTERM to this row. This bypasses the intended duplicate-project rejection and can kill another project's client when the later project selection is effective.

A second counterexample does not require duplicate-project semantics. Give `--env-file` the single path value `/tmp/env -p rwb-20261006t000000-abc123-a events.env`, followed by the only actual project option `--project-name rwb-other-a events --json`. Whitespace splitting turns part of the filename into this run's project option and `events.env` into a false subcommand. This row also returns `compose` and receives SIGTERM in the fake-table seam. Neither reproduction launches Compose or sends a real signal.

Suggested fix: use lossless process argv for ownership where available, or narrow acceptance to an unambiguous generated command shape and reject ambiguous displayed input. Merely changing to shlex.split cannot recover quotes already absent from ps output. Add negative matcher and no-signal regressions for spaced values before a later project declaration and project-looking text inside one option value. Keep the original event-reader positive regression.

Evidence: `/tmp/astra-tilt-r2-probes.log`.

## Verified behavior

- The original round-1 exec/run payload counterexamples now return unowned. Normal duplicate declarations, unknown leading options, missing subcommands and attached `-pX` are rejected. Existing new no-signal regressions pass.
- The exact recorded PPID-1 events row remains owned. The fake failed-ci orphan is terminated through real HostTransport teardown; unrelated test processes survive.
- The contradictory note that a `bash -c` wrapper counts as owned is false for the actual matcher. `_rest()` requires the command string to begin with the full private executable path. A string beginning `bash -c '/shared/tools/docker-compose ...'` returns None and the fake-table termination sends no signal. The existing wrapper-negative test also passes.
- Identity checking is correctly described in the helper as best effort. Each signal checks the PID's displayed argv and second-resolution start time; the check-to-kill race remains. There is no atomic PID-reuse guarantee and no process-group kill.
- A recognized surviving process returns exit 1 after termination attempts. Whole-table probe failure produces nonzero status. Host resource inventory includes recognized processes, and teardown records nonempty inventory or inventory failure. Current run.py removes the work directory only when cleanup_problems is empty. That deletion gate was checked in source, not by a full main-lifecycle execution.
- Offline shell status probes still give process=7/Docker=0 -> 7, process=0/Docker=9 -> 9, process=7/Docker=9 -> 9, and 0/0 -> 0. The fake Docker phase runs after process failure. Its subshell still honors errexit. Evidence: `/tmp/astra-tilt-r2-status.log`.
- Recorder cleanup is registered with addCleanup(recorder.close). This test run emitted no ResourceWarning.

## Tests

```text
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest bench/tests/test_tilt_process_cleanup.py bench/tests/test_additional_adapters.py -v
Ran 34 tests in 6.166s
OK
```

This is 12 process-cleanup tests plus 22 adjacent adapter tests, run together against the current checkout. Full output: `/tmp/astra-tilt-r2-tests.log`. Additional fake-table probes reproduce the finding and the wrapper/unknown-option/path-space behaviors. The prior implementation agent's 19-subcase old-matcher mutation result was not independently repeated.

## Limits and exact failure behavior

Unknown options before the first presumed subcommand return None, not ProbeError. Such a process is omitted from both listing and termination; if it is the only leak, termination returns 0 and inventory is empty. A space-containing option value before the project token can have the same silent omission behavior. Example: `--project-directory /tmp/my work -p <this-run-project> events`. Thus these limitations can yield a false clean teardown and permit work-directory deletion; they do not reliably block cleanup. Spaces after an already parsed project may still match, as the original Tilt event-reader shape would, but can also cause the false ownership finding above. Unknown options after a falsely inferred subcommand are not checked either.

A space in the executable's configured tools path alone does not imply a miss: the matcher removes the full literal path before splitting the rest. Shared-tools Tilt itself remains excluded. Shared-tools Compose ownership is precisely where the remaining false-positive matters.

No fresh live Tilt validation was performed. The earlier runtime's leaked event reader and bad-config false positive remain historical evidence, not a new runtime pass. Concurrent core agents are active. This report does not dismiss, replace or pre-judge combined-suite verification after their edits settle. The 34 passing tests do not approve out-of-scope core changes, and the remaining finding prevents approval of this process patch.

## Fingerprints

Observed HEAD: `201ad6ef66d5f15e0a13dcb00977d021245fe3ac`.
Scoped SHA-256 values matched the supplied helper/test fingerprints and were unchanged across verification:

```text
ff6e546a728ceb86c796ce74acc686cd115b4a8934d7674d813321e38cdb9514  bench/rwb/adapters/tilt.py
963ae041ebdd15bb0495c3f5d77e33e53672dd6f4a8b06af20aae1b3993550ba  bench/adapters/tilt/owned_processes.py
b1da568ce24fcd0b4c0b193d6ae6e4ae644e291c5d48906a7567418c2947c609  bench/tests/test_tilt_process_cleanup.py
```

Dependency fingerprints observed after tests, for reproducibility only:

```text
67d04e016691c9361ca52ce86196ac3ede4cab55d6cc3ead13b5a827c28c8d32  bench/run.py
b4462a9b7ac54425d2d877ee8d2679ef963951d6797704e19bf0a04db4c5775e  bench/rwb/adapters/base.py
a4e2c00016164775dc9064e412723f95995c88817daa73c1b44b2d95878508c3  bench/rwb/scenario.py
15e690cc72e65a566364f1d9f1b4c26ed6f99806e11cfa1c270dc24b6fb5b528  bench/rwb/transport.py
```
