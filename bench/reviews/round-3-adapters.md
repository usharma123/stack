# Astra adapter review, round 3

Verdict: REQUEST_CHANGES. Reviewed the 18 external adapters after the five
round-2 findings were implemented. All seven preserved fault injections now fail
as intended; ownership, retained-volume and Berth port checks pass. All 102
relevant adapter tests pass. No real services or timings ran.

## Remaining P2: full container cleanup masks removal failure

`rwb/adapters/devcontainers.py:224`: `cleanup_host` runs the owned-resource
`remove` receipt and then `shared-cleanup`. Failure of the former is lost if the
latter succeeds. Stateful fake Docker reproduced Lando and DDEV cleanup exiting
0 despite refused removal and an owned container remaining running. The foreign
container was retained. Successful removal also exits 0 as expected.

Fix: attempt both cleanup stages, retain either failure, return nonzero if either
failed. Test the full generated shell body, not extracted receipt lines.

Reproduction during review: `/tmp/rwb-astra-r3-1gviclxi/repro_full_cleanup.py`.
Original-case replay: `/tmp/rwb-astra-r3-1gviclxi/repro_fixed.py`.
The implementation task owns only devcontainers.py and test_container_adapters.py.
