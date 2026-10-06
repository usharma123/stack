# Stack benchmark runtime resume

Read-only recovery, 2026-10-06 15:38 UTC. Checkout `/Users/utsavsharma/.t3/projects/stack`, branch `codex/realworld-competitor-bench`. Only this `/tmp` handoff was written. No benchmarks, tests, services, installs, builds, cleanup, code edits, result edits or delegation performed.

## Current activity and ownership

- Observed: process snapshots at 15:37 and 15:38 UTC contain no benchmark runner, report runner, Dev Containers command, benchmark build or run-token process. All seven PIDs stored by the partial Dev Containers attempt, `96536 96938 97287 97308 97335 97701 97706`, were absent from `ps -p`. These are stale registrations, not authority to signal future occupants of those PIDs.
- Observed: Docker context `desktop-linux`; `docker ps -a`, network and volume inventories exactly match the 15:25:52 pre-run snapshot in `bench/reviews/sol-container-runtime.md`. Queries for `rwb.run`-labelled containers, networks and volumes are empty. Name/label inventory also shows no run-owned container, network or volume. No active benchmark run is visible. Process snapshots cannot prove future host idleness.
- Observed: one leftover owned image, `rwb-20261006t152552-67fb2a-a-app:latest`, ID/digest `sha256:893157558933308cdbb22d077465124ed455040f19d054077425f67281d3b010`. Labels `com.docker.compose.project=rwb-20261006t152552-67fb2a-a`, service `app`; step 5 build output names this image. It has no `rwb.run` label, so that filter alone would miss it. Base layers/build cache are shared and are not established as exclusively owned.
- Observed: the only `rwb-*` directory in the current host temp root is `/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t152552-67fb2a-4qahtfnx`. It holds `pids/` and `w/`, including `w/a` and private `w/_state`. The reusable CLI cache is `/Users/utsavsharma/.cache/rwb-bench-tools/devcontainers-0.89.0`, identified by recorded argv. Treat it as benchmark cache across runs, not a per-run teardown target.
- Observed: existing `ev-mise` container `1548935c19cae5cc976a56613ad9c59fbc5f0799278ed531880ad68acfcb87ae` remains running from before this attempt. `docker top` shows only `sleep infinity`. CIT and AITesting containers also predate the attempt. They are unrelated to this run and must remain untouched.
- Inferred: the Dev Containers executor was interrupted after lock hashing and before a start receipt. The interruption cause is unknown. No active process was recovered. Resource absence does not supply the missing teardown receipts.

## Partial Dev Containers attempt

Exact directory `bench/results/smoke-devcontainers-1`, run `20261006t152552-67fb2a`, started `2026-10-06T15:25:52Z`, host transport, 2 repeats / 1 warmup, `keep=false`. Initial `meta.json` still has `completed=false`, `valid=false`, no finish and no final `reportable` field. There is no `outcomes.json` or `summary.md`.

`steps.jsonl` contains six successful, non-timeout receipts: preflight, install CLI, tool version, A prepare, A image build, A lock hash. Last receipt is sequence 6 at 15:26:03 UTC. A seventh PID directory exists without a recorded step or raw log. Do not infer that services started or that workload checks passed. Version receipt establishes Dev Containers 0.89.0, Node 24.16.0, Docker client 28.1.1 / server 29.1.3 linux/arm64, host Compose 2.40.3-desktop.1.

Keep this directory as incomplete diagnostic evidence. Do not append a new run to it or fabricate its missing outcomes. Before a fresh attempt, parent must separately resolve the exact leftover image/workdir using ownership checks. This handoff authorizes no cleanup command.

## Source and review state

The checkout changed while inspection ran. Initial HEAD was `97959a4`, with transport and its tests dirty. Latest observed HEAD is `19d4aacc9b62681c47fe8b76d2b56fafa212f96f`.

