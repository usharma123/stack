# AgentDX benchmark protocol

Status: active pilot protocol. The harness implements everything below unless a line is
marked **not implemented**. No real agent experiment has been run under it yet. Real testing
is authorized; a first run still needs its agent CLI, models, options, credentials route and
spend limit chosen (see the README). The two 2026-10-20 hands-on sessions (Opus, Astra) are
discovery evidence and stay separate.

## Question and primary outcome

Does Stack let a coding agent finish the same development task with less human help than
the project's existing setup?

Primary outcome: **autonomous completion**. That is the share of assigned trials that meet
all of the following:

- an independently verified task result: the project's existing smoke test and the hidden
  acceptance checks both pass,
- a valid human transcript review recording no help,
- nothing the agent started still running when it stopped,
- the arm's own teardown stopping everything, without the harness reclaiming by PID.

Every trial in the plan stays in the denominator, so `not_run`, `invalid`, `unreviewed` and
`review_invalid` are never successes. The agent's final message never decides a result.
Completion, time and cost are not combined into a score.

The default runner gives the agent no human channel: stdin is closed and nobody answers. Help
is therefore impossible by construction, and the review records `help_requests` instead:
where the agent asked for help or stopped on a blocker. A harness with a human channel must
declare one in the plan. **Not implemented:** recording help events in such a channel, beyond
validating them in the review schema.

## Matched arms

Each pair runs the same task, model, agent command, model options, prompt, application
source, tool and service versions, grader, locale and budget in two arms. Model options are
controlled only when they reach the agent's argv through `{option:NAME}` placeholders, and a
literal flag that contradicts them refuses the plan. A plan without options runs on the
harness defaults, and the report says so.

- `stack`: `stack.toml` and a committed `stack.lock`; the pinned `stack` on PATH; a README
  using `stack up`, `run`, `exec`, `restart` and `down`.
- `existing`: `mise.toml` with the same pins and the same mise backends Stack locks; a
  project-written `dev.py` that starts PostgreSQL, Redis and the app natively; a README
  using `dev.py`. A `stack` stub on PATH answers "command not found".

The existing arm is synthetic glue written for this benchmark, not a real project's history.
It is a reasonable "scripts in the repo" baseline, not every team's setup. It explicitly
initializes PostgreSQL as UTF8, which a project author would do and Stack's preset does not
expose. **Not implemented:** Docker Compose or devcontainer control arms.

Every trial gets fresh runtime stores and a fresh checkout. Trials are serialized per host by
a lock file. The plan records:

- tasks, models, harness name, options with where each enters argv, and the agent command
  (immutable once planned),
- the Stack binary hash, platform label, host, locale and cache mode,
- the budget, seed and benchmark file hashes.

A trial refuses to run if the plan's benchmark files or binary changed. Arm order alternates
within each task/model block, starting from a seeded coin flip.

Cache modes:

- `cold`: everything is downloaded and installed inside the trial root.
- `warm`: mise downloads, mise installs and the uv cache are shared through `--cache-root`.
  Service data, state and the supervisor stay per trial.

Locale is a plan variable. Inherited `LANG`/`LC_*`, `PG*` and service URL variables are
dropped, and the plan's locale is applied to both arms. A noninteractive shell on a Mac is
local evidence, not evidence about hosted Linux.

## Tasks and acceptance

Every task has:

- a starting project with a seeded defect or a missing feature,
- a reference solution in `oracles/`, and grader-only data in `hidden/`,
- an existing smoke test that must pass before and after the reference,
- an exact list of `required_checks`, and one designated check the starting code must fail.

A submission passes only through both gates: the existing smoke test, run through the arm's
`exec`, and the hidden acceptance checks. A change that breaks existing behaviour fails even
if every hidden check passes. The documented task loop (`stack run test`, `dev.py test`) is
recorded but not gated, because Stack 0.1.22's `run` gives custom services an unreachable URL
(R1). Gating on it would fail every Stack trial on a product defect.

`run.py validate` proves all of this through the same `grade` function that scores agent
work, in fresh runtimes, and records cleanup and global-state receipts. The starting code
must keep the smoke test green and fail its designated hidden check; the reference must pass
both gates.

Graders, references and hidden data live outside the agent's workspace, which sits under a
`/tmp` root. That is an access convention, not a security boundary: an agent that searches
the host can find this repository. Within it, the grader computes every check from what it
observes (HTTP responses, rows, files, returned values). Nothing a submission prints, returns
as a verdict or does in an exit handler can pass a check. The slug grader reads a random nonce
and its inputs over stdin before the submission loads. The trial re-hashes the benchmark files
after grading. Out of scope: editing the benchmark files undetected, reading the grader's
memory, or tracing it.

