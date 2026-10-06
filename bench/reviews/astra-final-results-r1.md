# Astra final results review, round 1

Verdict: APPROVE for publication of the descriptive metrics specified below. NO BLOCKING FINDINGS.

Reviewed checkout `/Users/utsavsharma/.t3/projects/stack`, branch `codex/realworld-competitor-bench`, HEAD `ee96b465dc30d6a59933c6d99b499f55194104fc`. This is a final results review. It does not reopen approved implementation or require blocked tools to pass.

The parent may copy this exact review to `bench/reviews/astra-final-results-r1.md` and use that path as `session.review` and each attempt’s `result_review`. Approval covers 24 workload lanes, the DDEV adapter-block evidence, the Guix environment-block evidence, and exclusion of the interrupted dnvr attempt. It does not authorize timings for the latter three evidence records.

No repository or raw evidence was edited. Only `/tmp` review scripts, receipts, candidate manifests and reports were created. No services, tests, builds, benchmark reruns, network installs, delegation or commits ran. The previously reported 382-test suite result was not independently rerun.

## Publication metadata finalization

Severity P3, nonblocking for metric validity. `bench/measurements/final-20261006-reviewed-1/manifest.json:1982` still describes 26 invocations and a promise throughout the session. There are 27 attempts with one interruption. The separate resumption declaration at line 1440 is accurate, but `report.py:555` selects session fields and does not copy that declaration into the generated report. Update the mutable manifest’s serialization text to explicitly cover 15 completed attempts, one interrupted attempt, and 11 resumed completed attempts. Limit the concurrency attestation to the two executor-active periods. Do not call the intervening interval idle. Evidence is the 27 descriptors, the partial raw tree, the sealed supplement and the removal receipts described below.

Expected review finalization, not a finding: populate `session.review` at line 1441 and all attempt `result_review` fields; change the mutable `session.result_review` and attempt `review_status` metadata from pending to reviewed. Replace the pending-review sentence in `session.timing_policy` at line 1983. Preserve the original plan and supplement bytes, run states, raw hashes, all failures and block classifications. Historical Sol verification remains a valid pre-review record and must not be rewritten as approval.

A concrete metadata-only candidate is `/tmp/astra-results-r1/publication-candidate-manifest.json`. Its reporter invocation exited 0 with complete 26-entry evidence coverage, exactly 96 eligible metrics and eight omitted metric rows. The candidate’s metric values and omissions are identical as parsed JSON to the review-reference-only candidate. Parent publication must regenerate the report against the final repository manifest path and hash. These `/tmp` reports are review dry runs, not a claim that publication has occurred.

## Exactly approved metrics

Every row below is approved for `first_task.a`, `first_task.b`, `repeat.entry` and `repeat.app_read`, variant `default`, for the selected run in the reviewed manifest. First-task values below are summed outer task-step seconds. Repeats are outer p50 / p95 milliseconds. Full wall spans, excluded preparation, receipt sequences, warmups and inner distributions remain in the candidate report. These are observations, not rankings.

| Tool | first_task.a s | first_task.b s | repeat.entry p50 / p95 ms | repeat.app_read p50 / p95 ms |
|---|---:|---:|---:|---:|
| stack | 11.048 | 3.636 | 87.497 / 113.021 | 205.405 / 238.397 |
| mise | 10.405 | 3.573 | 73.548 / 111.284 | 195.845 / 255.729 |
| flox | 35.672 | 6.046 | 74.993 / 97.757 | 210.869 / 247.051 |
| devbox | 71.174 | 6.984 | 117.768 / 286.232 | 238.773 / 298.21 |
| devenv | 25.276 | 13.265 | 101.634 / 110.878 | 235.787 / 367.231 |
| nix | 31.839 | 13.22 | 866.5 / 1084.716 | 965.862 / 1094.007 |
| pixi | 8.736 | 4.176 | 73.573 / 81.927 | 179.013 / 187.914 |
| compose | 18.752 | 14.929 | 71.296 / 82.307 | 628.934 / 676.348 |
| devcontainers | 14.229 | 13.242 | 355.302 / 387.364 | 947.126 / 1081.168 |
| devpod | 16.83 | 16.945 | 781.406 / 1010.087 | 1436.507 / 1674.465 |
| lando | 18.807 | 17.497 | 291.895 / 312.633 | 871.643 / 911.778 |
| process-compose | 29.924 | 13.025 | 879.626 / 962.429 | 1018.763 / 1486.801 |
| services-flake | 33.151 | 15.59 | 898.312 / 1008.007 | 988.412 / 1061.521 |
| pkgx | 11.785 | 3.66 | 136.772 / 144.844 | 260.317 / 350.973 |
| dnvr | 50.034 | 15.168 | 950.476 / 1043.441 | 976.824 / 1089.532 |
| workz | 8.362 | 5.984 | 21.108 / 23.113 | 181.461 / 203.449 |
| worktrunk | 7.555 | 7.475 | 28.654 / 31.336 | 180.093 / 184.15 |
| git-grove | 9.662 | 8.132 | 109.905 / 120.493 | 637.6 / 681.99 |
| isola | 9.469 | 2.166 | 64.35 / 72.939 | 172.651 / 194.172 |
| berth | 11.786 | 9.22 | 110.024 / 121.03 | 587.888 / 641.14 |
| branchbox | 8.804 | 7.005 | 161.961 / 174.607 | 624.765 / 653.074 |
| tilt | 10.08 | 8.753 | 67.249 / 73.848 | 254.296 / 358.355 |
| organist | 52.36 | 16.377 | 1132.498 / 1226.317 | 1308.313 / 1377.731 |
| vagrant | 35.971 | 30.478 | 2456.605 / 2667.259 | 2516.719 / 3115.045 |