- Native runtime fixes are committed at `c0ed55a`; no post-fix native result directories exist yet.
- Reporter is committed at `4e73801`, approval recorded by `97959a4`; `round-3-report.md` approves source within offline scope, not final results.
- Transport is now committed at `8de73f5`; its diagnosis is committed at `19d4aac`. The previously missing `test_unreadable_pid_file_is_rejected` is present. Current transport hash matches Astra approval, `15e690cc72e65a566364f1d9f1b4c26ed6f99806e11cfa1c270dc24b6fb5b528`; core test hash also matches. Current transport-test hash is `fe48d21cecbea51b904c95dd7b5cde0e1f3dd6daa3ee12ff2825432493fae1e9`, newer than the review's 22-test snapshot. This executor did not run the new test or verify parent test logs.
- `round-1-native-transport.md` approves the offline implementation but explicitly leaves actual mixed-UID Linux timeout/group cleanup, native log realization and final resource absence to parent runtime validation.
- Latest dirty state contains only untracked `bench/research/HANDOFF.md` and `bench/reviews/sol-container-runtime.md`. No changes made here. No `AGENTS.md` was found under the project scan.
- `research/HANDOFF.md` has an old "Do not freeze roster yet" instruction. Current `SCOPE.md` supersedes it. Preserve the frozen 26 entries and Sol 6.1 High runtime/understanding, Opus implementation, Astra review, parent atomic-commit/PR roles.

## All 26 roster states

Paths below are relative to `bench/results/`. Every listed directory actually exists. There are 25 directories total, 13 lanes with completed diagnostic attempts, one partial lane, and 12 lanes without any result directory. All 26 still lack final measurements.

| Entry / CLI key | Exact existing result directories | Latest observed state and next task |
|---|---|---|
| Stack / `stack` | `smoke-stack-4`, `smoke-stack-5`, `smoke-stack-6` | Latest complete/valid; occupied-port fails with no conflict diagnostic. Earlier 4/5 passes were false. Preserve product finding; fresh reviewed run needed. |
| mise / Pitchfork / `mise` | `smoke-mise-1`, `smoke-mise-2` | Latest complete/valid, all applicable checks pass; fresh measurement needed. |
| Flox / `flox` | `smoke-flox-1`, `smoke-flox-2`, `smoke-flox-3` | Latest complete/valid, passes predate stricter conflict-evidence gate; fresh diagnostic needed. |
| Devbox / `devbox` | `smoke-devbox-1`, `smoke-devbox-2`, `smoke-devbox-3` | Latest complete/valid, all applicable pass; prior isolation/cleanup recipe failures retained. |
| devenv / `devenv` | `smoke-devenv-1`, `smoke-devenv-2`, `smoke-devenv-3` | Latest complete/valid, all applicable pass. |
| Nix / `nix` | `smoke-nix-1` | Complete/valid, passes; predates later core changes. |
| Pixi / `pixi` | `smoke-pixi-1`, `smoke-pixi-2` | Latest complete/valid, passes; predates later core changes. |
| Docker Compose / `compose` | `smoke-compose-1`, `smoke-compose-2` | Latest complete/valid, 26 pass; occupied-port not applicable. |
| Dev Containers / `devcontainers` | `smoke-devcontainers-1` | Incomplete, stopped after six receipts. Fresh diagnostic after ownership resolution. |
| DevPod / `devpod` | none | Untested; first serial diagnostic. |
| DDEV / `ddev` | none | Untested; first serial diagnostic. |
| Lando / `lando` | none | Untested; first serial diagnostic. |
| Process Compose / `process-compose` | `smoke-process-compose-1` | Complete/valid; occupied-port native readiness timeout exit 124. Rerun with committed diagnostics/log retention. |
| services-flake / `services-flake` | `smoke-services-flake-1` | Complete/valid; frozen-version mismatch and occupied-port timeout. Rerun committed wrapper/version/log fixes. |
| pkgx / dev / `pkgx` | `smoke-pkgx-1` | Complete/valid, all applicable pass; lock/frozen unsupported, services scripted. |
| dnvr / `dnvr` | `smoke-dnvr-1` | Complete/valid; version mismatch and missing occupied-port conflict evidence. Rerun committed Redis PATH/log fixes. Manual logs remain partial diagnostics. |
| GNU Guix / `guix` | `smoke-guix-1` | Complete/valid environment-blocked; canary clone EPERM under default sandbox. No workload realized. Keep blocked; no sandbox relaxation. |
| workz / `workz` | none | Untested; first serial diagnostic. |
| Worktrunk / `worktrunk` | none | Untested; first serial diagnostic. |
| GitGrove / `git-grove` | none | Untested; first serial diagnostic. |
| isola / `isola` | none | Untested; first serial diagnostic, database isolation boundary. |
| Berth / `berth` | none | Untested; first serial diagnostic. |
| BranchBox / `branchbox` | none | Untested; first serial diagnostic. |
| Tilt / `tilt` | none | Untested; first serial diagnostic. |
| Organist / `organist` | none | Untested; first serial diagnostic. |
| Vagrant / `vagrant` | none | Untested; first serial diagnostic, Docker provider only. |

