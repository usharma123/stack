# Stack AgentDX benchmark

Does a coding agent finish the same development task with less human help when the project
uses Stack than when it uses the project's existing setup? This benchmark runs matched trials
to measure that, plus scripted regression checks for the Stack defects that block agents.

The method is in [AGENTDX-PROTOCOL.md](AGENTDX-PROTOCOL.md). The findings that shaped it are in
[eval/agentdx-20261020/SYNTHESIS.md](../eval/agentdx-20261020/SYNTHESIS.md). The former 26-tool
competitor benchmark is retired to [eval/archive/competitor-20261006](../eval/archive/competitor-20261006/README.md).

## What is implemented

| Part | File | Status |
|---|---|---|
| 3 synthetic pilot tasks in 3 small apps | `tasks.json`, `fixtures/`, `oracles/`, `hidden/` | Validated in both arms (see below) |
| Stack arm: `stack.toml` + `stack.lock`, agent uses `stack` directly | `arms.py` | Implemented |
| Existing arm: `mise.toml` + project-written `dev.py` running native services | `existing/dev.py`, `arms.py` | Implemented |
| Hidden acceptance checks on grader-only data | `verify.py` | Implemented |
| Validation: starting code fails, reference passes, same grading path as agents | `run.py validate` | Implemented |
| Pre-registered plan, external agent trial, cleanup, grading | `run.py plan`, `run.py trial` | Exercised end to end with the scripted self-test agent only |
| Human transcript review and report | `run.py review-template`, `run.py report` | Implemented; no real review exists yet |
| Stack regression checks for the hands-on findings | `regressions.py` | Implemented, run on 0.1.22 |
| Offline unit tests | `tests/` | 23 tests |

**No real agent trial has been run.** Running one needs an agent CLI, its credentials and model
spend, which this checkout does not authorize. Every number so far comes from scripted
validation, self-tests or regression checks, and none of it is agent evidence.

## The tasks

| Task | App | What the agent must do | Hidden checks |
|---|---|---|---|
| `ledger-note-cache` | Python API, PostgreSQL 17.11, Redis 7.4.7 | Add an invoice note with an idempotent migration; fix a stale Redis read after PATCH | A second pre-existing invoice the agent never sees (with non-ASCII text), cache content, rerun of the migration |
| `catalog-sku-uniqueness` | Node HTTP API with a JSON file | Make SKU uniqueness ignore whitespace and case; keep the file unchanged on 409 | A second existing product, four duplicate spellings, file bytes |
| `slug-unicode` | Node library, tools only | Fix `slug()` for diacritics, punctuation and idempotence | Eight cases, two not in the prompt |

The apps are small, made-up projects, not established repositories. The existing arm's
`dev.py` is glue written for this benchmark in the style of a project's own scripts, not a
real project's history. It pins the same tool versions and the same mise backends that Stack
locks (`conda:postgresql`, `conda:redis-server`), and it initializes PostgreSQL with `-E UTF8`.

Each arm's README describes only its own workflow. In the existing arm a `stack` stub on
PATH answers "command not found", so a host-wide `stack` cannot leak into the control.

## Running

The host needs `python3` 3.11+, `git`, `mise` on PATH (for the existing arm; Stack can download
its own with `stack setup`), network access to conda-forge, GitHub releases, nodejs.org and
PyPI, and `lsof` on macOS. PostgreSQL refuses to run as root. Runs are serialized per host by
`/tmp/stack-agentdx-bench.lock`.

