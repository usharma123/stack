# Stack lane rerun after the occupied-port fix

Diagnostic rerun of the Stack lane only, with the binary built from this branch, to show the
checks that changed behaviour. It is not a session report: nothing here replaces or ranks
against the reviewed 26-tool results in [final-20261006-reviewed-1](../final-20261006-reviewed-1/),
and its timings come from a different day and container than every other lane's.

| | |
|---|---|
| Stack source | commit `a278a9acf30f38cf4037e36fe8faf7b49ab6110f` (branch `fix/occupied-port-install-logs`) |
| Linux ARM64 binary | SHA256 `932acfa6019a731873e5592a84a0e3b1841a95da872c6965a5340ca55e5843f0`, built with `rust:1-bookworm` `cargo build --release --locked` |
| Provider | mise 2026.10.3 (SHA256 `357260e2...`), Pitchfork 2.29.0, image `ev-base` |
| Run | `20261007t002031-19c681`, 2026-10-07 00:20:31 to 00:21:11 UTC, host Darwin 24.6.0 arm64, Docker transport |
| Command | `python3 bench/run.py --tool stack --out bench/results/stack-portfix-20261006-2 --option stack_binary=... --option stack_sha256=932acfa6... --repeats 20 --warmups 3` |
| Adapter | `bench/rwb/adapters/stack.py` as committed on this branch (frozen recipe `stack install`) |

## Outcome

27 pass, 2 observed, 0 fail (reviewed run: 26 pass, 2 observed, 1 fail).

| check | reviewed run | this run | evidence |
|---|---|---|---|
| occupied_port | fail: `start-failed-without-conflict-diagnostic (exit 1)`, error `stop_failed` | pass: `refused-at-start`, error `port_conflict` naming postgres, port 43560, holder pid/`nc` (attributed through `/proc`; `ev-base` has no `lsof`), hint `stack compile --reassign-ports`; the listener survived and was released by the harness | `logs/0105-e-occupy-port.*`, `logs/0106-e-start.*`, `logs/0107-release-listener.*` |
| lock.frozen_copy | pass via `compile --locked && up && down` | pass via `stack install`: lock unchanged, versions match, no services started | `logs/0095-c-frozen-setup.*`, `0096-c-lock-hash.*`, `0097-c-tool-versions.*` |
| cleanup.supervisors | observed: `pitchfork supervisor run` lingering | observed: same | `summary.md` |
| every other check | pass / observed | unchanged | `outcomes.json` |

Timings are recorded in `summary.md` for completeness. They are one diagnostic run on a
different day; do not compare them with the reviewed session's numbers.

## Files

- `summary.md`, `outcomes.json`, `meta.json`, `steps.jsonl`: the run's own records.
- `logs/`: tool versions, the frozen-copy and occupied-port steps.
- `raw-results.tar.gz` (SHA256 in `raw-results-receipt.txt`): the complete results directory
  including every command log.

An earlier rerun with the round-1 binary (`9ccde967...`, before the generation-change fix)
had the same 27/2/0 outcome; it is superseded by this one and not kept.
