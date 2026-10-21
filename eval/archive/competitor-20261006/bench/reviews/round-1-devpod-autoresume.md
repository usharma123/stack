# DevPod auto-resume review, round 1

Verdict: APPROVE. NO FINDINGS in the scoped change.

Reviewed the uncommitted change in /Users/utsavsharma/.t3/projects/stack on codex/realworld-competitor-bench, HEAD 7a545d92c8ca12604fcc96425f64cf7bb6413b73. Scope: bench/rwb/adapters/devpod.py, bench/CONTAINER-ADAPTERS.md, the single declaration assertion in bench/tests/test_container_adapters.py, and new bench/tests/test_devpod_autoresume.py. No repository edits, commits, delegation, network calls, live Docker commands, or service operations were performed. Scratch and test logs are in /tmp; Python bytecode writes were disabled.

## Findings and reasoning

NO FINDINGS.

The only executable adapter change is entry_auto_resumes = True. Scenario.stop_restart already supports this declaration. It skips a-after-stop and requires both stop.ok and gone.ok for stop.a to pass. StepResult.ok requires exit 0 and timed_out false. A failed stop or failed stopped probe cannot become a passing stop result. Probe timeout blocks restart and persistence. The unchanged probe inspects containers selected by the run-specific Compose project, and Docker command failures propagate. DevPod start still checks that the checkout resolves to the intended project.

The explicit a-restart still calls devpod up through start_and_identify. PostgreSQL persistence still requires a persisted receipt and restart identity checks; Redis persistence still requires its durable receipt. B survival and cache-after-restart remain exercised.

The new fake transport models SSH restarting an already-created stopped workspace. Its pre-fix subclass sets the declaration false and reproduces stop.a = fail with app after stop exit 0 and an actual fake resume recorded at a-after-stop. The fixed declaration suppresses that entry, preserves stop/probe evidence, and leaves explicit restart and persistence checks passing. This is a meaningful regression against the original behavior.

## Runtime evidence

Read bench/reviews/sol-runtime-resume-results.md and the original smoke-devpod-1 raw steps/logs. Steps 45, 46, and 47 all exited 0 with timed_out false. Step 46 lists app, PostgreSQL, and Redis containers as exited. Step 47 stderr shows those containers starting, and stdout returns successful identity with PostgreSQL started at 2026-10-06 15:46:07.107454+00. These receipts support the documentation correction. The original failed result was not edited or relabelled.

The post-fix live DevPod rerun remains pending. This verdict approves the source change and offline regression, not a new live benchmark outcome.

## Independent test evidence

- PYTHONDONTWRITEBYTECODE=1 TMPDIR=/tmp python3 -B -m unittest discover -s bench/tests -p 'test_devpod_autoresume.py' -v: 5 passed in 0.009s. Log: /tmp/astra-devpod-autoresume-r1-newtests.log.
- PYTHONDONTWRITEBYTECODE=1 TMPDIR=/tmp python3 -B -m unittest discover -s bench/tests -p 'test_container_adapters.py' -v: 41 passed in 20.961s. Log: /tmp/astra-devpod-autoresume-r1-container.log. Together, 46 passed.
- Additional in-memory fault matrix independently checked both a-stop and a-stopped-probe with (exit, timed_out) values (1, false), (124, true), and (0, true). All three stop cases returned fail; probe exit 1 returned fail and both probe timeout cases returned blocked. None passed.
- git diff --check for the tracked scoped files passed.
- The reported 283-test full-suite run was not independently repeated. No new live run or CLI dry-run was performed by this reviewer.

## SHA-256 fingerprints

Verified before and after testing; the supplied implementation and new-test fingerprints match.

```text
e8e3a4fb24c3dc4d0c3893b58a994a8f534878af85173b090e117c60da80b779  bench/rwb/adapters/devpod.py
311b272c40f1c54df96cb2b8ff5f00a4be804ac431b8ec2c10c4e5da76d7e8a4  bench/tests/test_devpod_autoresume.py
9867247c9de961fba15c42c588a1933e936372a37a561035683e35a5ae6cf521  bench/tests/test_container_adapters.py
f14913911ac153eeec961890afc6f115edd5de8cf7b886d5f46f94a5a9791e1d  bench/CONTAINER-ADAPTERS.md
3e3d818783446672a4b4d3c627677b286213b4eb70fa6131bfdedacdb22db088  bench/rwb/scenario.py
94fc00b00264eefda268024cf0ee0ea1e50be124e30a9cd41b3d92f09f54e04d  bench/results/smoke-devpod-1/steps.jsonl
```
