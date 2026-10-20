# Runtime resume results

Seven requested lanes were executed sequentially with 2 repeats, 1 warmup, default resources, and keep=false. Every run is diagnostic and reportable=false. Valid/completed describes trustworthy receipts and clean teardown, not a tool pass. No final performance report or ranking was produced.

The actual checkout was codex/realworld-competitor-bench at ff4d71824c6027df2c09be1b70ea5953bdc50d4c. Compared with supplied HEAD 19d4aac, only two runtime inventory/review documents were added. Host Darwin 24.6.0 arm64, harness Python 3.12.9. Docker desktop-linux: client 28.1.1, server 29.1.3 linux/arm64. Host Compose 2.40.3-desktop.1. Container-family commands run on the host against Linux Docker containers. Native-extra commands run through Docker exec in ev-nix. Their actual Nix receipt is Determinate Nix 3.23.0 / Nix 2.35.2. All exact source/config/fixture/glue hashes and version receipts remain in each meta.json and steps.jsonl.

## Interrupted-run cleanup

Original bench/results/smoke-devcontainers-1, run 20261006t152552-67fb2a, remains byte-for-byte unchanged and incomplete. A live process recheck found no executor before cleanup. The exact image tag rwb-20261006t152552-67fb2a-a-app:latest matched sha256:893157558933308cdbb22d077465124ed455040f19d054077425f67281d3b010 and Compose project rwb-20261006t152552-67fb2a-a. All containers were inspected and none referenced the image or run. No matching network or volume existed. Exact tempdir ownership and its project token were verified. Its contents were archived before deletion, then only that image tag was removed without force and only the exact tempdir was removed. Image ID and tempdir absence were verified. No archived PID was signalled.

Receipt: [sol-runtime-resume-cleanup.json](sol-runtime-resume-cleanup.json). Full raw cleanup receipt and tar archive are under bench/results/runtime-resume-cleanup with SHA-256 values in the receipt. Shared layers, caches, ev-mise and unrelated containers were not cleanup targets.

## Dev Containers CLI

- Exact attempt: `bench/results/smoke-devcontainers-2`; run `20261006t154310-ba3a3f`; 2026-10-06T15:43:10Z to 2026-10-06T15:44:39Z.
- Source: `ff4d71824c6027df2c09be1b70ea5953bdc50d4c`; transport `host`; image `host transport; workload image pins/identities in receipts`.
- Completed `True`, valid `True`, reportable `False`. Outcome counts: `{"not_applicable": 1, "observed": 2, "pass": 26}`. Raw steps: 67; every stdout/stderr file exists: `True`.
- All applicable checks pass. Native start/entry and separate-container workload identity, source token, isolation, persistence and frozen copy are realized. Host-port collision is not applicable. CLI 0.89.0 and Node 24.16.0; actual workload Python 3.13.16, uv 0.12.23, PostgreSQL 17.6, Redis 8.10.2. PostgreSQL 17.6 is a declared lane deviation. No separate service-log artifact paths are declared for this lane; native CLI build/up output is retained in raw command logs.
- Version receipt files: `logs/0003-tool-version.stdout` and `logs/0003-tool-version.stderr` exit 0, `logs/0009-a-tool-versions.stdout` and `logs/0009-a-tool-versions.stderr` exit 0, `logs/0025-b-tool-versions.stdout` and `logs/0025-b-tool-versions.stderr` exit 0, `logs/0056-c-tool-versions.stdout` and `logs/0056-c-tool-versions.stderr` exit 0.
- Native artifact files copied: 0. Copy/probe receipts and file sizes/hashes are preserved in `bench/results/runtime-resume-verification/smoke-devcontainers-2.json`; native raw diagnostics remain in `logs/`.
- Cleanup problems `[]`; artifact errors `[]`. Independent exact container/network/volume/image name and label queries, full image-label inspection, host process and tempdir checks show outer resources absent: `True`. This verification includes Lando's normalized ownership token where applicable.

## DevPod (Docker provider)