Grading uses a new runtime. Before agent teardown, the harness copies the submission once
into read-only `submitted-tree/` under the trial evidence directory, outside the agent
workspace. The patch and file hashes come from that copy, and grading overlays that exact
tree, including deletions. Benchmark-owned, runtime and private-config paths are excluded
from the overlay; symlink targets are never read or copied. The fresh runtime owns writable
copies. The report checks the retained tree and binds the graded overlay manifest to the
capture. The tree stays with the evidence after cleanup. Read-only permissions and separate
paths are a same-host convention, not a sandbox.
Grading ends with one of these outcomes:

| Outcome | Meaning | Trial |
|---|---|---|
| `graded` | Every step ran; both gates decide | scored |
| `startup_failed` | The submitted app did not start, and the reference passed in a fresh control runtime | scored task failure |
| `infrastructure_failed` | The submitted app did not start, and neither did the reference | invalid |
| `setup_failed` | Installing locked tools failed three times | invalid |
| `harness_error` | Anything else | invalid |

A failed start can still be environmental if the environment recovers before the control
runs; the control makes that unlikely, not impossible.

The pilot bank is three synthetic tasks: a migration plus cache fix (Python, PostgreSQL,
Redis), an API and storage fix (Node), and a tools-only library fix (Node). That is not
enough for product-wide claims. **Not implemented as agent tasks:**

- concurrent worktrees and handoff between them,
- service-failure recovery,
- watched source changes,
- lease expiry and gc,
- secrets,
- noninteractive hosted bootstrap,
- workspace disposal.

