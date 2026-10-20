# AgentDX hands-on sessions, 2026-10-20

Two independent single-agent sessions used Stack 0.1.22 (source
`8b3402fa1db8f11917b5ba33a333c9093545d9ce`) as a coding agent would. Neither read the other's
findings. They are discovery evidence, not a matched experiment: no control arm, one tester
each, and tasks the tester wrote and graded.

| Session | Report | Scripts and fixtures |
|---|---|---|
| Opus 5.5 | [opus/REPORT.md](opus/REPORT.md) | [opus/](opus/) |
| GPT-6 Astra | [astra/REPORT.md](astra/REPORT.md) | [astra/](astra/), see [astra/README.md](astra/README.md) |

The synthesis across both, and the benchmark built from them, is in [SYNTHESIS.md](SYNTHESIS.md).

## Provenance of these copies

The report directories are byte-identical copies of the testers' originals (checked with
`diff -rq`; only Astra's ignored `__pycache__/` was not copied):

- Opus: `/Users/utsavsharma/.t3/worktrees/stack/t3-6c91bf15/eval/agentdx-20261020/opus/`
- Astra: `/Users/utsavsharma/.t3/worktrees/stack/t3-e39fed11/eval/agentdx-20261020/astra/`

Raw receipts are git-ignored and stay in the testers' worktrees. The reports cite them by
relative names (`receipts/017`, `ledger-exec-endpoints.json`), which resolve against:

- Opus: `/Users/utsavsharma/.t3/worktrees/stack/t3-6c91bf15/eval/results/agentdx-opus-20261020T132129Z/`
- Astra: `/Users/utsavsharma/.t3/worktrees/stack/t3-e39fed11/eval/results/agentdx-astra-20261020-094034/`

Opus kept one stopped runtime root as failure evidence: `/tmp/sah.v0Pz` (632 MB, the
`SQL_ASCII` cluster from the hosted-style bootstrap). Delete it by exact path only when that
evidence is no longer needed.

## Caveats that apply to both

- Exact `@ushawarma/stack@0.1.22` was not on npm during either session. Both used a source
  build (Opus also a local `npm pack`). Neither is a published artifact.
- Opus's "hosted" run is a clean noninteractive macOS shell (`env -i`), not hosted Linux.
  Astra's Linux evidence is Docker LinuxKit arm64 on the same Mac, not a hosted-agent vendor.
- The fixture fixes were written by the testers, so completing them shows the workflows can
  work, not that agents complete them with less help.
