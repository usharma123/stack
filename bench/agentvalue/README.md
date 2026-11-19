# Stack agent-value benchmark

Several coding agents work at once on one repository, each in its own git worktree, against a
Python app with PostgreSQL, Redis and a worker. The benchmark compares a Stack arm with a strong
baseline (mise plus a per-worktree `dev.py`) on task success, teardown, isolation, time, tool use
and cost.

- [RESEARCH.md](RESEARCH.md): how multi-agent worktree development is done, what breaks, and how
  to evaluate agents. Verified citations.
- [PROTOCOL.md](PROTOCOL.md): the design and scoring, fixed before any paired trial.
- [REPORT.md](REPORT.md): the first baseline.

## Layout

| Path | What |
|---|---|
| `fixture/` | The app every run starts from |
| `tasks.json` | The three concurrent tasks and their required hidden checks |
| `arms/` | `project.md` (shared README part), `stack/` (`stack.toml`, README part), `baseline/` (`mise.toml`, `dev.py`, README part) |
| `hidden/` | Grader-only data and checks. Never copied into a trial repository |
| `oracles/` | Reference solutions, used only for validation |
| `harness/run.py` | `plan`, `pair` (one paired trial end to end), `arm` |
| `harness/grade.py` | The arm-independent hidden grader |
| `harness/observe.py` | Process attribution and the connection sampler |
| `harness/report.py` | The report over a plan |
| `harness/validate.py` | The harness self-check |
| `results/` | Plans, run records and reports. Traces and other large files are gitignored but kept on disk in the same place |

## Running

The host needs: macOS, `python3` 3.11+, `git`, `mise` and `lsof` on PATH, `claude` (Claude Code)
logged in, and network access for the first warm-up of `~/.cache/stack-agentvalue/warm`. The
grader runs the pinned PostgreSQL 17.11, Redis 7.4.7, Python 3.13.16 and uv 0.12.23 from
`~/.local/share/mise/installs` (`mise install postgres@17.11 redis@7.4.7 python@3.13.16 uv@0.12.23`).

```sh
export DEVELOPER_DIR=/Library/Developer/CommandLineTools
cargo build --release --locked

# 1. Validate the harness: grader, reference agents in both arms, planted faults (about 5 min).
python3 bench/agentvalue/harness/validate.py --out bench/agentvalue/results/validation-<date>

# 2. Fix a plan (configuration, arm order, file hashes). Refused if the file exists.
python3 bench/agentvalue/harness/run.py plan --name baseline-1 --pairs 5 --seed <n> \
  --out bench/agentvalue/results/baseline-1/plan.json

# 3. Run one paired trial end to end (both arms, planned order). Repeat for each index.
python3 bench/agentvalue/harness/run.py pair --plan bench/agentvalue/results/baseline-1/plan.json \
  --index 0 --results bench/agentvalue/results/baseline-1

# 4. Report over every planned run (missing runs stay in the denominator).
python3 bench/agentvalue/harness/report.py --plan bench/agentvalue/results/baseline-1/plan.json \
  --results bench/agentvalue/results/baseline-1
```

`pair` refuses to run if the Stack binary or any benchmark file changed since the plan. To rerun
the baseline after a Stack change, build it, make a new plan and run every pair again.
