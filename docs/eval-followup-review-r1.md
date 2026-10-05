# Evaluation follow-up, Astra review round 1

Historical verdict: **request changes**. The subsequent fixes and current limitations are
recorded in [the resolution notes](eval-followup-review-resolution.md).

Reviewed the working tree on `eval-followup` against `a1358335984610926384c58d9a83bd383301d647`, including new implementation files, the acceptance plan, implementation record, historical summary, E2E logs and pilot evidence. At the end of review HEAD was `48cccefcac373c85735c740c311fce162201c078`, an evaluation-preservation commit. Its historical fixtures/results and the existing nine-tool MCP assertion are not implementation regressions. I made no product edits, commits, pushes, merges or releases.

## Blocking implementation findings

### R1. P1: GC checks one generation but stops whichever generation later occupies its daemon ID

Location: `src/session.rs:1470-1496`, `src/provider/mise.rs:351-357`.

After checking the recorded PID and port, GC issues a separate `pitchfork stop <id>`. Nothing in that request carries the checked PID or generation. A restart or re-registration between those operations can replace the daemon under the same ID. The project advisory lock does not serialize external Pitchfork/mise operations. GC then stops the replacement, contrary to its stated refusal to stop restarted or replacement daemons.

A deterministic fake-supervisor reproduction returned the recorded live PID to `status`, then switched to a second review-owned process during `stop`. GC killed the replacement and returned `stopped: true` once the old PID exited. This demonstrates the protocol gap, not a claim that a real supervisor race was forced. Pitchfork 2.29.0's `stop_locked` looks up the current daemon by ID after acquiring its own stop lock. Its process-start-time check protects against stale OS PID reuse, but does not bind the operation to Stack's earlier observation. See [provider implementation](https://github.com/jdx/pitchfork/blob/v2.29.0/src/supervisor/lifecycle.rs).

Fix: make ownership validation and stopping one conditional provider operation, or give each launch an immutable generation-specific provider identity that cannot be reused by another checkout/generation. Another status query alone does not close the race. If safe stopping is unavailable, retain the record and fail closed. Also require `port == Some(record.port)`; the present `is_some_and` accepts a missing port as confirmed ownership.

Acceptance: synchronize a replacement after status and before stop; the replacement must survive and GC must retain the unresolved record. Test missing port and reused PID separately, retaining Pitchfork's start-time protection.

### R2. P1: inherited mise configuration can change both resolution and the meaning of an exact pin

Location: `src/provider/mise.rs:53-61`, `src/provider/mise.rs:227-233`, `src/project.rs:339`.

Moving the resolver's cwd to the cache does not isolate its configuration. `MISE_CONFIG_FILE`, global aliases, parent configuration and other provider settings still apply. More importantly, installation and execution read the project's mise configuration again, so even a correctly generated exact release can be interpreted as an alias for another release.

Reproduced with the pinned, checksum-verified mise 2026.9.18 binary:

- An inherited `MISE_CONFIG_FILE` containing `[alias.python.versions]` with `"3.13" = "3.12.9"` made `stack compile` record requested `3.13`, resolved `3.12.9`.
- With a lock pin of `3.13.16`, a project `mise.toml` alias `"3.13.16" = "3.12.9"` left `stack compile --locked` successful. `mise ls --json` described the generated Stack config as requesting `3.13.16` but selected version and install directory `3.12.9`.

The second case needs no modified lock and breaks the fresh-machine exact-version guarantee. Backend aliases can also change what tool a name denotes. This is consistent with mise's [configuration rules](https://mise.jdx.dev/configuration.html) and its [version resolver](https://github.com/jdx/mise/blob/v2026.9.18/src/cli/latest.rs).

Fix: define and enforce one provider configuration boundary through resolution, installation, daemons and exec. Either isolate relevant configuration or explicitly reject effective provider drift before using it. Record backend identity where needed. Preserve intentional app environment composition without allowing unrecorded aliases to reinterpret pins.

Acceptance: real-mise tests for inherited config overrides, global aliases, a cache beneath a configured parent, project exact-version aliases and backend aliases. A locked invocation must install/use the locked tool and release or fail before installation/start. Do not resolve again in status/exec.

### R3. P1: native validation can use and stop the caller's services

Location: `tests/e2e/native.sh:24-35`, `eval/harness/pilot.py:67-70`.

The new native runner changes HOME/XDG but inherits `PITCHFORK_STATE_DIR`, `STACK_STATE_DIR`, `STACK_CACHE_DIR` and mise directory/config overrides. Explicit overrides outrank the isolated HOME. Its machine-wide `gc` calls can therefore reclaim sessions from the caller's registry, and cleanup calls `pitchfork supervisor stop` against an inherited supervisor. The pilot clears `PITCHFORK_STATE_DIR` but still inherits Stack and mise overrides, so its fresh-cache and isolation claims have the same gap.

Fix: explicitly place all Stack/provider state, config, cache and supervisor directories under the run directory, and sanitize conflicting configuration-selection variables. Check the effective supervisor destination before broad cleanup. Preserve only intentionally supported inherited settings, such as network credentials.

Acceptance: run each harness with every relevant override pointing at separate sentinel state and a live sentinel service. The sentinel must remain running, its files unchanged, and all run state/downloads must stay inside the declared isolation directory. This is a new host-runner safety issue, not the pre-existing Docker runner's fixed container names.

### R4. P2: locked mode accepts floating, empty and nonrelease values as exact pins

Location: `src/project.rs:347-360`, `src/lock.rs:88-108`, `src/provider/mise.rs:89-94`.

Lock reuse validates the requested string and tool identity but never validates `resolved`. For a normal Python `3.13` request, I independently set `resolved` to `latest`, `3.13`, `system`, `path:/tmp/foreign` and the empty string. Every `compile --locked` exited zero and rendered that value. This allows a damaged or hand-edited lock to silently restore floating or host-dependent behavior. Resolver output validation also accepts almost any single token, including known floating selectors other than `latest`.

Fix: validate pin semantics when reading/reusing locks and accepting resolver output, with backend-aware handling of legitimate non-SemVer releases. Reject known selectors and empty/nonrelease values for release requests. Do not require network access during locked validation.

Acceptance: all the reproduced values must be rejected for a release request, without modifying the lock/provider output or contacting the resolver. Keep legitimate non-SemVer exact versions covered.

### R5. P2: partial startup failures lose the ownership information needed after deletion

Location: `src/session.rs:598-611`, `src/session.rs:617-633`.

The launch record initially has no PID or daemon ID. A failed `mise daemons start` returns immediately. A later env/status error also returns before saving observed ownership; only readiness-deadline failure saves it. Thus a service successfully started alongside a failing service cannot be reclaimed after deletion. This is directly relevant to docker-final, whose log shows Postgres started while Redis failed.

Independent fake-provider reproduction: start launched a listening review-owned process and returned failure. The indexed service contained only port/liveness fields. After deleting the app, GC returned `gc_incomplete` because no supervisor ID was recorded. Retrying cannot recover information that was never persisted.

Fix: persist qualified provider identities before launch where possible and reconcile/save observed startup state on every post-start failure path. Do not swallow ownership-save failures. Keep uncertain cases conservative, but avoid unnecessarily turning an ordinary partial failure into permanent manual cleanup.

Acceptance: two-service partial start, provider env failure after start, status failure after an earlier successful observation, readiness timeout, and interrupted startup followed by checkout deletion. GC must reclaim confirmed owned survivors and retain genuinely uncertain records. Add the specifically missing regression for saving PIDs/IDs after failed verification.

### R6. P2: socket preflight and cleanup metadata do not use the complete effective provider environment

Location: `src/session.rs:705-707`, `src/session.rs:558-560`, `src/provider/mise.rs:465-490`, `src/doctor.rs:46-47`.

Preflight extracts only `PITCHFORK_STATE_DIR` from `mise env`; HOME and XDG_STATE_HOME come from Stack's parent process. Doctor reads only the composed explicit Pitchfork override. Real mise confirmed that `[env] HOME` changes the provider environment. With HOME set there to `/tmp/` plus 105 characters, doctor still reported the caller's 75-byte socket as healthy. `up` uses the same wrong inputs, and persists the predicted directory for later GC.

Two related cases also remain: a relative Pitchfork state directory is saved as relative and later interpreted from GC's unrelated cwd; root bypasses prediction even with an explicit absolute `PITCHFORK_STATE_DIR`, although Pitchfork checks that override before its root/SUDO_USER fallback. The latter also prevents any provider record from being saved for root sessions.

Fix: derive the path from all effective provider inputs, resolve relative paths against the actual provider cwd, and record an absolute destination. Honor explicit overrides before applying root fallback rules. Doctor must distinguish an unchecked effective setting from a verified path.

Acceptance: configured HOME, Linux XDG_STATE_HOME, HOME plus `~/pf`, a relative state directory followed by GC from another cwd, and root with an explicit state directory. Overlong effective paths must fail before install; short paths must record the actual supervisor destination.

### R7. P2: GC drops ownership on nonterminal or unknown supervisor states

Location: `src/session.rs:1476-1482`, `src/session.rs:1444-1446`.

Every status except exactly `running` becomes successful reclamation when the recorded PID is dead. That includes an errored daemon awaiting retry, starting/stopping transitions, and malformed/missing status. Pitchfork has retry machinery; absence of a live process at one instant does not prove the daemon will remain stopped. A controlled `errored` response with no PID made GC return success and delete the index without issuing any stop/cancellation.

Fix: recognize only confirmed terminal states, confirm/cancel future supervised starts through an ownership-safe operation, and preserve records for unknown/transitional/retrying states. Do not turn an unrecognized provider response into successful cleanup.

Acceptance: a retry scheduled after GC's status response must not leave an unindexed service; starting, stopping, unknown and missing status must fail closed until terminal ownership-safe cleanup is confirmed.

## Unmet scope

### R8. P2: broad preset-service locking acceptance is still unmet

Location: `src/provider/mise.rs:24-29`, `src/project.rs:326-330`, `docs/eval-followup-plan.md:16-25`.

The contract covers preset-service versions, but `cockroachdb`, `nats` and `spicedb` are knowingly left unlocked, as are omitted preset versions. A warning is honest disclosure, but does not make the same locked input reproduce service versions after upstream changes. Treat workstream 1 as partial, not complete. The `prefix:` and `sub-` classifications also deserve correction: these are resolvable release selectors, not inherently equivalent to a local path or `system`; mise's pinned `latest` implementation explicitly resolves `sub-N` requests.

Implement verified mappings/default resolution for supported presets and resolvable release selectors. Where the provider contract is genuinely unknown, locked mode should refuse to claim reproducibility. Truly nonrelease requests can retain an explicitly documented nonreproducible mode, with consistent warnings for services as well as tools.

Acceptance: move upstream for each known preset and omitted-default case, then reproduce the same release from a fresh cache. Cover explicit update and locked rejection of unsupported/unpinned service inputs. No artifact-checksum requirement is implied here.

## Port failure assessment

The failure is real: `eval/results/followup-e2e-20261005/docker-final.log:73-74` records Redis port 42037 rejected with PID 0 while Postgres started. The later passing run does not fix it. `src/ports.rs` is unchanged from the base, so the allocation defect is pre-existing rather than introduced here.

The logs contain no contemporaneous socket table proving that an ephemeral client owned 42037. PID 0 means owner identification failed; it does not itself identify TIME_WAIT or an established connection. The explanation should be labelled a supported hypothesis, not a proven diagnosis of that run.

I independently reproduced the proposed mechanism in an isolated Linux container: ephemeral range `32768 60999`; a client using local port 42037 caused a bind with SO_REUSEADDR to fail with `Address already in use`, while a connection-based listener probe returned `Connection refused`. This supports the mechanism. It does not establish the historical socket's state.

Severity: P2 startup reliability, not demonstrated wrong-instance access. An additive fix is within the follow-up scope: choose new automatic reservations outside the effective ephemeral range, retain existing reservations, and let explicit reassignment migrate an idle project. `ports::assign` already reuses existing reservations independently of `find_free`, so changing new-allocation policy does **not** require migrating every reservation. Preserve fixed ports and active generations. Add tests for retaining old pins, new allocation outside the range, and an established client occupying an old reservation. Never silently move an active service. Update the implementation record's assertion that a fix requires migrating all reservations.

## Evidence and other acceptance decisions

- Temporary review reproductions are retained in `/tmp/stack-astra-r1/`: `resolve.py`, `repro2.py`, `partial.py`, `socket.py`, and `port.c`. They use isolated state and only signal review-owned processes. The alias-install and nonterminal-status cases are also described above with their exact inputs.
- Independently passed 24 compile tests, 6 provider/socket unit tests, and 5 targeted runtime tests covering identity probes, deleted-project cleanup, active execution after deletion, stale/reused PIDs and replaced directories. `git diff --check` and actionlint passed. I did not repeat full E2E, clippy or package suites without cause.
- ShellCheck 0.11.0 reports informational SC2329 for the EXIT-trap cleanup function in `native.sh`. This is not a demonstrated runtime bug, but the local result is not literally diagnostic-free. Suppress the false positive locally or state the checked version precisely.
- Raw final native and docker-final2 logs support eight completed scenarios each. docker-final's failure remains preserved. Native Linux x64 and the new remote CI jobs have not been independently executed here. The CI matrix invokes the native harness, includes Linux OCI, explicitly skips macOS OCI without a registry, and gates packaging on services. Workflow definition is not a passing CI run.
- The pilot's run-2 events match 12 task results, 12 task successes, two expected service-kill refusals, 4/4 reported controlled failures handled and 30 successful exec samples per phase. The reported fresh/cached latency values match summary.json. These are scripted observations, not agent productivity. The real-agent protocol is a valid remaining external gate under the written plan, not a fabricated completed study.
- Pilot measurement limits should be clearer. Orphans count only PIDs captured in successful `up`/recovery responses; failed `up` returns with a default zero orphan count. Identity-command failures and recovery failures do not themselves make the final run fail. Strengthen the runner to record unknown measurements, include launch/supervisor census after failures, and fail on failed identity/recovery/cleanup checks. The inspected successful run has no unexpected command failures, so this does not invalidate its task count; it limits the general zero-orphan guarantee.
- Earlier `resolve_failed` instead of `install_failed` is an intentional, documented and tested boundary change. Accept it. Bundle-preserving v1 migration, explicit updates, one shared cross-platform release with install failure on unsupported platforms, and absence of artifact checksums are clearly described and acceptable design choices, subject to R2/R4/R8.
- Identity probes correctly strip inherited and provider token variables and use bounded capture. The targeted malformed output, flooding, timeout/descendant cleanup and generation tests pass. Tokens remain trusted-code instance markers, not secrets. README's wording that a probe can "only" learn a token from the service is too strong because trusted probes can read generated files; qualify it as the intended protocol, consistent with the existing trust disclaimer.

The next correction round should address R1-R8, add the targeted acceptance tests, and update the implementation record with remaining limits. Keep the original evaluation data intact. Remote CI and real-agent execution remain separate validation gates.
