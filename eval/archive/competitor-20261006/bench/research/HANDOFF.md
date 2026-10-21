# Implementation handoff

Research is in progress. Sol 6.1 High owns each competitor note; Opus 5.5 owns benchmark implementation. Parent owns commits and execution coordination. Branch starts at Stack commit `06c351acc6a0744d918dc571c063ff1f6c300700`.

Early source-backed findings from researchers, to verify against completed notes:

- Flox 1.17.0: keep one activation alive across repeated commands; close it for cleanup. Hook failure alone is not a readiness receipt.
- Devbox 0.18.4: native plugin probes exist, but initial service up is asynchronous. Assign per-checkout PGPORT/REDIS_PORT. Use the supported start operation for an existing manager rather than repeatedly creating a manager.
- devenv 2.4.0: use `config.processes.<service>.ports.main.value` for application URLs. Existing ev-devenv image has 2.3.1 and must be upgraded or clearly reported as older.
- mise 2026.10.3 / Pitchfork 2.29.0: independent clones need explicit differing base ports, while linked worktrees receive automatic offsets. Test both only if their configuration differences are reported.
- Pixi 0.81.0: prefer native Pixi PyPI lock dependencies; service lifecycle is user scripting.
- Nix 2.35.2: local image contains Determinate Nix 3.23.0 distribution. Plain shells need explicit service scripts. Previous runner skipped all service tasks for Nix.
- Compose 5.6.0: local plugin is older, but `eval/results/latest-2026-10-05/bin/mac/docker-compose` may contain current standalone CLI. Digest override requires both `--resolve-image-digests --lock-image-digests`.
- Dev Containers CLI 0.89.0: use explicit independent Compose project names, persist labels and use Compose for teardown; CLI has no down command.

Existing binaries to inspect and verify, never assume current identity:

- `eval/results/latest-2026-10-05/bin/linux/{mise,pixi}`
- `eval/results/latest-2026-10-05/bin/mac/{mise,docker-compose}`
- `eval/results/latest-2026-10-05/package/package/binaries/*/stack` are released 0.1.4, NOT necessarily current main implementation.

Benchmark Stack current checkout; if adding a released baseline, label both explicitly. Retain build revision, binary hashes, host/container architecture, raw task evidence, recipe/lock hashes, and all missing/blocked cells. No broad or global cleanup. Comprehensive competitor discovery is still in progress; more adapter requests will follow. Do not freeze roster yet.

Parent built the unchanged Stack product source on 2026-10-06 from `06c351acc6a0744d918dc571c063ff1f6c300700`:

- Native macOS binary `target/release/stack`, SHA256 `5ada2f164826c390d122b1308984d1d8ff748aabe87a811a23d5c1da138c63e5`.
- Linux ARM64 binary `/tmp/stack-bench-build/target/release/stack`, SHA256 `1d338d2cd92c4ee898037c9ee39dfbccb7c1e3dabae8180fbf65662ac015ce20`.
- Linux build: `docker run --rm --init --name stack-bench-build-20261006 -v "$REPO":/src:ro -v /tmp/stack-bench-build:/out -w /src -e CARGO_TARGET_DIR=/out/target -e CARGO_HOME=/out/cargo rust:1-bookworm cargo build --release --locked`.
- Builder image ID `sha256:93ce27a88655056a51dbdd8f5f2d7ddc071c7b0070fb288a37b5a285fc83971e`.
- Cargo.lock SHA256 `2739df41aa0e397eed1aadf0a2462df049f5bce543a215a3bf5fa52e23c95e30`.
- `cargo test --locked` passed. No benchmark timing ran concurrently with builds.

New dedicated researchers: Process Compose, DDEV, Lando, DevPod. Dev Containers researcher is active too. Treat these as adapter candidates; final coverage ledger follows broad search. Retain compact raw evidence for committed results, not just a prose report pointing at ignored data. We will serialize application/timing runs after implementation and methodology review.

Research corrections and additional candidates:

- Compose research correction: the CLI PreRunE enables digest resolution for `--lock-image-digests`; that flag alone DOES resolve. Earlier handoff's assertion requiring both flags was incomplete source reading. See final compose.md.
- Canonical fixture needs ONE Python constraint shared across adapters. Some draft researcher examples use 3.12; align them to 3.13 or explicitly explain different resolved versions without pretending exact equivalence.
- Container-aware identity: data_directory, Redis dir and internal port may be identical strings in separate containers. Require different container/volume identity, PostgreSQL system_identifier, Redis run_id and distinct markers. Do not falsely require different in-container path strings.
- DevPod stop check must use Docker inspection/status, not `devpod ssh A`, which restarts A automatically. `devpod delete` retains named volumes; explicit owned-volume cleanup is required.
- DDEV is PHP-first; Python runs as a custom service. Its default shared network means use unique `ddev-${DDEV_SITENAME}-db`/redis names and validate IDs rather than generic aliases.
- Broad search found services-flake, rohansx/workz, isola, Berth, BranchBox as close functional competitors. Dedicated services-flake/workz researchers are active. More research follows for others. Scope and current source validity need verification before scoring.