No additional exclusion applies to these 96 metric groups. Each repeat group has exactly 20 samples after three excluded warmups. The 13 eligible docker-transport lanes have 20 inner samples per repeat group. The 11 eligible host-transport lanes have no inner timing receipts, so their inner p50/p95 remain null and must not be represented as zero or compared with measured inner times.

DDEV omits all four metrics because setup exits before the remaining main outcomes exist. `ddev/logs/0005-a-setup.stderr:1` records a pull attempt for the not-yet-built local `ddev-rwb-20261006t173718-c83372-a-app` image. This is an adapter blocker. The retained setup failure does not establish a DDEV product failure or startup time.

Guix omits all four metrics because provisioning stops at the build-sandbox canary with exit 77, `clone: Operation not permitted`, and `RWB-BLOCKED` in `guix/logs/0005-provision-guix-canary.stderr`. No successful tool-version receipt or workload execution follows. This is an environment blocker; desired runtime pins are not realized versions.

The original `dnvr` directory is excluded entirely. Its missing outcomes and final interval are expected interruption evidence, not defects in the selected `dnvr-retry-1` run. The reporter’s excluded-row evidence warnings are preserved. Its partial samples contribute to none of the approved distributions.

## Verified integrity and chronology

I independently hashed every file in all 27 raw directories, compared all 26 final inventories with `resume-final-integrity.json`, regenerated all 26 attempt descriptors, checked the interrupted tree against its separate inventory, and reparsed all 2,740 final steps, 5,480 stdout/stderr references and 142 retained artifact files. All reporter artifact-copy and containment gates passed. The 15 earlier attempt descriptors exactly match `pre-resume-manifest.json`.

The independent audit passed 8,960 assertions; the supplemental chronology audit passed 20. It reparsed all 480 entry samples, 480 app-read samples and 144 warmups. Every app read returns an `ok` read result with SKU `keeper-a`. Both checkout identities in every eligible lane match successful source-token writes and adapter-declared module paths. URL/server ports or retained port-map receipts match. Distinct PostgreSQL/Redis storage keys and successful app isolation checks establish each declared boundary. Migration, CRUD, cache and pytest receipts passed, first-task sums match raw step durations, and nearest-rank p50/p95 values were independently recomputed.

The original plan was sealed at 17:19:10Z before the first measurement at 17:19:21Z. All 26 completed nanosecond intervals are ordered and nonoverlapping. The partial dnvr run starts at 17:48:31Z and its last receipt is `repeat.app_read-sample-009` at 17:50:25Z; the cancellation instant is unknown. A 17:52:06Z snapshot retains exactly one extra container. Two captured identities agree on its full ID, name, image, creation time, start time, live state and ownership labels. Exact container `746ad57ecd0107f1c93d35dff4204329420b2823b4635adac2688e9146477f45` is removed at 17:53:26Z; the clean baseline at 17:53:29Z proves absence. The supplement is sealed at 17:54:36Z, before retry at 17:54:37Z. The remaining execution order matches both supplement and original roster. Parent authorization and the no-concurrent-workers statement are accepted as attestations; interval receipts do not independently prove whole-host idleness.

