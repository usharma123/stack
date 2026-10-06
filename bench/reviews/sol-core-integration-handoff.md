# Core integration handoff

Read-only audit for the sole Opus 5.5 core/original-eight implementer. No implementation,
tests, services, provisioning, lifecycle commands or benchmark measurements ran.
Only this handoff was written. External-adapter fixes belong to the other Sol handoff.

Snapshot: 2026-10-06 13:53:18 UTC, branch `codex/realworld-competitor-bench`,
HEAD `eb7dade7b6dba6ca97e7d4e7451f4d07d8bfd2e8`, dirty working tree. Core is changing
concurrently; recheck each item before editing. Snapshot SHA256 below hashes the
newline-separated full SHA256/path rows for `run.py`, `rwb/scenario.py`, `rwb/testing.py`,
`rwb/verify.py`, `rwb/adapters/base.py`, `tests/test_core.py`, `rwb/adapters/registry.py`,
in that order, with paths prefixed `bench/`:
`68a2b622b2f9d672c4a2e588c849545ad5e5a787438dd25b0808413add7485c1`.

Inputs read: `ADAPTER-CONTRACT.md`, all five family handoffs, current core and adapter
hooks, `research/HANDOFF.md`, and existing original-eight research notes. Claims below
are source findings, not evidence that a real adapter lifecycle works.

## Already implemented

| Request | Current source and existing test |
|---|---|
| Guix provisioning exit 77 becomes blocked | `scenario.py:139` raises `ProvisionBlocked`, preserving reason and step. `run.py:114` records all main checks blocked, completed, no measurement timings, and leaves validity subject to cleanup. `test_core.py:500` tests the exception, reason and no setup; `:520` tests the blocked summary. Guix's options and changed-daemon title/pins already exist in `adapters/guix.py:40`. |
| isola stop keeps data endpoints | `scenario.py:345` decides stop from stop/probe when `stop_keeps_data_endpoints=True`, and records app reachability as `stop.a.data_endpoints: observed`. `adapters/isola.py:52` declares it. `test_core.py:489` covers accepting reachable endpoints only with the hook. |
| DDEV entry resumes stopped project | `scenario.py:345` skips the after-stop app call entirely when `entry_auto_resumes=True`; stop/probe decide the outcome. `adapters/ddev.py:40` declares it. This implements the parent's requirement to avoid restarting during verification. |
| Frozen C cleanup tracking | `scenario.py:419-425` tracks C before the frozen command if `frozen_setup_starts_services` is true, or the returned frozen body contains the returned start body. Current container family `frozen_setup` embeds `self.start(co)` in `adapters/devcontainers.py:187`. `test_core.py:481` covers embedded start and no-start recipes. |
| Fake source path and container identities | `testing.py:43` honors `adapter.app_source_path(co)`; `:160` supplies checkout-specific instance JSON. `test_core.py:612` now rejects fail/blocked outcomes and requires downstream checkpoints, instead of only checking no error. |
| NAT port-map gate | `scenario.py:71` runs the adapter mapping immediately after identity; `verify.py:75` validates evidence and numeric published/target ports, and `:94` checks both against URL/server ports. `test_core.py:339` covers absent, malformed and mismatched mappings; `:351` covers failed map command. Do not weaken this for host-port adapters. |
| Setup scopes and first verified task | `run.py:99,181` preserve `setup_scope` and cache notes; `scenario.py:252` gates `first_task` on setup/start/deps/migrate/CRUD/cache/tests. `test_core.py:449` covers passing gates, failed pytest and receipt selection. `run.py:199` explicitly says not to rank phase timings across tools. |
| Artifact and command-output hooks | `base.py:244,249` already supplies `artifacts` and `diagnostics`; `scenario.py:592` runs diagnostics before teardown. Family requests for these hooks are stale, though their use/order still needs attention below. |

## Remaining core work

- [ ] **Fake NAT receipt.** `FakeTransport.exec` has no successful `*-port-map` response.
  `WorktreeHostAdapter.port_map`, inherited by workz/Worktrunk, therefore returns empty
  output under the shared contract fake, and `start.a` fails validation. Their family
  tests use a separate `DockerFakeTransport` in `test_worktree_adapters.py:54`, so they
  do not close the common-fake gap. Add the default mapping from the current fake
  identity's ports, with checkout-specific evidence. Preserve `world.outputs`, codes
  and mutators so malformed/missing receipts can still be injected. Test workz and
  Worktrunk through the shared `run_fake` to isolation, stop, frozen copy and cleanup;
  retain a deliberately different URL/server-port test with a valid Docker mapping,
  plus wrong published/target, missing evidence and command failure negatives.