- Exact attempt: `bench/results/smoke-devpod-1`; run `20261006t154453-d998e7`; 2026-10-06T15:44:53Z to 2026-10-06T15:46:36Z.
- Source: `ff4d71824c6027df2c09be1b70ea5953bdc50d4c`; transport `host`; image `host transport; workload image pins/identities in receipts`.
- Completed `True`, valid `True`, reportable `False`. Outcome counts: `{"fail": 1, "not_applicable": 1, "observed": 2, "pass": 25}`. Raw steps: 70; every stdout/stderr file exists: `True`.
- All applicable checks except stop.a pass. Steps 45/46 confirm successful stop and all A app/PostgreSQL/Redis containers exited. Step 47 devpod ssh recreates/starts those containers and returns identity with PostgreSQL started at 15:46:07 UTC. This is auto-resume behavior, incorrectly declared false by the adapter, not evidence that native stop failed. CLI v0.6.15, asset SHA-256 0c50934f4199732ff37e89708bc5c028ecda75d854a151cb2644e4ba39f4d7f7. Actual workload Python 3.13.16, uv 0.12.23, PostgreSQL 17.6, Redis 8.10.2. PostgreSQL 17.6 is a declared lane deviation. No separate service-log artifact paths are declared; native workspace lifecycle output is retained in raw command logs.
- Retained outcome `stop.a` = `fail`: stop exit 0, probe exit 0, app after stop exit 0; evidence steps `[45, 46, 47]`.
- Version receipt files: `logs/0004-tool-version.stdout` and `logs/0004-tool-version.stderr` exit 0, `logs/0010-a-tool-versions.stdout` and `logs/0010-a-tool-versions.stderr` exit 0, `logs/0026-b-tool-versions.stdout` and `logs/0026-b-tool-versions.stderr` exit 0, `logs/0057-c-tool-versions.stdout` and `logs/0057-c-tool-versions.stderr` exit 0.
- Native artifact files copied: 0. Copy/probe receipts and file sizes/hashes are preserved in `bench/results/runtime-resume-verification/smoke-devpod-1.json`; native raw diagnostics remain in `logs/`.
- Cleanup problems `[]`; artifact errors `[]`. Independent exact container/network/volume/image name and label queries, full image-label inspection, host process and tempdir checks show outer resources absent: `True`. This verification includes Lando's normalized ownership token where applicable.

## DDEV

- Exact attempt: `bench/results/smoke-ddev-1`; run `20261006t154657-415b85`; 2026-10-06T15:46:57Z to 2026-10-06T15:47:04Z.
- Source: `ff4d71824c6027df2c09be1b70ea5953bdc50d4c`; transport `host`; image `host transport; workload image pins/identities in receipts`.
- Completed `True`, valid `True`, reportable `False`. Outcome counts: `{"fail": 1, "observed": 1, "pass": 1}`. Raw steps: 9; every stdout/stderr file exists: `True`.
- Environment blocked at step 5 a-setup, before any workload. DDEV v1.25.4 installed and ran its CLI version receipt; binary SHA-256 33314cdadac214c630023ca3f7a82a6d0b743814a7ebf56ea53bc6d776dfb5af. stderr says: Failed to pull DDEV images: error getting credentials - err: exit status 1, out: ``. This is host Docker credential-helper failure, not a DDEV product outcome. The harness retains setup.a=fail and meta.blocked is absent; this review classifies the cause without rewriting the result. PostgreSQL 17.6-bookworm, Python 3.13.16 and Redis 8.10.2 remain intended pins only. No workload versions, identities or service logs were realized.
- Retained outcome `setup.a` = `fail`: exit 1; evidence steps `[5]`.
- Version receipt files: `logs/0003-tool-version.stdout` and `logs/0003-tool-version.stderr` exit 0.
- Blocker raw files: `logs/0005-a-setup.stdout`, `logs/0005-a-setup.stderr`; native exit `1`, outer timed_out `False`.
- Native artifact files copied: 0. Copy/probe receipts and file sizes/hashes are preserved in `bench/results/runtime-resume-verification/smoke-ddev-1.json`; native raw diagnostics remain in `logs/`.
- Cleanup problems `[]`; artifact errors `[]`. Independent exact container/network/volume/image name and label queries, full image-label inspection, host process and tempdir checks show outer resources absent: `True`. This verification includes Lando's normalized ownership token where applicable.

## Lando