```sh
cargo build --locked
python3 -m unittest discover -s bench/tests -v                       # offline

# 1. Validate each task/arm cell (about 1-3 minutes each, cold).
python3 bench/run.py validate --task ledger-note-cache --arm stack \
  --stack target/debug/stack --out eval/results/<new>/validate-ledger-stack

# 2. Product regression checks (expected to fail on 0.1.22, see below).
python3 bench/regressions.py --stack target/debug/stack --out eval/results/<new>/regressions

# 3. Pre-register an experiment. The agent command is fixed in the plan.
python3 bench/run.py plan --kind agent_experiment --models <model-id> --harness <name> \
  --agent-command '["<agent-cli>", "--model", "{model}", "--prompt-file", "{prompt_file}"]' \
  --stack target/debug/stack --platform macos-arm64-local --cache cold --seed <n> --repeats 5 \
  --out eval/results/<new>/plan.json

# 4. Run every planned trial in plan order, one at a time.
python3 bench/run.py trial --plan <plan.json> --id '<trial id>' --results eval/results/<new>/trials

# 5. A human reviews each transcript and saves review.json next to its summary.
python3 bench/run.py review-template --plan <plan.json> --id '<trial id>' --results <trials dir>

# 6. Report over the whole plan.
python3 bench/run.py report --plan <plan.json> --results <trials dir> --out <report.json>
```

The agent starts in the trial checkout, so give the agent command absolute paths.
Placeholders in the agent command: `{model}` (required), `{prompt}` or `{prompt_file}`
(required), `{workspace}` and `{transcript_dir}` (save tool-call transcripts there). The agent
runs with its working directory in a fresh checkout, stdin closed and no way to ask a human,
under per-trial Stack, mise, Pitchfork, XDG, uv and npm stores. `HOME` is kept. An agent CLI
that keeps its credentials under `XDG_CONFIG_HOME` needs a wrapper that restores that one
variable for itself only.

To test the harness without a model, use `--kind harness_selftest` with
`selftest_agent.py` as the command. Such plans are never publishable:

```sh
python3 bench/run.py plan --kind harness_selftest --models selftest --harness selftest \
  --agent-command '["python3", "'"$PWD"'/bench/selftest_agent.py", "--mode", "reference", "--model", "{model}",
                    "--prompt-file", "{prompt_file}", "--transcript-dir", "{transcript_dir}"]' ...
```

## How a trial is scored

1. A fresh runtime root and checkout are created, with one git commit as the starting point.
2. The agent runs within the plan's budget. Whatever it leaves running is recorded
   (`left_running_at_agent_exit`) before the harness touches anything.
3. The diff against the starting commit is saved as `submission.patch`.
4. The harness stops the agent's workspace with the arm's own `down`, and stops the root's
   Pitchfork supervisor in the Stack arm. It then looks for any process whose command line,
   working directory or an open file is under the runtime root. If any remain, the harness
   stops them, but the trial's cleanup is still recorded as failed.
5. Grading uses a second fresh root: grader-only seed data, the submitted files copied over
   the starting project (never `stack.toml`, `stack.lock`, `mise.toml`, `dev.py`, seeds, smoke
   tests or runtime directories), harness-driven `setup` and `up`, then `verify.py` through the
   arm's `exec`. The task passes only when the grader receipt lists exactly the task's
   `required_checks`, all true.
6. `report` re-derives pass/fail from the grader receipt, checks every evidence hash, the plan,
   binary and locale identity, and the review. It counts a trial as autonomous only when:
   - the task is verified,
   - the review is valid and records no human help,
   - and nothing was left running at agent exit.

   Missing, invalid and unreviewed trials stay in every denominator.

The agent's final message is never read for scoring.

## Validation results on Stack 0.1.22

See [eval/agentdx-20261020/SYNTHESIS.md](../eval/agentdx-20261020/SYNTHESIS.md#benchmark-validation)
for the exact runs. In short, every cell is gradeable. In the Stack arm, the project's
documented test command `stack run test` fails for both service apps because of finding P1-1.
That is the product defect the agent will meet, so it is part of what the benchmark measures,
not a reason to drop the cell.

Under `--locale '{"LC_ALL": "C"}'` the ledger task fails validation in the Stack arm. Its
PostgreSQL cluster is `SQL_ASCII` (finding R3), and even the grader's seed cannot be
inserted. Do not plan that cell until R3 is fixed. The native arm validates under both
locales.
