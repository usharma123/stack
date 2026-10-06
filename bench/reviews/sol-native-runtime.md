# Native runtime diagnostics

These are sequential diagnostic runs, with 2 repeats and 1 warmup. They are not final performance results. Raw receipts are preserved in each result directory. No implementation changes were made by this executor.

## Process Compose

- Result: `bench/results/smoke-process-compose-1`, run `20261006t144928-e45c28`.
- Source: `b0cb035719a15fada54cb53c8eb646f23fa28760`, dirty due to concurrent container-family work. Full source/config hashes are preserved in `meta.json`. Native adapter SHA-256 `b88fa9488524338fb950b27e381d8f34d641d17f26ec8b98bfddfd8cbc5fa633`.
- Status: valid, completed, reportable false. Every main lifecycle/workload/isolation/persistence/lock/bad-config check passed except `occupied_port`, which failed with `readiness-infra-fault (exit 124)`.
- Actual versions: Process Compose 1.122.0 commit 23b0aca; Determinate Nix 3.23.0 / Nix 2.35.2; Python 3.13.15; uv 0.12.22; PostgreSQL 17.11; Redis 8.10.2.
- Failure evidence: step 64 detached start exits 0; step 65 native `project is-ready --wait` gives no conflict evidence before 120s timeout; step 67 cleanup says no manager answers on E's UDS. No service log artifacts were declared, so the manager failure reason was not preserved.
- Classification: a real 120-second native-readiness deadline occurred. Missing log retention is a recipe gap; product conflict behavior is unresolved. Preserve native readiness and exit 124. Add bounded diagnostics on failure and retain service logs. Do not map the timeout to a passing conflict check.
- Cleanup: steps 70/71 have empty service/supervisor lists; step 72 removes the container successfully. Independent Docker queries for this exact run label returned no containers, networks or volumes, all exit 0. No cleanup problems.

## services-flake

- Result: `bench/results/smoke-services-flake-1`, run `20261006t145407-7ac1b3`.
- Source: `f29e8f07737a68c040870535a9a6605ccd9d6cef`; full source/config hashes in `meta.json`. Adapter SHA-256 `e5326fc650af24c0efc2a3741d56fd1f914bd403221bc08f6efa83be001ca8e0`.
- Status: valid, completed, reportable false. Main lifecycle/workload/isolation/persistence/bad-config checks pass. `lock.frozen_copy` fails with versions differ; `occupied_port` fails with readiness-infra-fault exit 124.
- Actual versions: Determinate Nix 3.23.0 / Nix 2.35.2; Python 3.13.15; uv 0.12.22; PostgreSQL 17.11; Redis 8.10.2. Bundled Process Compose version is pinned to 1.122.0 but has no successful CLI version receipt in this run.
- Recipe bug: steps 10 and 55, A/C tool-version commands exit 127 with `process-compose: command not found`, though both print the same other versions. The environment exposes the `services` wrapper. Proposed minimal fix: require `services` and obtain its bundled CLI version via `services version` in `ServicesFlakeAdapter.tool_versions()`.
- Occupied port: start step 63 exits 0; native readiness step 64 times out after 120 seconds without conflict evidence. No declared service-log artifacts. Preserve deadline/native exit semantics; add log artifacts and bounded failure diagnostics as proposed for Process Compose.
- Cleanup: steps 69/70 have empty service/supervisor lists; step 71 removes the container. Independent Docker queries for this exact run label show no containers, networks or volumes, all exit 0. No cleanup problems.

## pkgx

