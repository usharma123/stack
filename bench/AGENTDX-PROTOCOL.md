# AgentDX benchmark protocol

Status: active pilot protocol. The harness implements everything below unless a line is
marked **not implemented**. No real agent experiment has been run under it yet. The two
2026-10-20 hands-on sessions (Opus, Astra) are discovery evidence and stay separate.

## Question and primary outcome

Does Stack let a coding agent finish the same development task with less human help than
the project's existing setup?

Primary outcome: **autonomous completion**. That is the share of assigned trials that meet
all of the following:

- an independently verified task result,
- a valid human transcript review recording no help,
- nothing the agent started still running when it stopped.

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
source, tool and service versions, grader, locale and budget in two arms:

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

- tasks, models, harness name, options and the agent command (immutable once planned),
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

`run.py validate` proves all of this through the same `grade` function that scores agent
work, in fresh runtimes, and records cleanup and global-state receipts.

Graders, references and hidden data live outside the agent's workspace, which sits under a
`/tmp` root. That is an access convention, not a security boundary: an agent that searches
the host can find this repository.

Grading uses a new runtime. It overlays the submitted files except benchmark-owned files and
runtime directories, then lets the harness run setup and startup. Setup is retried twice
because it only installs locked artifacts; a setup failure that survives the retries makes
the trial invalid (infrastructure). A submission whose app does not start fails the task.

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

- the prompt, and full agent stdout/stderr with timing and exit receipts,
- the agent's own transcript directory, and the diff against the starting commit,
- what was left running at agent exit,
- cleanup receipts, and grading receipts with the grader's JSON,
- a summary with the hash of every evidence file.

Retries within the budget are agent behavior. A rerun after a harness fault needs a new
results directory, linked by hand to the original; receipts are never overwritten.

A human reviewer fills `review.json` from `run.py review-template`. The report rejects a
review whose:

- trial, summary or transcript hashes, model, harness or options differ from the evidence and plan,
- reviewer or harness version is missing,
- interventions are malformed, outside the agent's time window, or recorded although the plan has no human channel,
- token or cost figures lack a stated source.

Tokens and cost are `null` unless the provider reports them.

## Ownership and cleanup

Cleanup runs through the arm's own `down`; in the Stack arm the harness then stops the
trial root's Pitchfork supervisor. Afterwards the harness looks for the arm's recorded PIDs
and ports, and for any process whose command line, working directory or an open file is under
the trial root. That last check catches failed starts that recorded nothing. A process that
detached with none of those under the root would be missed, so the record means "none
observed", not "proven none". Leftovers are recorded as a cleanup failure, then stopped by
exact PID after rechecking the start time and command. Nothing is ever signalled by name.

Global receipts hash the user's mise, Pitchfork and Stack config and state files, and list
the user's mise installs. Another program on the host can change those too, so a difference
is reported with its paths and makes the run invalid until inspected.

Disk use after `down` (root, service data, tool installs) and Stack's port reservations are
recorded per grading run. They describe lifecycle cost and are not scored.

## Reporting

`run.py report` reports, per trial:

- status: `passed`, `failed`, `timed_out`, `invalid`, `unreviewed`, `review_invalid` or `not_run`,
- verified task, autonomous, left running, agent seconds, startup seconds,
- review counts.

Per arm it gives counts, matched pairs with both arms scored, and discordant pairs. A report
is `publishable` only when every planned trial is scored and the plan is an
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