All 27 baseline container records, nine network records and 137 volume records exactly match the clean resumption baseline, every recorded per-lane after snapshot and final validation. Container name/image/state/start/restart fields are preserved. Raw inventory command outputs match snapshot keys and succeeded. Final owned-tempdir inventory is empty. The retained handoff process command shows no direct benchmark invocation. No cleanup action was performed during this review.

Frozen harness, fixture, shared-glue and lane-config hashes match current files and all run declarations. Source-approval hashes and the frozen reporter hash match. Current tracked product source, Cargo.toml and Cargo.lock have no diff against Stack source revision `06c351acc6a0744d918dc571c063ff1f6c300700`. The actual local Linux ARM64 binary hash matches `1d338d2cd92c4ee898037c9ee39dfbccb7c1e3dabae8180fbf65662ac015ce20`, the run’s provision/version receipts and the retained successful build receipt. The benchmark harness revision and older unchanged product-source revision are intentionally distinct.

## Failures and comparison limits that must remain visible

Preserve these observed outcomes. None is a dependency of an otherwise passing first-task or repeat metric under the frozen reporting contract.

| Tool | Outcome | Evidence |
|---|---|---|
| stack | `occupied_port` fail | `stack/outcomes.json`, steps 105, 106; start-failed-without-conflict-diagnostic (exit 1) |
| mise | `bad_config` blocked | `mise/outcomes.json`, steps 95, 96, 97; setup exit 1: failure output lacks the intended diagnostic /99\.99\.99 or postgresql_99/ |
| devpod | `bad_config` blocked | `devpod/outcomes.json`, steps 98, 99, 100, 101; setup exit 0, start exit 1: environment prerequisite failed before the intended refusal: 13:37:08 info devcontainer up: start container: build and extend docker-compose: inspect image python:3.13.99-slim-bookworm: get image config remotely: retrieve image python:3.13.99-slim-bookworm: err |
| process-compose | `occupied_port` fail | `process-compose/outcomes.json`, steps 103, 104, 105; readiness-infra-fault (exit 124) |
| services-flake | `occupied_port` fail | `services-flake/outcomes.json`, steps 102, 103, 104; readiness-infra-fault (exit 124) |
| git-grove | `bad_config` blocked | `git-grove/outcomes.json`, steps 98, 99, 100; setup exit 1: environment prerequisite failed before the intended refusal: ERROR: failed to build: failed to solve: error getting credentials - err: exit status 1, out: `` |
| berth | `bad_config` blocked | `berth/outcomes.json`, steps 92, 93, 94; setup exit 1: environment prerequisite failed before the intended refusal: error getting credentials - err: exit status 1, out: `` |
| branchbox | `bad_config` blocked | `branchbox/outcomes.json`, steps 91, 92, 93, 94; setup exit 0, start exit 1: environment prerequisite failed before the intended refusal: error getting credentials - err: exit status 1, out: `` |
| tilt | `bad_config` blocked | `tilt/outcomes.json`, steps 95, 96, 97, 98; setup exit 0, start exit 1: environment prerequisite failed before the intended refusal: postgres │ error getting credentials - err: exit status 1, out: `` |
| vagrant | `bad_config` blocked | `vagrant/outcomes.json`, steps 92, 93, 94, 95; setup exit 0, start exit 1: environment prerequisite failed before the intended refusal: docker: error getting credentials - err: exit status 1, out: `` |

Stack’s occupied-port failure specifically includes `stop_failed` and “mise daemons stop failed” at `stack/logs/0106-e-start.stdout:1`. Process Compose and services-flake occupied-port readiness exits are 124 and remain failures, not successful conflict detection. There are exactly seven bad-config blocks: mise, DevPod, GitGrove, Berth, BranchBox, Tilt and Vagrant. Mise lacks the required targeted error diagnostic under the frozen classifier; the other six encounter credential/prerequisite failures before the intended refusal.

The host is Darwin 24.6.0 arm64, harness Python 3.12.9, Docker client 28.1.1 and daemon 29.1.3 Linux ARM64. Requested CPU/memory fields are null, meaning default limits, not standardized resource isolation. Existing unrelated services remained running. Cache state was warm from prior diagnostics; there was no global purge. Setup recipes, native worktree preparation, transport overhead, start/readiness boundaries and runtime versions differ. One first-task observation and 20 samples from one run do not support a confidence interval, rank, universal cold-cache claim or native macOS product comparison.