- Exact attempt: `bench/results/smoke-lando-1`; run `20261006t154724-37151b`; 2026-10-06T15:47:24Z to 2026-10-06T15:48:10Z.
- Source: `ff4d71824c6027df2c09be1b70ea5953bdc50d4c`; transport `host`; image `host transport; workload image pins/identities in receipts`.
- Completed `True`, valid `True`, reportable `False`. Outcome counts: `{"fail": 1, "observed": 1, "pass": 3}`. Raw steps: 14; every stdout/stderr file exists: `True`.
- Environment blocked at step 9 a-start, before any workload. Lando v3.26.9, python plugin 1.4.3, postgres plugin 1.6.0 and redis plugin 1.3.0 install and version receipts succeed. Verified CLI SHA-256 8dd9b6306a05816c3d8cdb2f4c4b9fd6d9df76490992fb0f2e331a894fd92b95; supplied Compose 2.40.3 SHA-256 8cd7eb5f95bacb536cc407111662e2c205d67d9abfea5dcb8400be8418db60d1. Startup still bootstraps another orchestrator under private _state/lando/bin/docker-compose-v2.40.3, which has no separate runtime hash receipt. It attempts host CA installation and fails with sudo requiring a terminal/password, then image pulls fail with error getting credentials - err: exit status 1, out: ``. No sudo credentials were supplied. Skipping explicit lando setup did not prevent bootstrap. Preserve both environment blockers and the adapter configuration defects. Intended PostgreSQL 17.6.0 Bitnami / Redis 8.10.2 / Python 3.13.16 / uv 0.12.23 were not realized. The harness retains start.a=fail and meta.blocked is absent; this review does not edit that metadata.
- Retained outcome `start.a` = `fail`: start exit 1; evidence steps `[9]`.
- Version receipt files: `logs/0005-tool-version.stdout` and `logs/0005-tool-version.stderr` exit 0.
- Blocker raw files: `logs/0009-a-start.stdout`, `logs/0009-a-start.stderr`; native exit `1`, outer timed_out `False`.
- Native artifact files copied: 0. Copy/probe receipts and file sizes/hashes are preserved in `bench/results/runtime-resume-verification/smoke-lando-1.json`; native raw diagnostics remain in `logs/`.
- Cleanup problems `[]`; artifact errors `[]`. Independent exact container/network/volume/image name and label queries, full image-label inspection, host process and tempdir checks show outer resources absent: `True`. This verification includes Lando's normalized ownership token where applicable.

## Process Compose 1.122.0 (Nix toolchain)

- Exact attempt: `bench/results/smoke-process-compose-2`; run `20261006t154835-48a018`; 2026-10-06T15:48:35Z to 2026-10-06T15:52:12Z.
- Source: `ff4d71824c6027df2c09be1b70ea5953bdc50d4c`; transport `docker`; image `sha256:fbf87c18aced2d3c969df40d15b137943429882f4283fe256c2f481a7af17c6a`.
- Completed `True`, valid `True`, reportable `False`. Outcome counts: `{"fail": 1, "observed": 2, "pass": 26}`. Raw steps: 83; every stdout/stderr file exists: `True`.
- All applicable checks except occupied_port pass. CLI Process Compose 1.122.0 commit 23b0aca, binary SHA-256 3f8f17edb599c94e805f465ac2b70efb676e334665aaca2cccff0b21e53910f7. Step 63 verifies the owned port listener; step 64 detached start succeeds; step 65 native readiness returns 124 after its 120-second internal timeout and emits bounded diagnostics. Retained E processes.log contains PostgreSQL Address already in use and port 25436; manager log records PostgreSQL exit 1 followed by project shutdown. occupied_port remains fail, readiness-infra-fault. No timeout-to-pass conversion. Actual workload Python 3.13.15, uv 0.12.22, PostgreSQL 17.11, Redis 8.10.2; Python patch and uv differ from container-family lanes. Both are realized logs: .rwb-state/logs/process-compose.log and processes.log. A processes.log is present but empty; B and E process logs are nonempty. C/D artifact probes can be absent because they did not start services.
- Retained outcome `occupied_port` = `fail`: readiness-infra-fault (exit 124); evidence steps `[63, 64, 65]`.
- Version receipt files: `logs/0004-tool-version.stdout` and `logs/0004-tool-version.stderr` exit 0, `logs/0011-a-tool-versions.stdout` and `logs/0011-a-tool-versions.stderr` exit 0, `logs/0026-b-tool-versions.stdout` and `logs/0026-b-tool-versions.stderr` exit 0, `logs/0056-c-tool-versions.stdout` and `logs/0056-c-tool-versions.stderr` exit 0.
- Native artifact files copied: 6. Copy/probe receipts and file sizes/hashes are preserved in `bench/results/runtime-resume-verification/smoke-process-compose-2.json`; native raw diagnostics remain in `logs/`.
- Cleanup problems `[]`; artifact errors `[]`. Independent exact container/network/volume/image name and label queries, full image-label inspection, host process and tempdir checks show outer resources absent: `True`. This verification includes Lando's normalized ownership token where applicable.

