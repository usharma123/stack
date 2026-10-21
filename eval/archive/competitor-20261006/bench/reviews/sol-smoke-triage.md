# Original eight diagnostic smoke triage

Reviewed 2026-10-06, read-only except this file. No services, provisioning, reruns,
measurements, result edits, or source edits were performed. The latest numbered
run for each original tool is the requested run. All eight have `completed=true`,
`valid=true`, `reportable=false`, empty `errors`, and empty `cleanup_problems`.
These are diagnostic runs with two samples and one warmup. They are not final
performance evidence, and `valid` does not mean every tool check passed.

Across 232 recorded outcomes: 214 pass, 16 observed, one fail, one not applicable,
zero blocked, zero harness errors. Seven lanes have no recorded applicable check
failure. Stack has one. Flox's occupied-port pass is stale under the current gate.

| Tool | Latest directory | Exact run ID | UTC interval | Recorded outcomes | Classification |
|---|---|---|---|---|---|
| Stack | `smoke-stack-6` | `20261006t133206-23275f` | 13:32:06 to 13:32:33 | 26 pass, 2 observed, 1 fail | Complete; occupied-port diagnostic failure |
| mise | `smoke-mise-2` | `20261006t135507-c69b8a` | 13:55:07 to 13:55:42 | 27 pass, 2 observed | Complete; applicable checks pass |
| Flox | `smoke-flox-3` | `20261006t140516-6dca0c` | 14:05:16 to 14:08:13 | 27 pass, 2 observed | Complete; recorded pass, occupied-port validation pending |
| Devbox | `smoke-devbox-3` | `20261006t142943-1603d1` | 14:29:43 to 14:33:48 | 27 pass, 2 observed | Complete; applicable checks pass with current core |
| devenv | `smoke-devenv-3` | `20261006t134607-82d8b0` | 13:46:07 to 13:48:20 | 27 pass, 2 observed | Complete; applicable checks pass |
| Nix + project scripts | `smoke-nix-1` | `20261006t133358-9d2710` | 13:33:58 to 13:35:55 | 27 pass, 2 observed | Complete; applicable checks pass |
| Pixi + project scripts | `smoke-pixi-2` | `20261006t134057-ac7a64` | 13:40:57 to 13:41:29 | 27 pass, 2 observed | Complete; applicable checks pass |
| Compose | `smoke-compose-2` | `20261006t135242-8f76ce` | 13:52:42 to 13:54:35 | 26 pass, 2 observed, 1 n/a | Complete; applicable checks pass |

All intervals are on 2026-10-06. The per-run `meta.json`, `outcomes.json`, and
`steps.jsonl` are the authoritative receipts; step references below resolve to
their `logs/NNNN-label.stdout` and `.stderr` files.

## Remaining behavior and validation

### Stack: safe refusal with the wrong diagnostic

`smoke-stack-6` occupied_port has evidence 65 and 66. Step 64 reads the assigned
PostgreSQL port, 43560. Step 65 successfully starts the benchmark listener. Step
66, `e-start`, exits 1 with `stop_failed`, `mise daemons stop failed`, and provider
output `no matching project daemons; define [daemons] in mise.toml`. The receipt
stops at `stop_previous`; neither an address-in-use diagnostic nor a service
launch is recorded. Step 67 releases the listener successfully. This supports
the recorded `start-failed-without-conflict-diagnostic` failure. It does not
support an unsafe connection or a claim that the foreign process was killed.

Current `src/session.rs:629` calls `down_locked` for a first launch without a
session. In `down_locked`, `src/session.rs:870` treats an accepting configured
port as reason to invoke provider stop, even with no recorded live service.
`src/provider/mise.rs:439` propagates the provider's stop failure. This matches
the archived failure path. The adapter hash still matches the run; the conflict
classifier hash also matches. Stack-4/5 marked the same type of unrelated refusal
as passing. Those classifications are superseded by Stack-6 and must not count
as successful conflict detection.

`IMPLEMENTATION.md` records an additional no-squatter control and manual
reproduction. Those control transcripts are not among this run's archived
steps, so this review does not claim to have independently verified them.
Keep the product unchanged and report the diagnostic failure as requested.

### Flox: current gate has not been exercised

Flox-3 steps 60 to 63 successfully start the listener, return start exit 0,
install app dependencies, then return app wait exit 3 after PostgreSQL timeout.
The outcome says `detected-at-readiness`, but that evidence contains no conflict
diagnostic and there is no `e-conflict-logs` step. Current
`bench/rwb/scenario.py:566` requires relevant tool logs or wait output for failed
scripted readiness. Current `FloxAdapter.conflict_logs` supplies service status
and PostgreSQL/Redis logs, but its hash differs from Flox-3. The archived pass
therefore does not verify the current occupied-port checkpoint. This is an
unexecuted validation follow-up, not an unresolved missing-hook patch.

