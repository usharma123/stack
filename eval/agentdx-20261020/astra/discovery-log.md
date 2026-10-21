# Discovery and manual-action log

The first exploratory reads happened before the receipt directory existed. This file records them separately rather than pretending they have captured stdout files.

1. Confirmed cwd, `git rev-parse HEAD`, and clean `git status --short`. Read the required unslop skill. Searched the standing memory registry for workflow/isolation references; did not open old evaluation reports, the Opus report, or the parent benchmark draft. Findings in REPORT.md come from this run.
2. Read `docs/user/worktrees.md` first for isolation. Read `Cargo.toml`, `npm/package.json`, README, `examples/isolated-stack.sh`, `docs/user/agents.md`, `install.md`, `commands.md`, `bundles.md`, `secrets.md`, `identity-probes.md`, and later `guarantees.md`.
3. Checked `command -v mise pitchfork docker cargo node uv`, `uname -a`, `docker info --format '{{.OSType}} {{.Architecture}}'`, and `docker image ls`. Host mise and Docker were available; no standalone pitchfork was on PATH. Reused the already-present rust:1.93 image only for the run-owned container.
4. Checked that `eval/results/test` is ignored. Created the requested report directory and ignored raw receipt directory, a short isolated root for the supervisor socket, and a before snapshot of global provider configuration/state. Read isolation code to copy all its state variables, retaining HOME.
5. CLI help, exact version, setup, npm availability and source-build commands are captured in raw receipts. No alternative tester report was read. No web research was needed for product behavior; the live npm registry was queried directly.
6. Read `src/mcp.rs` lines 1-130 and its discovery/schema section to construct the stdio client. This was an explicit source lookup, not a claim of zero-doc discovery. Used newline JSON-RPC and retained full requests/responses.
7. While investigating the endpoint discrepancy, searched `src/session.rs`, `src/provider/mise.rs`, and `src/provider/task_config.rs`; read provider config selection, captured task env handling, and loopback URL rendering. Checked shell-specific endpoint values through Stack. A search for `src/config.rs` and then `src/model.rs` failed because those files do not exist; corrected the lookup to `src/manifest.rs`.
8. Searched for API_URL in generated fixture configs and local mise/Pitchfork config directories to rule out a matching override. No user configuration was changed. Printed only selected endpoint values and relevant environment-key names, not the full inherited environment or auth values.
9. Looked up plain-provider fnox syntax in `tests/e2e/10-secrets.sh` after reading the user secret guide. The actual fnox runtime was downloaded and tested with fake values. No private provider or real secret was queried.
10. Read `src/manifest.rs` Task/Service fields after the rejected dependency attempt. Added the dependent worker as an application fixture and explicitly restarted it after demonstrating stale loaded state.
11. Read `src/main.rs` captured execution selection and `src/process.rs` capture/spawn and forwarding sections only after observing external-SIGTERM orphans. The source explanation is marked as inference in the report; process receipts establish the failure.

Manual changes and recovery are visible in the reusable drivers. `bootstrap.py`, `ledger_work.py`, and `fix_ledger.py` implement the fixture progression. `catalog_work.py` restores the seed after its intentionally failing baseline mutates storage. `second_checkout.py` creates a detached worktree without commits. `lifecycle.py` preserves the old app before same-path recreation. `tools_bundle.py` preserves/restores the good lock around checksum corruption.

Additional commands executed interactively, with full argv receipts:

- Retry Linux build with non-login bash after `cargo: command not found`.
- Compare task/exec endpoints with a deliberate inherited API_URL; test sh/bash/zsh and downloaded mise.
- Query PostgreSQL encoding, set only the run-owned Linux database's client encoding to UTF8, and rerun the unchanged app integration test.
- Create a second isolated root-owned Linux project, run setup/locked compile/up/status/down, and retain the expected PostgreSQL privilege failure.
- Install and inspect the actual older npm 0.1.21 package in an isolated prefix.
- Preserve recreated app, remove only the run-owned extra worktree, verify source identity/product diff, and scan Stack artifacts for fake-secret leakage.

Cleanup was explicit. No old/global process was killed by name or broad pattern. Native cancellation repros were naturally bounded to 15 or 30 seconds; final scans proved absence. Linux commands were wrapped by container-side `timeout -k 5 120`; native command wrappers used a process-group timeout and eight-second grace. Readiness fault injection used `stack up --timeout 2s` followed by status/down. Deliberate slow-network injection was not run.
