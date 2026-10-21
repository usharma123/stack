# Fixing the AgentDX findings: what changed and how it was checked

Branch `fix/agentdx-findings-20261020`. It starts from the approved benchmark head
`065086dba914b886d1f46763a80b30c045e16021`; the product base is
`8b3402fa1db8f11917b5ba33a333c9093545d9ce` (Stack 0.1.22). The branch has one plain commit per
finding. Every distinct finding from the [Opus](opus/REPORT.md) and [Astra](astra/REPORT.md)
reports and the [synthesis](SYNTHESIS.md) is classified, with its commit, its proof and what
remains, in [REMEDIATION-LEDGER.md](REMEDIATION-LEDGER.md). The two reports, the synthesis and
the archived benchmark are unchanged. Neither the benchmark files (`bench/`) nor their
fingerprints changed, so the harness that was approved at `065086d` is still the one in use.

Raw evidence is git-ignored, under `eval/results/agentdx-product-fixes-20261020/`. Paths
below are relative to that directory.

The first independent review (Astra, an AI reviewer, of `a401cfe`) requested changes: four
defects in this branch's own fixes and two evidence corrections. Both are dealt with in
[round 2](#round-2-corrections-after-the-independent-review). The second (Astra, of `f58c09f`)
passed F1 and F4 and the ordinary F2 and F3 cases, and requested changes for one boundary case
of each of F2 and F3; they are dealt with in
[round 3](#round-3-corrections-after-the-second-independent-review). The third (Astra, of
`b1bcd9e`) found round 3's corrections in place and requested changes for one command-admission
case next to them, derived from the source; it is dealt with in
[round 4](#round-4-correction-after-the-third-independent-review). The fourth (Astra, of
`e34817c`) found round 4's correction sound and requested changes for two cancellation cases in
the callers of the shared process helpers, derived from the source; they are dealt with in
[round 5](#round-5-corrections-after-the-fourth-independent-review). A new independent review of
the round-5 head is pending. Nothing here was pushed, released or published, and 0.1.22 is still
not on npm. Every binary below is a source build.

After round 5 was approved, pushed and assessed, the owner decided the open supervisor question
(ledger O1). The change is recorded in the
[follow-up of 2026-10-21](#follow-up-2026-10-21-default-supervisor-cleanup). The rounds below
describe the branch as it was at their heads.

## Follow-up 2026-10-21: default supervisor cleanup

| | |
|---|---|
| Decision | The owner was asked: "when `stack down` stops the last session using a supervisor that Stack owns, should it stop that supervisor too?" The answer was "Stop it automatically for complete cleanup" |
| Start | Clean `4f7201c1fbf4013ca084832d4f4ea65126b3b128`, which is `c73f38a` (the approved code) plus the documentation-only Fable assessment |
| Tested head | `b80318cc51b680ceb64f9f12ba719723494f9d94`, clean tree, one plain commit: code, tests and user and design docs. The documentation commit after it changes only `eval/agentdx-20261020/` |
| Binary | `cargo build --locked` (cargo 1.93.1, debug, macOS 15.7.2 arm64), sha256 `ec32d2363c1c11c07f7263c4d0f221c56e4df8d7bef1e300d40d8a6f2ec296ed`, frozen in `gates/bin/stack`. `cargo test` rebuilt the same hash |
| Evidence | `eval/results/agentdx-supervisor-default-20261021/` (git-ignored; paths in this section are relative to it) |

What changed, and why each safeguard exists, is in the
[ledger](REMEDIATION-LEDGER.md#follow-up-2026-10-21-plain-down-stops-the-supervisor-stack-started-owner-decision-on-o1).
In short:

- Plain `down`, `session::down` and MCP `stack_down` with the field omitted stop the supervisor
  and its `log-sink` helpers. This happens only after the services are confirmed gone, and only
  when the supervisor carries this Stack state's owner mark and nothing else uses it.
- Otherwise the supervisor is kept and `supervisor.reason` says why.
- `--keep-supervisor` and `"stop_supervisor": false` opt out. `--stop-supervisor` is still
  accepted.

While making the change, one more defect turned up. The old opt-in, run on a root where no
supervisor was running, started one through `pitchfork list`, and that supervisor was then
killed with the command's process group (`before/probe-4f7201c.txt`). Now nothing is listed
unless a supervisor is already running.

Gates, all with the frozen binary at the tested head (`gates/summary.txt`, `gates/gates-summary.txt`):

| Gate | Result |
|---|---|
| clippy `--all-targets -D warnings` | 0 findings |
| `cargo test --locked --all-targets` | 534 passed, 0 failed, 13 ignored (lib 243, runtime 173: 4 more than round 5's 530) |
| node tests | 41 passed, 1 skipped |
| real-mise provider boundary | passed |
| macOS native e2e, with a run-owned local registry | 12/12, including 5-oci; registry container removed |
| six regressions, cold | 6 FIXED; `down-before-up` now also reports `supervisor` |
| eight validation cells, cold | 8/8 valid, `globals_changed` [] |
| bench unit tests | 124 OK in the checkout and 124 OK in a `git archive` export. `bench/` and its fingerprints are unchanged |
| LinuxKit (local Docker arm64, rust:1.93, unprivileged user; not a hosted runner) | built from `git archive b80318c` (binary `0a12e5be…0488`); lib 242 passed / 13 ignored; runtime `down supervisor gc restart up_ mcp` 74 passed, including both new supervisor tests (environment read from `/proc`); six regressions 6 FIXED; the same probe (`linux/probe.txt`) gave the same outcomes in every step; container removed |

Proof beyond the gates:

- **Real mise and Pitchfork 2.29.0 probe** (`probes/probe.txt`; script `scripts/probe.sh`,
  hash in `probes/script.sha256`). The root is isolated, and tool installs come from the
  existing `/tmp/afc` cache. What it showed:
  - Two sessions shared one supervisor. The first `down` kept it ("the session of …/b uses
    this supervisor"), and b stayed healthy. The last `down` stopped the supervisor and both
    log-sinks, leaving none.
  - A repeated `down`, and one in a checkout that never ran `up`, reported "it was not
    running". No supervisor start was logged.
  - These all behaved as documented: MCP with `{}`, `--keep-supervisor` and MCP `false`
    (kept, then a plain `down` stopped it), and the legacy `--stop-supervisor`.
  - A non-Stack `pitchfork run` daemon kept the supervisor until that daemon was stopped. A
    supervisor that was already running when Stack first used it was kept and is still alive;
    the probe stopped it itself. A held shared lock kept the supervisor for 5 s.
  - Concurrent down and up ran 5 rounds in one order (down first: stopped, and c's up
    started a fresh supervisor and verified) and 3 in the other (c's up in flight: kept for
    c's session). c was healthy after every round.
  - At the end, the unrelated sentinel process and an unsaved file were intact.
  - These race rounds depend on timing. The lock ordering itself is proven by the
    deterministic unit and runtime tests.
- **Before** (`before/`):
  - The 4f7201c binary on the same setup left `supervisor run` and a `log-sink` running after
    two plain downs. Its opt-in, on an idle root, started a supervisor.
  - The two new runtime tests, on the 4f7201c source, fail (`before/runtime-on-4f7201c.log`).
    They fail at their first step that needs the new owner record, so the binary probe is the
    behavioural before.
- **Globals**: hash-only receipts of the user's Stack, mise and Pitchfork files
  (`globals-before.json`, `globals-after.json`) are identical. The user's global supervisor
  was not running at the start, and no command here addressed it.

Not done:

- push, PR, merge or release;
- a rerun of any agent trial: the pilot report stays at
  `60aa3b115ce727e354a209f7ab2bf83eb184d2d070da48e63b83cced19365f84`, verified but not reviewed
  by a person, and `complete` and `publishable` remain false;
- a hosted Linux run.

Next: a new independent review of the follow-up.

## Round 5: corrections after the fourth independent review

| | |
|---|---|
| Tested head | `34369efb10cece72432843235fa8fd2ba4d55c3c`, clean tree: round 4's head `e34817c` plus two commits, R5-F1 `c0e6238` and R5-F2 `34369ef`. The documentation commit after them changes no code |
| Binary | `cargo build --locked` (cargo 1.93.1, debug), `stack 0.1.22`, sha256 `84f3c9064f5e7f17e2d0e3e754a8bb223d5b673225bcc10117aed43678973287`, frozen in `gates/bin/stack`. `cargo test` rebuilt the same hash |
| Evidence | `eval/results/agentdx-product-corrections-r5-20261020/` (git-ignored; paths in this section are relative to it). Earlier rounds' directories are untouched |

What each finding was, its fix, proof and remaining limit are in the
[ledger](REMEDIATION-LEDGER.md#corrections-after-the-fourth-independent-review-round-5). In short:

- **R5-F1** A user's command was named for signal forwarding, and the registry lock released,
  before its run made the monitor that watches it. A signal passed on to the command in
  between was taken as already seen, so the 10 s cancellation grace never started. An attached
  `exec` without `--timeout` could then wait for a command ignoring SIGTERM indefinitely; a
  captured or `--timeout` run waited for its own deadline. The monitor is now made under the
  naming lock, for all three runs that watch a user's command.
- **R5-F2** An attached run (`exec`/`run` without `--json` or `--timeout`) decided whether to kill
  what an exited command left in its group before the signals that were the command's had all
  been handled. A SIGTERM still queued for the signal thread as the command exited 0 left a
  descendant ignoring SIGTERM running after Stack. Retirement now makes that decision itself:
  an exited command's leader stays unreaped, and its group still signalled, until every signal
  that arrived by then is handled, and the group is killed before the reap if one was the
  command's. Provider commands run by `output` follow the same order, so a signal queued as one
  exited now reaches its group before it ends Stack. A command that ends unsignalled still keeps
  what it started.

Both findings are source-derived: the reviewer gave the schedules from the code and did not run
them, and no real Stack run here showed either before the fix. Each was reproduced
deterministically by unit tests on the unfixed code. The final tests, with their test-only
hooks (compiled into test builds only, like the earlier reader gate), were applied to the code
before each fix by `scripts/unfix_f1.py` and `scripts/unfix_f2.py` in scratch exports
(`before/r5-f*-tests-on-unfixed.diff`; `scripts/before.sh`).

- `before/r5-f1-unit.log`: a SIGTERM is passed on and handled once the command is named, before
  the run watches. The command ignores it. The captured and deadline runs time out after 40 s, and
  the attached run does not return within the 60 s bound. After the fix, each run kills the group
  once the 10 s grace passes, with no second signal.
- `before/r5-f2-unit.log`: three schedules leave the command's SIGTERM-ignoring sleep running. In
  the first, an attached command's SIGTERM is held back while it exits 0. In the second, a signal
  is passed on as the attached command is retired. In the third, a provider command's queued
  signal never reaches the sleep it started. The unsignalled control keeps its sleep both before
  and after.

After the fix all 41 process tests pass (`after/r5-unit.log`). Twenty repeats of every signal
schedule test pass (`after/r5-repeat20.txt`: round 3's two, round 4's three and round 5's
seven, with nothing left running). `INVARIANTS.md` in the evidence directory states, for each
caller, what holds at admission, monitor initialization and refresh, grace, normal completion,
received-but-unhandled cancellation, retirement and reaping, and which test covers each.

### Gates at the round-5 head (`gates/`)

| Gate | Result |
|---|---|
| `cargo clippy --locked --all-targets -- -D warnings` (1.93.1) | exit 0 |
| `cargo test --locked --all-targets` | exit 0: 530 passed, 0 failed, 13 ignored, across 15 test binaries (round 4's 523 plus round 5's seven; the seven new ignored tests are their inner halves, each run in its own process by its passing outer test) |
| `node --test tests/*.test.mjs` | exit 0: 41 pass, 1 skipped |
| Provider boundary against the real mise (`/opt/homebrew/bin/mise` 2026.10.3) | exit 0: 2 passed |
| `tests/e2e/native.sh`, macOS 15.7.2 arm64 | exit 0: 12 of 12, including `5-oci` against a run-owned `registry:2` container removed afterwards |
| `bench/regressions.py`, cold | exit 0: all six `FIXED`, controls passed, cleanup clean, `globals_changed: []` |
| Eight validation cells, cold | all valid: no-op rejected, reference passing, task loop passing before and after, cleanup clean, `globals_changed: []` in every cell |
| Bench unit tests | 124 pass in the checkout and in a clean `git archive HEAD` export |
| Linux, local Docker LinuxKit arm64 (`linux/`, `linux-e2e/`), not a hosted runner | Debian, `rust:1.93`, built from `34369ef` as an unprivileged user (binary `600247d2…`). Unit tests 239 passed, 13 ignored (the twelve inner halves, each run by its outer test, and `procinfo`'s sleeper helper), including round 5's seven, round 4's three and round 3's two schedule tests. The runtime tests that the signal, exec-termination and setup fixes touch: 27 passed. The six regressions run cold: all `FIXED`, `globals_changed: []`. The native e2e suite passed all 11 scenarios it ran; OCI was skipped for want of a registry. Both containers were run-owned and removed |

The global Stack/mise/Pitchfork hash receipt before (`globals-before.json`) equals round 4's
final one; the receipt after (`globals-after.json`) equals it too.

### Probes, on the frozen binary (`after/probes/`)

- `scripts/attached_probe.py` (new): attached `stack exec`, with no terminal, against real mise
  2026.10.3. The command starts a sleep ignoring SIGTERM and checks that its parent is the binary
  under test. It then sends that parent SIGTERM and exits 0. It runs once under the reviewer's
  delayed signal thread (`delay_forwarder_v2.dylib`, hash unchanged), once without it, and once
  as an unsignalled control. On the round-4 binary (`attached-r4/`) the delayed case exits 0 with
  the sleep still running; the probe then killed it. On the round-5 binary (`attached-r5/`) the
  sleep is gone. Both binaries clean up the case without the delay, both keep the control's
  sleep, and the sentinel and unsaved file survive. This probe is timing-assisted (a 500 ms
  pause), not a deterministic schedule. Its first version was invalid and is kept in
  `drafts/attached-probe-v1-invalid/` with the reason: the sleep held Stack's output pipes, so
  the harness waited for it to end before checking.
- `scripts/probes.sh` reruns round 3's cancellation probes (`r3/`), as round 4 did. The
  reviewer's `late_signal_v2.py` and its control give exit 143, `cancelled`, the later setup not
  run, the sentinel alive and unsaved work kept. Round 3's `late_signal_r3.py` covers CLI
  graceful, CLI ignoring SIGTERM, MCP and a partial wave, all `cancelled` with exit 143. Every
  field matches round 4's receipt except ports, PIDs, paths and the binary hash.
- R5-F1 has no real-binary probe. Its window lies between two statements in Stack's own thread,
  which no external delay can hold open; only the unit tests control it.

### What was not rerun

- No agent trial. The six-trial pilot describes round 1's binary only. Its report still hashes
  `60aa3b11…`, and no human has reviewed it.
- The F1/F4 and readiness probes were not rerun. This round touches only signal monitoring and
  retirement in `src/process.rs`, and the unit and runtime tests for those paths pass at this
  head.
- No hosted runner, Linux x64 or Intel macOS.

### Remaining limits of round 5

- A signal the kernel has not yet delivered to Stack's handler when a command is retired still
  belongs to what follows, as in rounds 3 and 4. Waits for the signal thread are still unbounded.
- The grace is timed from when the run first sees a forwarded signal, not from the forward
  itself. Once the run is watching, that is at most one poll interval.
- Attached `exec` still exits with the command's own status after a passed-on signal, as
  documented ("reports the command once it is done"). A command that exits 0 on SIGTERM gives
  exit 0. Only its group is now cleaned up.
- On a terminal the command stays in Stack's process group, so only its leader is ever
  signalled or killed. That path has no end-to-end test here, because none of the tests run on
  a terminal.

## Round 4: correction after the third independent review

| | |
|---|---|
| Tested head | `f6a87b6327804fb47763b98b31e69843a0c1cdf1`, clean tree: round 3's head `b1bcd9e` plus one commit, R4-F1 `f6a87b6`. The documentation commit after it changes no code |
| Binary | `cargo build --locked` (cargo 1.93.1, debug), `stack 0.1.22`, sha256 `bf7a01c3fb9f41e717048aa8c93f2af6520b577177a4c1e498b316d79aca4f85`, frozen in `gates/bin/stack`. `cargo test` rebuilt the same hash |
| Evidence | `eval/results/agentdx-product-corrections-r4-20261020/` (git-ignored; paths in this section are relative to it). Earlier rounds' directories are untouched |

What the finding was, its fix, proof and remaining limit are in the
[ledger](REMEDIATION-LEDGER.md#corrections-after-the-third-independent-review-round-4). In short:

- **R4-F1** A command was admitted in two steps: the signals counted so far were settled, then
  the registry lock was taken and the count read again as the command's start. A signal that
  reached Stack's signal thread between the two belonged to no command and was not yet
  handled. With nothing else running, that thread would then end Stack with the signal's
  default action while the new command (a setup, an exec, a provider command) kept running in
  its own process group. Admission is now one decision under the lock: the count read there is
  spawned with only if every signal through it was handled; otherwise the lock is released, the
  wait happens, and the decision is taken again.

This finding is source-derived: the reviewer gave the schedule from the code and did not run it,
and no real Stack run here showed it either. It was reproduced deterministically by unit
tests against the unfixed `spawn_named`. Test-only hooks at the admission points (compiled
into test builds only, like round 3's reader gate) deliver a real SIGTERM at the chosen point
while the signal thread is held. `before/r4-unit.log` (tests and hooks applied to the unfixed
code: `before/r4-tests-on-unfixed.diff`) shows the defect. With the signal arriving after the
earlier settlement and before the decision, the command was admitted, and its `sleep`
(PID 23917) was still alive after its Stack process died of SIGTERM; the outer test then killed
it. With a user's command running, the next command started before the signal was handled.
After the fix, all three new tests pass (`after/r4-unit.log`), 20 of 20 repeats
(`after/r4-repeat20.txt`). The third test, a signal arriving after the boundary that must reach
the admitted provider command, passes both before and after: it guards the boundary rather than
reproducing the defect.

### Gates at the round-4 head (`gates/`)

| Gate | Result |
|---|---|
| `cargo clippy --locked --all-targets -- -D warnings` (1.93.1) | exit 0 |
| `cargo test --locked --all-targets` | exit 0: 523 passed, 0 failed, 6 ignored, across 15 test binaries (round 3's 520 plus R4-F1's three; the three new ignored tests are their inner halves, run in their own process by the passing outer tests, as R3-F2's two are) |
| `node --test tests/*.test.mjs` | exit 0: 41 pass, 1 skipped |
| Provider boundary against the real mise (`/opt/homebrew/bin/mise` 2026.10.3) | exit 0: 2 passed |
| `tests/e2e/native.sh`, macOS 15.7.2 arm64 | exit 0: 12 of 12, including `5-oci` against a run-owned `registry:2` container removed afterwards |
| `bench/regressions.py`, cold | exit 0: all six `FIXED`, controls passed, cleanup clean, `globals_changed: []` |
| Eight validation cells, cold | all valid: no-op rejected, reference passing, task loop passing before and after, cleanup clean, `globals_changed: []` in every cell |
| Bench unit tests | 124 pass in the checkout and in a clean `git archive HEAD` export |
| Linux, local Docker LinuxKit arm64 (`linux/`, `linux-e2e/`), not a hosted runner | Debian, `rust:1.93`, built from `f6a87b6` as an unprivileged user (binary `fbd244cb…`). Unit tests 232 passed, 6 ignored (the five inner halves, each run by its outer test, and `procinfo`'s sleeper helper, as before), R4-F1's three and R3-F2's two included; the runtime tests the signal and setup fixes touch 26 passed; the six regressions cold all `FIXED`, `globals_changed: []`. The native e2e suite passed 11 of 11 run scenarios, OCI skipped for want of a registry. Both containers were run-owned and removed |

The global Stack/mise/Pitchfork hash receipt before (`globals-before.json`) equals round 3's
final one; the receipt after (`globals-after.json`) equals it too.

### Probes, on the frozen binary (`after/probes/`)

`scripts/probes.sh` reruns round 3's cancellation probes against real mise 2026.10.3 and
Pitchfork 2.29.0, under the reviewer's delayed signal thread (`delay_forwarder_v2.dylib`, hash
unchanged). The reviewer's `late_signal_v2.py` and its control are copied unmodified from round
3's `probes/reviewer/`, with hashes checked. Both give exit 143, `cancelled`, the later setup
not run, status unhealthy, the sentinel alive and unsaved work kept. Round 3's
`late_signal_r3.py` covers a CLI setup exiting 0, one ignoring SIGTERM (10 s grace), MCP, and a
started dependency wave. Every case is `cancelled`, exit 143, with no later setup, and the
started dependency is stopped by `down`. These match round 3's results. No probe root left a
process. They do not exercise the new admission schedule itself, which only the unit tests
control.

### What was not rerun

- No agent trial. The six-trial pilot describes round 1's binary only; its report still hashes
  `60aa3b11…`, and it remains unreviewed by a human.
- The F1/F4 and readiness probes (`real_gc_v2.py`, `file_probes.py`, `readiness_spawn_v2.py`,
  `readiness_r3.py`) were not rerun: this change touches only command admission in
  `src/process.rs`, and the unit and runtime tests for those paths pass at this head.
- No hosted runner, Linux x64 or Intel macOS.

### Remaining limits of round 4

- A signal the kernel has not yet delivered to Stack's handler still belongs to what follows,
  as in round 3.
- Admission, like retirement, waits for the signal thread without a timeout. A stream of
  signals can keep re-running the decision. A signal with no recipient ends Stack in any case.
- With a user's command running, a signal reaches it and Stack goes on, so a command admitted
  on another thread afterwards still runs, owned and named, once the signal has been handled.
  Whether to start it after a cancellation is the caller's check, as before (`up` checks before
  each setup).

## Round 3: corrections after the second independent review

| | |
|---|---|
| Tested head | `ba5360f5af432fd0a98a97da3d1e3f83ef849b33`, clean tree: round 2's head `f58c09f` plus one commit per finding, R3-F2 `dbb5085`, R3-F3 `ba5360f`. The documentation commit after it changes no code |
| Binary | `cargo build --locked` (cargo 1.93.1, debug), `stack 0.1.22`, sha256 `ce14a650eedd7c1c5345a276adf8e920864b15f402900304a0cfac7c05217b99`, frozen in `gates/bin/stack`. `cargo test` rebuilt the same hash |
| Evidence | `eval/results/agentdx-product-corrections-r3-20261020/` (git-ignored; paths in this section are relative to it). Rounds 1 and 2's directories are untouched |

What each finding was, its fix, proof and remaining limit are in the
[ledger](REMEDIATION-LEDGER.md#corrections-after-the-second-independent-review-round-3). In short:

- **R3-F2** Stack's signal handler only records a signal; a separate thread acts on it. A
  command's run returned without waiting for that thread, so when it ran late a setup that got
  SIGTERM and exited 0 was judged by its exit status: `up` went on and succeeded. A run now
  returns only once every signal that arrived while its command ran was acted on, and no
  command starts while a signal that already arrived is unhandled.
  *(Note added in round 4: the second half held only for signals counted before admission
  took the registry lock. One arriving in between was neither handled first nor the new
  command's: R4-F1, [round 4](#round-4-correction-after-the-third-independent-review). The
  text above is kept as written.)*
  *(Note added in round 5: the run did wait for those signals, but an attached run decided
  whether to clean up its command's group before that wait: R5-F2,
  [round 5](#round-5-corrections-after-the-fourth-independent-review).)*
- **R3-F3** The restart prerequisite gate took a `ready_cmd` that could not be spawned or read
  as passing, and stopped the dependent. A `ready_cmd` now passes only when it ran and exited 0
  in time; one that could not run is `dependency_not_ready` with the reason, before anything is
  touched, and timeout reports name it too.

Both reproduced on round 2's frozen binary `03fd21e6…` before any change, with the reviewer's
own scripts unmodified (`before/late-signal/`: exit 0, the later setup ran, status healthy;
`before/readiness-spawn/`: api stopped, then `setup_failed`). R3-F2's schedule is controlled:
the reviewer's test-only interposer (`probes/reviewer/delay_forwarder_v2.*`, hashes in
`source-sha256.txt`) holds Stack's signal thread 500 ms after it reads a real SIGTERM. It was
loaded only into the probes' own Stack invocations and changes no source. That shows the
ordering defect; it is not a claim about how often a scheduler stalls that long.

Each fix's new tests fail on the code before it and pass after (`before|after/*.log`). R3-F2's
two unit tests run in a process of their own, with Stack's signal thread held by a test-only
gate (compiled into test builds only), so the schedule is deterministic rather than timed;
without the fix one returns before the signal was handled and the other starts a command first
(`before/f2-unit-revert.diff` is exactly what was removed for that run). Both passed 20 of 20
repeated runs (`after/f2-unit-repeat20.txt`).

### Gates at the round-3 head (`gates/`)

| Gate | Result |
|---|---|
| `cargo clippy --locked --all-targets -- -D warnings` (1.93.1) | exit 0 |
| `cargo test --locked --all-targets` | exit 0: 520 passed, 0 failed, 3 ignored, across 15 test binaries (round 2's 518 plus R3-F2's two; R3-F3 extends two existing runtime tests; the two new ignored tests are R3-F2's inner halves, run by the passing ones) |
| `node --test tests/*.test.mjs` | exit 0: 41 pass, 1 skipped |
| Provider boundary against the real mise (`/opt/homebrew/bin/mise` 2026.10.3) | exit 0 |
| `tests/e2e/native.sh`, macOS 15.7.2 arm64 | exit 0: 12 of 12, including `5-oci` against a run-owned `registry:2` container removed afterwards |
| `bench/regressions.py`, cold | exit 0: all six `FIXED`, controls passed, cleanup clean, `globals_changed: []` |
| Eight validation cells, cold | all valid: no-op rejected, reference passing, task loop passing before and after, cleanup clean, `globals_changed: []` in every cell |
| Bench unit tests | 124 pass in the checkout and in a clean `git archive HEAD` export |
| Linux, local Docker LinuxKit arm64 (`linux/`, `linux-e2e/`), not a hosted runner | Debian, `rust:1.93`, built from `ba5360f` as an unprivileged user (binary `9aef4e56…`). Unit tests 229 passed, 3 ignored, R3-F2's two included; the runtime tests these fixes touch 26 passed, R3-F3's two included; the six regressions cold all `FIXED`, `globals_changed: []`. The native e2e suite passed 11 of 11 run scenarios, OCI skipped for want of a registry. Both containers were run-owned and removed |

The global Stack/mise/Pitchfork hash receipt (`globals-before.json`, `globals-after.json`) is
identical before and after, and to round 2's final one.

### Probes, on the frozen binary

- **The reviewer's scripts, unmodified** (`probes/reviewer/`, hashes checked against the review
  directory; only the binary and output directory arguments differ; `file_probes.py`, which
  names both inside the script, has `file_probes.py.adaptation.diff`). `late_signal_v2.py`
  (delayed thread) and its control: exit 143, `cancelled`, the later setup not run, status
  unhealthy, sentinel alive. `readiness_spawn_v2.py`: the ordinary-PATH control and the no-`sh`
  restart are both `dependency_not_ready`, db and api identities and the session bytes kept.
  F1 and F4 controls still pass: `real_gc_v2.py` (real Redis 7.4.7 ledger entry from `up`)
  keeps the data by default, leaves the redirected-parent and copy-in-place trees whole, and a
  normal reclaim removes only the data directory and its marker; `file_probes.py` gives 1 of 12
  concurrent inits succeeding (`after/reviewer/`).
- **`scripts/late_signal_r3.py`**, real mise 2026.10.3 and Pitchfork 2.29.0, every Stack, mise,
  Pitchfork and XDG directory isolated, under the same delayed thread: a setup exiting 0 via
  the CLI, a setup ignoring SIGTERM (ended after the 10 s grace), the same via MCP (the call is
  answered `cancelled`, `isError`, and the server exits 143), and a dependency wave already
  started before the cancelled setup. Every case is `cancelled`, exit 143, no later setup ran,
  the cancelled service was not started; in the last case the started dependency stays
  recorded and `down` stops it. Round 2's binary (`before/late-signal-r3/`) instead succeeds via
  the CLI and MCP and starts the cancelled service in the wave case.
- **`scripts/readiness_r3.py`**, real mise and Pitchfork: with db's ready_cmd failing, unable to
  run with its marker absent, and unable to run with its marker present, `restart api` is
  `dependency_not_ready` with identities, setup log and session bytes kept; with db ready, the
  restart replaces only api; with db's process ended (identity-checked SIGTERM), it is refused.
  Round 2's binary (`before/readiness-r3/`) stops api in the no-`sh` case and then reports the
  session stale.

Signals went only to identity-checked PIDs; the unrelated sentinels survived, unsaved files
were unchanged, and no process of any probe root was left.

### What was not rerun

- No agent trial. The six-trial pilot describes round 1's binary only; its report still hashes
  `60aa3b11…`, and it remains unreviewed by a human.
- On Linux the whole Rust suite was not run: the unit tests and the runtime tests these fixes
  touch were.
- No hosted runner, Linux x64 or Intel macOS.

### Remaining limits of round 3

- A signal the kernel has not yet delivered to Stack's handler when a setup's command is
  retired belongs to what follows: it is passed on to the next command, or ends Stack. It is
  never taken as success. Waiting for the signal thread is not bounded by a timeout; that
  thread does nothing but read reports and pass signals on.
- A `ready_cmd` that cannot run blocks `restart` of what depends on it until it can; `up`
  waits for it, then times out naming why.

## Round 2: corrections after the independent review

| | |
|---|---|
| Tested head | `18e50a79c6a39a34d5457af0e1df50f01d5bf293`, clean tree: the 31 round-1 commits plus one per finding, F4 `c25702f`, F2 `33a9a07`, F3 `2afca9f`, F1 `18e50a7`. The documentation commits after it change no code |
| Binary | `cargo build --locked` (cargo 1.93.1, debug), `stack 0.1.22`, sha256 `03fd21e68bdee0205850121228b43ec895c09ea3307c2fd57852d118cf4a8784`, frozen in `gates/bin/stack`. `cargo test` rebuilt the same hash |
| Evidence | `eval/results/agentdx-product-corrections-r2-20261020/` (git-ignored; paths in this section are relative to it). Round 1's directories are untouched |

What each finding was, its fix, proof and remaining limit are in the
[ledger](REMEDIATION-LEDGER.md#corrections-after-the-independent-review-round-2). In short:

- **F1** `gc --reclaim-data` followed a directory above the data that had been replaced by a
  link, and removed an independent copy. Data is now bound to where it resolved and which
  directory it is, reached without following links, and removed through the handles checked.
- **F2** A setup that handled SIGTERM and exited 0 let `up` start the service and exit 0. A
  signal passed on to a setup now cancels the start (`cancelled`, exit 128 plus the signal).
- **F3** `restart` of a dependent alone ran its setup and started it with a dependency down.
  A dependency outside the restart must now run and be ready first, or nothing is touched.
- **F4** Concurrent `stack init`s could overwrite each other, and a dangling link was
  followed. The manifest is now published whole by a rename that never replaces anything.

Each fix's new tests fail on the code before it (`before/*.log`) and pass after (`after/*.log`). F1's
runtime test fails first at its new assertion that `up` records the location; the unsafe
removal itself is shown by the old binary in `before/f1-gc-bound-probe.json`.

### Gates at the round-2 head (`gates/`)

| Gate | Result |
|---|---|
| `cargo clippy --locked --all-targets -- -D warnings` (1.93.1) | exit 0 |
| `cargo test --locked --all-targets` | exit 0: 518 passed, 0 failed, 1 ignored, across 15 test binaries (round 1's 508 plus 10 new tests) |
| `node --test tests/*.test.mjs` | exit 0: 41 pass, 1 skipped |
| Provider boundary against the real mise (`/opt/homebrew/bin/mise` 2026.10.3) | exit 0 |
| `tests/e2e/native.sh`, macOS 15.7.2 arm64 | exit 0: 12 of 12, including `5-oci` against a run-owned `registry:2` container removed afterwards, and `7-deleted-project`, whose `gc --reclaim-data` removes real Redis data through the new binding |
| `bench/regressions.py`, cold | exit 0: all six `FIXED`, controls passed, cleanup clean, `globals_changed: []` |
| Eight validation cells, cold | all valid, as in round 1, task loop passing before and after in every cell |
| Bench unit tests | 124 pass in the checkout and in a clean `git archive HEAD` export |
| Linux, local Docker LinuxKit arm64 (`linux/`), not a hosted runner | Debian, `rust:1.93`, built from `18e50a7` as an unprivileged user (binary `afa55da9…`). Unit tests 227 passed, 1 ignored; the runtime tests these fixes touch 25 passed; the six regressions cold all `FIXED`, cleanup clean, `globals_changed: []` (`linux/`). The native e2e suite passed 11 of 11 run scenarios, OCI skipped for want of a registry, `7-deleted-project` included (`linux-e2e/`). Both containers were run-owned and removed |

The global Stack/mise/Pitchfork hash receipt (`globals-before.json`, `globals-after.json`,
taken with the benchmark's `globals_snapshot()`) is identical to round 1's.

### Probes

- **The reviewer's own scripts** (`probes/reviewer/`) ran against the round-1 binary
  (`before/`) and the round-2 binary (`after/`). Only their output directory and binary path
  were changed (`adaptation.diff`; original hashes in `original-sha256.txt`). Concurrent init:
  6 of 12 succeeded before, 1 after, every other one `project_exists`; the dangling link was
  followed before and is refused, with nothing created, after. GC: the copy's data and marker
  were removed before and nothing is removed after. Those GC probes write a ledger without the
  new location, which the round-2 binary keeps for that reason alone, so
  `scripts/gc_bound_probe.py` repeats them with an entry bound as `up` now records it:
  before, the redirected parent and a copy in place lose their data; after, both are left
  whole and say why, the default keeps the data, and a normal reclaim removes only the
  directory and its marker, not a file beside them (`before|after/f1-gc-bound-probe.json`).
- **Real mise 2026.10.3 and Pitchfork 2.29.0**, every Stack, mise, Pitchfork and XDG directory
  isolated: the reviewer's `lifecycle_probes.py` now gives `dependency_not_ready` (exit 1,
  setup log unchanged) and `cancelled` (exit 143, nothing started, status unhealthy).
  `scripts/lifecycle_supplement.py` adds a setup ignoring SIGTERM (cancelled after the 10 s
  grace, exit 143, the second service's setup not run), an alive dependency failing its
  ready_cmd (refused, with db, cache and api PIDs, process identities and the session id
  unchanged), and a positive control restarting only api. Signals went only to
  identity-checked PIDs, the unrelated sentinel survived, unsaved files were unchanged and no
  process of the probe roots was left.

### What was not rerun

- The six-trial pilot (below) ran against round 1's binary and describes it only. No new agent
  trial was run.
- On Linux the whole Rust suite was not run: the unit tests and the runtime tests these fixes
  touch were (above).
- No hosted runner, Linux x64 or Intel macOS (an Intel macOS `cargo check` compiles).

### Remaining limits of round 2

- Data recorded before F1's fix, and data whose directory no verified `up` saw, are reported
  and kept, never removed, until an `up` binds them. Reclaiming is Unix only.
- Outside a setup, a signal during `up` or `restart` still ends Stack at once, with no JSON
  result, as before; the launch record is kept for `status` and `down`.
- `restart` never starts a dependency it was not given: `stack restart <dependency>` or
  `stack up` does.
- An interrupted `stack init` can leave a hidden `.stack.toml.*` temporary file.

## Round 1

### Tested code

- Tested head: `15ff636c190d6e8ee96cc9ab7be4e16df19db9a8`, clean tree. It has 31 commits since
  `065086d`; the ledger maps each one to its finding.
- Binary: `cargo build --locked` (cargo 1.93.1, debug), `stack 0.1.22`, sha256
  `1cad860237a06ac7014c12010e21c0518f4ecf39cca9200ccbad5b5f8224c00e`. Frozen copies are in
  `gates-r2/bin/stack` and `pilot-r2/bin/stack`. The binary embeds no docs, and there is no
  `build.rs`, so the docs-only commits after the tested head leave it unchanged.
- The final commit, `15ff636` (N1, Redis locale), was first committed as `44ebdde`. Clippy then
  failed on a single-element loop in its test (`gates-r2/clippy.log`, exit 101). The test was
  fixed in that same commit (`gates-r2/amend-note.txt`), and `gates-r2/clippy-2.log` exits 0.
  The product code is identical, and so is the binary hash.

### Gates at the tested head (all from `gates-r2/`)

| Gate | Result |
|---|---|
| `cargo clippy --locked --all-targets -- -D warnings` (1.93.1) | exit 0 (`clippy-2.log`) |
| `cargo test --locked --all-targets` (1.93.1) | exit 0: 508 passed, 0 failed, 1 ignored, across 15 test binaries (`cargo-test.log`) |
| `node --test tests/*.test.mjs` | exit 0: 41 pass, 1 skipped (`node-tests.log`) |
| Provider boundary against the real mise (`/opt/homebrew/bin/mise` 2026.10.3) | exit 0 (`provider-boundary-real-mise.log`) |
| `tests/e2e/native.sh`, macOS 15.7.2 arm64, host mise 2026.10.3 | exit 0: 12 of 12 scenarios passed (`e2e.jsonl`). This includes `5-oci`, for the first time on this host, against a run-owned `registry:2` container that was removed afterwards (`e2e-registry.txt`) |
| `bench/regressions.py`, cold | exit 0: all six `FIXED`, controls passed, cleanup clean, `globals_changed: []` (`regressions-cold/summary.json`) |
| Eight validation cells, cold (`scripts/validate_all.sh`) | all valid: starting code fails, reference passes, smoke through `exec` passes, cleanup clean, globals unchanged. The documented `stack run test` loop passes before and after in every Stack cell, where 0.1.22 failed it in both service apps (R1). The ledger/Stack `LC_ALL=C` cell, which 0.1.22 could not run (R3), is valid (`validation-cold.txt`) |
| Bench unit tests, `python3 -m unittest discover -s bench/tests` | 124 tests pass in the checkout (36.1 s) and in a clean `git archive HEAD` export (35.7 s) |
| `cargo fmt --check` | not a repository check: CI does not run it, there is no `rustfmt.toml`, and files unchanged since the base differ too (`cargo-attempt1-zsh-exitcodes-lost.log`). Not counted |

The global Stack/mise/Pitchfork hash receipt was taken with the benchmark's own
`globals_snapshot()`, and it records hashes only. It is identical to the predecessor's
start-of-work receipt (`gates-r2/globals-before.json` against `globals-before-start.json`).

#### Earlier gate runs, kept as they were

These ran at `ec06451`, before the Redis fix, with binary `9d99203b…`. They are in `gates/`:

- `e2e-native.log`: `3-leases` failed. With D3, `status` now exits 1 for an expired owner
  lease, and the scenario still expected success.
- `e2e-native-2.log`: a single-scenario rerun failed on a missing directory, a wrapper mistake.
- `e2e-native-3.log`: `7-deleted-project` failed. It still asserted that `gc` returns nothing
  after a removed checkout, which is the behaviour L1 changes.
- Both scenarios were updated, in fixups folded into their finding commits. The `7-deleted-project`
  update also checks that ports are released, that data is kept, and that `--reclaim-data`
  then removes it.
- `e2e-native-4` then passed 11 of 11 run scenarios, with OCI skipped.
- `task-env-provenance-real-mise.log` failed before the R1 test adjustment;
  `task-env-provenance-real-mise-2.log` passes.
- Warm and cold validation, cold regressions and both bench test runs passed there too.

#### Linux (local Docker LinuxKit arm64, Debian 13, not a hosted runner)

- `linux/`, at `ec06451`: the source build and `setup` succeeded as an unprivileged user, and
  e2e passed 11 of 11 run scenarios. As root, `up` refused Postgres in 9 ms, before any download
  (`root-up.log`, X8).
- The regression run exited 2: `removed-checkout-reclaim` was `invalid` because Redis would not
  start under `LANG=en_US.UTF-8`, a locale the image lacks. That is finding N1.
- `linux2/`, at snapshot `c719b3c`, whose tree is byte-identical to the committed N1 fix: Redis
  starts and answers `PING` under the missing locale (`redis-bad-locale.log`), and all six
  regressions are `FIXED` (exit 0).
- Linux was not re-run after the test-only amend.
- There was no hosted Linux, Linux x64 or Intel macOS run, and no hosted-agent vendor run.

### Real-agent pilot

This is the first real-agent run of the benchmark: one model, three tasks, both arms, one
repetition, so six trials. It ran in `pilot-r2/` against the round-1 binary above (`1cad8602…`).

#### Setup

| | |
|---|---|
| Agent | Claude Code 2.1.304 (`/Users/utsavsharma/.local/bin/claude`), model `claude-opus-5-5`, `--effort high`. The effort setting is bound into argv by the plan (`agent_options_binding`) |
| Isolation | Each trial gets a fresh CLI process and context. Its hashed argv (`agent.json`) passes `--safe-mode`, `--strict-mcp-config`, `--no-session-persistence` and `--disable-slash-commands`, denies `Read`/`Edit` of the source checkout, and stdin is closed. That is what was asked, not proof of what the CLI did: every init record lists no MCP servers, skills or slash commands, but all six also list 13 plugins. No plugin tool use appears in any transcript, which does not show that plugins had no effect. The `Read`/`Edit` denials are not a sandbox; Bash could still have reached the checkout. The CLI's normal login route was used; no credential was copied or printed |
| Plan | `pilot-r2/plan.json`: seed 20261020, warm cache `/tmp/afx-warm`, `LANG=en_US.UTF-8`, a 600 s budget per trial (60 minutes at most in total), counterbalanced order, run by `pilot-r2/run.sh` |
| The predecessor's plan | `pilot/plan.json` was kept unchanged and never run. Its argv put the prompt after the variadic `--disallowedTools`, which swallows it: `argv-check/old-argv.*` exits 1 with "Input must be provided". This plan moves `{prompt}` to straight after `-p` (`argv-check/new-argv.*`, exit 0) and binds the final binary. The arm order is the same |

#### Outcome (`pilot-r2/report.json`, re-derived from hashed receipts)

| Trial | Hidden checks + smoke | Agent seconds | Turns | Bash / Write calls | Left at agent exit | CLI-reported cost (USD, list) |
|---|---|---|---|---|---|---|
| ledger / existing | pass | 62.3 | 9 | 7 / 1 | nothing | 0.25 |
| ledger / stack | pass | 65.9 | 9 | 7 / 1 | the root's Pitchfork supervisor (L6) | 0.22 |
| catalog / stack | pass | 29.3 | 6 | 5 / 0 | the root's Pitchfork supervisor (L6) | 0.13 |
| catalog / existing | pass | 23.8 | 7 | 5 / 1 | nothing | 0.15 |
| slug / existing | pass | 32.4 | 7 | 5 / 1 | nothing | 0.16 |
| slug / stack | pass | 28.5 | 5 | 4 / 0 | nothing | 0.12 |

- **All six are valid and `verified_task`, with no timeouts.**
- **None counts as `autonomous`.** All six are `unreviewed`. A trial counts as autonomous only
  after a human transcript review, and none has happened. No `review.json` was written. Unfilled
  templates are in `pilot-r2/review-templates/`. Machine-extracted facts are in
  `pilot-r2/transcript-facts.json`: model and version from the CLI's init event, tool calls,
  every Bash command, and usage. They are labelled as not a review.
- **The report has `publishable: false`, `complete: false` and no scored pairs.** That is
  correct while the reviews are missing.
- **Usage and cost** are the CLI's own `result` figures, at list prices. They are not a billing
  record. Total: $1.04 and 242 s of agent time.
- **Background model calls.** The CLI also made tiny `claude-haiku-5-5` calls of its own,
  visible in `modelUsage` at about $0.0001 each. Opus 5.5 is the main model in every init
  record and assistant message; high effort is shown by the launch argv, and no server-side
  effort value is observable.

#### What the transcripts show

These are facts only; reading them is the reviewer's job.

- **The documented loop works.** In both Stack service tasks, the agent ran `stack run test`
  and it passed, without the `exec` workaround that R1 forced on 0.1.22.
- **The supervisor stays behind.** Both Stack service agents ended with a plain `stack down`.
  This left the root's Pitchfork supervisor running (`supervisor_left_running: 2`). That is
  documented L6 behaviour, since `--stop-supervisor` is opt-in. The ledger records it as
  pilot observation O1. The harness stopped the supervisor in teardown, which it does for every
  Stack trial.
- **No source-checkout access was observed.** Its path appears in none of the six transcripts.
  That describes these transcripts; it is not a sandbox guarantee (see Isolation). Both slug
  agents extended the arm's smoke test. The grader never takes benchmark-owned files from a
  submission, so the protected original smoke test and the hidden checks decided those grades.
- **Global state was not touched.** The Stack/mise/Pitchfork hash receipts are identical
  before and after (`pilot-r2/globals-*.json`), and no trial process remained afterwards.

#### Independent AI transcript review (not a human review)

The independent reviewer of `a401cfe` (Astra, an AI agent: GPT-6-Astra, high effort) read all six
complete transcripts. Its summary, its six per-trial reviews and its supplement report are kept
with their source hashes in
`../agentdx-product-corrections-r2-20261020/ai-pilot-review/` (relative to the evidence root
above). It found:

- All six receipt-verified task outcomes stand. No human intervention or help request, and no
  source-checkout, hidden-grader, oracle or private-configuration access, appears in the tool
  calls, and nothing suggests an answer passed between trials.
- The ledger/existing agent's own test changed a seeded amount, so its baseline smoke failed
  after a restart. The agent restored the seed and reran the smoke itself: recovery, not help.
- Both Stack service agents stopped with a plain `stack down`, leaving the root's supervisor to
  the harness's teardown (O1). That is not complete cleanup by the agent.
- The Stack slug agent left `/tmp/slugcheck.mjs`, outside the trial root: a file-hygiene limit.
  The file is still there. It was not deleted, because nothing proves it belongs to that trial
  alone.
- The two catalog agents used the same global temporary backup name, one after the other; each
  removed it.
- The CLI-reported list cost of all six is $1.0382743 (not a billing record), and the agent time
  242.2 s, excluding harness start-up and grading.

That review is an AI review. The protocol requires a human one: "A human reviewer fills
`review.json`" (`bench/AGENTDX-PROTOCOL.md` line 158, also its line 19 and `run.py`'s report limits). So
it does not satisfy that gate. No `review.json` was written, `pilot-r2/report.json` is
unchanged (sha256 `60aa3b11…`), and the reviewer re-derived it exactly with the unchanged
harness: 6 verified, 6 unreviewed, no scored pairs, `complete: false`, `publishable: false`, and
no autonomy claim.

#### Limits

- Six trials with one repetition is a smoke test of the method. It supports no comparison
  between arms, no speedup and no statistics.
- The tasks are small and synthetic, and every trial passed in both arms. These tasks therefore
  do not separate the arms on this model.
- Local macOS only. No hosted agent ran, because no safely configured hosted route existed here.
- No trial counts as autonomous until a human reviews its transcript and fills `review.json`,
  as the protocol requires. The AI review above does not satisfy that, and no human review has
  been done.
- The pilot describes binary `1cad8602…` (tested head `15ff636`) only. It was not rerun after
  the corrections that followed the independent review.

## What remains open

- **H1, handing database state between worktrees** (snapshot, clone, `exec --from`). This
  needs a product decision on data ownership and copy semantics, and was not started.
- **Distribution.** 0.1.22 is not published, and this branch makes no release.
- **Hosted platforms.** No hosted runner, Linux x64 or Intel macOS was available. Every Linux
  claim above is local LinuxKit.
- **Benchmark follow-ups (B1)** are in the ledger: a Compose/devcontainer arm, a disposal task
  family, and gating on the documented loop now that R1 is fixed. They would change the
  approved harness, so none was made.
- **Remaining limits per fix** are in the ledger's last column. Examples: R2 cannot forward a
  SIGKILL to Stack, R3 does not convert existing `SQL_ASCII` clusters, R6a is opt-in per
  service, and L1 data from before this branch is not in the data ledger.