Devbox-3 demonstrates the stricter path. Steps 61 to 65 include a successful
listener, start exit 0, app timeout, then `e-conflict-logs` exit 0 containing
`could not bind IPv4 address "127.0.0.1": Address already in use` and PostgreSQL
port 25436. Its applicable checks pass and its adapter/core hashes match current
files. Nix-1 step 60 and Pixi-2 step 61 directly report that port 25436 is already
in use. These are scripted lifecycle diagnostics, not native Nix/Pixi service
features. mise-2 step 61 names port 25436 and the listener PID in its refusal.

devenv-3 planned port 55434 is occupied, then step 69 identifies its own checkout
on PostgreSQL port 55435 with checkout-specific data and source. Its recorded
`relocated` pass has a successful identity, rather than relying on an unrelated
failure. Compose publishes no host service ports, so its occupied_port outcome
is correctly n/a for this recipe.

## Cleanup receipts

All eight record passing `cleanup.processes`. Their final service scans exit 0
and print no service processes. `cleanup.supervisors` is an observation, not a
passing assertion that no supervisor exists.

| Run | Recorded cleanup evidence | Limit |
|---|---|---|
| Stack-6 | Listener release 67; E/C/B/A cleanup 68 to 71 exit 0; empty service scan 72 | Step 73 observes Pitchfork PID 527. No container-remove step is archived. |
| mise-2 | Listener release 62; E/B/A cleanup 63 to 65; empty scan 66; container-remove 68 exit 0 | Step 67 observes Pitchfork PID 462 before container removal. |
| Flox-3 | Listener release 64; E/B/A cleanup 65 to 67; empty service/supervisor scans 68/69; container-remove 88 exit 0 | Artifact copies 71 to 87 occur after checkout cleanup in this old core. |
| Devbox-3 | Artifacts before teardown 66 to 78; release 79; E/B/A cleanup 80 to 82; empty scans 83/84; container-remove 85 exit 0 | No recorded cleanup problem. |
| devenv-3 | Release 70; E/B/A cleanup 71 to 73; empty scans 74/75; container-remove 86 exit 0 | Declared artifact probes 76 to 85 find no files after teardown. |
| Nix-1 | Release 61; E/C/B/A cleanup 62 to 65; empty scans 66/67; container-remove 77 exit 0 | Artifacts collected after checkout cleanup. |
| Pixi-2 | Release 62; E/C/B/A cleanup 63 to 66; empty scans 67/68; container-remove 78 exit 0 | Artifacts collected after checkout cleanup. |
| Compose-2 | E/B/A cleanup 59 to 61; empty service scan 62; host-cleanup 64 exit 0; empty host-leftovers 65 | Host-leftovers checks owned project containers, volumes, networks, and images. |

The parent separately reports a current live
`docker ps -a --filter label=rwb.owner --format '{{.Names}} {{.Status}}'` with
empty output after the core checkpoint. That supports current container absence
as reported by the parent. It does not create a missing archived removal receipt
for Stack-6. Current `run.teardown` already copies artifacts before cleanup;
the older order and missing devenv artifacts are validation history, not a new
implementation assignment.

## Source reconciliation

Every run records a dirty working tree. Stack-6 records harness commit
`af0bab3b12c41bed43c87dfabd0812106d2f6ab4`; Devbox-3 records
`c239656db36b76c31a8da529e9d5fabd1a2c47fc`; the other six record
`eb7dade7b6dba6ca97e7d4e7451f4d07d8bfd2e8`. Commit names alone do not identify
the running source; the recorded file hashes provide the comparison.

All eight config-file sets match current adapter config files. All eight
`verify.py` and `transport.py` hashes match current files. Devbox-3's adapter,
`base.py`, `common.py`, `scenario.py`, and `run.py` also match. The other seven
predate current `base.py`, `scenario.py`, and `run.py`. Stack/Nix/Pixi also predate
current `common.py`. Their old core lacks some current artifact-order, version
filtering, NAT validation, and disclosure changes, so rerun current code before
any measurements are used.

Adapter hashes, recorded prefix to current prefix:

| Adapter | Recorded | Current | Interpretation |
|---|---|---|---|
| Stack | `9871f4fadb03` | `9871f4fadb03` | Same recipe |
| mise | `976138d66e18` | `976138d66e18` | Same recipe |
| Flox | `aef736e8a89b` | `73b0b4faead5` | Current conflict log hook needs validation |
| Devbox | `55ce14d0664e` | `55ce14d0664e` | Same recipe, current core |
| devenv | `03f4fcb85467` | `03f4fcb85467` | Same recipe |
| Nix | `56049ff505c1` | `056861586c51` | Current adapter changed; do not claim current-code validation |
| Pixi | `6042d7605a27` | `6042d7605a27` | Same recipe |
| Compose | `c7a39c6e0759` | `c7a39c6e0759` | Same recipe |

