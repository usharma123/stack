# Competitor discovery

Scope is frozen as of 2026-10-06 at the user's instruction. [SCOPE.md](../SCOPE.md) lists Stack and all 25 selected competitors. No additional competitors will be added.

The search covered reproducible tool environments, local service orchestration, container development, agent worktree isolation, and adjacent cloud/Kubernetes/VM workflows. Eleven Exa queries requested 115 result slots. Ten returned 105 hits with 93 distinct URL strings; one query timed out, and later targeted queries covered that category. These are discovery counts, not 93 validated competitors. The exact query/result ledger is [search-ledger.json](search-ledger.json).

The initial evaluation roster supplied Flox, Devbox, devenv, mise/Pitchfork, Nix, Pixi, Compose, Dev Containers, Worktrunk and workz. Follow-up primary-source inspection added Process Compose, services-flake, DDEV, Lando, DevPod, pkgx, dnvr, Guix, isola, Berth, BranchBox, GitGrove, Tilt, Organist and Vagrant before the freeze. Every selected competitor has a separate Sol 6.1 High investigation in this directory. Namesakes and prerelease/current-main differences are identified in the individual notes.

The shared workload concerns a local project's tools, application dependencies, stateful services, and independent checkouts. Package environments that need service scripts are labeled accordingly. Container tools retain their container boundary. Worktree tools are evaluated through their supported environment/provider wiring. Their different isolation guarantees are reported rather than collapsed into one score.

Other search results, including cloud control planes, Kubernetes-focused tools, shell helpers, templates and unassigned emerging projects, are discovery context only. They were not assigned before the user closed scope and receive no benchmark results. Their appearance in the ledger does not mean they were tested or excluded for poor quality.

Research notes distinguish implementation inspection, static configuration checks and actual execution. Candidate snippets are implementation guidance and can differ from the final common fixture. Results must refer to the checked-in adapter/configuration actually executed, including package patch differences and installation blockers. Old `eval/` reports provide historical context only.