## services-flake (process-compose-flake)

- Exact attempt: `bench/results/smoke-services-flake-2`; run `20261006t155227-bd13fe`; 2026-10-06T15:52:27Z to 2026-10-06T15:56:11Z.
- Source: `ff4d71824c6027df2c09be1b70ea5953bdc50d4c`; transport `docker`; image `sha256:fbf87c18aced2d3c969df40d15b137943429882f4283fe256c2f481a7af17c6a`.
- Completed `True`, valid `True`, reportable `False`. Outcome counts: `{"fail": 1, "observed": 2, "pass": 26}`. Raw steps: 91; every stdout/stderr file exists: `True`.
- All applicable checks except occupied_port pass. Fixed A/C tool-version receipts now exit 0 via services version, and lock.frozen_copy passes. services-flake source 0ba7183cab54ffbd0be70cb95694f024701afd2b; actual bundled Process Compose 1.122.0 commit 673850d, distinct from the standalone Process Compose commit. Step 62 verifies the owned port listener; step 63 detached start succeeds; step 64 native readiness returns 124 after its 120-second internal timeout. Retained E processes.log includes native PostgreSQL bind failure and port 25436. occupied_port remains fail, readiness-infra-fault. Actual workload Python 3.13.15, uv 0.12.22, PostgreSQL 17.11, Redis 8.10.2. Native logs are copied from .rwb-state/sf/process-compose.log and processes.log, excluding database directories. C has manager log but no process log. Python patch and uv differ from container-family lanes.
- Retained outcome `occupied_port` = `fail`: readiness-infra-fault (exit 124); evidence steps `[62, 63, 64]`.
- Version receipt files: `logs/0003-tool-version.stdout` and `logs/0003-tool-version.stderr` exit 0, `logs/0010-a-tool-versions.stdout` and `logs/0010-a-tool-versions.stderr` exit 0, `logs/0025-b-tool-versions.stdout` and `logs/0025-b-tool-versions.stderr` exit 0, `logs/0055-c-tool-versions.stdout` and `logs/0055-c-tool-versions.stderr` exit 0.
- Native artifact files copied: 7. Copy/probe receipts and file sizes/hashes are preserved in `bench/results/runtime-resume-verification/smoke-services-flake-2.json`; native raw diagnostics remain in `logs/`.
- Cleanup problems `[]`; artifact errors `[]`. Independent exact container/network/volume/image name and label queries, full image-label inspection, host process and tempdir checks show outer resources absent: `True`. This verification includes Lando's normalized ownership token where applicable.

## dnvr a66c2bb (tmux runner)

- Exact attempt: `bench/results/smoke-dnvr-2`; run `20261006t155633-f096d1`; 2026-10-06T15:56:33Z to 2026-10-06T16:00:45Z.
- Source: `ff4d71824c6027df2c09be1b70ea5953bdc50d4c`; transport `docker`; image `sha256:fbf87c18aced2d3c969df40d15b137943429882f4283fe256c2f481a7af17c6a`.
- Completed `True`, valid `True`, reportable `False`. Outcome counts: `{"observed": 2, "pass": 27}`. Raw steps: 78; every stdout/stderr file exists: `True`.
- All applicable checks pass, including lock.frozen_copy and occupied_port. Step 59 verifies the owned listener. Step 60 start returns its original exit 1 after the 120-second pg.url readiness-key wait; it is not an outer timeout. Its stderr and retained E pg.json name Address already in use and port 25436. The conflict gate passes on actual native diagnostic evidence without changing the start exit. All 26 native artifact files are retained, including PostgreSQL JSON/text logs, tmux pane logs, and PTY transcripts. Actual dnvr source pin a66c2bbabb67293812a5c39855ab0ecf6af21d41 is untagged, not a CLI version string. Fixed A/C tool-version receipts both succeed, including Redis and tmux, and lock.frozen_copy passes. Actual workload Python 3.13.15, uv 0.12.22, PostgreSQL 17.11, Redis 8.10.2 and tmux 3.7c are recorded by successful A receipt. PTY driver start/readiness and Ctrl-C stop are scripting around the native tmux runner; their time scope stays declared. Native artifacts are copied from .dnvr/logs. Python patch and uv differ from container-family lanes.
- Version receipt files: `logs/0003-tool-version.stdout` and `logs/0003-tool-version.stderr` exit 0, `logs/0009-a-tool-versions.stdout` and `logs/0009-a-tool-versions.stderr` exit 0, `logs/0023-b-tool-versions.stdout` and `logs/0023-b-tool-versions.stderr` exit 0, `logs/0052-c-tool-versions.stdout` and `logs/0052-c-tool-versions.stderr` exit 0.
- Native artifact files copied: 26. Copy/probe receipts and file sizes/hashes are preserved in `bench/results/runtime-resume-verification/smoke-dnvr-2.json`; native raw diagnostics remain in `logs/`.
- Cleanup problems `[]`; artifact errors `[]`. Independent exact container/network/volume/image name and label queries, full image-label inspection, host process and tempdir checks show outer resources absent: `True`. This verification includes Lando's normalized ownership token where applicable.

