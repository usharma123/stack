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
| Offline unit tests | `tests/` | Counterexamples for grading, required observations, timing, cleanup and submission capture; run counts in the synthesis |

**No real agent trial has been run yet.** Real testing is authorized. What is still open is
the configuration of a first run: which agent CLI and version, which model IDs and options,
how the CLI's credentials reach a trial (the runner keeps `HOME` and isolates
`XDG_CONFIG_HOME`), and the spend limit per trial and in total. Once those are chosen, start
with a bounded pilot, for example one model on the three tasks with two repetitions (12
trials), before any larger study. Every number so far comes from scripted validation,
self-tests or regression checks, and none of it is agent evidence.

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
   The harness clones that commit into `<root>/harness/baseline.git`, outside the checkout.
2. The agent runs within the plan's budget, with the planned model and options substituted
   into the planned command. On budget expiry or cancellation the harness stops everything the
   command started. That is its process group, plus every descendant it can trace through
   parent PIDs, including children that outlive the leader or leave its group. A process that
   both left the group and lost its parent before the stop is found only by the root ownership
   check in step 4. Whatever is left running when the agent stops is recorded before the
   harness touches anything.
3. The submission is the agent's whole change against the kept starting commit: commits,
   edits, deletions and new files, including files the agent's `.gitignore` hides. It is
   captured once into a retained read-only `submitted-tree/` outside the agent workspace.
   `submission.patch` and `submission.json` describe those captured bytes and list the agent's
   commits. Runtime and private-config paths are excluded; symlink targets are never read.
   Nothing is written to the agent's `.git`.
4. The harness stops the agent's workspace with the arm's own `down`, and stops the root's
   Pitchfork supervisor in the Stack arm. It then looks for any process whose command line,
   working directory or an open file is under the runtime root. If any remain, the harness
   stops them, and the trial is not counted as autonomous.
5. Grading uses a second fresh root: grader-only seed data, then the retained submitted tree
   applied over the starting project, including its deletions. Cleanup writes to the live agent
   workspace cannot change the files graded. The report compares the overlay manifest with
   the captured file hashes and checks that the retained tree still matches. Benchmark-owned
   files are never taken from the submission: `stack.toml`, `stack.lock`, `mise.toml`, `dev.py`, seeds, smoke
   tests and runtime directories. The harness runs `setup` and `up`, the project's existing smoke
   test through the arm's `exec`, and then `verify.py` through `exec`. The task passes only
   when two conditions both hold:
   - the smoke test passes, so existing behaviour still works,
   - and the grader prints exactly one result listing exactly the task's `required_checks`,
     all true.

   The documented task loop (`stack run test`, `dev.py test`) is recorded but does not gate,
   because Stack 0.1.22's `run` breaks it in service apps (R1).
6. If the submitted app does not start, the harness grades the reference solution in a fresh
   control runtime. If the reference passes, the trial is a scored task failure
   (`startup_failed`). If it fails too, the trial is invalid (`infrastructure_failed`). Setup
   that fails three times is also invalid.
7. Every observation is written to its own evidence file and hashed: the agent receipt, what
   was left running, the submission, each cleanup, the grade, global state before and after,
   and the benchmark files after grading. `report` re-derives every verdict from those files.
   It never trusts the summary's own booleans. A summary that disagrees with its records,
   a missing or changed record, or an unhashed file makes the trial invalid. It counts a trial
   as autonomous only when:
   - the task is verified,
   - the review is valid and records no human help,
   - nothing was left running at agent exit,
   - and the arm's own teardown stopped everything without the harness reclaiming by PID.

   Missing, invalid and unreviewed trials stay in every denominator.

Required observations must be complete objects with correctly typed fields. JSON `null`,
an empty cleanup object or omitted residue lists do not establish a clean runtime. Final
cleanup is compared independently with its summary, and the latest cleanup must show
recovery even when an earlier reclaim succeeded. This applies to agent, final, grading and
reference-control runtimes. Reclaim after an already empty observation is contradictory;
grading or control cleanup requiring reclaim is invalid even if it succeeds.

Every command receipt must match its expected role and exact stdout/stderr digests. This
includes setup retries, the observed task loop, down and supervisor stop. Missing outputs or
out-of-window command timing invalidate the row. Known survivors or unresolved identities
from any command invalidate the trial, even if a later root scan is empty. A nonzero down
with no leak remains a product observation.

