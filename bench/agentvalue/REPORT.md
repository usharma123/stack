# Stack agent-value benchmark: first baseline

Date: 2026-11-18. Branch `bench/agent-value`.

- **Stack under test.** Source `dc4b980`, the base of this branch, cut from
  `origin/fix/exercise-findings`. Stack 0.1.23, release build, binary sha256
  `4c7a836d408af2e1b9a0201a0ad1ff90544415bcd0c524495f160aceab8c4ea3`. `cargo test --locked --all-targets`:
  746 passed.
- **Harness.** At `b02a207`, validated 42/42.
- **Plans.** `pilot-1` and `baseline-1`; their parameters are under
  [Plan parameters](#plan-parameters).
- **Raw results.** Not in git. They are kept on this machine; see
  [Where the raw results are](#where-the-raw-results-are).
- **Method.** [PROTOCOL.md](PROTOCOL.md), with research in [RESEARCH.md](RESEARCH.md). Every number
  below can be regenerated on this machine from the local results with
  `python3 bench/agentvalue/harness/report.py --plan <plan> --results <dir>`.

This build still has the known gap that `stack down` reports "confirmed" while commands started
through `stack exec` keep running. No agent in this baseline left such a command running, so the gap
did not show up in any measure (see [What this does not show](#what-this-does-not-show)). Rerun the
same plan configuration on the build with the fix, from a new plan, and compare.

## Headline

**At this difficulty, both arms reach the ceiling.** All 30 agent sessions in the baseline (15 per
arm) and all 6 in the pilot:

- passed the hidden checks;
- stopped everything they started;
- touched no other worktree's or the main checkout's state;
- exited cleanly.

So the benchmark cannot tell Stack from a competent per-worktree setup script on outcomes. This is
the null result the protocol expected in advance for contamination, and it also holds for success and
teardown.

The arms differ only in small amounts of effort, all within run-to-run spread:

- Stack sessions were a median 3 s faster.
- They used the same median number of tool calls per paired task, but one more in the queue task.
- They cost US$0.004 more.

The clearest agent-facing difference is a first-contact friction in Stack: `stack status` in a
never-started checkout exits 1. It is the first improvement target below.

## Configuration (matched in both arms)

| | |
|---|---|
| Host | macOS 27.2.1, Apple M5 Pro, 18 cores, 64 GiB. No other benchmark ran; the machine was otherwise in normal use |
| Agent | Claude Code 2.1.327 for pairs 0–2 and 2.1.328 for pairs 3–4 (see [Deviations](#deviations)). Model `claude-sonnet-5-5`, effort `high`. Headless, with `--safe-mode`, tools `Bash, Read, Edit, Write`, `bypassPermissions`, and no MCP. Per agent: 1800 s and US$8. No human channel |
| Concurrency | 3 agents at once, one worktree each, plus the main checkout's running services (the bystander) |
| Tasks | `invoice-notes` (additive migration + cache), `pay-receipts` (queue + worker idempotency), `customer-table` (destructive migration + seed) |
| Arms | **Stack**: `stack.toml` + `stack.lock` (postgres 17.11, redis 7.4.7 presets; api with identity probe and migrate `setup`; worker; seed init task). **Baseline**: `mise.toml` with the same pins and backends, plus `dev.py` (per-worktree ports from a lock-guarded registry, own data under `.dev/`, identity and health checks, confirmed `down`). Agent-facing README: 505 words (Stack) and 482 (baseline) |
| Prompts | Identical in both arms (`harness/run.py` `PROMPT` plus the task text in `tasks.json`). Saved per session as `prompt.txt` |
| Starting commit | A fresh one-commit repository per run (`Starting point`), with no benchmark files in its history |
| Order | Pairs alternate the first arm. In the baseline, Stack went first in pairs 0, 2 and 4 and the baseline in pairs 1 and 3. Arms in a pair ran back to back |
| Caches | A shared warm download cache (`~/.cache/stack-agentvalue/warm`). Per-run Stack, mise and XDG state |

## Baseline results (baseline-1: 5 pairs, 30 sessions, all 10 runs valid)

Sample size: 15 agent sessions per arm (5 runs × 3 tasks), in 5 paired episodes.

| Measure | Stack | Baseline |
|---|---|---|
| Task success (hidden checks) | 15/15 | 15/15 |
| **Gated success** (task + teardown + isolation + clean CLI exit) | **15/15** | **15/15** |
| Teardown gate held | 15/15 | 15/15 |
| Isolation gate held | 15/15 | 15/15 |
| Episodes with all three agents gated-successful | 5/5 | 5/5 |
| Used its own database (positive evidence) | 15/15 | 15/15 |
| Committed / ran the tests | 15/15 / 15/15 | 15/15 / 15/15 |
| Timed out / CLI failures / API errors | 0 / 0 / 0 | 0 / 0 / 0 |
| Seconds to a verified result | median 37.8, IQR 32.4–44.9, range 23.9–57.4 | median 42.2, IQR 35.3–47.7, range 27.5–56.9 |
| Tool calls per session | median 6, IQR 5–7, range 4–9 | median 5, IQR 5–6, range 5–12 |
| Turns per session | median 7, IQR 6–8, range 5–10 | median 6, IQR 6–7, range 6–13 |
| Sessions with a tool error | 4/15 | 1/15 |
| Environment errors (address in use, refused, wrong DB) | 0 | 0 |
| Broad kill commands | 0 | 0 |
| Cost per session, USD (CLI-reported list price) | median 0.103, IQR 0.079–0.109 | median 0.089, IQR 0.077–0.106 |
| Benchmark-file access in traces | 0 | 0 |
| Run wall time, s (setup + agents + checks + grading + disposal) | 93.9–110.3 | 93.4–110.2 |

**Paired differences (stack − baseline, same pair and task, 15 pairs of sessions):**

| Measure | Median | Stack higher / lower / equal | Per-pair mean difference (pairs 0–4) |
|---|---|---|---|
| Seconds | −2.96 | 4 / 11 / 0 | +0.8, −4.4, +5.3, −4.0, −7.4 |
| Tool calls | 0 | 7 / 4 / 4 | +1, −1, +2, −0.3, −1 |
| Turns | 0 | 7 / 4 / 4 | +1, −1, +2, −0.3, −1 |
| Cost, USD | +0.004 | 10 / 5 / 0 | +0.004, +0.009, +0.010, +0.001, −0.007 |

Gated success and task success: every per-pair difference is 0. The bootstrap over pairs gives an
interval of [0, 0]. No pair is discordant.

**Per task** (medians over 5 repetitions, Stack | baseline). Every repetition is in the second table.

| Task | Seconds | Tool calls | Cost USD | Notes |
|---|---|---|---|---|
| invoice-notes | 31.0 \| 34.4 | 5 \| 5 | 0.076 \| 0.075 | The least variable task in both arms |
| pay-receipts | 41.4 \| 42.6 | 6 \| 6 | 0.103 \| 0.089 | Stack used one more call than its pair in 4 of 5 pairs |
| customer-table | 44.9 \| 47.7 | 8 \| 6 | 0.114 \| 0.117 | The most variable task in both arms (baseline 5–12 calls) |

Every repetition, in pair order:

| Task | Arm | Seconds (pairs 0–4) | Tool calls | Turns | Cost USD |
|---|---|---|---|---|---|
| invoice-notes | stack | 32.4, 31.0, 34.9, 30.8, 23.9 | 5, 4, 6, 5, 5 | 6, 5, 7, 6, 6 | 0.076, 0.079, 0.088, 0.075, 0.071 |
| invoice-notes | baseline | 34.4, 35.3, 36.4, 30.1, 27.5 | 5, 5, 5, 5, 5 | 6, 6, 6, 6, 6 | 0.077, 0.072, 0.081, 0.075, 0.074 |
| pay-receipts | stack | 37.2, 44.6, 51.2, 34.3, 41.4 | 6, 7, 6, 5, 7 | 7, 8, 7, 6, 8 | 0.100, 0.107, 0.103, 0.088, 0.104 |
| pay-receipts | baseline | 42.5, 49.5, 39.7, 44.0, 42.2 | 5, 6, 5, 6, 6 | 6, 7, 6, 7, 7 | 0.089, 0.091, 0.086, 0.089, 0.101 |
| customer-table | stack | 57.4, 53.0, 44.9, 39.9, 37.8 | 8, 9, 9, 6, 6 | 9, 10, 10, 7, 7 | 0.119, 0.136, 0.114, 0.109, 0.109 |
| customer-table | baseline | 47.7, 56.9, 38.8, 42.9, 55.6 | 6, 12, 5, 6, 10 | 7, 13, 6, 7, 11 | 0.117, 0.134, 0.106, 0.105, 0.132 |

**Order:** gated success was 15/15 whatever the arm's position, so these data can show no order
effect, and cannot rule one out.

**Disposal** (each worktree removed by the arm's documented procedure):

- **Processes, ports, data.** No process, listening port or service data directory was left in any
  run.
- **Stack metadata.** Stack leaves about 90 KB per run: its records of the three removed checkouts
  (identity-token, project and task-history records). `stack ps` lists them as `reclaimable: true`
  even after `stack gc --reclaim-data`.
- **Baseline.** It leaves nothing outside the removed worktree.
- **Recovery.** Every arm `down` on the as-left checkout exited 0 with nothing to stop.

**Pilot** (pilot-1, 1 pair, 6 sessions, same configuration): the same picture. Both runs were
valid, 6/6 sessions had gated success, and the cost was US$0.64. The pilot's purpose was to find
harness defects, and it found none. It is reported separately, as the protocol requires; it would
not change any number above.

## Gate failures

**There were none in any agent trial**: no teardown, isolation or disposal failure in the pilot or
the baseline. Every gate was shown to fire on planted cases in validation (42 expectations, all
passing at `b02a207`):

| Planted case, both arms | Gate result |
|---|---|
| Agent leaves its services running | Teardown fails for that agent only. All of PostgreSQL, Redis, the API, the worker and (Stack) the supervisor are listed. The arm's `down` then recovers the dirty checkout |
| Agent runs its migration against the main checkout's database | Isolation fails, attributed to that agent (`migration_in_main`) |
| Agent restarts its worker with `pkill -f` and kills the main checkout's worker | Isolation fails, attributed to that agent (`main_process_killed`) |
| Clean reference agents | Every gate holds; nothing is left after disposal |

The first validation of `f9f524c` missed the planted kill once in the Stack arm. The killed worker was still finishing its one-second queue wait
when the bystander was checked. The harness now waits 5 s after the last agent before comparing
(`b02a207`). That was a harness defect, not a Stack result.

## Which measures to improve against

These verdicts apply the criteria fixed in PROTOCOL.md: a validated detector, not at floor or ceiling
in both arms, and a spread small next to a change we would act on.

| Measure | Verdict | Why |
|---|---|---|
| Gated success, task success, teardown, isolation, episode success | **Not usable to improve against now.** Keep them as gates that must hold | At ceiling in both arms (15/15). They cannot move upward. Their validated detectors make them the right guards for any change to Stack: a change that breaks them is rejected |
| Tool calls and turns per session | **Usable, with care** | Not at floor. Within-arm IQR/median is 0.33 (Stack) and 0.2 (baseline), under the 0.5 limit. But the arm difference (median paired 0, Stack +1 in 7 of 15) is about the size of the spread. Expect to see only a change of about one call per session or more |
| Sessions with a tool error | **Usable, but weak** | 4/15 versus 1/15. Per-run counts in the Stack arm range 0–2, so a change must be at least 3 sessions of 15 to read as more than noise. Each error was inspected by hand (below), which is what makes it interpretable |
| Seconds to a verified result | **Usable for large changes only** | IQR/median is 0.33 and 0.29. The paired median of −3 s (Stack faster in 11 of 15) is well inside the spread, so it is not evidence of a speed difference |
| Cost per session | **Usable for large changes only** | Spread is similar to time. The +US$0.004 median follows the extra calls |
| Environment errors, broad kill commands, benchmark access, CLI failures | **Not usable** (floor, 0 in both arms) | They stay as diagnostics |
| Disposal metadata residue | **Deterministic, not statistical** | 3 records per Stack run, every run. It needs no repetitions: a fix either removes them or does not |

## First diagnosed failure worth a targeted improvement

**`stack status` in a checkout that was never started exits 1 and reads as four failures.** Agents use
`status` as their first look at a worktree. In 5 of 15 Stack sessions, `stack status` was the first
environment command; in the baseline arm, 2 of 15 began with `dev.py status`. In a never-started
checkout, Stack prints:

```
no session
postgres     NOT READY  no launch record in this checkout; run `stack up`
redis        NOT READY  no launch record in this checkout; run `stack up`
api          NOT READY  no launch record in this checkout; run `stack up`
worker       NOT READY  no launch record in this checkout; run `stack up`
postgres     init pending
```

It exits with code 1, so the agent CLI records a failed tool call. Three of the four Stack tool errors
in the baseline are exactly this:

- `pair0-stack/customer-table`;
- `pair1-stack/customer-table` and `pair2-stack/invoice-notes`, each at the end of a compound command
  run before `stack up`.

The fourth is unrelated: a script run with the bare interpreter could not import psycopg. That
friction is shared by both arms. In the same state, `dev.py status` exits 0 with
`"note": "nothing started; run python3 dev.py up"`, and none of the baseline's tool errors comes from
it.

The cost is small but real:

- an extra round trip and an error in about a fifth of Stack sessions;
- it plausibly accounts for part of Stack's +1 call in the queue task.

A targeted change could be either of these:

- report a never-started checkout as one clear state ("not started; run `stack up`") rather than four
  `NOT READY` lines;
- distinguish "nothing was ever started here" from "something is unhealthy" in the exit status or the
  JSON.

How to evaluate it: rerun both arms. It should reduce sessions with a tool error (4/15 now) and
calls per session, while every gate stays at 15/15. Note that `status` exiting nonzero when the stack
is not healthy is a deliberate contract. The owner decides whether "never started" belongs under it.

### Other Stack findings from building the arm (not measured by agents)

1. **A service that does not listen cannot be supervised.**
   - Every service gets a port, and `up` requires it to accept connections.
   - A Redis queue worker therefore fails `up` with `not_ready` ("port … is not accepting
     connections").
   - The Stack arm had to give the worker an optional health port (`WORKER_HEALTH_PORT`).
   - A team adopting Stack hits this on day one with any background worker.
2. **`stack gc --reclaim-data` leaves the removed checkouts' records**, about 30 KB each, which
   `stack ps` reports as `reclaimable: true`. Ports and service data are released correctly.
3. The known `stack down` / `stack exec` gap was not exercised. No agent left an `exec` command
   running.

## What this does not show

- **No contamination contrast.** Both arms isolate by design. A naive arm (one shared database, a
  common setup per RESEARCH.md §3) is needed to test "none of them will touch another's database"
  against a setup that can fail. It was out of scope for this baseline.
- **No failure recovery.** No task injects a crash: an agent killed mid-task, a service dying, or a
  dependent left stale. Those are where Stack's supervisor, identity checks and confirmed `down`
  should differ from a script.
- **No scale.** Three agents, not five or ten.
- **One model, one CLI, one host, one app.** A weaker model, lower effort or harder tasks would leave
  room below the ceiling.
- **The known `stack down` gap.** Agents here never left an `exec` command running, so its fix cannot
  move these numbers. A planted-fault check (an agent leaves `stack exec -- sleep` running, then runs
  `stack down`) would show it. The teardown gate already observes such a process independently of
  what `down` reports.

## Deviations

1. **The agent CLI updated itself on the host** from 2.1.327 to 2.1.328 at 21:21, during a host agent
   server restart, between baseline pairs 2 and 3. Every pair has both arms on one version, so
   pairing holds. Plans now pin the versioned binary (PROTOCOL.md change log).
2. **Baseline pair 3 was interrupted** by that restart before any agent started, and was rerun whole
   under a rule set before its outcomes were seen: an externally interrupted pair is rerun whole
   into the same plan, so both arms stay back to back in time. Its completed baseline half and the
   partial Stack run are kept locally (`baseline-1/superseded-interrupted/`) and excluded.
3. **Pair 4 was refused once** by the plan's fingerprint check: I had added the descriptive
   `harness/analyze_traces.py` mid-plan. It was moved out of the tree for the run and added back
   afterwards. No file that affects a trial changed.

## Review and harness defects found before the trials

An independent reviewer (GPT-6.2 Sol) reviewed the benchmark twice before the pilot. Every finding
and its fix is in the PROTOCOL.md change log.

- **Round 1 found:**
  - two grader holes, each reproduced: a worker that never issued receipts, and a destructive,
    API-breaking migration, both of which passed;
  - a sandbagged baseline recovery path;
  - gates that could pass bad trials;
  - **a host credential logged by the harness into a committed record.** The record was redacted and
    the protocol commit rewritten (`eaf1cb7` → `d1ef3c5`) and force-pushed. The old commit may still
    be fetchable by SHA on GitHub until garbage collection. **The credential should be treated as
    exposed and rotated by the owner.**
- **Round 2 found:**
  - unsafe baseline signalling;
  - a host-wide planted `pkill`;
  - grader timeouts;
  - redactor gaps;
  - a notes grader that accepted cache bypass.

  All are fixed. Accepted limitations are listed under "Accepted for the pilot".

Harness defects found by validation and smoke runs, all fixed before the trials:

- process-identity checks broken by trailing whitespace, which left a postgres and a redis of early
  self-tests running (found and stopped);
- Redis invisible to marker attribution;
- non-UTF-8 process environments breaking the sampler;
- compliance missing long commands;
- the settle race above;
- agents inheriting the host session's `CLAUDE*` variables, including `CLAUDE_EFFORT`.

## Cost and time

| | Sessions | CLI-reported cost (USD, list) |
|---|---|---|
| Baseline (baseline-1) | 30 | 2.91 |
| Pilot (pilot-1) | 6 | 0.64 |
| CLI smoke checks (one task, one arm each) | 2 | 0.15 |
| **Agent trials total** | **38** | **3.70** |

Not included above:

- The research workflow: 14 agents (Opus researchers, synthesizer and critic; Sonnet verifiers),
  about 194k output tokens. One further Opus agent applied the critique, about 204k tokens.
- The two review rounds by GPT-6.2 Sol.

No cost figure is reported for either; their token counts are as stated.

Wall-clock time:

- the baseline: about 17 minutes of run time, ten runs of 93–110 s each;
- the pilot: about 4 minutes;
- the whole investigation: about 5 hours, including two host restarts.

## Plan parameters

Both plans were written and committed before their runs.

| | pilot-1 | baseline-1 |
|---|---|---|
| Pairs and arm order | 1 pair: Stack, then baseline | 5 pairs: Stack first in pairs 0, 2, 4; baseline first in pairs 1, 3 |
| Seed | 20261118 | 1118 |
| Stack source and binary | `dc4b980`, sha256 `4c7a836d408af2e1b9a0201a0ad1ff90544415bcd0c524495f160aceab8c4ea3` | the same |
| Harness revision when the plan was written | `c116374` | `3acd041` (both run the harness code of `b02a207`; only results and plans were committed in between) |
| Agent model and effort | `claude-sonnet-5-5`, `high` | the same |
| Agent CLI | Claude Code 2.1.327 | 2.1.327 in pairs 0–2, 2.1.328 in pairs 3–4 (see Deviations) |
| Agent command | `claude -p <prompt> --model claude-sonnet-5-5 --effort high --output-format stream-json --verbose --safe-mode --strict-mcp-config --no-session-persistence --disable-slash-commands --permission-mode bypassPermissions --tools Bash,Read,Edit,Write --max-budget-usd 8` | the same |
| Concurrency | 3 agents at once, plus the main checkout's services | the same |
| Budgets per agent | 1800 s and US$8 | the same |
| Tasks | `invoice-notes`, `pay-receipts`, `customer-table` | the same |
| Locale, sampler | `en_US.UTF-8`, one sample per second | the same |
| Host | macOS 27.2.1, Apple M5 Pro, 18 cores, 64 GiB; mise 2026.11.9, git 2.56.1 | the same |

**Starting commits.** Every run builds a new one-commit repository, so each run has its own starting
commit. Its content is the fixture plus the arm's files, and the hash differs only through the commit
time and, in the Stack arm, the compiled lock.

| Plan | Pair | Arm | Position | Starting commit | Run seconds | Sampler samples | State left outside the worktrees after disposal (bytes) |
|---|---|---|---|---|---|---|---|
| pilot-1 | 0 | stack | 1 | `c67815f53adf` | 104 | 47 | 90112 |
| pilot-1 | 0 | baseline | 2 | `9f3f9503e4ea` | 129 | 67 | 0 |
| baseline-1 | 0 | stack | 1 | `6643c642b348` | 110 | 52 | 90112 |
| baseline-1 | 0 | baseline | 2 | `5187fd49187e` | 101 | 44 | 0 |
| baseline-1 | 1 | baseline | 1 | `563c036ad7c8` | 110 | 52 | 0 |
| baseline-1 | 1 | stack | 2 | `a4b94de68f77` | 107 | 48 | 90112 |
| baseline-1 | 2 | stack | 1 | `952ec3cfaca1` | 106 | 47 | 86016 |
| baseline-1 | 2 | baseline | 2 | `d23f36218657` | 93 | 38 | 0 |
| baseline-1 | 3 | baseline | 1 | `030ada105bd1` | 98 | 41 | 0 |
| baseline-1 | 3 | stack | 2 | `d47d398e6ac7` | 94 | 38 | 94208 |
| baseline-1 | 4 | stack | 1 | `022f8bcdeb20` | 95 | 39 | 94208 |
| baseline-1 | 4 | baseline | 2 | `d5d70e7c1723` | 110 | 51 | 0 |

## Every session

### baseline-1

| Pair | Arm | Position | Task | Hidden checks | Teardown | Isolation | Clean exit | Seconds | Tool calls | Turns | Tool errors | Cost USD | CLI |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 0 | stack | 1 | invoice-notes | pass | pass | pass | pass | 32.4 | 5 | 6 | 0 | 0.076 | 2.1.327 |
| 0 | stack | 1 | pay-receipts | pass | pass | pass | pass | 37.2 | 6 | 7 | 0 | 0.100 | 2.1.327 |
| 0 | stack | 1 | customer-table | pass | pass | pass | pass | 57.4 | 8 | 9 | 1 | 0.119 | 2.1.327 |
| 0 | baseline | 2 | invoice-notes | pass | pass | pass | pass | 34.4 | 5 | 6 | 0 | 0.077 | 2.1.327 |
| 0 | baseline | 2 | pay-receipts | pass | pass | pass | pass | 42.5 | 5 | 6 | 0 | 0.089 | 2.1.327 |
| 0 | baseline | 2 | customer-table | pass | pass | pass | pass | 47.7 | 6 | 7 | 0 | 0.117 | 2.1.327 |
| 1 | baseline | 1 | invoice-notes | pass | pass | pass | pass | 35.3 | 5 | 6 | 0 | 0.072 | 2.1.327 |
| 1 | baseline | 1 | pay-receipts | pass | pass | pass | pass | 49.5 | 6 | 7 | 0 | 0.091 | 2.1.327 |
| 1 | baseline | 1 | customer-table | pass | pass | pass | pass | 56.9 | 12 | 13 | 1 | 0.134 | 2.1.327 |
| 1 | stack | 2 | invoice-notes | pass | pass | pass | pass | 31.0 | 4 | 5 | 0 | 0.079 | 2.1.327 |
| 1 | stack | 2 | pay-receipts | pass | pass | pass | pass | 44.6 | 7 | 8 | 0 | 0.107 | 2.1.327 |
| 1 | stack | 2 | customer-table | pass | pass | pass | pass | 53.0 | 9 | 10 | 1 | 0.136 | 2.1.327 |
| 2 | stack | 1 | invoice-notes | pass | pass | pass | pass | 34.9 | 6 | 7 | 1 | 0.088 | 2.1.327 |
| 2 | stack | 1 | pay-receipts | pass | pass | pass | pass | 51.2 | 6 | 7 | 0 | 0.103 | 2.1.327 |
| 2 | stack | 1 | customer-table | pass | pass | pass | pass | 44.9 | 9 | 10 | 1 | 0.114 | 2.1.327 |
| 2 | baseline | 2 | invoice-notes | pass | pass | pass | pass | 36.4 | 5 | 6 | 0 | 0.081 | 2.1.327 |
| 2 | baseline | 2 | pay-receipts | pass | pass | pass | pass | 39.7 | 5 | 6 | 0 | 0.086 | 2.1.327 |
| 2 | baseline | 2 | customer-table | pass | pass | pass | pass | 38.8 | 5 | 6 | 0 | 0.106 | 2.1.327 |
| 3 | baseline | 1 | invoice-notes | pass | pass | pass | pass | 30.1 | 5 | 6 | 0 | 0.075 | 2.1.328 |
| 3 | baseline | 1 | pay-receipts | pass | pass | pass | pass | 44.0 | 6 | 7 | 0 | 0.089 | 2.1.328 |
| 3 | baseline | 1 | customer-table | pass | pass | pass | pass | 42.9 | 6 | 7 | 0 | 0.105 | 2.1.328 |
| 3 | stack | 2 | invoice-notes | pass | pass | pass | pass | 30.8 | 5 | 6 | 0 | 0.075 | 2.1.328 |
| 3 | stack | 2 | pay-receipts | pass | pass | pass | pass | 34.3 | 5 | 6 | 0 | 0.088 | 2.1.328 |
| 3 | stack | 2 | customer-table | pass | pass | pass | pass | 39.9 | 6 | 7 | 0 | 0.109 | 2.1.328 |
| 4 | stack | 1 | invoice-notes | pass | pass | pass | pass | 23.9 | 5 | 6 | 0 | 0.071 | 2.1.328 |
| 4 | stack | 1 | pay-receipts | pass | pass | pass | pass | 41.4 | 7 | 8 | 0 | 0.104 | 2.1.328 |
| 4 | stack | 1 | customer-table | pass | pass | pass | pass | 37.8 | 6 | 7 | 0 | 0.109 | 2.1.328 |
| 4 | baseline | 2 | invoice-notes | pass | pass | pass | pass | 27.5 | 5 | 6 | 0 | 0.074 | 2.1.328 |
| 4 | baseline | 2 | pay-receipts | pass | pass | pass | pass | 42.2 | 6 | 7 | 0 | 0.101 | 2.1.328 |
| 4 | baseline | 2 | customer-table | pass | pass | pass | pass | 55.6 | 10 | 11 | 0 | 0.132 | 2.1.328 |

### pilot-1

| Pair | Arm | Position | Task | Hidden checks | Teardown | Isolation | Clean exit | Seconds | Tool calls | Turns | Tool errors | Cost USD | CLI |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 0 | stack | 1 | invoice-notes | pass | pass | pass | pass | 30.5 | 5 | 6 | 0 | 0.090 | 2.1.327 |
| 0 | stack | 1 | pay-receipts | pass | pass | pass | pass | 51.3 | 6 | 7 | 0 | 0.097 | 2.1.327 |
| 0 | stack | 1 | customer-table | pass | pass | pass | pass | 42.1 | 7 | 8 | 1 | 0.122 | 2.1.327 |
| 0 | baseline | 2 | invoice-notes | pass | pass | pass | pass | 35.9 | 5 | 6 | 0 | 0.076 | 2.1.327 |
| 0 | baseline | 2 | pay-receipts | pass | pass | pass | pass | 44.4 | 5 | 6 | 0 | 0.089 | 2.1.327 |
| 0 | baseline | 2 | customer-table | pass | pass | pass | pass | 75.9 | 13 | 14 | 1 | 0.166 | 2.1.327 |

## Where the raw results are

The `results/` directory is not in git (it was tracked until the commit that added this section).
Everything a reader needs is in this report and in PROTOCOL.md. The raw records stay on the machine
that ran the trials:

`/Users/utsavsharma/.t3/worktrees/stack/bench-agent-value-91f22f7a971eac43/bench/agentvalue/results/`

- `pilot-1/` and `baseline-1/`: the plan, and per run a `run.json` (every observation), the grades,
  each agent's prompt, argv, full stream-json trace, stderr, submitted tree and patch, the sampler
  log, and the generated `report.json` and `report.md`. `baseline-1/superseded-interrupted/` holds the
  interrupted pair 3.
- `validation-b02a207/`: the binding validation (42/42). Earlier validation runs are kept for the
  defects they found.
- About 3.6 MB of traces across 167 sessions, self-tests included. Every retained file was scrubbed
  of credential-like values.

## Decisions for the owner

1. **Make the benchmark discriminate before hill-climbing on it.** At this difficulty both arms are at
   ceiling. In rough order of expected signal per dollar:
   - fault injection (an agent killed mid-task with the arm's own recovery; a service crash with a
     dependent);
   - a naive shared-database baseline arm, to test the README's isolation claim directly;
   - five concurrent agents;
   - a weaker model or lower effort.

   Each needs its own pre-registered plan.
2. **Whether `stack status` should exit nonzero for a checkout that was never started**: the first
   improvement target.
3. **Whether to support services that do not listen** (workers), and whether `gc --reclaim-data`
   should also drop the removed checkouts' records.
4. **Rotate the host credential** exposed in `eaf1cb7`.