- [ ] **Artifact copy order.** `run.py:131-132` calls `scenario.cleanup()` before
  `collect_artifacts`. This contradicts `Adapter.artifacts`' before-teardown contract.
  Worktree cleanup can remove the checkout, and Vagrant explicitly declares
  `.vagrant/machines` at `adapters/vagrant.py:153`, which native destroy alters.
  Collect file artifacts before checkout cleanup. Keep diagnostics before teardown,
  and ensure a copy/diagnostic exception cannot skip teardown. Test a fake artifact
  that exists before cleanup and disappears during cleanup; assert it is captured.
  Also inject copy failure and assert checkout, host and transport teardown still run.

- [ ] **DDEV behavioral regression.** Add a core fake test that raises if
  `a-after-stop` is invoked under `entry_auto_resumes=True`. Assert stop passes on
  successful stop/probe, fails for either nonzero result, and is blocked on probe
  timeout. Existing family tests only check the attribute/source recipe, and the
  shared fake would not expose a real DDEV auto-start side effect.

- [ ] **Timing boundary disclosure.** `task_t0` and `task_steps` begin inside
  `bring_up` at `scenario.py:152`, after `prepare`. Native worktree creation is in
  `prepare` for workz, Worktrunk, BranchBox and isola, while Berth creates its worktree
  inside `setup`. Current first-task values therefore include different creation
  work. At minimum, record/display that the task starts from an already prepared
  checkout and disclose which tool operations were excluded. If the agreed metric
  is creation-to-tests, count declared user-facing preparation consistently without
  charging fixture-copy/token glue as tool latency. Test both a prepare-creation
  lane and a setup-creation lane with explicit receipt membership; do not infer
  equivalence from a passing first-task gate. dnvr's PTY/readiness is already inside
  its start body, so label it start-to-ready, not bare process-start latency.

## Coordination and final checks

- isola still inherits `99\.99\.99|postgresql_99` at this snapshot, although its D
  recipe points at `127.0.0.1:1`. This is an **external-adapter owner fix**, not work
  to duplicate here. Its regression must inject the actual refused-endpoint
  diagnostic. The default fake D error always prints `postgres@99.99.99` and the
  breaker body at `testing.py:156`, which masks the wrong inherited pattern. An
  unrelated resolution/version error must not prove refusal of isola's endpoint.
- C tracking is present but substring inference is only a compatibility fallback.
  Explicitly declare `frozen_setup_starts_services=True` for any recipe that starts
  through another helper or a modified start body. Test that declaration with a
  body lacking the exact start substring, including failed frozen setup, and verify
  C cleanup runs. Do not claim this missing for the current container family.
- `ContainerAdapter.core_hooks_required()` still lists the already implemented
  stop/C hooks. Treat it as a requirements list, not a current unresolved-defect
  list. Its wording/pins are external-owner coordination.
- `registry.available()` silently skips missing modules. Compose appeared while this
  audit was running and exists at the final snapshot. Once original-eight work is
  complete, add a roster assertion so a green contract run proves all 26 frozen
  adapters, including announced variants, were actually exercised.
- Guix remains externally blocked until a verified release digest and successful
  daemon canary exist. Do not invent a digest, silently disable chroot, or label
  PG16/Redis7 as the canonical PG17/Redis8 cohort. The core exit-77 hook is ready.
- Original-eight runtime prerequisites are already documented, so no new competitor
  research is needed: Flox must retain an activation between commands and not trust
  hook success as readiness (`research/flox.md:28,276`); Devbox needs working Nix and
  per-project manager routing (`research/devbox.md:151,228`); devenv must use allocated
  `processes.*.ports.main.value` (`research/devenv.md:215`); Pixi package/wheel
  availability remains an execution gate (`research/pixi.md:324`); Compose requires
  a working daemon/build/pull path and preserves the credential-helper environment
  blocker (`research/compose.md:421`); mise version listings do not prove installations
  (`research/mise.md:15`). Preserve these as first-run watch points, not discovered
  tool failures. Stack's build revision/hash must match the intended product source.

After fixes, run the offline core tests and full discovery, with no live services.
Check that every adapter reaches the intended checkpoints; a no-error result or
an available-only roster is insufficient. Real runs remain parent-serialized.