Startup seconds come from the setup and startup steps required by the arm and grading
outcome. All setup attempts count. Missing or malformed timing stays missing and invalidates
the row; it is never converted to zero. Tools-only tasks have no `up` timing. Failed setup
does not require later unreachable steps. A malformed trial cannot abort the other assigned rows.

The agent's final message is never read for scoring.

### Model options

Plan options with `--options '{"reasoningEffort": "high"}'`. Each option must reach the agent
command as a placeholder, as in `--reasoning-effort {option:reasoningEffort}`, or the plan is
refused. Values are passed verbatim: strings as written, numbers and booleans as JSON. A
literal flag named like a planned option or like `model` must carry that placeholder, so
`--reasoning-effort low` next to a planned `high` is refused. The trial's agent receipt must
show exactly the planned argv. The reviewer then records `observed_options` from the
transcript; the template leaves it empty, and a review whose observed options differ from
the plan is invalid. An agent CLI that cannot take a setting on its command line needs a
wrapper script that can, and the wrapper's receipt belongs in the transcript. Otherwise plan
no options; the report then labels the run as using the harness defaults, not controlled
settings.

### Usage and cost

`tokens` (nonnegative integer `input`, `output`, `cache_read`, `cache_write`, `reasoning`,
`total`) and `cost_usd` (`value`, finite and nonnegative) are `null` unless the provider reports
them. Each must name its source as `{"path": ..., "sha256": ...}`, pointing either to a file
in the trial evidence (such as a transcript with usage) or to a file the reviewer adds under
`review-evidence/` in the trial directory. Nothing is estimated or converted.

### What the grader trusts

Graders, references and hidden data sit outside the agent's workspace by convention; there
is no sandbox. Within that, `verify.py` computes every check from values it observes itself:
HTTP responses, database rows, files, and the return values of `slug()`. Submitted code
cannot print or return its own verdict. For the slug task, the grader reads a random nonce
and its inputs over stdin before the submission loads, so exit handlers, `process.exit` and
prototype tampering cannot fake the result. Out of scope are attacks outside the result
channel: editing the benchmark files, reading the grader's memory or nonce, or tracing it.
The trial re-hashes the benchmark files after grading, so edits to them are detected.

### Cleanup and PID reuse

Every signal, the SIGKILL escalation included, goes to one recorded process after its start
time and command line are checked again. For leftovers under a runtime root, the harness also
checks that the process is still under that root. On Linux a pidfd holds the process between
the check and the signal, and the kernel start tick catches reuse within the same second. On
macOS `lstart` has one-second resolution, and a few microseconds separate the check from
`kill`. The harness never signals by name or by a group it cannot attribute.

A child that calls `exec` retains its start identity. If the command tree still establishes
ownership, cleanup tracks its new command before escalation. If it cannot establish that
ownership, the receipt lists the process as `unresolved` and leaves it unsignalled. A reused
PID is never adopted as the old process.

## Regression checks

`regressions.py` gives each check a verdict. `fixed` means Stack behaves as documented.
`reproduced` means the defect is present. Either verdict needs the check's control to pass
and its cleanup to be clean; otherwise the check is `invalid` and says why. Every control
states what it shows. `down-before-up`'s control is a sibling checkout of the same project
that runs `up` and then a confirmed `down`. The exit status is:

- 0 when every check is fixed,
- 1 when the run is valid and at least one defect reproduced,
- 2 when any check is invalid or the user's global Stack, mise or Pitchfork state changed.

## Validation results on Stack 0.1.22

See [eval/agentdx-20261020/SYNTHESIS.md](../eval/agentdx-20261020/SYNTHESIS.md#benchmark-validation)
for the exact runs. In short, every cell is gradeable. In the Stack arm, the project's
documented test command `stack run test` fails for both service apps because of finding R1.
That is the product defect the agent will meet, so it is part of what the benchmark measures,
not a reason to drop the cell.

Under `--locale '{"LC_ALL": "C"}'` the ledger task fails validation in the Stack arm. Its
PostgreSQL cluster is `SQL_ASCII` (finding R3), and even the grader's seed cannot be
inserted. Do not plan that cell until R3 is fixed. The native arm validates under both
locales.