- Result: `bench/results/smoke-pkgx-1`, run `20261006t145935-99505a`.
- Source: `9486e72b90aa3300e32968d06882b67f41ff4424`; full source/config hashes in `meta.json`. Adapter SHA-256 `df72533be1bc087b874fb5c827dc40cb36bf68bec244e4c90d20402f96b5534c`.
- Status: valid, completed, reportable false; all applicable checks pass. Lock creation and frozen setup are unsupported, as declared.
- Actual versions: pkgx 2.11.0; dev 1.8.1; Pantry commit 2df061bd184985428bc17aba4a8a8c1e2fd39781; Python 3.13.15; uv 0.12.22; PostgreSQL 17.2; Redis 8.10.0. Preserve PostgreSQL and Redis deviations from the canonical workload.
- Service lifecycle, readiness, per-checkout ports/data, isolation, status and stop confirmation are benchmark scripting. The successful occupied-port refusal at step 57 is the shared script's diagnostic, not a native pkgx service feature.
- Cleanup: steps 62/63 have empty service/supervisor lists; step 64 removes the container. Independent Docker queries for this exact run label show no containers, networks or volumes, all exit 0. No cleanup problems.
- Unresolved fixes: none established by this diagnostic run.

## dnvr

- Result: `bench/results/smoke-dnvr-1`, run `20261006t150105-3e8efe`.
- Source: `9486e72b90aa3300e32968d06882b67f41ff4424`; full source/config hashes in `meta.json`. Adapter SHA-256 `27dc88155fea190883475f68687f107bf0730a462c22de524d4a2ec5989ab2d6`.
- Status: valid, completed, reportable false. Lifecycle/workload/isolation/persistence/bad-config pass. Native tmux runner, scripted PTY readiness and Ctrl-C stop workflow were exercised successfully in A/B. Start timings include the PTY driver and readiness.
- Actual versions: Python 3.13.15, uv 0.12.22 and PostgreSQL 17.11 from partial CLI receipts; Redis 8.10.2 from actual app/server identity. dnvr untagged commit a66c2bbabb67293812a5c39855ab0ecf6af21d41 is a source pin, not a CLI version. tmux store path says 3.7c, but its CLI version command was never reached.
- Recipe bug: A/C tool-version commands at steps 9/52 exit 127 because `redis-server` is absent from the devshell PATH. Redis is only a runtime input of the process wrapper. This produces the false frozen-copy mismatch despite successful frozen setup and unchanged lock. Minimal fix: add pkgs.redis to the declared devshell packages so the requested version receipt succeeds. Different checkout-specific dnvr executable paths are recorded as observed.
- Occupied port: step 60 exits 1 after the 120s pg.url deadline and prints only generic readiness-key failure plus exited/running status. The check fails because the evidence lacks the conflict diagnostic. A read-only copy at `artifacts-manual/e-conflict-live-logs/pg.json` preserves PG's explicit address-in-use diagnostic and port 25436. Proposed minimal fix: print bounded native logs on driver readiness failure, retain exit 1 and declare `.dnvr/logs` artifacts.
- Additional raw preservation: `artifacts-manual/a-live-logs` and `b-live-logs` are snapshots copied read-only before teardown, after verifying run ownership. They are partial snapshots, not final complete logs. E's conflict snapshot is also preserved.
- Cleanup: steps 65/66 have empty service/supervisor lists; step 67 removes the container. Independent Docker queries for this exact run label show no containers, networks or volumes, all exit 0. No cleanup problems.

## Guix