Transport is lane-specific. Host transport does not mean every workload component is native: container lanes invoke native host CLIs but run services/app in Linux containers. Workz and Worktrunk execute the app in actual host checkouts with service containers. GitGrove runs the actual worktree source inside its app container. Isola proves separate databases/logical Redis databases on shared servers, not separate service processes. Worktree lifecycle glue, Nix/Pixi/pkgx service scripts, Flox activation, DNVR readiness and other declared scripted modes must retain their labels. Unsupported and not-applicable outcomes do not become passes. The complete feature map is retained per lane in `audit.json`.

Actual A/B runtime versions agree within each lane and differ across lanes as follows. This table comes from raw identity receipts, not requested pins.

| Tools | Python | PostgreSQL | Redis |
|---|---|---|---|
| stack, mise, compose, isola, berth, branchbox | 3.13.16 | 17.11 | 8.10.2 |
| flox, devenv, nix, pixi, process-compose, services-flake, dnvr, organist | 3.13.15 | 17.11 | 8.10.2 |
| devbox | 3.13.15 | 17.10 | 8.10.2 |
| devcontainers, devpod, lando, workz, worktrunk, git-grove, tilt, vagrant | 3.13.16 | 17.6 | 8.10.2 |
| pkgx | 3.13.15 | 17.2 | 8.10.0 |

Raw successful CLI version receipts were read for all 25 lanes that reached them and satisfy the frozen version strings. Complete CLI text, hashes, package/source pins and uv differences remain in `resume-independent-result-evidence.json` and the raw logs. For example Stack is 0.1.4 with mise 2026.10.3; Flox is 1.17.0-g486737b; Devbox 0.18.4; devenv 2.4.0+b904dcb; Nix 2.35.2 via Determinate 3.23.0; Pixi 0.81.0; standalone Compose 5.6.0 differs from Desktop Compose 2.40.3 used by other recipes. DNVR is source commit `a66c2bbabb67293812a5c39855ab0ecf6af21d41`, not a tagged CLI-version assertion. Organist’s setup log records automatic Nix sandbox fallback; no sandbox-isolation guarantee follows. Guix’s sandbox block remains separate.

The README transport paragraph reflects earlier coverage and should not be used as the final per-lane transport inventory. Publish these results with the recorded lane transports and version table. No benchmark-source fix or rerun is required for this descriptive publication. Raw-bundle packaging is the parent’s pending delivery work and is not a defect in this review.

## Reproduction commands and receipts

The read-only audit scripts and reporter command receipts are retained under `/tmp`. Run only in a fresh scratch output directory if reproducing; reporter output directories must not already exist.

```sh
python3 -B /tmp/astra-results-audit-r1.py
python3 -B /tmp/astra-results-extra-r1.py
python3 -B bench/report.py --manifest bench/measurements/final-20261006-reviewed-1/manifest.json --out /tmp/astra-results-r1/pending --require-complete
python3 -B bench/report.py --manifest /tmp/astra-results-r1/candidate-manifest.json --out /tmp/astra-results-r1/candidate --require-complete
python3 -B bench/report.py --manifest /tmp/astra-results-r1/publication-candidate-manifest.json --out /tmp/astra-results-r1/publication-candidate --require-complete
```

All three reporter commands exited 0. Pending report: complete evidence coverage, zero reported metric groups. Candidate reports: complete evidence coverage, 96 reported metric groups, eight omissions. `pending-command.json`, `candidate-command.json`, `publication-candidate-command.json`, `audit.json`, `extra-audit.json`, and `product-diff-receipt.json` retain the receipts. The initial scratch audit hit a wrong field-name assumption, `run_order`; I corrected it to the actual frozen `roster` before the successful complete audit. Final readback also caught a scratch-only combined-digest ordering mismatch between string and Path ordering for `dnvr` and `dnvr-retry-1`. I corrected the combined-root calculation to Path-component ordering and independently matched the reporter; all per-attempt digests matched throughout. No evidence files were changed.

## SHA256 fingerprints

Paths below are repository-relative unless absolute. Report fingerprints identify these exact dry-run bytes; generated time, manifest path and finalized metadata will produce new publication report hashes.