Completed/valid means trustworthy receipts, not every check passed. Old attempts remain diagnostics. Their raw command durations must never be promoted into final performance numbers.

## Prerequisites and unresolved runtime checks

- Observed available on host: Python, bash, Docker, Node, npm, Git, cargo, rustup, curl, hdiutil, pkgutil, shellcheck. Docker inventory proves daemon access. It does not prove every lane's private PATH, credential handling, HTTPS downloads or build succeeds.
- Existing images include `ev-base`, `ev-nix`, `ev-flox`, `ev-devbox`, `ev-devenv`, `ev-mise`. Do not rebuild or update them implicitly; record actual image IDs and CLI versions.
- Dev Containers/DevPod/DDEV/Lando need reachable host Docker/Compose and temp-path sharing; Dev Containers needs Node/npm. Check DevPod agent download/project naming, DDEV custom DB-image merge and auto-resume handling, Lando pinned plugins/orchestrator and failed-image diagnostics. Their first lifecycle is unverified.
- workz/Worktrunk need pinned macOS ARM64 provider downloads, host Git/Docker and private Docker credentials; GitGrove additionally needs Node/npm. Verify actual source-token/NAT receipts, port relocation and native worktree cleanup. Shared credentials may still hit the documented Docker Desktop noninteractive failure; do not assume the Compose lane workaround applies to other lanes.
- isola needs its verified Linux ARM64 archive and mise conda PostgreSQL/Redis tools in `ev-base`; verify actual per-worktree database/Redis-index isolation and destruction. Berth's default provision builds pinned `3b93287` with private cargo directories, or accepts a separately hash-checked `berth_binary`/`berth_sha256`; complete any build before timed session work. BranchBox uses verified macOS release 0.13.4; first native runtime/worktree/source receipts remain unverified.
- Tilt needs pinned macOS binaries plus Docker/Compose. Organist needs `ev-nix`, reachable Nix daemon/substitutes and its process-scoped `lazy-trees = false`; verify its Honcho holder/log/stop workflow. Vagrant privately unpacks verified 2.4.9 DMG using hdiutil/pkgutil, and uses `force_host_vm=false`; verify relocated launcher and Docker provider without installing globally.
- Native retries must verify real services-flake `services version`, dnvr Redis/tmux versions, log artifact copies and preserved native exit semantics. Exit 124 remains an infrastructure deadline failure even when new logs contain conflict text.
- Guix archived verified mirror URL is `https://mirrors.kernel.org/gnu/guix/guix-binary-1.5.0.aarch64-linux.tar.xz`, digest `a5d58b1d0294cad6adb1f2aff627d37feb5db763fdffbceb8551f2b12123cf39`. Use those URL/digest options for any fresh blocked evidence run. Default sandbox canary already failed. Do not use `--disable-chroot`; a workload measurement needs a separately suitable environment, not a hidden workaround. Workload pins PG16/Redis7 remain deviations.
- Stack Linux binary exists at `/tmp/stack-bench-build/target/release/stack`; current hash verified `1d338d2cd92c4ee898037c9ee39dfbccb7c1e3dabae8180fbf65662ac015ce20`. Research handoff records product source `06c351acc6a0744d918dc571c063ff1f6c300700`. Final selector additionally requires a durable build receipt plus product revision/fingerprint. No build receipt was identified; the specific candidate files `/tmp/stack-bench-build-receipt.txt` and `/tmp/stack-bench-build.log` are absent. This is not an exhaustive search for parent receipts.