- Result: `bench/results/smoke-guix-1`, run `20261006t150655-589109`.
- Source: `9486e72b90aa3300e32968d06882b67f41ff4424`; full source/config hashes in `meta.json`. Adapter SHA-256 `7730659e3de1846f177fbd81a40e143b33ad2beadb77c64c2afd6c11fbcf11b3`.
- Status: valid, completed, environment blocked, reportable false, empty timings. All workload checks are blocked. This is not a Guix product failure or performance measurement.
- Actual version: install receipt step 4 prints `guix (GNU Guix) 1.5.0`. The intended workload versions Python 3.13.13, PostgreSQL 16.14, Redis 7.2.6 and uv 0.10.12 are source pins only; no workload environment was realized. Preserve the PostgreSQL 16 / Redis 7 deviation.
- Provisioning: mirror archive download succeeds and its SHA-256 is verified as `a5d58b1d0294cad6adb1f2aff627d37feb5db763fdffbceb8551f2b12123cf39`. Default daemon sandbox was retained, no flags were passed. Both preflight namespace probes were denied. The canary successfully fetched a hello substitute, then local build failed `guix build: error: clone: Operation not permitted`. Step 5 returns 77 with RWB-BLOCKED.
- Cleanup: steps 6/7 show no service/supervisor processes; step 8 removes the container. Independent Docker queries for this exact run label show no containers, networks or volumes, all exit 0. No cleanup problems.
- Ancillary core gap: cleanup receipts 6/7 emit `/tmp/rwb-pids/<seq>: Permission denied`, though their commands exit 0. Root provisioning creates the shared pid-registration directory before agent commands. A future timed-out agent command in a mixed-user lane could lack its pid file. Proposed separate core fix: ensure user-writable, owned per-step pid registration. This run is safe because its container was removed and verified absent. No implementation changes made here.

## Final state and patch briefs

All five first diagnostics are complete, sequentially. No benchmark remains running. Every run has valid trustworthy receipts and verified outer-resource cleanup; valid does not mean every check passed. No retries or implementation edits were performed by this executor.

- Process Compose: add `.rwb-state/logs` artifacts and bounded status/log diagnostics in `ProcessComposeAdapter`, currently hooks absent at `bench/rwb/adapters/process_compose.py:125`. In `bench/adapters/process-compose/rwb-pc.sh:35`, capture the native readiness exit status solely to print diagnostics, then preserve that exact status and the native 120s deadline. Add `conflict_logs()` for applicable consumers; native occupied-port evidence currently comes from readiness stdout/stderr, so this hook alone does not change the current result.
- services-flake: use the actual `services` wrapper for the bundled Process Compose version receipt in `bench/rwb/adapters/services_flake.py:48`. Retain `.rwb-state/sf` logs and bounded readiness failure diagnostics, preserving deadline/status.
- dnvr: expose pkgs.redis in `bench/adapters/dnvr/dnvr/flake.nix:35` for the existing version command. Declare `.dnvr/logs` artifacts and print bounded native service logs at the existing readiness-key failure in `bench/adapters/dnvr/rwb-dnvr.sh:57`, retaining the existing refusal status.
- Guix: default sandbox is denied by this container environment. Do not relax daemon sandbox flags or classify this as a tool failure. No source change is needed to make the blocked result honest.

### Source receipts

| Tool | run.py SHA-256 | Adapter SHA-256 | Config hashes |
|---|---|---|---|

| process-compose | `92e0239e3b2f275bd54d12f2bb4b75862fc2621f97363c6cbb03d57c153b1651` | `b88fa9488524338fb950b27e381d8f34d641d17f26ec8b98bfddfd8cbc5fa633` | preserved in that run meta.json |
| services-flake | `92e0239e3b2f275bd54d12f2bb4b75862fc2621f97363c6cbb03d57c153b1651` | `e5326fc650af24c0efc2a3741d56fd1f914bd403221bc08f6efa83be001ca8e0` | preserved in that run meta.json |
| pkgx | `92e0239e3b2f275bd54d12f2bb4b75862fc2621f97363c6cbb03d57c153b1651` | `df72533be1bc087b874fb5c827dc40cb36bf68bec244e4c90d20402f96b5534c` | preserved in that run meta.json |
| dnvr | `040c5f51c6c8df2be9e57f9854366d687a04f6d194ac9c9b680e4e6142aa5deb` | `27dc88155fea190883475f68687f107bf0730a462c22de524d4a2ec5989ab2d6` | preserved in that run meta.json |
| guix | `040c5f51c6c8df2be9e57f9854366d687a04f6d194ac9c9b680e4e6142aa5deb` | `7730659e3de1846f177fbd81a40e143b33ad2beadb77c64c2afd6c11fbcf11b3` | preserved in that run meta.json |