| File | SHA256 |
|---|---|
| `bench/measurements/final-20261006-reviewed-1/manifest.json` | `a7a35a074217d632270715e80347885a81415dc5806ae4d94ed66fc056b714aa` |
| `bench/measurements/final-20261006-reviewed-1/plan.json` | `d051730e7b85f9c83eb842abbfa64bf8d76a358195487d8b9aac10b78ff4e74e` |
| `bench/measurements/final-20261006-reviewed-1/plan-receipt.json` | `d894693acc9cda1568c0fb65b0979ec1070ad7c1a2ce47fccb110bd767a27036` |
| `bench/measurements/final-20261006-reviewed-1/resume-plan-20261006.json` | `cddb6140471ff55f62449d4a25e6a297ca609d092d58fa32da455a9c691e23c7` |
| `bench/measurements/final-20261006-reviewed-1/resume-plan-20261006-receipt.json` | `47159a4b3845a2925bcef0e886aefe7eb63d0ff150a0f96b4670a70cddcaf320` |
| `bench/measurements/final-20261006-reviewed-1/evidence/resume-final-integrity.json` | `bc4b3ebcf106286a6d2ba5653d77c968407393ca0e302f9d07986d209780a3b5` |
| `bench/measurements/final-20261006-reviewed-1/evidence/resume-independent-result-evidence.json` | `b0f8183a55fa4cd22f30dff8f2844d4ee88c78059a752a6c9b54c1bc54700eeb` |
| `bench/measurements/final-20261006-reviewed-1/evidence/resume-pending-review-report-verification.json` | `a653f086fe996e0d8d3075c38e14eb7100f83d11d6eb8da1fda3d80d0bf2baca` |
| `bench/measurements/final-20261006-reviewed-1/evidence/resume-all26-attempt-descriptors.json` | `803d6aa0619689675e6abf54a5f797975b1cfe664cb4b30a19c1d9a7bee51f87` |
| `bench/measurements/final-20261006-reviewed-1/evidence/resume-final-validation.json` | `bee246ca98d85558c0e443ee9f8b9819a3f3ab1c2e8caf04e7e8d1429b92694f` |
| `bench/measurements/final-20261006-reviewed-1/evidence/resume-20261006-interrupted-container-removal.json` | `7173c371771fb9d53548ebe090a9c7cb126df592bb031b4d7ee5c033bc50e6bd` |
| `bench/report.py` | `f7bacbb250635017cbfcf67711e2ebea430f45babf0379c83f7c9df4dcf9f434` |
| `bench/reviews/sol-final-measurement-session.md` | `9d9cb59c4c3440968d09deb32ee8fd1361d654c93e119644c2fa7fbc57f870dc` |
| `bench/provenance/stack-20261006/original-stack-build-thread-receipts.json` | `dd06255cd6097860b32c62d3b6eb8f5f6277508f47f81b8eba603eafe4f39868` |
| `/tmp/astra-results-r1/audit.json` | `9aba1571b6b7b94b52d541aed4d393a798ec6ee535ba2592273634283cb57ac7` |
| `/tmp/astra-results-r1/extra-audit.json` | `b20245b6513ec7516d0b2c460d58d77df62ad694d4b45dfc6e40202cc368f776` |
| `/tmp/astra-results-r1/candidate-manifest.json` | `f456ef5181447d251ea3faf3a200b4ec3572541d915f7accde7d0c7dac4cce5a` |
| `/tmp/astra-results-r1/candidate/report.json` | `47f29e752be9af8346f77f8938ece773b8d8b4eddb7363642ba79494a3e1b40b` |
| `/tmp/astra-results-r1/candidate/report.md` | `8e4da5a37ed80e2129166824515aee6798726ef85e3665721e879660aeb882fd` |
| `/tmp/astra-results-r1/pending/report.json` | `d13fb857f4d7446aa4c74bce4e2bc212dce2b50ac28c140d1ba02f50cdf394de` |
| `/tmp/astra-results-r1/publication-candidate-manifest.json` | `6142bd038a6de60b1c603c0ffd841c0391a4b3ad34515a8e0e84b51c615f6ab2` |
| `/tmp/astra-results-r1/publication-candidate/report.json` | `84ee661ee629ddcd6b73aa437f7575069293dc39a8b2ef6f21c9eab896653cae` |
| `/tmp/astra-results-r1/publication-candidate/report.md` | `3bbcb04a6cc525bedd8ece98749f3f4f6e26784feff64fddac304794e958355a` |
| `/tmp/astra-results-r1/pending-command.json` | `67a4c1a2047c768c0ad09d72ddaeb8d39c3924658c703276a9c56e4a94655d05` |
| `/tmp/astra-results-r1/candidate-command.json` | `5a2212f0b5bd0bea58b8a9103c661fd8674cce97ea404286bc5feaa210251800` |
| `/tmp/astra-results-r1/publication-candidate-command.json` | `4ce4be65ea142b3bde4d893b1d7dbd80857f9130e03e6389a55f786a6f270cc7` |
| `/tmp/astra-results-audit-r1.py` | `aae030164209b27abcddbf670e18a0c425c3cdbac67f33f0ca351892a3bb143e` |
| `/tmp/astra-results-extra-r1.py` | `ee5df0de7a99aa535877079e994a8b262ad09b9c39d42aba15e001133cbde6f2` |

