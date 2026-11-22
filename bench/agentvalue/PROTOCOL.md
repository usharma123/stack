# Stack agent-value benchmark: protocol

Status: **version 2**, fixed on 2026-11-19 before any version 2 trial. Version 1 (three agents, no
faults) is the text up to [Version 2](#version-2-fault-injection-five-agents-the-fixed-stack-build);
its first baseline is in [REPORT.md](REPORT.md). Version 2 adds to it and replaces only what its
section says it replaces. Every change to scoring, gates or analysis is a separate commit that says
why, listed under [Changes](#changes) with its commit.

The research behind these choices is in [RESEARCH.md](RESEARCH.md). The harness is in `harness/`.
The commands are in [README.md](README.md).

## Question

Several coding agents work at once on one repository, each in its own git worktree, against a
Python app with PostgreSQL and Redis. Do they reach verified, clean, isolated results more often,
or with less effort, when the repository uses Stack than when it uses the setup a competent team
writes for itself?

This is not "Stack versus bare worktrees". The research found that bare worktrees with one shared
database are a straw man, because every serious tool tells teams to add a setup layer
(RESEARCH.md §2–§4). The baseline is therefore a per-worktree setup script that already isolates
ports and data.

### Expected result, stated in advance

Against this baseline, cross-worktree contamination should be close to zero **in both arms**,
because both give each worktree its own services. If it is, the isolation gate shows no difference.
In that case any value Stack shows must come from somewhere else:

- task success,
- teardown that the agent actually completes,
- time,
- tool calls, errors and cost.

The README claim "none of them will touch another's database" is tested here only in the sense
that a failure would show up. A comparison that could show Stack preventing contamination needs a
weaker baseline: a naive arm with one shared database. That arm is not part of this protocol.

## Design

### Repository and tasks

`fixture/` is a small invoice API:

- Python 3.13, with code that needs 3.13 (`copy.replace`), so the pinned tools matter.
- PostgreSQL 17.11, with SQL migrations recorded in `schema_migrations` and seeded demo data.
- Redis 7.4.7, used both as a cache and as a job queue.
- A worker that runs the queued jobs.
- A pytest suite and a smoke test that talk to the running services.

The harness builds a new repository from it for every run, with one commit `Starting point`. That
history contains no grader, reference or benchmark file.

Three tasks run **at the same time**, one per agent and worktree (`tasks.json`):

| Task | Branch | What it exercises |
|---|---|---|
| `invoice-notes` | `feature/invoice-notes` | An additive migration, an API change and cache invalidation |
| `pay-receipts` | `feature/pay-receipts` | A queued job, the worker, and idempotency under at-least-once delivery |
| `customer-table` | `feature/customer-table` | A destructive migration (column dropped, data moved) and a seed change. If it ran against another worktree's database or the main checkout's, it would break that app |

Every task needs the agent's own services running to develop and test it.

### The bystander

Before any agent starts, the harness starts the main checkout's services with the arm's own
command. This stands for a developer's running environment. The harness then plants a canary:
invoice 9001 and a Redis key. No task has any reason to touch the main checkout.

### Worktrees and provisioning

The harness creates the three worktrees with `git worktree add -b <branch>`, as an orchestrator
does. Each agent then sets up its own worktree by following the README. The benchmark therefore
measures provisioning by the agent: usability as well as isolation. Nothing is pre-started in any
worktree.

All three agents launch at the same moment, which stresses setup races (RESEARCH.md, R14).

### Arms

**Stack.** The repository contains `arms/stack/stack.toml` and a `stack.lock` compiled by the run.
The harness copies the binary built from this worktree onto PATH as `stack`.

- Services: preset `postgres` and `redis`; the `api` (identity probe, `setup` runs `migrate.py`);
  the `worker`.
- Tasks: `migrate`, `seed` (an init task, run once per new database), `test`, `smoke`.
- The worker serves an optional health port when `WORKER_HEALTH_PORT` is set. The shared README
  states that contract, and both arms use it. It exists because Stack verifies every service by
  connecting to its port, and this build has no way to declare a service that does not listen.
  That is a finding about Stack, recorded in the report.

**Baseline.** The repository contains `arms/baseline/mise.toml`, which pins the same versions with
the same mise backends Stack locks (`conda:postgresql`, `conda:redis-server`), and
`arms/baseline/dev.py`. `dev.py` is the per-worktree setup layer the research describes
(RESEARCH.md §4):

- ports from a machine-wide registry under a lock: a 10-port slot from a hash of the worktree path,
  moving to the next slot that no other worktree holds and nothing else listens on;
- a lock per worktree for `up`, `restart` and `down`;
- an `up` that resumes a partial start, keeping what runs and starting the rest, and redoes an
  interrupted `initdb`;
- its own PostgreSQL (`initdb -E UTF8`), Redis, API and worker per worktree, with data under `.dev/`;
- migrations on every `up` and `restart` (as Stack's `setup` runs them), and seeding once per new
  database (the same rule as Stack's init task);
- an identity check on the API and a health check on the worker, `status` and `env` output, `exec`,
  `test`, `smoke`, `migrate`, `seed`, `restart`, `logs`. Commands that need only the database work
  while the API is down;
- `down`, which stops each app's whole process group and confirms only when every recorded process
  is gone and its port is closed; `down --purge` also releases the ports.

A `stack` stub on PATH answers "command not found", so a host-wide Stack cannot leak into this arm.

**Why this baseline and not another.** Docker Compose per worktree is common, but Docker Desktop is
not running on this host. On macOS it would also add a virtual machine to every start, which
confounds "Stack" with "native processes versus a VM" (critic item B1). A shared Postgres with one
database per worktree is cheaper but weaker. The native per-worktree script is the strongest
baseline that runs here.

The same author wrote both arms, so there is an allegiance risk. The baseline is therefore reviewed
independently before the first paired trial, with instructions to look for sandbagging (see
[Review](#review)).

**What both arms share:**

- the same prompt;
- a README with the same project section and a "Running it in this worktree" section that lists the
  same operations in the same order: up, status, exec/env, test/smoke/migrate/seed, restart, logs,
  down. The sections are 252 words (Stack, which also names `--json` and MCP) and 229 words
  (baseline);
- the same pinned versions and the same warm download cache;
- the same locale (`en_US.UTF-8`);
- the same grader;
- the same machine, one arm at a time.

### Agent

| | |
|---|---|
| Harness | Claude Code 2.1.327, headless: `claude -p <prompt> --model claude-sonnet-5-5 --effort high --output-format stream-json --verbose --safe-mode --strict-mcp-config --no-session-persistence --disable-slash-commands --permission-mode bypassPermissions --tools Bash,Read,Edit,Write --max-budget-usd 8` |
| Model | `claude-sonnet-5-5`, effort `high`, in both arms |
| Budget | 1800 s wall-clock per agent and US$8 per agent (the CLI's own cap) |
| Isolation of agent state | `--safe-mode` turns off user CLAUDE.md, memory, plugins, hooks and skills. Sessions are not persisted, `DISABLE_AUTOUPDATER=1` is set, and the CLI version is pinned and recorded from each trace |
| Human channel | None: stdin is closed. Human interventions are therefore 0 by construction and are reported as such, not measured |

Why Sonnet 5.5 and not Opus 5.5:

- In the earlier single-agent pilot, Opus passed every task, which leaves no room to see a
  difference.
- Sonnet is a common choice for running several agents at once.
- At half Opus 5.5's price ($2/$10 versus $4/$20 per million tokens), it buys more repetitions.

An Opus replication is a later, separate run. The model never differs between arms within a plan.

Agents can see the host filesystem, including this repository; there is no sandbox. Traces are
scanned for any reference to the benchmark's own files (`benchmark_access`). Any hit is reported
with its trace and the trial is reviewed by hand.

## Trial flow

One run is one arm with all three agents. The harness (`harness/run.py`, function `run_arm`) does
these steps:

1. **Machine.** A fresh root `/tmp/avr<pair><arm>-*`, with a short path because of Unix socket path
   limits. The machine-level stores for Stack, mise, XDG and Pitchfork live under `root/m` and are
   shared by the bystander and the three agents, as on one developer machine. Downloads and
   installs come from the shared warm cache `~/.cache/stack-agentvalue/warm`.
2. **Repository and bystander.** Build the repository, start the bystander, plant the canary, and
   record its fingerprint and its processes.
3. **Worktrees.** Create the three worktrees, then record the size of `root/m`.
4. **Agents.** Start the 1 s sampler, then launch all agents. When an agent exits, or is stopped at
   its budget, record at once everything still attributed to its worktree.
5. **Checks.** After the last agent:
   - wait 5 s for processes an agent signalled at the end to finish exiting, then stop the sampler;
   - check the bystander;
   - stop what the agents left;
   - capture each submission;
   - read every worktree's `schema_migrations`;
   - grade each submission;
   - remove each worktree with the arm's documented procedure and record what is left;
   - stop the bystander;
   - scan the whole root.

   A run is invalid if any process under the root survives every cleanup.

Arms in a pair run one after the other, never at the same time.

### Attribution of processes (both arms, one rule)

Every process the harness starts for a worktree carries `AGENTVALUE_WT=<worktree>` in its
environment, and every descendant inherits it. A process belongs to a worktree if any of these
holds:

- it carries that marker;
- its command line, working directory or any open file is under the worktree;
- its command line, working directory or any open file is under that checkout's own state
  directories. For Stack these are `state/data/<key>` and `state/supervisors/<key>`, as listed by
  `stack ps`. The baseline keeps everything under the worktree.

Redis overwrites its environment area, so the paths are needed as well as the marker. Only the
marker's value is ever read from another process's environment, and it is never stored.

## Measures and how each is detected

All per-agent measures come from retained records (`run.json`, the trace, `grade.json`). The agent's
final message never decides anything.

| Measure | Definition and detector |
|---|---|
| **Task success** | `harness/grade.py`, the same for both arms. The grader starts its own pinned PostgreSQL and Redis on private ports and runs the submitted code twice. **fresh**: an empty database, the submitted `migrate.py` and `seed.py`, the API and worker, then the *original* smoke test and *original* API tests. **upgrade**: the *starting* code's migrations and seed, then hidden data the agent never sees (non-ASCII names, duplicate customer names, an existing receipt), then the submitted migrations, API and worker, then the task's hidden checks (`hidden/verify.py`). Success needs both runs to complete, the smoke and original tests to pass, and every required check to be true. Benchmark-owned files (`smoke.py`, `pyproject.toml`, `uv.lock`, arm files, README) always come from the starting code |
| **Teardown gate** (per agent) | Observed by a thread per agent as soon as its CLI exits (the delay is recorded): no live process attributed to its worktree, and no port listened on by one. Ownership paths are taken fresh at that moment. Processes of the agent CLI's own binary are listed separately and do not count. Anything recorded fails the gate, whether or not the arm's `down` would later stop it |
| **Isolation gate** (per agent) | No contamination event names this agent as the culprit, and the run has no contamination event without a culprit. The events: (a) a migration from an agent's branch recorded in the main checkout's `schema_migrations`; (b) any other change to the bystander fingerprint (columns, invoice and receipt contents, Redis keys and values; PostgreSQL write counters are recorded but never gate, because statistics flushes move them); (c) a durable bystander process (postmaster, Redis, the API, the worker, Stack's supervisor) gone at the end (pid and start time); (d) a migration from one agent's branch in another worktree's database, read by starting a private postgres on that database after the agents finish; (e) the main checkout's canary row, matched by its token, in a worktree database; (f) a TCP connection, seen by the 1 s sampler, from a process of one worktree to a loopback port another worktree's process listens on (Stack's own `ps`/`status` probes excluded). Culprit for (a) and (d): the agent whose branch has that migration file. For (c): the agents whose broad kill commands name the lost process's program. For (f): the client's worktree. A worktree database that exists but cannot be read makes the run invalid. A migration found in a database that matches no branch's files (a renamed migration, say) is listed for review and does not gate. A gap between samples can hide a short connection, so (f) is evidence of access, never proof of its absence |
| **Gated success** (primary) | Task success **and** teardown gate **and** isolation gate **and** a clean CLI exit (exit 0 and a `success` result event) |
| **Episode success** | All three agents of a run have gated success: a system-level outcome, analogous to pass^k |
| **Time to a verified result** | Agent wall-clock seconds from launch to CLI exit (monotonic clock), reported for gated successes only. It includes the agent's own provisioning of its worktree, and excludes the harness's setup and grading. Failures are counted, never imputed |
| **Compliance** (secondary) | Whether the agent committed on its branch and ran the tests (`stack run test`, `dev.py test` or `pytest` in its commands). Reported, never gated: grading uses the whole worktree |
| **Tool calls, turns, retries** | From the stream-json trace: `tool_use` blocks (all, and Bash), `num_turns`, tool results with `is_error` (the proxy for retries), and tool results matching an environment-error pattern (address in use, connection refused, wrong database or role) |
| **Broad kill commands** | Bash commands matching `pkill`, `killall`, `fuser -k`, `kill -9`, `lsof -t … | xargs kill`, or `kill $(…)`. Counted; collateral damage is proven only through event (c) |
| **Cost and tokens** | The CLI's `result` event: `total_cost_usd` (list price, not a bill) and usage |
| **Used its own database** | Positive evidence that the agent really used its services: its worktree database exists and has its own new migration applied. Not a gate; a guard against "passed the gates by never starting anything" |
| **Human interventions** | 0 by construction (no channel); reported, not measured |
| **Recovery of a dirty checkout** (per agent, not a gate) | Right after the agents, before any harness cleanup, the arm's documented `down` runs in each worktree exactly as its agent left it. Recorded: what the agent left, and what is still attributed after `down`. Only then does the harness stop anything left, which is recorded separately |
| **Disposal residue** (per arm) | After each worktree's documented removal (Stack: `stack down`, `git worktree remove`, then `stack gc --reclaim-data`; baseline: `dev.py down`, `git worktree remove`), record: processes still attributed, ports agents listened on that still listen, worktree directories left, and the growth of `root/m` with a directory listing. Service data (a non-empty PostgreSQL or Redis data directory of a removed worktree, located through the arm's own records taken before removal) left behind fails the arm's lifecycle check. Metadata records under 1 MiB are listed as observations |
| **Benchmark access** | Tool inputs that mention this repository, `bench/agentvalue`, the grader's directory, `oracles/` or `hidden/`. Any hit is reported with the trace |

### Validity

- A run is **invalid** when any of these happens: a harness error, a process surviving every
  cleanup, a grader that cannot start its own services, or the bystander failing to start. Its three
  agent slots stay in the denominator as invalid, never as successes or failures.
- An agent whose CLI ends with an API or infrastructure error stays a scored failure. It is listed
  separately, and two in one run stop the experiment for investigation.
- A trial is never rerun into the same results directory. A rerun after a harness fault goes into a
  new directory, is linked in the report, and keeps the original.

## Plan, order, cost

- **Pilot:** one pair (two runs, six agent sessions), to shake out harness defects. It is reported
  separately. Pilot data is pooled into the baseline only if no benchmark file changed afterwards,
  which `run.py pair` enforces by refusing a changed plan.
- **Baseline:** five pairs (ten runs, 30 agent sessions).
  - The arm that goes first in pair 0 comes from a seeded coin flip. Later pairs alternate
    (AB, BA, AB, …), so each arm goes first in two or three of the five pairs.
  - Each pair runs both arms back to back, so time-of-day drift falls inside pairs.
- **Expected cost:**
  - US$0.3–1.5 per agent session, extrapolated from the earlier pilot's Opus costs of US$0.12–0.25
    per small single-agent task, and from these tasks being larger.
  - So about US$2–9 per run, and US$25–60 for pilot plus baseline.
  - The hard ceiling is 36 sessions × US$8 = US$288. If the first pilot run averages more than
    US$3 per session, I stop and report before the baseline.
- **Expected duration:** 10–25 minutes per run, which includes setup, up to 30 minutes of agents,
  about 1 minute of grading and disposal. That is about 3–5 hours in all.

## Analysis (fixed now)

- **Per arm and measure:** k/n with the assigned count, and median, interquartile range, range and
  SD for continuous measures.
- **Per task:** the outcome of every repetition.
- **Paired comparison:**
  - For each pair, the stack − baseline difference in the mean of gated success over the three
    tasks, and the same for task success.
  - A percentile bootstrap over **pairs** (whole runs resampled, never agents, because the agents
    of one run are correlated), plus the list of discordant task pairs.
  - With five pairs this is descriptive: no significance test is claimed, and none of the numbers
    is a speedup claim.
- **Variability:** the spread of each measure across repetitions within an arm, and per task.
- **When a measure is reliable enough to improve against.** All three of these must hold:
  1. Validation shows its detector fires on a planted case and stays quiet on a clean one.
  2. It is not at floor or ceiling in both arms.
  3. Its spread within an arm across the five repetitions is small next to the change we would act
     on. For rates: a change of at least 2 of 15 agent slots must exceed the run-to-run range. For
     times and counts: the IQR is under half the median.

  Measures that fail are reported as not yet usable, with the reason.
- **Order effects:** outcomes are tabulated by position (first or second in the pair). With five
  pairs this can show an order effect but cannot rule one out.

## Review

Before the first paired trial, one independent reviewer (GPT-6.2 Sol, full access) reviews the
whole of `bench/agentvalue/`, in particular:

- whether the baseline is sandbagged;
- whether any gate can pass a bad trial or fail a good one;
- whether the grader can be gamed.

Findings and what was done about each are recorded in REPORT.md. Any fix that touches scoring is a
commit listed below.

## Version 2: fault injection, five agents, the fixed Stack build

### Why

The version 1 baseline hit the ceiling in both arms: 15/15 on task success and every gate. A
benchmark at the ceiling cannot judge a change to Stack. Version 2 makes the episodes harder in
three ways, and leaves the baseline arm, the agent model and its settings exactly as they were.

Version 1 and version 2 results are not compared with each other: the Stack build, the task set
and the concurrency all differ.

### What stays the same

- The fixture app, the grader design, and the three version 1 tasks with their hidden checks.
- The baseline arm: `arms/baseline/dev.py`, `mise.toml` and its README section, unchanged.
- The agent: Claude Code headless, `claude-sonnet-5-5`, effort `high`, the same flags and budgets
  (1800 s and US$8 per session), identical in both arms. The plan pins the CLI's versioned binary.
- The prompt for an ordinary agent, the process attribution rule, the bystander and its canary.
- Every version 1 measure, gate and validity rule, unless a line below replaces it.

### What changes

**1. The Stack build.** The Stack arm uses `fix/exercise-findings` at `a5d4785`, built with
`cargo build --release --locked` from a `git archive` of that commit, outside any checkout
(`~/.cache/stack-agentvalue/stack-a5d4785/`). Release binary sha256:
`b0931a8be40bc65855a1139d10a8f7f66c136e5a564b58cf9994a58f511fdd17`. In this build `stack down`
exits 70 (`stop_unconfirmed`) while a command Stack started for the checkout (`exec`, `run`, MCP)
still runs, and names its PID. The plan records the revision and the hash, and `pair` refuses to
run if the binary's hash differs.

**2. Five agents.** Five agents start at the same moment, one worktree each, beside the main
checkout's running services. Two tasks join the three from version 1:

| Task | Branch | What it exercises |
|---|---|---|
| `invoice-list` | `feature/invoice-list` | A new `GET /invoices` with status and customer filters and id-based paging, and a migration that adds an index |
| `void-invoices` | `feature/void-invoices` | A new status reached through `POST /invoices/<id>/void`, a migration that replaces a check constraint, state rules (409s) and cache invalidation |

Both are validated like the others: the starting code fails, the reference passes, and near-miss
mutants fail the check they should.

The machine budget is recorded per run: cores, memory, the 1-minute load average before the
bystander starts and after the last agent exits, the number of concurrent sessions, and each
session's time and dollar budget. Both arms of a pair run on the same host, one after the other.

**3. Fault injection.** Every pair has one scenario and one victim task, both fixed by the plan and
the same in both arms.

*The trigger, the same in both scenarios and both arms.* The trigger is the first 1 s sample in
which the victim's worktree has all four of its services running and listening: the PostgreSQL
postmaster on the data directory the arm keeps for that worktree (`.dev/postgres` in the baseline,
the directory `stack ps` lists in the Stack arm), its Redis server, its API and its worker (the
worker on its health port, which both arms use). Each must be the arm's own service: listening on
the port the arm recorded for it in that worktree (`dev.py`'s state file, `stack ps`), with the
matching command, and belonging to that worktree. A second PostgreSQL, Redis or API that an agent
starts itself never qualifies. The trigger is an observation that all four listen, the same in both arms, so the fault
does not land in the middle of one arm's start; it is not proof that the arm's `up` command has
returned. The sample time is taken after the sample's process and socket listings, and the 600 s
limit below runs from the victim's launch to that sample.

*Scenario `agent-kill`.*

1. All five agents start.
2. 3 s after the trigger, the harness sends SIGKILL to the victim's agent CLI process, and to that
   process only. Whatever tool command it was running is left as it is, as after a crash.
3. 10 s after the kill, the harness starts a fresh agent session in the victim's worktree: the same
   CLI, model, settings and budget, with the recovery prompt below. Nothing delays this launch.
4. The other four agents (the survivors) are not told anything and are not touched.
5. The episode ends when the survivors and the recovery agent have exited or hit their budgets.

*Scenario `service-kill`.*

1. All five agents start.
2. 3 s after the trigger, the harness sends SIGKILL to the victim worktree's postmaster. Nothing
   else is done: the victim's own agent has to notice and cope.
3. The episode ends when all five agents have exited or hit their budgets.

PostgreSQL is the service killed in every `service-kill` pair, because it holds the state.

*The target.* Immediately before the signal the harness checks again that the process is the
planned one: its pid, its start time, its command, and for the postmaster its data directory and
that it still belongs to the victim's worktree; and that the victim's CLI is still running. macOS
gives start times to the second and has no signal bound to a process identity, so a pid reused
within that second is a residual risk, which is stated here and not otherwise handled.

*What is recorded at the kill and at the handoff:* the processes of the victim's worktree (listed
in the 2 s before the kill), the tool command the victim's CLI had outstanding (the last command in
its trace with no result yet), and whether a process started by the killed CLI is still running at
the handoff (listed in the 2 s before the recovery agent starts, so the listing cannot delay it).

*A missed fault.* If the victim's CLI exits before the kill is due, if the planned process is gone
or replaced when the kill is due, if any of the four services is no longer the same process
listening on the same port when the kill is due, or if no trigger is seen within 600 s of launch, the fault is recorded as
missed and nothing is injected. A kill sent more than 1 s after it was due, or a recovery launch
more than 1 s late, still counts as injected and is reported as late. The run stays valid for the ordinary measures. Its
fault measures are missing: a missed fault is never a successful fault episode. The pair is run
once more as a whole (both arms, the same scenario, victim and order) into a new results directory,
and the second attempt is the one compared. A second attempt is allowed only when the first
attempt's own records show a missed fault or an invalid run, and once it has been started it
governs: if it misses, is invalid or is incomplete, the pair is missing and the first attempt is
not used. The start of a second attempt is recorded before anything else it does, so an attempt
that dies before writing a result still governs. The report lists every attempt, and counts every session launched in any attempt.

*The recovery prompt* (identical in both arms; `{branch}` and `{instruction}` as in the ordinary
prompt):

> You are taking over from another coding agent. It was working on the task below in this git
> worktree (the current directory, on branch {branch}) and stopped unexpectedly partway through. It
> will not come back. Its edits may be incomplete, and it may have left this worktree's services
> running or half started. Other agents' worktrees, and the main checkout, have services running
> while you work.
>
> Your task:
> {instruction}
>
> Read README.md first: it explains how to run the project and its services in this worktree. Check
> what state the worktree and its services are in, then finish the task. Before you finish, run the
> tests against your worktree's running services and make sure they pass, and commit your work on
> your branch. When you are done, stop everything that is running for this worktree, including
> anything the previous agent left, and leave other worktrees' and the main checkout's services
> alone.

**4. The teardown procedure.** After the episode, the harness tears down every worktree exactly as
it was left, with the arm's documented procedure. Nothing is cleaned by the harness first.

- **Stack:** `stack down`. If it exits 70 with `stop_unconfirmed` and lists commands of this
  checkout (details with `kind: "command"`, not `another_checkout`), the harness does what the hint
  says: it stops those PIDs (SIGTERM, 10 s, then SIGKILL) and runs `stack down` again, for at most
  three rounds. A named PID is signalled only if the harness's own attribution also places it in
  this worktree; otherwise it is recorded and left alone. Then `git worktree remove --force`, and
  after the last worktree `stack gc --reclaim-data`.
- **Baseline:** `python3 dev.py down`, then `git worktree remove --force`.

Two observations are taken per worktree:

- **after `down`** (and Stack's follow-up rounds): every process attributed to the worktree, and
  every port one of them listens on;
- **after removal** (and Stack's `gc`): the worktree directory, and any non-empty PostgreSQL or Redis
  data directory of that worktree, located through the arm's own records taken before removal.

Only after the first observation is recorded does the harness stop anything left, and that is
recorded separately. A process the harness had to stop counts as left, at whichever observation it
was found.

Every process that is attributed to the worktree and tied to the run counts in these observations
and in the teardown gate at an agent's exit, the agent CLI's own helper processes included. Tied is
the test used for signalling (Validity, "What the harness may signal"): the run's marker, descent
from a process the harness launched, or an arm service identified by its command and its data or
state directory, or placement in the run (its working directory is inside the run's root and it
started after the run began, which catches a detached command started with a clean environment). A
process carrying the marker of one worktree is attributed to that worktree only, whatever files of
another worktree it has open. A process with no readable marker is attributed by its working
directory before any open file. A process that only has a file of the worktree open, such as an
operator following a log, is listed beside the observation and does not count. The limit: a
leftover with no marker, no recorded parent, a working directory outside the run's root and no arm
service shape is listed, not gated. The data directories checked are the union of the arm's own records and the data
directories of every PostgreSQL and Redis the sampler saw running for that worktree, so a missing
record cannot hide one. If the harness cannot establish the inventory (the arm's records cannot be
read, or a process or open-file listing fails or comes back empty), the observation is unknown and
the run is invalid; an unreadable inventory never passes.

The follow-up rounds are Stack's documented operator procedure, and the baseline documents none.
So two results are reported for every worktree, in both arms: **clean after the first `down`**, and
**clean after the documented procedure** (the gate). The time and the signals the follow-up cost
are reported with it. A worktree that is clean only after the follow-up is not described as a
success of a single `down`.

The sampler keeps running through the teardown, and the bystander is checked again after the last
worktree is removed and before its own `down`.

### Gates in version 2

The version 1 gates still apply to every ordinary session: task success, teardown at the agent's
exit, isolation, and a clean CLI exit. Version 2 adds these.

| Gate | Definition and detector |
|---|---|
| **Teardown residue** (per worktree; the arm's lifecycle) | After the teardown procedure: no process attributed to the worktree and no port one listens on (first observation), and no worktree directory or service data directory (second observation). Reported for every worktree, and separately for the fault victim's worktree, where it is the gate the fault scenarios were built for |
| **Others untouched** (per run) | The run has no contamination event at all, from the launch of the agents to the end of the teardown: none of version 1's events (a) to (f), none of event (g), and no teardown collateral (below). The injected fault itself is never an event |
| **Teardown collateral** | While the harness tears down worktree X, a durable service process of another worktree that has not been torn down yet, or of the main checkout, disappears, or the bystander's fingerprint changes. Decided from the harness's own listing of durable processes taken just before and just after each worktree's procedure, and from the bystander check after the last removal; the 1 s sampler is too coarse for it. A process lost between one worktree's teardown and the next is listed for review and does not gate, and so is a loss of a process that its own worktree's session had itself stopped in the 30 s before the episode ended (its arm's `down`, a `restart` covering that service, or a kill identifying that process): a slow exit of its own, under the own-command rule stated for event (g). A recent command that did not target the lost process, or that started before the lost process existed, excuses nothing. A bystander that was already unreadable at the end of the episode is an episode observation, not teardown collateral. Attributed to the arm, not to an agent |
| **Isolation, event (g): a sibling's service lost** | A durable service process of worktree X (postmaster, Redis, the API, the worker, Stack's supervisor) disappears between two samples, and another session ran a kill-like command (the version 1 pattern) that **identifies that process**: its pid is an operand of the kill command itself (a number elsewhere in the same shell text does not count), its `pkill`/`killall` pattern matches the lost process's command line, or it kills by a port that process listened on. Only a plain, unconditional invocation gates: a kill behind `&&`, `||` or another condition, one with flags the harness does not model (`-s`, `pkill -x`), or any other form it cannot read with certainty is listed for review. The invocation must also have completed without an error result: a kill that returned an error (for example `No such process`), or one naming several targets, is listed for review, and signal 0 signals nothing and is ignored. A pattern or port form signals every process it matches, so each matching durable process lost in the window that already existed when the command returned is an event of that one command; this is the contamination the gate exists for, not an ambiguity. A matching process that started after the command returned, or in the same second, cannot be shown to have been signalled by it and is listed for review. A command counts as running from the arrival of its trace line to the arrival of its result, and that interval must overlap the interval between the two samples, widened by 1 s. A command with no result, because its CLI was killed, may have run at any time until the episode ends; a loss it could explain is listed for review, not gated. The culprit is the session that ran it. Timing alone never gates. If X's own session, in the 10 s before, ran its arm's `down`, a `restart` that covers the lost service, or a kill that identifies the same process, the event is ambiguous: listed for review, not gated (an `up` is not such a command: it does not stop a running service). The rule for an own command is the same here and in teardown collateral. It **excuses** a loss only if the lost process already existed when the command started and the command certainly ran: a statement of its own, or a member of an `&&` chain that is the last statement of the command (so the command's result is the chain's), with a result that is not an error. A chain member inside a longer block may have run, because the block's result does not show it, and so may any member of a statement that contains `||`, which can mask a failure. A command that only reads (`lsof`, `pgrep`, `ps`) stops nothing and is never an own command, and `xargs` is a kill only when the command it runs is one of the modelled kills. **The classifier only sorts.** Every gating event, every loss sent to review and every excused loss in a trial is reported with the command text and its trace location and is read by hand before a result is used, because no reading of shell text is complete. It **may have run** if it sits behind `||` or another condition, returned an error (a `down` or `restart` can fail after stopping a service), or has no result: the loss is then listed for review and gates nothing. A command that started before the lost process did, or that does not target it, excuses nothing. A loss that no identifying command explains is listed for review and does not gate, because an app can exit through its own agent's code. A loss counts whether or not X has a session running at that moment, which covers the time between an injected CLI kill and the recovery agent. In `service-kill`, only the injected postmaster and its own PostgreSQL child processes are exempt. Every durable service is resolved to its worktree through its open files, not only those holding a socket, and the report shows any gap between samples longer than 3 s |
| **Cross-worktree connections, event (f), in `agent-kill`** | Each connection event carries its time and the client process's role marker. One made by a process the killed CLI started fails the run-level gate and is not charged to the recovery session; one made by a process of the recovery session is charged to it. A client with no role marker is never assigned to a session by time alone: the event fails the run-level gate only |
| **Recovery session** (agent-kill) | The recovery agent is scored like any session: hidden checks on the worktree's final tree, teardown at its exit (everything attributed to the worktree, whoever started it), isolation, and a clean CLI exit |

The killed agent's own session is not scored: the harness ended it. In the `agent-kill` scenario the
victim's slot is the recovery session.

**Truthful `down`** is reported next to the residue gate. A false confirmation is the arm's first
`down` exiting 0 while the first observation still finds a process of that worktree. It is counted
per arm over the `down` runs that exited 0; runs that exited nonzero or timed out are counted
separately. It is not a separate gate, because the residue gate already fails in that case.

### Measures in version 2

| Measure | Definition |
|---|---|
| **Gated success per slot** (primary) | Five slots per run. Ordinary slots as in version 1. In `agent-kill` the victim's slot is the recovery session |
| **Survivors finish** | Gated success among the four agents that were not the victim |
| **Recovery** (agent-kill) | Recovery gated success, and for the recovery session: seconds from its launch to its exit, tool calls, turns, tool errors and cost |
| **Victim copes** (service-kill) | Both: the victim's gated success, and **service restored**: a new postmaster on the worktree's own data directory seen listening after the kill and before the agent exits. Gated success alone is reported as "completed after the injection", because the grader runs its own services and would pass a session that never brought its database back. Reported beside it, as description and not part of the measure: whether the agent started one of the arm's test commands after the kill and its tool result was not an error. A shell command's text cannot prove what ran or how it ended (`… || true`), so this is not scored. The victim's seconds, tool calls, turns and tool errors are reported, with the same task's figures from the same arm and scenario where it was not the victim beside them, as description only |
| **Episode success** | `agent-kill`: four survivors and the recovery session have gated success, the residue gate holds for every worktree, and others are untouched. `service-kill`: all five have gated success, the victim copes, and the same two run-level gates hold. An episode whose fault was missed has no episode result |
| **Cost** | Per session, from the CLI's result event, over every session launched in any attempt, valid or not. A killed CLI writes none, so its spend is unknown: the report gives the number of sessions launched and the number with a cost, and for the pilot's stop rule an unknown cost counts as the highest cost seen in that pilot. If no session of the pilot has a cost, the main run does not start |
| **Fault facts** | Injected or missed; seconds from launch to the kill; what the victim's worktree was running at the kill and at the end of the episode |
| **Teardown** | Per worktree: `down`'s exit status, Stack's follow-up rounds and the PIDs it named, false confirmations, and both observations |
| **Machine budget** | As recorded per run (see "Five agents") |
| Effort measures | Seconds, tool calls, turns, tool errors, environment errors, kill commands and cost per session, as in version 1 |

### Validation before any trial (both arms)

`harness/validate.py` must show each new gate firing on a planted case and staying quiet on a clean
one, in both arms, with scripted agents:

| Planted case | Expected |
|---|---|
| Five clean reference agents, no fault | Every gate holds; nothing left after teardown |
| `agent-kill` on a scripted victim, with a scripted recovery agent that finishes the task | The fault is injected at the planned point; recovery has gated success; the residue gate holds |
| `agent-kill`, and the recovery agent leaves the worktree's services running | The recovery session fails teardown at exit; the arm's `down` then clears the worktree, so the residue gate holds |
| A process the arm does not manage is left in a worktree (started without the arm's tooling) | The residue gate fails for that worktree in both arms |
| `service-kill` on a scripted victim that starts its services again | The postmaster is killed at the planned point; the victim passes; the kill is not counted as contamination |
| `service-kill` on a scripted victim that does not react | The victim fails its task; other agents are unaffected |
| A scripted agent kills another worktree's worker by name while that worktree's agent is running | Event (g), attributed to the killer |
| **A command left running under the arm's `exec` at teardown**: the agent starts a long command through `stack exec` / `dev.py exec`, runs the arm's `down` and exits | Teardown at exit fails in both arms. Stack at `a5d4785`: `down` is not confirmed, names the PID, and the procedure then leaves the worktree clean. Stack at `dc4b980` (the version 1 build, run as a control): `down` confirms while the command runs, a false confirmation, and the residue gate fails. Baseline: `dev.py down` confirms while the command runs, a false confirmation, and the residue gate fails. The baseline is left as it is; this is recorded as its behaviour |

| In every injected case | The listing taken before the kill shows all four services of the victim's worktree, in both arms; a scripted victim whose services come up slowly is still killed only after all four listen |
| `agent-kill` while the killed CLI's tool command keeps running past the recovery launch | The handoff record shows it; a cross-worktree connection it makes fails the run-level gate and is not charged to the recovery session |
| `service-kill` on a scripted victim that finishes its code and never brings its database back | "Completed after the injection" may hold; "victim copes" does not |
| The reviewer's round 2 reproductions | `echo <lost pid>; kill -9 <other pid>` is not event (g). A connection from a client with no role marker during recovery fails the run-level gate and is not charged to the recovery session. A second postmaster in a scratch directory under the victim's worktree does not trigger the fault and is never the target. A staged path `a[1]` beside a clean `a1` is scanned as itself. A credential-like key holding a list is redacted. A trace that cannot be scrubbed is never published |
| A kill command naming a pid that is not the lost process, and a kill naming the lost process while its own agent only ran `up` | The first is not event (g); the second is event (g), attributed to the killer |
| The runner is interrupted before the kill, during the 10 s before the recovery launch, and during that launch | Nothing of the run survives, and no session starts after cleanup has begun |
| A sentinel value planted in a scripted agent's output and files | It is absent from every retained file of a completed run and of an interrupted run |
| The commit scan on staged files with quoted paths, a binary file, a rename, a symbolic link and a commit message, each carrying a sentinel | Every one is reported |
| Teardown of one worktree stops another worktree's service (planted through a scripted arm command) | Teardown collateral, and the run-level gate fails |

The version 1 planted cases (services left running, a migration run against the main checkout, a
kill of the main checkout's worker) and every grader mutant stay in the validation. Mutants added
after the reviews: an `invoice-list` index on `status` alone, or with another column between
`status` and `id`, must fail; a `void-invoices` constraint that also allows any other status
(`refunded`, `archived`, or anything a wider condition lets through, such as a long string) must
fail: the schema must restrict the status to exactly open, paid and void, shown both by the
constraint's structure and by probes of many kinds of value; a redundant extra constraint is
allowed; a `void-invoices` reference that never caches a void invoice must
pass.

### Validity, added rules

- A missed fault does not invalidate a run (see above).
- The marker that records the start of a rerun counts only if it names the same plan digest, pair
  and attempt.
- A run is invalid if the recovery agent cannot be started, if the harness's own kill cannot be
  shown to have hit the planned process, if an inventory for the residue gate cannot be
  established, or if a session's trace reports a model other than the plan's.
- An invalid run is rerun once as a whole pair, like a missed fault. A valid run with a missing
  grade or a missing session is invalid, not a loss.
- A trial plan must match this protocol in full: the Stack binary's sha256 above, the agent
  command, model, effort and budgets of version 1, the fault delays above, a scenario and a victim
  in every pair with the pairs exactly as the plan's seed generates them, the agent CLI pinned to
  its versioned binary, the 1 s sample interval, one pair per scenario for the pilot and five for
  the main run, and the warm cache path the plan records. `pair` checks these before each arm. A session that ends successfully without reporting its model makes the run invalid.
  Self-test plans may differ.
- **What the harness may signal.** Only a process it can positively tie to the run: one carrying
  the run's marker, one descended from a process the harness launched, a service of an arm
  identified by its command together with its data or state directory, or one placed in the run:
  its working directory is inside the run's root and it started after the run began (the root is a
  fresh directory that only the run uses, and nobody works inside it while the run is live: an
  operator's process started there would be taken for the run's). A process that merely has a
  file of the run open, or whose command mentions a path of the run, is listed and never signalled.
  A process group is signalled only when a current member passes that test. Stack's follow-up
  checks, before each signal, that the named process is still placed in that worktree and tied to
  the run. Processes the sampler recorded as descendants are also looked for by identity at the final
  cleanup, not only by path or marker.
- The final process cleanup and its check run whatever else fails, including the publishing of
  records.
- Each worktree's database is checked for foreign migrations in every PostgreSQL data directory
  the arm records or the sampler saw in use for it. If those cannot be reconciled or read, the
  isolation observation is unknown and the run is invalid. The harness starts or stops PostgreSQL
  only on a data directory under the run's root; one seen outside it is recorded and never touched
  (no worktree's database lives there).
- If the runner is interrupted, it first stops anything that could still start a session, then
  cleans up, then checks that nothing of the run is left.
- **The host stays awake and on AC power.** Before each arm, `pair` requires the power source to
  read as AC power; battery power or an unreadable power source refuses the arm. Each run records
  the power source at its start and end. Suspension is measured directly: the run clock is the
  system's uptime clock, which stops while the host sleeps, and the harness reads beside it the
  monotonic clock that keeps counting during sleep, at the start and the end of the run. If the two
  diverge by more than 0.5 s over the run (half the 1 s lateness tolerance of the fault timing), the
  host was suspended, the kill point and every interval are unreliable, and the run is invalid and
  rerun as a whole pair like any other invalid run. The difference between wall time and the run
  clock is recorded as a diagnostic only, since a wall clock can be stepped. Every check that can
  refuse a rerun comes before its start marker is written. If the second arm of a pair is refused
  for power after the first arm ran, or a signal stops `pair` between the arms, the missing arm is
  recorded as an invalid run that never started, so the pair is rerun whole.
- Nothing under `results/` is committed. Raw records, traces and generated reports stay on the
  machine that ran the trials (`/Users/utsavsharma/.t3/worktrees/stack/bench-agent-value-91f22f7a971eac43/bench/agentvalue/results/`), and
  the report carries every table and plan parameter inline.
- **What can reach a record.** Agents, the bystander and every arm command get an environment the
  benchmark builds itself: `PATH`, `SHELL`, `TMPDIR` and `LANG` are fixed benchmark values (the
  `PATH` starts with the run's own bin directory and the pinned agent CLI's directory, then a fixed
  system list that keeps `python3` the interpreter version 1 ran on), and the
  per-run store locations and markers are the harness's own. Only `HOME`, `USER` and `LOGNAME` come
  from the host, because the agent CLI cannot sign in without them; the user name and home path
  also appear in every path. No other host variable reaches an agent, so none can reach a record
  through an agent's environment. The harness never records an environment.
- Nothing is written into the results directory unscrubbed. The sampler's log is staged like the
  traces, and process command lines are scrubbed before they are written anywhere. File names of a
  submitted tree are scrubbed like contents. Scrubbed copies are written to a directory the harness
  owns, never beside the files of a submission. The redactor also covers bearer tokens and the whole
  value of credential-bearing headers: `Authorization` and `Proxy-Authorization` of any scheme
  (every parameter of a Digest header included), `X-API-Key`, `X-Auth-Token`, `Cookie` and
  `Set-Cookie`. A header value that something else redacted only in part is redacted in full, and a
  header's extent is decided on its own line, whatever the surrounding text looks like.
- An agent's trace is written to a staging file under the run's root. It is scrubbed there, and
  only the scrubbed file is moved into the results directory, also when a run is interrupted or
  invalid; a trace that cannot be scrubbed is not published, and the record says so. Submissions
  are captured to staging and published the same way. Binary files and symbolic links of a
  submitted tree are not retained; the record lists them. The redactor covers `KEY=value`, JSON
  keys, and parsed objects whose credential-like key holds a string, a list or a nested object.
- The limit: agents run as the host user and could read a host file and print it. A value with no
  credential-like key next to it cannot be recognised. This is a trusted-agent experiment, as in
  version 1.
- Every commit goes through `harness/commit.py`, which scans every staged blob in full (text,
  binary, renamed files and link targets) and the commit message for credential patterns and for
  the value of any host environment variable outside `HOME`, `USER` and `LOGNAME`, and refuses to
  commit on a hit. Each staged path is read by its exact name from one reading of the index, never
  through a pattern. Values are also matched after JSON string escapes are decoded by a conforming decoder, surrogate
  pairs included. The commit is made from
  the very tree that was scanned, without running hooks, so nothing can be added between the scan
  and the commit. A variable whose value is an existing path is not treated as a secret: the
  worktree's own path is in `PWD` and in this document. The rule covers every commit to this
  repository made for the benchmark; the throwaway repositories the harness builds for agents are
  not committed anywhere.

### Plan, order and cost

- **Pilot:** one pair per scenario (4 runs, 22 sessions), to shake out the harness. Reported
  separately.
- **Main run:** five pairs per scenario (10 pairs, 20 runs, 110 sessions: 100 ordinary and 10
  recovery).
  - Pairs alternate scenarios. Within a scenario, the first arm of its first pair comes from a
    seeded coin flip and later pairs alternate.
  - Within a scenario, the victims are a seeded permutation of the five tasks, so each task is the
    victim once. Both arms of a pair have the same victim.
- **Expected cost:** version 1 sessions cost about US$0.10 each. Expect US$0.10–0.25 at five-way
  concurrency with faults, so about US$3–6 for the pilot and US$11–28 for the main run. The hard
  ceiling is US$8 for each session launched: 132 sessions, plus those of any rerun pair. If the pilot averages more than US$1 per session, stop and
  report before the main run.
- **Expected duration:** 3–5 minutes per run, about 1.5 hours of trials in all.

### Analysis (fixed now)

- Everything is reported **per scenario and per arm**, with k/n and the assigned count, and for
  continuous measures the median, interquartile range and range.
- **Pairs** are compared within a scenario. A pair has the same victim task in both arms, so paired
  differences for recovery and for the victim are within one task.
- **Which sessions a measure covers.** Rates and effort measures cover the scored slots of pairs in
  which both arms' runs are valid. Effort measures are reported twice: over all scored sessions,
  and over sessions with gated success ("to a verified result"). A paired difference for a measure
  uses only tasks for which both arms have a value in that pair. Fault measures (recovery, victim
  copes, episode success, the victim worktree's residue) use only pairs in which the fault was
  injected in both arms.
- **When a measure separates the arms beyond run-to-run spread.** These are descriptive decision
  rules, fixed in advance, not tests. Decided per scenario, and only when the measure has a paired
  value in all five pairs; with fewer it is reported as not decided:
  - *Rates over slots* (25 slots per arm and scenario): the pooled difference is at least 3 slots,
    and no pair's difference has the opposite sign.
  - *Per-run binary measures* (5 per arm and scenario: recovery, episode success, residue gate for
    the victim's worktree, others untouched): the arms differ in at least 2 of the 5 pairs, all in
    the same direction.
  - *Continuous measures* (seconds, tool calls, turns, cost; the recovery and victim figures): the
    per-pair mean differences have the same sign in all five pairs, and the median paired
    difference is larger than half of the larger within-arm interquartile range.

  Anything else is reported as not separated. Five same-signed pairs out of five is a two-sided
  sign-test p of 0.0625, so even a "separated" measure is a descriptive finding, not a
  significance claim.
- A measure at floor or ceiling in both arms is reported as such, as in version 1.
- **If both arms are still at the ceiling** on the gates and on gated success, the report says so
  and recommends which of the two changes held back from this version to try next: a naive
  shared-database arm, or a weaker agent model.
- **What a difference would mean.** Each arm is a whole workflow: its tooling, its start-up
  behaviour, its instructions and its documented teardown procedure. A difference between the arms
  is a difference between those workflows on this host. It cannot be assigned to the Stack binary
  alone.
- Planted validation cases are never mixed into trial results. Where a planted case separates the
  arms by construction (the `exec` case above), the report says it is a planted case.

### Review

The one reviewer session (GPT-6.2 Sol) reviews this amendment and the harness changes before the
first paired trial, and the report before it is handed back. Findings and what was done about each
are recorded in the report. A fix that touches scoring is a commit listed below.

## Changes

On 2026-11-19 the owner rewrote this branch's history to correct commit author emails. File trees,
messages and timestamps did not change, but every commit hash did. The hashes in this log and in the
reports up to and including `a6dda8f` are the original ones, kept as they were recorded. They still
exist locally under `refs/backup/commit-attribution/20261119-120117/original/`, and the map from old
to new hashes is at `/Users/utsavsharma/.t3/commit-attribution/20261119-120117/commit-map.json` on
the machine that ran the trials. Rows added after the rewrite carry current hashes.

| Commit | Change | Reason |
|---|---|---|
| `d1ef3c5` | Protocol fixed | — |
| `c829c39` | Hidden checks: a new required check `existing_api_intact` in every task (PATCH still updates the amount in the database; receipts still list). Pre-existing statuses and receipts are compared at the SQL level. `pay-receipts`: the grader runs the worker itself, proves the job is queued while the worker is held back and that no receipt exists before it runs, replays the captured job with the worker stopped and checks it was consumed, and requires that a 409 or 404 queues nothing. `customer-table`: a unique constraint on the name is required. `invoice-notes`: the note must be stored in the database and the read path must still use the cache. The migration ledger must match the new migration files exactly, and the rerun check covers constraints, indexes and row counts | The independent review reproduced two submissions that passed while wrong (a worker that never issues receipts; a migration that drops the unique name, deletes receipts and breaks PATCH). Each is now a validation mutant that must fail |
| `c829c39` | Gated success also requires a clean CLI exit. Teardown is observed per agent at exit with fresh ownership paths; CLI helpers are excluded. Isolation: write counters no longer gate, kill culprits must name the lost program, the canary is matched by token, unreadable worktree databases invalidate the run, Stack's probes are not cross-worktree traffic, connections must reach a loopback listener. Compliance is reported. The arm's own `down` is tried on the dirty checkout before harness cleanup | Review findings 6–9, 14 and 15: gates that could pass a bad trial or fail a good one |
| `c829c39` | Baseline `dev.py`: a lock-guarded port registry, a per-worktree lock, resumable `up`, migration on `restart`, a worker health check, commands that need only the database, and a `down` that confirms ports are closed. Shared README: the worker health contract | Review findings 1–3: the baseline's recovery and shutdown were weaker than its README promised, which would have handicapped it |
| `c829c39` | The harness never logs a whole environment: addresses are read with a three-variable query, and every recorded output and retained trace is scrubbed of credential-like values. Run-qualified process markers, marker-based cleanup, grader partial-start cleanup, stop rules enforced by `pair`, a host lock, plan digests bound into runs, agent-facing Markdown and this protocol bound into the plan | Review finding 4 (a host credential reached a committed record; removed from the branch by rewriting the protocol commit), 5, 12 and 13 |
| `82731b8` | Round-2 review fixes. Baseline `down` signals only processes proven to be the worktree's (recorded pid and start time, or group members whose command or working directory is inside it); a foreign listener is reported, never killed; the lifecycle lock lives outside `.dev/` and is held through `--purge`; `up` adopts services it started but never recorded and restarts apps that do not answer; `migrate` needs only PostgreSQL. Grader: bounded waits, group stop after leader exit, cleanup in `finally`; the notes cache check proves a cache hit (a database-only change must not show through), the rerun check compares table contents, uniqueness must not be partial, statuses are compared in SQL, payment must be persisted before the worker runs, and jobs must go on a Redis list (the README's contract). Gates: a failed `ps` or `stack ps` raises (the run becomes invalid) instead of reading as "nothing running"; listeners are matched by full endpoint; a killed bystander that makes the fingerprint unreadable is contamination, not an invalid run; independent schema changes are no longer pinned on a migration's owner. Agents inherit no credential-like variable and none of the host harness's (`CLAUDE*`, `T3*`, `ANTHROPIC_*` and similar); the redactor covers bare and JSON key shapes; `arm` runs only self-test plans; the report rejects runs whose plan, pair, arm, model or kind differ; budget stops are not CLI failures; recovery needs a successful `down`; no interval with fewer than three pairs | Round-2 review (P0 1–5, P1 6–10, P2 12). Three new validation mutants reproduce its counterexamples and must fail |
| `c93c85f` (kept in a note beside the results until the last pair) | An externally interrupted pair is rerun whole into the same plan, so both arms stay back to back in time. Its completed half is kept under `results/baseline-1/superseded-interrupted/` and excluded. The decision was made before looking at that half's outcomes | The host's agent server restarted during baseline pair 3 and killed its Stack run before any agent started. The bystander it had started was stopped with Stack's own `down`, and nothing survived |
| `3ac2567` | Deviation recorded, not a scoring change: the agent CLI on the host updated itself from 2.1.327 to 2.1.328 at 21:21, between baseline pairs 2 and 3, outside the harness (`DISABLE_AUTOUPDATER` applies to the child sessions only). Every pair has both arms on one version. New plans pin the agent command to the versioned binary path, so a host update cannot change the CLI under a plan | Found in the traces' init records after the baseline |
| `ad20055` | **Version 2.** Fault injection (`agent-kill` with a recovery agent, `service-kill`), five agents with two new tasks, the Stack arm built from `a5d4785`, the teardown procedure and the teardown-residue, others-untouched, sibling-loss and recovery gates, the planted `exec`-at-teardown check, an allowlisted agent environment and a scan of every commit, and the version 2 plan and analysis | The version 1 baseline was at the ceiling in both arms and could not judge a change to Stack. Fixed before any version 2 trial |
| `5a34d58` | **Results are no longer in git.** The whole `results/` directory is ignored, and the 415 files tracked there until now are removed from the index (history is not rewritten; every file stays on disk). Reports and this protocol carry the tables, sample sizes, variability, deviations, plan parameters and change log inline, and say where the raw results are on the machine that ran them. Version 2 results follow the same rule from the start. The credential scan before each commit stays | Owner's instruction: raw outcomes and traces are retained locally, not in the repository |
| `a2e4452` | Clarifications made while implementing version 2, before any trial. (1) The `agent-kill` trigger ignores a port the agent CLI itself listens on: it is the first sample in which one of the worktree's services listens. (2) What the victim's worktree was running is listed in the 2 s before the kill, so a slow listing cannot delay the kill. (3) Event (g): a loss is only known to lie between two samples, so the 10 s and 5 s windows are applied to that whole interval; an agent's own stop or restart includes the arm's `up`, `down`, `restart`, `pg_ctl stop`, `redis-cli shutdown` and any `kill`. (4) In `service-kill`, losses in the victim's worktree in the 10 s after the injected kill are listed for review and are not event (g). (5) A measure is called separated only with at least five compared pairs, so a pilot reports "not decided"; for continuous measures the within-arm IQR is over per-run means (recovery and victim figures: their raw values). (6) "Victim copes" compares with the same task in the same arm and scenario when it was not the victim. (7) Trace-line times are arrival times at the harness, polled every 50 ms. (8) The shared README part said `GET/POST /invoices`; only `POST` exists in the starting code, so it now says `POST /invoices` (both arms) | Ambiguities found while building the harness. None was decided after seeing a trial |
| `a6dda8f` | **Changes after review round 1, before any trial.** The fault trigger is now a readiness milestone shared by both arms (all four services of the victim's worktree listening), with the target's identity checked again before the signal. Event (g) needs a kill that identifies the lost process, and timing alone never gates. Connections in `agent-kill` are attributed by time and role. The teardown observations count every attributed process, an unknown inventory invalidates the run, and "clean after the first `down`" is reported beside the gate. "Others untouched" now runs through the teardown, with teardown collateral as an event. "Victim copes" needs the service restored and a test passed after the fault. A missed fault or invalid run is rerun once as a whole pair and is never a success. A measure is decided only with five paired values. Trial plans are checked against this protocol. Records: benchmark-owned environment values, traces published only scrubbed, a full-blob commit scan on the commit path. Hidden checks: `invoice-list` needs an index led by `status` that also covers `id`, full items compared on every page and more than 100 rows; `void-invoices` checks the whole status domain and no longer requires a stale read after a void | The reviewer showed that the first trigger landed at different provisioning stages in the two arms (validation records: Stack with all four services up, the baseline still in `dev.py up`), reproduced two wrong submissions that passed and a correct one that failed, and found gates that timing alone could trip or clear, report denominators that counted a missed fault as a success, and scan gaps |
| `05c8d27` | The code for review round 1, and the clarifications made while writing it, before any trial. (1) The handoff listing is taken in the 2 s before the recovery launch. (2) Teardown collateral is decided from the harness's own listings just before and after each worktree's teardown; a loss between two teardowns is listed, not gated. (3) Event (g) also accepts a kill by a port the lost process listened on. (4) The commit scan does not treat an existing path as a secret. (5) The fixed `PATH` keeps the `python3` version 1 ran on. (6) A harness defect found by validation: `run_arm` left SIGINT and SIGTERM ignored after its first run in a process, so later runs in the same validation started agents that ignored SIGTERM; it now restores the handlers | Review round 1's findings and the implementer's report. None was decided after seeing a trial |
| `6c69e1f` | **Changes after review round 2, before any trial.** The trigger is described as an observation of four listeners on the worktree's own data directory, and a service lost before the kill makes the fault missed; late kills are reported as late. A rerun governs once started. Event (g) reads the kill command's own operands, an own `restart` must cover the lost service, and a command of a killed CLI runs to the end of the episode. A client with no role marker is never charged to a session by time. Teardown collateral excludes a worktree's own slow exits. The residue inventory adds every data directory the sampler saw in use. **"Victim copes" is now gated success plus the observed restoration of the database; the agent's own test run after the fault is reported as description only.** Cost covers every launch in every attempt. Trial plans are checked in full. Traces and submissions are scrubbed in staging and only then published. The commit scan reads each staged path by its exact name. Hidden checks: the `invoice-list` index must lead with `status` then `id`; the `void-invoices` status set is read from the schema and must be exactly the three | The reviewer confirmed the round 1 design fixes and reproduced narrower failures: a raw trace copied into the results before scrubbing, the commit scan reading `a1` for a staged `a[1]`, a kill attributed from `echo 123; kill -9 456`, "copes" passing on `stack run test \|\| true`, and an index and a constraint variant that still passed |
| `c9ed844` | The code for review round 2, and the clarifications made while writing it, before any trial. (1) The recheck at the kill requires the same four processes the trigger saw, so a service restarted in those 3 s makes the fault missed. (2) In the Stack arm the trigger can fire only once `stack ps` lists the worktree's PostgreSQL data directory. (3) The 30 s window for a worktree's own slow exits is measured from each own command's start. (4) A kill whose target the harness cannot read from the command (`kill $(pgrep …)`) is listed for review, never gated. (5) Any own `restart` covers a lost Stack supervisor, which is not a named service. (6) The rule that a successful session must report its model applies to trial plans; scripted validation agents report none. (7) Trace arrival times hold only line numbers and times and are written straight to the results. (8) A detector defect found by the validation of `dbf1da8`: a process caught while exiting shows in `ps` with a bare name, lost its attribution and dropped out of tracking, so a planted kill went unreported once in three runs. An exiting process now counts as gone, and a durable process stays tracked by pid and start time | Review round 2's findings and the implementer's report. None was decided after seeing a trial |
| `aed4127` | **Changes after review round 3, before any trial.** The trigger's four listeners must be the arm's own services on the ports the arm recorded. Event (g) gates only a plain, unconditional kill; conditional and unmodelled forms, and commands of a killed CLI with no result, go to review. A slow-exit excuse applies only to a process its own session had targeted. The harness signals only processes tied to the run by marker, descent or verified service identity; a process that merely has a run file open is listed. The final cleanup runs whatever else fails. Database isolation uses every data directory seen for a worktree. Nothing is written to the results unscrubbed, the sampler's log included; scrubbed copies are written outside submitted trees. The commit is made from the scanned tree without hooks, and escaped values are decoded. A rerun's start is recorded first. Plans bind the sample interval and the pair counts. `void-invoices`: the status restriction must be closed in structure and under probes | The reviewer confirmed the round 2 bugs fixed and reproduced: process arguments written to the sampler log before scrubbing, the scrubber writing through a planted symbolic link, the scan missing a JSON-escaped value, cleanup signalling a process that only had a run file open, `false && kill -9 123` gating, and a constraint with `OR length(status) > 10` passing |
| `9273a4c` | The code for review round 3, and the clarifications made while writing it, before any trial. (1) The grader's own processes are tied to the run by a per-grading token in their environment, the same kind of tie as the run marker. (2) Descent covers descendants of any tied process, so a child of a verified arm service may be signalled. (3) A PostgreSQL data directory the sampler saw in use and that no longer exists at the isolation check is listed, not invalidating; one that exists and cannot be read still invalidates. (4) Self-test plans accept `AGENTVALUE_SELFTEST_UNREADABLE_CAPTURE=1`, which plants an unreadable capture so the publication-failure path can be validated; trial plans ignore it. (5) The `status` check of `void-invoices` is decided from the constraint's structure plus 30 probe values. (6) **Host-awake rule (added by the orchestrator):** a development validation run stalled because the host slept on battery with its lid closed. `pair` now refuses battery power, and a run whose wall time and run clock differ by more than 5 s is invalid | Review round 3's findings and the implementer's report; the sleep was found in the host's power log during development validation. None was decided after seeing a trial |
| `8f6ff29` | **Changes after review round 4, before any trial.** Teardown observations and the exit gate count processes tied to the run; a process that only has a worktree file open is listed. Event (g) gates only a kill that completed without error. An own command excuses a loss only if it certainly ran and the lost process existed when it started; an uncertain one sends the loss to review. The host-awake rule measures suspension directly (uptime clock against the sleep-inclusive monotonic clock, 0.5 s), requires AC power before each arm, and keeps the wall-clock difference as a diagnostic. The redactor covers credential-bearing headers in full; the commit scan decodes JSON strings with a conforming decoder. Stack's follow-up requires a run tie before each signal; PostgreSQL is started or stopped only under the run's root; a rerun marker must name the same plan; recorded descendants are looked for at the final cleanup. Hidden checks: the `status` constraint of `void-invoices` also accepts a disjunction of exact equalities (`status = 'open' OR status = 'paid' OR status = 'void'`), which the round 3 checker wrongly rejected | Review round 4 (GPT-6.2 Sol, at `9f4b558`) found seven remaining defects that could change a score or let a credential reach a record in normal operation, and four that need an adversarial agent or operator. All eleven are fixed. None was decided after seeing a trial |
| `077bb71` | The code for review round 4, and the clarifications made while writing it, before any trial. (1) Process start times come from `ps` at 1 s resolution, so an own command that started in the same second as the lost process counts as "may have run" (review), not as excusing. (2) An own `down` or `restart` the harness cannot parse counts as "may have run". (3) The redactor re-encodes JSON text it changes, which redacting doubly encoded JSON needs; text it does not change stays byte for byte. (4) The disposal summary counts only run-tied processes, like the other observations. (5) A validation stops on an outside SIGINT or SIGTERM after the current run's cleanup and exits non-zero. (6) The planted sentinel agent also echoes credential headers and holds a connection with them on its command line, so its own slot is expected to fail isolation in that validation case | Review round 4's findings and the implementer's report. None was decided after seeing a trial |
| `748dbca` | **Changes after review round 5, before any trial.** A process placed in the run (working directory inside the run's root, started after the run began) counts in the scored inventories and may be signalled, so a detached command with a clean environment is not missed; a process with one worktree's marker is attributed to that worktree only. An own command inside a longer block, and any read-only command, no longer excuses a loss. A power refusal cannot consume a rerun or strand a pair. A partly redacted header is redacted in full. **Not changed, with the reason:** the reviewer asked that a successful `pkill -f` matching two lost workers go to review. It stays two events: `pkill` signals every match, and a broad kill that takes down several siblings is the contamination this gate measures | Review round 5 (GPT-6.2 Sol, at `5b97eb0`) confirmed six round 4 items fixed and reproduced five remaining ones. None was decided after seeing a trial |
| `cfc75cd` | The code for review round 5, and the clarifications made while writing it, before any trial. (1) A command sent to the background with `&` counts as "may have run". (2) An own command the harness cannot parse but which names the lost pid or port with `kill` counts as "may have run". (3) A never-started record carries the plan identity (scenario, victim, model, plan kind) so the report can match it; it is no session, cost or denominator. (4) A signal that arrives during a run's protected cleanup is recorded, and `pair` then starts no further arm. (5) Counted processes record why they are tied. (6) **Limit found:** macOS does not show the environment of platform binaries (`/usr/bin/*`, `/bin/*`), so their marker cannot be read. Such a process is attributed by its open files and working directory, and the marker-first rule cannot apply to it: a platform-binary process of one worktree holding a file of another open at an observation would be charged to the other. Any such row shows in the record with its tie reason, and the report's gate failures are read with that in mind | Review round 5's findings and the implementer's report. None was decided after seeing a trial |
| `63d870b` | **Changes after review round 6, before any trial.** A broad kill is blamed only for matching processes that existed when it returned. A statement containing `||` never proves an own command ran, and `xargs` is a kill only when it runs one. A signal between the arms leaves a never-started record so the pair can be rerun. Header extents are decided per line. A markerless process is attributed by working directory before open files. **Decided, not changed:** placement in the run's root still counts and may be signalled; the reviewer's objection (an operator's process started inside a live run's root) is accepted as a stated limit with an operator rule, because the alternative misses an agent's detached command. **The pilot starts after these fixes validate, without a further full review round**: shell-text classification is declared a sorting aid whose every output in a trial is read by hand, and the reviewer checks this round's fixes alongside the pilot, before the main run | Review round 6 (GPT-6.2 Sol, at `c268406`). Each of six rounds found narrower forms of the same classes; the remaining risk is bounded by manual adjudication rather than by another round. None was decided after seeing a trial |
| `dfe58c6` | The code for review round 6, and the clarifications made while writing it, before any trial. (1) A header inside a JSON string on its line ends at that string's end; anywhere else it runs to the end of the line. (2) The existence check for a blamed kill applies to pid forms too; when a record lacks the lost process's start time or the run clock, the kill is judged on its text alone (trial records always carry both). (3) No member of a statement containing `||` certainly ran, the first included, because the block's result can hide its failure. (4) A signal that stops `pair` before any arm ran writes no record: attempt 1 can simply be started again | Review round 6's findings and the implementer's report. None was decided after seeing a trial |

### Accepted for the pilot

These review points are not fixed. Each one limits what the results can claim:

- **A trusted-agent experiment.** Agents and submitted code run as the same user as the grader, so
  a submission could read or alter benchmark files. The trace scan only detects direct references.
  Any hit is adjudicated by hand before a result is used, and nothing here is a security claim.
- **Cleanup is observational.** A detached process that leaves the run root, drops the marker
  (Redis overwrites its environment) and closes every file under the root would not be found. The
  record means "none observed".
- **Migration evidence is read after the run.** A migration that was renamed, shares a filename
  with another branch's, or sat in a database purged by its agent can lose its attribution.
  Unexplained ledger entries are listed for review.
- **The cross-worktree connection sampler runs once a second.** A short connection between samples
  is missed.
- **(Version 2) Shell text is sorted, not understood.** The rules that decide whether a kill is
  blamed for a lost service, or whether an agent's own command excuses a loss, read command text
  from the trace. Six review rounds each found another form they misread. The remaining risk is
  handled by reading: every such event, review item and excuse in a trial is reported with its
  command text and trace location and adjudicated by hand.
- **(Version 2) Placement in the run's root is taken as belonging to the run.** A process with no
  marker and no recorded parent counts, and may be stopped, if its working directory is inside the
  run's root and it started after the run began. An operator who starts a process from inside a live
  run's root would have it counted and stopped. The operator rule is not to work there during a run.
  The reviewer preferred not counting such processes; that would miss a detached command an agent
  started with a clean environment, which the reviewer's previous round required to be counted.
- **(Version 2) System binaries hide their marker.** macOS does not show the environment of
  binaries under `/usr/bin` and `/bin`, so such a process is attributed by working directory and
  open files only.

## Limitations

- One synthetic app on one Mac (macOS 27.2.1, Apple M5 Pro, 18 cores, 64 GiB), one agent harness
  and one model. Scaffold and model changes move agent scores by several points
  (RESEARCH.md §5). Linux is out of scope.
- Three concurrent agents, not five or ten. Git-level races appear more often at higher counts.
- No naive shared-database arm, no Compose arm, no off-the-shelf isolation tool arm.
- The same author wrote both arms, which the review partly offsets.
- Agents can read the host. Access to benchmark files is detected, not prevented.
- The sampler sees connections once a second, and process attribution can only report "none
  observed".
- The build under test has the known gap that `stack down` reports "confirmed" while commands
  started through `stack exec` still run. The teardown gate here is checked independently of what
  `down` reports, so that gap shows up as a teardown failure if an agent leaves such a command
  running. The fix is not in this build.