## Transport and timeout limits

No outer DockerTransport timeout was naturally exercised in these lanes. The Process Compose and services-flake 124 statuses are native internal timeout exits, with outer timed_out=false; they do not exercise TIMEOUT_KILL. Docker exec lane wrapper steps ran as agent, so mixed root/agent PID registration and root timeout group cleanup remain unverified by this diagnostic batch. The live Process Compose registration receipt, bench/results/runtime-resume-verification/smoke-process-compose-2-live-pids.json, confirms actual separate per-sequence mode-0700 agent:agent directories and captures Linux process groups/sessions. No synthetic timeout, sandbox relaxation, or signal to an archived PID was added.

## Bounded implementation handoff

Only Opus should implement these changes, then Astra should review them and the parent should commit. This executor changed no harness, adapters, fixtures or product source. Preserve every failed attempt and use new output names after approved fixes.

1. DevPod: declare entry_auto_resumes=true in bench/rwb/adapters/devpod.py and correct the conflicting stopped-workspace assertion in its module docstring and CONTAINER-ADAPTERS.md. Research already describes auto-start. Use the existing stop/restart protocol for auto-resuming entries; keep the ownership-checked stopped probe and explicit restart/persistence receipts. Add an offline regression asserting no after-stop app command for this adapter; rerun DevPod. Do not relabel this original stop.a failure.

2. DDEV/Lando: preserve the raw Docker credential-helper failure as an environment blocker. Adapt their host client environment to the already-reviewed private Docker-config pattern in ComposeAdapter.host_env/provision, retaining the current daemon context and anonymous public pulls without modifying ~/.docker or copying private registry credentials. Verify the actual DDEV/Lando engine path uses the private config. Add a prerequisite classification path if this still cannot be established, rather than presenting an unreachable image as a product failure. No sandbox relaxation or global credential change.

3. Lando: prevent automatic host CA installation through pinned, documented private bootstrap settings. The research recipe already names setup.skipInstallCa; verify the exact v3.26.9 hook behavior rather than blindly invoking broader lando setup. Fix/verify orchestrator selection so startup uses the checksum-verified executable and records its actual identity. Current top-level orchestratorBin did not suppress auto-download. Keep pinned plugin and Compose versions, and prove no sudo/global service configuration is attempted on the fresh run.

4. Native-extra: all three version/log fixes are realized; dnvr has no new confirmed adapter defect. For Process Compose/services-flake, both native 120-second occupied-port failures remain. These are diagnostic readiness behavior, not a missing-log defect and not evidence of an outer transport cleanup fault. Any later recipe change to terminal-failure handling requires an explicit semantics decision and review; keep the present native deadline and failure result until then.

## Final verification

All seven attempts are sequential and non-overlapping by their recorded Unix intervals. All use the same harness file hashes. Final source-byte, interrupted-result and unrelated-resource checks are recorded separately under bench/results/runtime-resume-verification/final-state.json. All 27 preexisting containers retain their original IDs, images, states and start timestamps. Network and volume inventories match the pre-cleanup snapshot exactly; ev-nix retains the image ID used by all three native retries. Harness, adapter config, fixture and shared-glue hashes are unchanged. No live benchmark executor remains at handback. All requested attempts are diagnostic only; other roster lanes and final measurements were outside this executor brief.
