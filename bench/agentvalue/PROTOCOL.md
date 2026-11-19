# Stack agent-value benchmark: protocol

Status: fixed before any paired trial. Every later change to scoring, gates or analysis must be a
separate commit that says why, and is listed under [Changes](#changes) with its commit.

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

## Changes

| Commit | Change | Reason |
|---|---|---|
| `d1ef3c5` | Protocol fixed | — |
| `c829c39` | Hidden checks: a new required check `existing_api_intact` in every task (PATCH still updates the amount in the database; receipts still list). Pre-existing statuses and receipts are compared at the SQL level. `pay-receipts`: the grader runs the worker itself, proves the job is queued while the worker is held back and that no receipt exists before it runs, replays the captured job with the worker stopped and checks it was consumed, and requires that a 409 or 404 queues nothing. `customer-table`: a unique constraint on the name is required. `invoice-notes`: the note must be stored in the database and the read path must still use the cache. The migration ledger must match the new migration files exactly, and the rerun check covers constraints, indexes and row counts | The independent review reproduced two submissions that passed while wrong (a worker that never issues receipts; a migration that drops the unique name, deletes receipts and breaks PATCH). Each is now a validation mutant that must fail |
| `c829c39` | Gated success also requires a clean CLI exit. Teardown is observed per agent at exit with fresh ownership paths; CLI helpers are excluded. Isolation: write counters no longer gate, kill culprits must name the lost program, the canary is matched by token, unreadable worktree databases invalidate the run, Stack's probes are not cross-worktree traffic, connections must reach a loopback listener. Compliance is reported. The arm's own `down` is tried on the dirty checkout before harness cleanup | Review findings 6–9, 14 and 15: gates that could pass a bad trial or fail a good one |
| `c829c39` | Baseline `dev.py`: a lock-guarded port registry, a per-worktree lock, resumable `up`, migration on `restart`, a worker health check, commands that need only the database, and a `down` that confirms ports are closed. Shared README: the worker health contract | Review findings 1–3: the baseline's recovery and shutdown were weaker than its README promised, which would have handicapped it |
| `c829c39` | The harness never logs a whole environment: addresses are read with a three-variable query, and every recorded output and retained trace is scrubbed of credential-like values. Run-qualified process markers, marker-based cleanup, grader partial-start cleanup, stop rules enforced by `pair`, a host lock, plan digests bound into runs, agent-facing Markdown and this protocol bound into the plan | Review finding 4 (a host credential reached a committed record; removed from the branch by rewriting the protocol commit), 5, 12 and 13 |
| `82731b8` | Round-2 review fixes. Baseline `down` signals only processes proven to be the worktree's (recorded pid and start time, or group members whose command or working directory is inside it); a foreign listener is reported, never killed; the lifecycle lock lives outside `.dev/` and is held through `--purge`; `up` adopts services it started but never recorded and restarts apps that do not answer; `migrate` needs only PostgreSQL. Grader: bounded waits, group stop after leader exit, cleanup in `finally`; the notes cache check proves a cache hit (a database-only change must not show through), the rerun check compares table contents, uniqueness must not be partial, statuses are compared in SQL, payment must be persisted before the worker runs, and jobs must go on a Redis list (the README's contract). Gates: a failed `ps` or `stack ps` raises (the run becomes invalid) instead of reading as "nothing running"; listeners are matched by full endpoint; a killed bystander that makes the fingerprint unreadable is contamination, not an invalid run; independent schema changes are no longer pinned on a migration's owner. Agents inherit no credential-like variable and none of the host harness's (`CLAUDE*`, `T3*`, `ANTHROPIC_*` and similar); the redactor covers bare and JSON key shapes; `arm` runs only self-test plans; the report rejects runs whose plan, pair, arm, model or kind differ; budget stops are not CLI failures; recovery needs a successful `down`; no interval with fewer than three pairs | Round-2 review (P0 1–5, P1 6–10, P2 12). Three new validation mutants reproduce its counterexamples and must fail |
| (next commit) | An externally interrupted pair is rerun whole into the same plan, so both arms stay back to back in time. Its completed half is kept under `results/baseline-1/superseded-interrupted/` and excluded. The decision was made before looking at that half's outcomes | The host's agent server restarted during baseline pair 3 and killed its Stack run before any agent started. The bystander it had started was stopped with Stack's own `down`, and nothing survived |

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