Where the hands-on sessions found defects in those areas, `regressions.py` covers them as
scripted product checks instead. The full coverage table is in
[SYNTHESIS.md](../eval/agentdx-20261020/SYNTHESIS.md#coverage).

## Trials, transcripts and review

`run.py trial` saves, per trial:

- the prompt, and full agent stdout/stderr with timing, exit and exact argv receipts,
- the agent's own transcript directory,
- the submission (`submitted-tree/`, `submission.patch`, `submission.json`), diffed against a copy of the
  starting commit kept outside the checkout, so commits, rewritten history, deletions and
  ignored files are all included,
- what was left running at agent exit (`left-at-agent-exit.json`),
- each cleanup (`agent-cleanup.json`, `final-cleanup.json`) and the grade (`grade.json`),
  with every step receipt and the grader's JSON,
- global state before and after (`globals-before.json`, `globals-after.json`), and the
  benchmark file hashes after grading (`fingerprints-after.json`),
- a summary with the hash of every evidence file.

Retries within the budget are agent behavior. A rerun after a harness fault needs a new
results directory, linked by hand to the original; receipts are never overwritten.

A human reviewer fills `review.json` from `run.py review-template`. The report rejects a
review whose:

- trial, summary or transcript hashes, model, harness or requested options differ from the evidence and plan,
- observed model or harness version is missing (the template leaves both empty), or, for a
  plan with options, `observed_options` from the transcript differ from the plan,
- interventions are malformed, outside the agent's time window, or recorded although the plan has no human channel,
- token or cost figures are not finite nonnegative numbers (token counts integers), or their
  source is not a file in the trial directory named by path and sha256.

Tokens and cost are `null` unless the provider reports them; nothing is estimated.

## Ownership and cleanup

Cleanup runs through the arm's own `down`; in the Stack arm the harness then stops the
trial root's Pitchfork supervisor. Afterwards the harness looks for the arm's recorded PIDs
and ports, and for any process whose command line, working directory or an open file is under
the trial root. That last check catches failed starts that recorded nothing. A process that
detached with none of those under the root would be missed, so the record means "none
observed", not "proven none". Leftovers are recorded as a cleanup failure, then stopped by
exact PID. Before every signal, the SIGKILL escalation included, the harness rechecks the
start time, the command line and that the process is still under the root. On Linux a pidfd
pins the process between check and signal; on macOS a few microseconds remain. Nothing is
ever signalled by name.

When a command's budget expires, or the run is cancelled, the harness stops the command's
process group and every descendant it can trace through parent PIDs. That includes children
that ignore SIGTERM, outlive their leader or leave its group. It uses the same per-process
identity check. Once the leader is reaped, a group led by a different process is treated as
someone else's.

An `exec` transition keeps the start identity. The harness follows the new command only
while the command tree still attributes it. Otherwise it records an unresolved process,
rather than reporting no survivors or signalling an uncertain identity. PID replacements
are excluded. Every invoked command preserves this uncertainty, including observation-only
task loops, down and supervisor stop. Any recorded survivor or unresolved identity invalidates
the trial with no verified or autonomous credit. A later empty root scan cannot clear it.
Stopped-command observations must include all four lists: terminated, killed, survivors and
unresolved. These checks retain the macOS clock-resolution and check-to-signal limits.

Grade, startup-control and teardown `command_cleanup` maps contain only uncertain stop
observations from their own invoked commands. The report applies one consistency check to
all of them: typed entries, matching command roles and exact agreement with the individual
receipts. An absent map is treated as empty for older clean evidence. It cannot hide an
uncertain receipt. A map that records uncertainty while the command receipt is clean is
contradictory and invalidates the trial too.

External cancellation is terminal for a trial. `invoke` records it and re-raises it, so any
invoked receipt containing `cancelled` makes the row invalid even if the summary claims
normal completion. This includes observation-only loops, cleanup, retries and reference
controls. New receipts record `process_started`; an explicit false permits cancellation
before launch without a stop observation. A started cancelled command requires all four
stop lists. A clean stop never makes a cancelled trial scored or autonomous. Budget timeout
remains separate: it requires a stop observation and follows the ordinary timeout and task
scoring rules when cleanup is certain.

A trial whose arm teardown needed the harness to reclaim processes is valid but not
autonomous. One where owned processes outlived every cleanup, or where a grading runtime
left any, is invalid.

The latest cleanup must establish recovery. An earlier successful reclaim cannot excuse
a later observation with survivors. A recorded final cleanup is required and compared with
its summary independently. Residue observations require all three typed lists: PIDs, ports
and owned processes. Missing lists or JSON `null` never mean clean. These rules apply to
grading and startup controls too. Reclaim after an empty observation is an impossible sequence and is rejected.
A grading or control runtime that required reclaim remains invalid even after recovery.
A nonzero down with no leak remains a product observation, rather than a task failure.

Global receipts hash the user's mise, Pitchfork and Stack config and state files, and list
the user's mise installs. Another program on the host can change those too, so a difference
is reported with its paths and makes the run invalid until inspected.

Disk use after `down` (root, service data, tool installs) and Stack's port reservations are
recorded per grading run. They describe lifecycle cost and are not scored.

## Reporting

`run.py report` re-derives every verdict from the trial's hashed records and never from the
summary's own booleans. Every command is bound to its expected role/stem and exact
stdout/stderr files through the receipt's output digests, including the startup control.
The verifier cannot select another command's output. A summary that contradicts its records,
a missing, changed or unhashed evidence file, or an agent receipt that is malformed, shows a
different argv from the plan or
falls outside the trial window makes the trial invalid. All command roles require valid
timestamps within the trial, including observation-only commands and cleanup. It reports, per trial:

- status: `passed`, `failed`, `timed_out`, `invalid`, `unreviewed`, `review_invalid` or `not_run`,
- grading outcome, verified task, autonomous, left running, whether the arm teardown was clean,
- agent seconds (from the agent receipt) and startup seconds (from the grading receipts),
- review counts, usage with its source, and observed options.

Startup metrics require receipts for each step reachable on the grading outcome, derived
from the arm's commands. All setup retry durations count. Durations and start timestamps
must be bounded nonnegative integers, in the trial window and in step order. Missing,
wrong-typed, infinite or negative values invalidate the row and leave the metric missing.
Tools-only tasks have no `up`; a setup failure needs no later startup or verifier receipt.
A failed submitted startup still needs a complete passing reference control. Corrupt trial
records become invalid rows without removing any other assignment from the report.

Per arm it gives counts, including scored startup failures, matched pairs with both arms
scored, and discordant pairs. A `startup_failed` trial is scored, so it stays in its pair.
A report is `publishable` only when every planned trial is scored and the plan is an
`agent_experiment` (never a `harness_selftest`).

**Not implemented:** confidence intervals, a task-level bootstrap, or a significance test.
Five pairs per cell is a diagnostic pilot. Choose the sample size for any claim from the
pilot's variance, and resample whole tasks rather than retries. Do not turn a pilot into a
speedup claim, and do not read Opus-vs-Astra differences as Stack's effect.

Correcting a task or grader changes the benchmark hashes, so old plans stop running. Rerun
both arms, and keep the original failed evidence.

## Method references

- [Anthropic, Demystifying evals for AI agents](https://www.anthropic.com/engineering/demystifying-evals-for-ai-agents):
  tasks, trials, transcripts and outcomes kept separate; validated graders; reviewed failures.
- [SWE-bench evaluation harness](https://www.swebench.com/SWE-bench/reference/harness/):
  reproducible environments; submitted patches tested independently; reference patches validated.
- [Anthropic, Quantifying infrastructure noise](https://www.anthropic.com/engineering/infrastructure-noise):
  infrastructure recorded and controlled as an experimental variable.
