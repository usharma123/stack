# Astra native and transport review, round 1

Verdict: approve the implementation within offline scope. NO FINDINGS of a verified implementation defect. The new transport tests passed; the explicit unreadable-PID regression from the handoff remains missing and should be added to complete its test checklist.

Reviewed branch `codex/realworld-competitor-bench`, HEAD `61ab8a121743496cf67032abbc985c0058a8e592`, with uncommitted changes. Compared the requested files against `bench/reviews/sol-native-runtime.md` and `bench/reviews/sol-transport-pid-handoff.md`. No repository edits, Docker calls, live services, provisioning, or delegation. The parent owns full-suite and live validation.

## Regression checklist

A new `bench/tests/test_transport.py` appeared during final verification. I read it and ran all 22 tests successfully. It closes the initially observed transport coverage gap: real wrapper failure paths, body status and umask preservation, root/agent argument separation, exact timeout cleanup routing, invalid PID rejection with a stub kill, persisted failed-cleanup evidence, and host registration are covered. The old core pathname assertion was also updated and passes.

One handoff-required case is still absent: an existing regular PID file that is unreadable to the executing user. `TimeoutCleanupScriptTest` tests absent and malformed files and a symlink, but not the `! -r` branch at `bench/rwb/transport.py:55`. Add a chmod-000 case that asserts exit 3 and no stub kill calls; skip only this permission case when running as root. The implementation's guard is present; this is an incomplete test-checklist item, not a demonstrated runtime defect or a blocking severity finding.

Useful additional cases are a timeout of the cleanup helper itself, a symlinked registration directory in cleanup, and services-flake success/non-124 readiness paths. The current stub hardcodes the helper's timed_out field to false, so helper-timeout metadata is not exercised. These are not separate verified implementation defects.

## Verification

- `PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s bench/tests -p test_native_extra_adapters.py -v`: 29 passed, including shell syntax and shellcheck. These tests use fake CLIs and do not start services.
- `PYTHONDONTWRITEBYTECODE=1 PYTHONPATH=bench:bench/tests python3 -m unittest test_core.TransportSafetyTest -v`: 4 passed on the updated snapshot.
- `PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s bench/tests -p test_transport.py -v`: 22 passed, no skips. Total targeted repository tests: 55 passed.
- `PYTHONDONTWRITEBYTECODE=1 python3 /tmp/astra-native-transport-probe.py`: passed. The retained scratch script imports the real wrapper and cleanup shell. Successful registration produced mode 0700 and preserved umask 0022 and body exit 13. Permission denial, existing directory, symlink, missing parent, and injected PID-write failure all returned 126 without the body sentinel. Missing, empty, 0, 1, negative, nonnumeric, leading-zero, spaced, and overlong PID inputs invoked no stub kill. PID 42 invoked only `-0 -- -42` and `-KILL -- -42`; injected kill failure returned 5. Stub Docker root and agent calls used different paths, exact matching cleanup paths, and retained failed cleanup metadata while the main step remained timed out. Real local HostTransport with `/bin/bash` on macOS handled workdir spaces, preserved exit 13, emitted no inner timing, and rejected a preexisting step directory before the body.

## Native adapter assessment

The Redis devshell package addition addresses the reported dnvr version-receipt failure. services-flake now queries its exposed `services` wrapper. New artifact declarations retain the intended native logs and avoid services-flake database directories. Process Compose and services-flake preserve the native readiness exit status, including 124, and add bounded status/log output. The outer scenario deadline is 300 seconds, so the existing 120-second native wait has room for the added 10-second status probe. No timeout-to-success mapping was added. dnvr keeps its existing readiness-key gate, exit 1, PTY timing scope, and scripted feature label while adding native-log evidence.

The native tests demonstrate these behaviors with prewritten log fixtures and fake executables. They do not establish that the pinned Process Compose or services-flake runtime actually writes the newly configured process log, or that the updated version commands succeed inside realized Nix shells. The next parent-owned serial diagnostic must verify those receipts and artifact copies. An occupied-port exit 124 remains a failed infrastructure deadline even if conflict text is now retained.

## Transport ownership and limits

The final implementation uses exclusive per-container/per-sequence directories under `/tmp`, mode 0700, with separate guarded directory creation and PID writing. The registration umask is confined to a subshell. Cleanup uses the exact timed-out step path in the named container as root; missing or malformed registration produces visible cleanup evidence. No broad kill or Docker cleanup operation was introduced. Host execution still uses the host runner's process group and requires no `setsid`; macOS BSD mkdir accepted the generated registration command in the local checks.

No actual mixed-UID execution, Linux setsid behavior, container process-group termination, or final resource absence was verified. Stub argv and shell tests establish routing and validation, not successful live cleanup. No full suite was run. Native additional coverage worth retaining includes services-flake success/non-124 paths and diagnostic status-probe failure; these are not separate verified implementation defects.

The workspace changed during review: the initial transport snapshot used random path suffixes and the old test assertion; the final snapshot uses deterministic container/sequence paths, rejects a symlinked host PID parent, and updates the assertion. It also moves host PID-parent validation before sequence allocation. The new 22-test transport file was included in final review. Transport probes and the four core tests were rerun against the final transport fingerprint. Approval applies only to the snapshots below.

## Final source fingerprints

- `bench/rwb/transport.py`: `15e690cc72e65a566364f1d9f1b4c26ed6f99806e11cfa1c270dc24b6fb5b528`
- `bench/tests/test_core.py`: `6ccaac35ad87ab57bc48888a38207ff489e6af33b0a43d3291b636079a235b66`
- `bench/tests/test_native_extra_adapters.py`: `81195f2c633e2c187ed2c7fd9cbdbd6775f092b82f7215de19dfc266dcf97519`
- `bench/adapters/dnvr/rwb-dnvr.sh`: `8b93ca6aae8d402e7c27b70b637396a671165a9a34965354d7e7e1596bf9b128`
- `bench/adapters/process-compose/rwb-pc.sh`: `5d39924bc8f7bbee356b209c539701bf308438cb7d3e42ed6e2470a08e071aee`
- `bench/adapters/services-flake/rwb-sf.sh`: `d20ba91ecd26c7f6f369594d944f87aaed472b3ef97803043020f5714dd7b24c`
- `bench/tests/test_transport.py`: `3d8b9f05fee62b455d0e0bc946ecfc36cce059c51cd0f49fa34d96e5aa3e1d95`
