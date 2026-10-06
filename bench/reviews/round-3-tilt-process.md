# Astra Tilt process review, round 3

Verdict: APPROVE within the bounded generated-command domain. NO FINDINGS.

The round-1 false ownership and round-2 whitespace false ownership / silent omission cases are resolved. This is approval of the three fingerprinted files' process-cleanup behavior, not a live Tilt pass or approval of concurrent harness changes.

## Scope

Reviewed `/Users/utsavsharma/.t3/projects/stack`. The supplied string `/Users/utsavsharma/.t3/projects/stack3Tiltfiles` is not an existing directory; the current checkout contains all three files with exactly the supplied SHA-256 hashes. HEAD observed: `6374702a3f8f76243ae1bb0bf49cd77486603f4e`.

Read the round-1 and round-2 reports, the original runtime-leak evidence in `bench/reviews/sol-remaining-runtime-results.md`, the helper, adapter and tests, and the relevant cleanup/error/deletion paths in run.py and scenario.py. No repository edits, commits, delegation, network, real Docker, services or builds. All scratch artifacts are in `/tmp`. Real process tests start and signal only test-owned sleep processes; Docker commands are stubs.

## Review conclusions

- `owned_processes.py:130-142` implements three outcomes. A matching private Compose executable with no exact run-project token is unowned. A first project option selecting this run, with no later token capable of selecting a project, is owned. Every other matching row mentioning the run's project raises `Ambiguous`, a `ProbeError` subclass.
- The bounded acceptance argument is sound for the generated shapes. No preceding option value can consume a project option placed first. Spaces in later paths cannot change that first selection. Later long project options and short options/clusters containing `p` cause ambiguity regardless of where whitespace splitting suggests a subcommand boundary. There is no premature subcommand return and no attempt to reconstruct general CLI semantics from ps display text.
- The original recorded PPID-1 event reader remains positive. Project-first harness Compose commands remain positive unless their payload contains a project-like option. Attached `-pX` and a project-only command are owned intentionally: lack of a subcommand does not select another project. The round-1 payload examples and both round-2 spaced-value counterexamples are now ambiguous rather than owned or silently omitted.
- `scan()` separates ambiguity from owned candidates. Termination signals only owned candidates after the existing identity recheck. A final scan prints ambiguous rows and returns 2; list also prints them and returns 2. Mixed fake tables terminate the actual event reader and leave every ambiguous or unrelated PID untouched.
- Error propagation is visible through real HostTransport teardown. An independently spawned ambiguous test process survived cleanup. Both host-cleanup and host-leftovers receipts named it as `ambiguous`; teardown returned `host cleanup exit 2` and `owned host resources remain`. The work directory remained present. The main lifecycle's deletion gate at `bench/run.py:154` requires no cleanup problems. That gate was source-checked, not exercised by running the entire main lifecycle.
- Cleanup still reaches the Docker phase after a process error. Independent shell probes verified process/Docker status pairs 2/0 -> 2, 0/9 -> 9, 2/9 -> 9 and 0/0 -> 0, using substituted bodies that invoke no Docker.

## Tests

`PYTHONDONTWRITEBYTECODE=1 python3 -m unittest bench/tests/test_tilt_process_cleanup.py bench/tests/test_additional_adapters.py -v`

37 tests passed in 7.084 seconds: 15 process-cleanup tests plus 22 adjacent adapter tests. Output: `/tmp/astra-tilt-r3-tests.log`.

`PYTHONDONTWRITEBYTECODE=1 python3 /tmp/astra-tilt-r3-probes.py`

Three independent scratch tests passed in 0.484 seconds. These cover real ambiguous-process teardown propagation, the cleanup status matrix, and two in-memory mutations. Mutating ambiguity into silent omission and mutating ambiguity into ownership each caused both selected no-signal/list regressions to fail. No source file was mutated. Script and output: `/tmp/astra-tilt-r3-probes.py`, `/tmp/astra-tilt-r3-probes.log`.

## Limits

- This is deliberately not a complete Compose CLI parser or a lossless argv proof. Environment-implicit/file-derived project selection and arbitrary option forms outside the generated-command domain are unsupported. Forged argv[0] and adversarial command display are outside the boundary.
- A legitimate owned exec payload containing `mkdir -p`, another project-like short option, or a later project flag can be ambiguous. It is not signalled; persistent ambiguity fails cleanup, retains the work directory and can require manual cleanup. Another project's payload mentioning this run can likewise block cleanup without being killed. This conservatism is explicit, not a silent-clean fallback.
- Shared-tools Tilt itself remains excluded. Compose ownership requires the exact configured or resolved executable path and explicit project evidence.
- Identity checks remain best effort. Displayed argv and second-resolution start time do not remove the check-to-signal PID reuse race. No process-group signalling was introduced.
- The original leak is historical evidence. No fresh live Docker/Tilt run occurred. The parent's combined suite after other agents' edits and its fresh live diagnostic remain pending; these focused results do not replace either.

## Fingerprints

All three scoped SHA-256 values matched the supplied values before and after verification:

```text
5383054065974a33c28160041cae9b65948b9e912cad64797e3f24b0bb426136  bench/rwb/adapters/tilt.py
08cf8b3fc51139203f0030eb111a0c382f2c75744a906dba0039a6e0aefb3ad5  bench/adapters/tilt/owned_processes.py
ba9df2715084d66afa31ec7b30df0f78bd969b28f788829dc288282c69a792b3  bench/tests/test_tilt_process_cleanup.py
```

End-of-review dependency fingerprints, recorded without approving their out-of-scope changes:

```text
e0c7c1006a88910f060dc4a1f19e9872a379e6bf8cecfae9c7565e17c3649e00  bench/run.py
b4462a9b7ac54425d2d877ee8d2679ef963951d6797704e19bf0a04db4c5775e  bench/rwb/adapters/base.py
a4e2c00016164775dc9064e412723f95995c88817daa73c1b44b2d95878508c3  bench/rwb/scenario.py
15e690cc72e65a566364f1d9f1b4c26ed6f99806e11cfa1c270dc24b6fb5b528  bench/rwb/transport.py
```