## Recommended next steps and commands, not executed here

1. Recheck process/resource state. Resolve only the exact partial Dev Containers resources under parent authorization. Preserve its raw directory. Confirm new transport test/full-suite receipts against `8de73f5`; offline approval does not establish live mixed-UID timeout cleanup.
2. Serialize fresh diagnostics: Dev Containers, then DevPod/DDEV/Lando; Process Compose/services-flake/dnvr post-fix retries; Flox under current gates; workz/Worktrunk/GitGrove, isola/Berth/BranchBox, Tilt/Organist/Vagrant. Any confirmed source bug returns to Opus, then Astra, then atomic parent commit and fresh affected run. No concurrent builds/installations/runs.
3. Once recipes are stable, freeze reviewed bytes, platform/transport/resources/cache/deviations and Stack build provenance in a parent-created manifest before new final runs. `bench/measurements/` does not currently exist. All 26 need fresh final-session evidence or explicit blocked/untested entries. Use 20 samples / 3 warmups as the handoff policy, not as an automatic reportability flag.
4. Review results with Astra, retain failed/rerun attempts as excluded with reasons, select exact receipts, and create a separate report. Only the four manifest-eligible first-task/repeat metrics may carry numbers. Preserve a durable hash-verified raw bundle for PR delivery because `bench/results` is ignored.

```sh
# Read-only checks before parent execution:
git status --short
git log -5 --oneline
ps -axo pid,ppid,pgid,lstart,etime,state,command | rg 'bench/(run|report).py|rwb-|devcontainer|docker build|cargo build'
docker ps -a --no-trunc --filter label=rwb.run
docker image inspect rwb-20261006t152552-67fb2a-a-app:latest
docker network ls --filter label=rwb.run
docker volume ls --filter label=rwb.run

# Parent-only future validation/execution after the prerequisites above:
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s bench/tests
python3 bench/run.py --tool devcontainers --out bench/results/smoke-devcontainers-2 --repeats 2 --warmups 1
# Same command shape, one at a time, with a fresh nonexisting output for every lane/retry.
# Stack also requires the verified binary/hash options; Guix requires verified archive options.
python3 bench/run.py --tool guix --out bench/results/smoke-guix-2 --repeats 2 --warmups 1 --option guix_binary_url=https://mirrors.kernel.org/gnu/guix/guix-binary-1.5.0.aarch64-linux.tar.xz --option guix_binary_sha256=a5d58b1d0294cad6adb1f2aff627d37feb5db763fdffbceb8551f2b12123cf39
python3 bench/report.py template --session-id final-20261006-reviewed-1 --out /tmp/stack-final-manifest.json
# Parent fills/reviews manifest BEFORE new session runs; never retrofit old smokes into it.
python3 bench/run.py --tool mise --out bench/results/final-20261006-reviewed-1/mise --repeats 20 --warmups 3
python3 bench/report.py attempt --run bench/results/final-20261006-reviewed-1/mise
# After all attempts and result review, use a fresh report output:
python3 bench/report.py --manifest /tmp/stack-final-manifest.json --out bench/measurements/final-20261006-reviewed-1/report --require-complete
```

Primary evidence: current process/Docker inventories, each `bench/results/*/meta.json` and `outcomes.json`, partial Dev Containers raw receipts, `README.md`, `SCOPE.md`, `sol-measurement-handoff.md`, `sol-native-runtime.md`, `sol-container-runtime.md`, `round-1-native-transport.md`, `round-3-report.md`, adapter handoffs and current adapter source. Older handoff claims were checked against current results/commits where possible.

Final check at approximately 15:40 UTC: still 25 result directories, all 26 roster rows present in this handoff, no benchmark process visible, and repository status still contains only the two preexisting untracked handoff files.
