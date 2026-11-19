# Stack agent-value benchmark

Several coding agents work at once on one repository, each in its own git worktree, against a
Python app with PostgreSQL, Redis and a worker. The benchmark compares a Stack arm with a strong
baseline (mise plus a per-worktree `dev.py`) on task success, teardown, isolation, time, tool use
and cost.

- [RESEARCH.md](RESEARCH.md): how multi-agent worktree development is done, what breaks, and how
  to evaluate agents. Verified citations.
- [PROTOCOL.md](PROTOCOL.md): the design and scoring, fixed before any paired trial (version 2:
  five agents, fault injection, the fixed Stack build).
- [REPORT.md](REPORT.md): the first (version 1) baseline.

## Layout

| Path | What |
|---|---|
| `fixture/` | The app every run starts from |
| `tasks.json` | The five concurrent tasks and their required hidden checks |
| `arms/` | `project.md` (shared README part), `stack/` (`stack.toml`, README part), `baseline/` (`mise.toml`, `dev.py`, README part) |
| `hidden/` | Grader-only data and checks. Never copied into a trial repository |
| `oracles/` | Reference solutions, used only for validation |
| `harness/run.py` | `plan`, `pair` (one paired trial end to end), `arm` (self-tests only). Fault injection, recovery sessions and the teardown procedure live here |
| `harness/grade.py` | The arm-independent hidden grader |
| `harness/observe.py` | Process attribution, the 1 s sampler (also the fault trigger and service-loss record) and reclaim |
| `harness/report.py` | The report over a plan (version 2 plans get the per-scenario report; version 1 plans the version 1 report) |
| `harness/validate.py` | The harness self-check |
| `harness/scripted_agent.py` | Scripted stand-ins for agents, for validation only |
| `harness/commit.py` | The only way to commit: scans every staged blob in full (text, binary, renames, link targets, path names) and the message, then commits |
| `harness/scan_staged.py` | The scanning library `commit.py` uses; also scans a directory (`--dir`) |
| `results/` | Not in git. Plans, run records, traces and generated reports are written here and kept on the machine that ran the trials; REPORT.md carries the tables inline and says where they are |

A run directory `results/<plan>/pair<i>-<arm>/` holds `run.json`, `sampler.jsonl`, and one directory per
session under `agents/`: `<task>/` for an ordinary agent and, in an `agent-kill` run, `<task>-recovery/`
for the recovery session (the killed session keeps `<task>/`, marked `killed_by_harness`, never scored).
Each session directory has `prompt.txt`, `argv.json`, `trace.jsonl` (the CLI's stream-json, unchanged),
`trace.arrivals.tsv` (the arrival time of every trace line in seconds since that session's launch),
`stderr.txt`, and for the scored session of each worktree `grade.json`, `submission.json`,
`submission.patch` and `submitted-tree/`. In `run.json`, `fault` holds the trigger, the kill, what the
victim's worktree ran at the kill and at the end, and the recovery launch; `teardown` holds, per
worktree, the arm's `down` (and Stack's follow-up rounds), the observation after `down`, the harness's
own reclaim, and the observation after removal; `machine` holds the machine budget.

## Running

The host needs: macOS, `python3` 3.11+, `git`, `mise` and `lsof` on PATH, `claude` (Claude Code)
logged in, and network access for the first warm-up of `~/.cache/stack-agentvalue/warm`. The
grader runs the pinned PostgreSQL 17.11, Redis 7.4.7, Python 3.13.16 and uv 0.12.23 from
`~/.local/share/mise/installs` (`mise install postgres@17.11 redis@7.4.7 python@3.13.16 uv@0.12.23`).

Version 2 uses the Stack build from `fix/exercise-findings` at `a5d4785`, built outside any checkout
(PROTOCOL.md, Version 2, item 1):

```sh
STACK=~/.cache/stack-agentvalue/stack-a5d4785/src/target/release/stack   # sha256 b0931a8b…

# 1. Validate the harness: grader (five tasks, all mutants), then every planted case in both arms,
#    and the exec case once with the version 1 build (target/release/stack at dc4b980) as control.
#    About 40 minutes.
python3 bench/agentvalue/harness/validate.py --out bench/agentvalue/results/validation-<rev>

# 2. Fix a plan: scenarios alternate, victims a seeded permutation per scenario, arm order per
#    scenario from a seeded coin flip. The binary's sha256 and `stack --version` are recorded.
#    Refused if the file exists. The pilot is one pair per scenario:
python3 bench/agentvalue/harness/run.py plan --name pilot-2 --pairs-per-scenario 1 --seed <n> \
  --stack $STACK --stack-rev a5d4785 --out bench/agentvalue/results/pilot-2/plan.json
#    The main run is five pairs per scenario (--pairs-per-scenario 5). A plan without faults
#    (self-tests) takes --pairs N instead of --pairs-per-scenario.

# 3. Run one paired trial end to end (both arms, planned order, planned scenario and victim).
python3 bench/agentvalue/harness/run.py pair --plan bench/agentvalue/results/pilot-2/plan.json \
  --index 0 --results bench/agentvalue/results/pilot-2

# 4. Report over every planned run (missing runs stay in the denominator).
python3 bench/agentvalue/harness/report.py --plan bench/agentvalue/results/pilot-2/plan.json \
  --results bench/agentvalue/results/pilot-2

# 3b. A pair whose first attempt had an invalid run or a missed fault is run once more, whole:
python3 bench/agentvalue/harness/run.py pair --plan bench/agentvalue/results/pilot-2/plan.json \
  --index 0 --attempt 2 --results bench/agentvalue/results/pilot-2      # writes pair0.2-<arm>

# 5. Every commit goes through the scan (it refuses on any hit and never prints the value).
python3 bench/agentvalue/harness/commit.py -m "message"
python3 bench/agentvalue/harness/scan_staged.py --dir <directory>     # a directory before staging
```

`plan` and `pair` (before each arm) refuse a trial plan that does not match the protocol: the
Stack binary's sha256 (a5d4785's), `--stack-rev a5d4785`, a scenario and a victim in every pair, the
agent CLI pinned to its versioned binary with a recorded hash, and the warm cache the runner uses.
`pair` also refuses if the Stack binary, the agent CLI binary or any benchmark file changed since
the plan. The report compares the latest complete attempt of each pair and lists every attempt. To rerun after a change, make a new plan and run
every pair again.

A self-test of one arm with scripted agents and a chosen fault:

```sh
python3 bench/agentvalue/harness/run.py plan --name selftest --kind harness_selftest --pairs 1 --seed 0 \
  --stack $STACK --stack-rev a5d4785 --out /tmp/selftest-plan.json
python3 bench/agentvalue/harness/run.py arm --plan /tmp/selftest-plan.json --arm stack --out /tmp/selftest-run \
  --scripted 'invoice-list=stall,invoice-list.recovery=reference' --scenario agent-kill --victim invoice-list
```

Agents, the bystander and every arm command the harness runs get an environment the benchmark
builds (`machine_env` in `harness/run.py`): `PATH` (the run's bin directory, the pinned CLI's
directory and `BASE_PATH`), `SHELL`, `TMPDIR` (under the run root) and `LANG` are fixed; only `HOME`,
`USER` and `LOGNAME` come from the host. No record holds an environment. Agent traces are written to
a staging file under the run root; only a scrubbed copy is published to the results directory (also
for an interrupted or invalid run), and every retained file is scrubbed after grading.