## Superseded failures

No remaining original-eight recipe bug is established by these smokes.

- mise-1 setup step 6 passed multiple files to single-file `mise trust`.
  Current recipe trusts each file separately; mise-2 setup passes.
- Pixi-1 setup step 6 used `PIXI_NO_PROGRESS=1`, rejected as a Boolean value.
  Current recipe uses `true`; Pixi-2 setup passes.
- Flox-1/2 failed restart or readiness. Current held-activation lifecycle uses
  native service stop and activation-based service restart; Flox-3 restart,
  persistence, and cleanup pass. Only current occupied-port validation remains.
- Devbox-1/2 reached shared Redis or failed B readiness and left a Redis process.
  Current init hook restores per-checkout ports after plugin exports and cleanup
  confirms stop. Devbox-3 isolation, B survival, frozen copy, and cleanup pass.
- devenv-1 provisioning failed on the Nix database lock permission before the
  daemon was ready. Current provisioning waits for the daemon; devenv-3 passes.
  devenv-2 also had a version-comparison failure and listener setup block;
  devenv-3 records matching version lines and successful relocation.
- Compose-1 public image pull failed through the host credential helper.
  Current provision creates a run-owned Docker client configuration for the same
  daemon without that helper. Compose-2 succeeds. This was a host integration
  limitation resolved in the recipe, not an unresolved Compose lifecycle failure.

## Bounded next work

No adapter worker should be dispatched merely to make seven passing lanes look
busy. Current hooks and cleanup changes are already implemented. Independent
Astra reviews and the parent's serialized reruns remain the next steps.

1. Parent reruns Flox occupied_port with the current log gate, then the original
   eight on the final source snapshot before collecting reportable measurements.
   No rerun or timing claim is supplied by this review.
2. Retain Stack's missing-conflict-diagnostic failure in the report. If a later
   product fix is explicitly selected, one worker can own only
   `src/session.rs` and `tests/runtime.rs`: distinguish an unowned accepting port
   from recorded service ownership during first-launch reconciliation and return
   an explicit conflict diagnostic without signaling the foreign listener. Add a
   control with no prior session and a foreign bound port, prove the foreign
   listener survives, and retain recorded-service shutdown behavior. Leave the
   adapter and classifier unchanged. This is a concrete future patch brief, not
   an instruction to change the frozen product now.
3. Preserve all old results. A fresh Stack run should archive outer container
   removal, and current pre-teardown collection should be checked for Flox and
   devenv artifacts. Do not retrofit old receipts.

## Version, cache, and timing limits

The parent's accepted defaults 2 to 7 in `IMPLEMENTATION.md` remain disclosed
cohort choices. No new user question is required.

- Stack uses Linux ARM64 binary SHA-256
  `1d338d2cd92c4ee898037c9ee39dfbccb7c1e3dabae8180fbf65662ac015ce20`
  and mise 2026.10.3. This review did not rebuild it or equate its hash with the
  current dirty source. mise uses Pitchfork 2.29.0. Flox is 1.17.0, Devbox 0.18.4,
  devenv 2.4.0, Pixi 0.81.0, and the standalone Compose binary is 5.6.0.
- Stack/mise/Compose use Python 3.13.16; other lanes use 3.13.15. Devbox uses
  PostgreSQL 17.10; the other seven use 17.11, with Alpine builds in Compose.
  Redis is 8.10.2. Flox uses uv 0.12.17, Nix/devenv/Devbox 0.12.22, and
  Stack/mise/Compose 0.12.23. Pixi resolves the PyPI dependency set itself.
- devenv uses nixpkgs `151fa4e8ddfdd8dd25d945ad94ed54a13de9f6e4` with the
  2.4.0 modules, rather than its research recipe's `addf7cf5` revision.
  Nix uses the same nixpkgs with the recorded Determinate 3.23.0 / Nix 2.35.2 image.
- Stack/mise/Pixi report fresh tool caches in their ev-base containers, followed
  by B reusing A's downloads. Nix/Flox/Devbox/devenv stores may be preseeded by
  image builds. Compose's host image/build cache was not cleared. No fully cold
  cross-tool cache comparison is established.
- Stack compile excludes tool installation, which occurs at up; compare the
  complete first task with disclosure rather than compile alone. Provisioned
  CLI installation is untimed. Nix/Pixi service behavior and Flox's activation
  holder include benchmark scripting.
- Compose uses host transport and the host Docker Desktop daemon, while the
  other seven use Linux containers on Apple Silicon. Host transport supplies
  outer timing only. Its `inner.failed=2` counters mean inner timing is absent,
  not that the successful repeated commands failed. Those counters cannot
  become an inner execution distribution or a functional-failure claim.
