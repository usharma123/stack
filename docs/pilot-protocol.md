# Pilot protocol

Two different things are measured, and their results must never be mixed:

1. **Scripted concurrency** (`eval/harness/pilot.py`): deterministic workers drive stack the way
   an agent task would, several projects at once, with controlled failures. It measures whether
   stack keeps instances separate and cleans up under concurrency. It says nothing about how
   productive agents are.
2. **Real-agent task completion** (this document, sections below): independent coding agents do
   real tasks in their own checkouts. It has not been run yet; no productivity claim exists.

## Scripted concurrency pilot

```sh
cargo build --release
python3 eval/harness/pilot.py --stack target/release/stack --projects 3 --rounds 2 \
  --phases fresh,cached --failures service_kill,runner_death
```

- Needs `mise` on PATH and port 5432 free (or occupied: withheld endpoints must never reach it).
- HOME and XDG directories are isolated under a short `/tmp/spl.*` directory. `fresh` starts
  with no installed tools or downloads; `cached` reuses that HOME with new project checkouts.
- Each round runs every project concurrently. A worker starts the stack with `--owner-pid` of a
  stand-in runner process, runs the bundled task (`uv sync`, `pytest`, `mise run seed`, the
  bundled `acme` CLI), checks over the app's own `DATABASE_URL`/`REDIS_URL` which Postgres data
  directory and Redis directory it reached, samples verified `exec` latency, then stops.
- Controlled failures, assigned deterministically: `service_kill` SIGKILLs a project's Postgres
  and requires the next `exec --require-all` to refuse, then recovers with `up`;
  `runner_death` kills the runner without `down` and requires `stack gc` alone to reclaim
  exactly that project's services.
- Output: a new `eval/results/pilot-<run id>/` with `metadata.json` (stack version, binary
  hash, source commit and dirtiness, mise version, platform), `events.jsonl` (every command,
  exit code, latency), `logs/` (full output of each command) and `summary.json` (per phase:
  tasks attempted/succeeded, wrong-instance incidents, orphaned service processes, controlled
  failures injected/handled, latency distributions). The exit code is nonzero if any task
  failed, any wrong instance was reached, any process was orphaned, or a failure was mishandled.

Only reviewed `summary.json` and run metadata should be committed. Full command logs and
event streams remain local and are ignored by Git. Preserve failed or invalid runs locally;
when publishing summaries, explain any harness correction alongside the valid rerun.

Review round 1 found measurement gaps in the current runner: orphan counts cover PIDs from
successful up/recovery responses, failed starts can default to zero, and failed identity or
recovery checks do not always fail the run. The recorded successful run had no unexpected
command failures, so its task counts remain observations; zero orphans is not a general
guarantee. Strengthen these checks before using the runner as an acceptance gate.

## Real-agent protocol (not yet executed)

Goal: measure task completion, wrong-instance incidents and orphaned processes when
independent agents, not scripts, use stack concurrently.

- **Arms.** Same tasks, same agent model and budget, two arms: (A) stack via MCP (`stack mcp`)
  and (B) the project's existing setup instructions without stack. Randomize arm order per task.
- **Tasks.** At least 12 tasks across at least 3 repositories that need Postgres and Redis
  (e.g. add a migration and endpoint, fix a failing integration test, add a cache layer). Each
  task has a hidden acceptance test run afterwards by the harness, never by the agent.
- **Concurrency.** 4 agents at once on one machine, each in its own fresh checkout, repeated
  over at least 3 rounds; one round per arm starts from an empty tool cache.
- **Controlled faults.** In a fixed, pre-registered subset of runs: a foreign Postgres on 5432,
  killing a service mid-task, killing the agent runner (expect lease reclamation), deleting a
  checkout while its services run (expect `stack gc` cleanup).
- **Recorded per run.** Task outcome (hidden test), wall time, agent tool calls, stack errors by
  code, every connection's reached instance (Postgres `data_directory`, Redis `dir`, logged by
  a wrapper), service processes alive after the agent finished and after `stack gc`, stack and
  provider versions, machine metadata.
- **Reported.** Completion rate per arm with confidence intervals, wrong-instance incidents and
  orphan counts (absolute), latency distributions. Nothing is reported as a speedup unless both
  arms ran under identical conditions in the same session.

Until this runs, the only measured claims are those of the scripted pilot and the end-to-end
scenarios.
