# Frozen benchmark scope

Frozen on 2026-10-06 at the user's instruction to close scope. No new competitors will be added.

The benchmark covers Stack and the 25 already-assigned competitors below. Each competitor has a dedicated Sol 6.1 High source investigation. Opus 5.5 implements the workload and adapters. Astra reviews the benchmark and results; confirmed findings are fixed and affected checks rerun before PR delivery.

| Entry | Implementation group |
|---|---|
| Stack | core |
| mise / Pitchfork | core |
| Flox | core |
| Devbox | core |
| devenv | core |
| Nix | core |
| Pixi | core |
| Docker Compose | core |
| Dev Containers | containers |
| DevPod | containers |
| DDEV | containers |
| Lando | containers |
| Process Compose | native-extra |
| services-flake | native-extra |
| pkgx / dev | native-extra |
| dnvr | native-extra |
| GNU Guix | native-extra |
| workz (rohansx) | worktrees |
| Worktrunk | worktrees |
| GitGrove | worktrees |
| isola | agent-env |
| Berth | agent-env |
| BranchBox | agent-env |
| Tilt | additional |
| Organist | additional |
| Vagrant | additional |

Run the shared application tasks using each tool's documented supported workflow. Record native versus project-scripted behavior. Database-per-worktree isolation and separate-server isolation are different valid boundaries; verify the promised data boundary and report it explicitly. Worktree tools must run each checkout's actual source. Keep setup, application latency, dependency versions, platform, and transport boundaries distinct.

Unavailable installations or incompatible runtime prerequisites remain explicit blocked/untested results with actual evidence, never a tool failure, success, or inferred timing. Do not modify the host's global service configuration or use unrelated services to force coverage. Account-only/cloud and other candidates from discovery are outside this frozen cohort and receive no benchmark score.

The discovery ledger records search provenance. Historical eval results are context only. New results must retain raw command receipts, checks, versions, source/config hashes, and cleanup outcomes.