Raw-tree algorithm: sort every regular file by relative path components, as Python Path ordering does and SHA256-hash the concatenation `relative_path + NUL + sha256(file) + LF`, encoded as UTF-8. This matches the reporter tree algorithm. The combined root digest also includes each attempt directory name.

Combined `bench/results/final-20261006-reviewed-1` digest: `5393a7f0a72da2ceba2b0b13f62ede25d003377bd23c6f0e75694634929f677f`.

| Raw directory | Tree SHA256 |
|---|---|
| `berth` | `db2bd8f07347632775214fb10c600753a4e585f5e86fa63c91fdb0160973caa2` |
| `branchbox` | `d59e12a9b2ccd3991a4bc05022d4c4f32f2af8d341009d6d6af1fca720898418` |
| `compose` | `406ae2cd667b329bab70f55b0033c140d7a258fede5557f0ed7b8b06b8c820ad` |
| `ddev` | `5f40c579a21a9b21486011c08744774a9284c8ee7b3959b159dbf681d47cb84b` |
| `devbox` | `bb1e791a98fae1a13e6b2700d09acbb082d4d75732c9886aa2262aa377f596e2` |
| `devcontainers` | `65c1a9a62478dcdfe06f9a8dc001de1954d06032e128e39a845acd6aea1a05e0` |
| `devenv` | `9b7a46eb8d113ed6dcbf7fba65b29bb386d1855013dc4dacf40c68cbb5bb6065` |
| `devpod` | `a9c0896574e12d18c25ba42a750219ec6f1a61388345d2f88b60080c5402c899` |
| `dnvr` | `a8b249ec4396ec6225831fa2b1f04070ceb90b858a0626621909ce2f4204ce41` |
| `dnvr-retry-1` | `978f91aefb8b421265254b2db310f7f2678aefb8f5b1509e8d489f9e0b5c0f4b` |
| `flox` | `25afb5f76ee34b83039a384b58df9fe9c1fddea81ff65fcb02f5f05f4cfbd5da` |
| `git-grove` | `d14b6cbaba928154bb371556e01ccd5113875151af8a898739fb11bb1def4ca2` |
| `guix` | `3c1e8330ea862e490acc8140a14e02e1e2af9db9e2fd6481564edcaff437f2f0` |
| `isola` | `3c798ba47644ab6b8fee186f5139188ad4ff206d7aced533c6cb91dd24128674` |
| `lando` | `de1b8245a505c3a9ee39635cc61ab323c50a73ad08006bb627eababd73fc5e4f` |
| `mise` | `3d2761fe61df73840ffee2dbb3d09b25ea93f98952be6ee896e0c0a26308bc8a` |
| `nix` | `0925da6cc7eefb7095d609176a56700f4f23b64c4361265dd3d660dd289710f5` |
| `organist` | `40fb60078bdfcc3e207ca7c3fc16e461a994fc19fdcfb5678c2488184fc5156d` |
| `pixi` | `a98972f2a8d4e97f7e59045e4f3a5f14af58d4d203708400f99cb5311170a07f` |
| `pkgx` | `58741aeb6bb474be31f6ace6a52a1e7c76f3dd12eac1ff5dc046d7139bc30b33` |
| `process-compose` | `b719f904c9384df74b207645fddba81d940b99c77440fa27edd7f4a8d10b0ecb` |
| `services-flake` | `bafaa67a6f03b9e4627ed7c906cdee39ff29f8fc2ab568a7f388a3187bd84617` |
| `stack` | `da7e63126e0e0f5277d13cfc715a631adc950c6a94767ea017ea96bbf14b829d` |
| `tilt` | `e94b302f2da680d17bc671f86038d559c4907b16840f0129254870ff80f64790` |
| `vagrant` | `46fd10fc341fa706fc02279e43ab78956fa3851476c9fc052b45c16be333a6e0` |
| `worktrunk` | `6e1865030561b7a18d8883da0a87a86b59bb8a0bce8dcdc415859e8f1d8a3d8b` |
| `workz` | `ecaffec34a7124f7b5fdcbbda714aeb7249aa86a6c5fcd2075fe5b231151d1d3` |
