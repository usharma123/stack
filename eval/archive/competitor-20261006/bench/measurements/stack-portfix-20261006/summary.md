# Stack (stack:default) run 20261007t002031-19c681

valid: True · completed: True · transport: docker · isolation boundary: service-instance
measurement: diagnostic: timings recorded per check; only passing checks' samples count · reportable: False

Setup scope: `stack compile` only: resolves versions, writes stack.lock and assigns ports. Tool and service installation happens in `stack up` (start), so first_task is the comparable time. The frozen recipe uses `stack install`, which installs from the lock without starting
Cache state: fresh ev-base container: no mise/Stack tool cache; downloads in A's `up`, reused by B (same container); pinned mise installed at provision (not timed)

| check | status | mode | detail |
|---|---|---|---|
| setup.a | pass | native |  |
| lock.created | pass | native |  |
| deps.a | pass | native |  |
| start.a | pass | native | readiness=native pg=43058 redis=46341 |
| migrate.a | pass | native |  |
| crud_cache.a | pass | native |  |
| tests.a | pass | native |  |
| start.repeat | pass | native | start exit 0 |
| setup.b | pass | native |  |
| deps.b | pass | native |  |
| start.b | pass | native | readiness=native pg=46899 redis=48130 |
| migrate.b | pass | native |  |
| crud_cache.b | pass | native |  |
| tests.b | pass | native |  |
| isolation | pass | native | boundary=service-instance |
| repeat.entry | pass | native |  |
| repeat.app_read | pass | native |  |
| status | observed | native | exit 0 |
| stop.a | pass | native | stop exit 0, probe exit 0, app after stop exit 3 |
| b.survives | pass | n/a |  |
| restart.a | pass | native | readiness=native pg=43058 redis=46341 |
| persist.pg | pass | native |  |
| persist.redis | pass | native | policy={'appendonly': 'yes', 'appendfsync': 'everysec', 'save': '3600 1 300 100 60 10000', 'dir': '/home/agent/.local/state/mise/daemons/cafd555678070c8/data/redis'} |
| cache.after_restart | pass | native |  |
| lock.frozen_copy | pass | native | setup exit 0; lock unchanged; versions match |
| bad_config | pass | native | setup exit 1 (intended diagnostic) |
| occupied_port | pass | native | refused-at-start |
| cleanup.processes | pass | native |  |
| cleanup.supervisors | observed | n/a | /home/agent/.local/share/mise/installs/pitchfork/2.29.0/pitchfork supervisor run |

End-to-end time to first verified work (setup, start, readiness, app deps, identity,
migrate, mark/CRUD/cache, pytest). `steps` sums those receipts; `wall` also includes
harness verification steps. This, not any single setup command, is the cross-tool task time.
It starts from an already PREPARED checkout; prepare (reported separately as prepare.<co>) covers: benchmark glue only: fixture + tool config copy, source token.

| task | steps s | wall s | receipts | checkout |
|---|---|---|---|---|
| first_task.a | 14.48 | 14.93 | 10 | first in run |
| first_task.b | 7.38 | 7.74 | 10 | second, A's lock, warm caches |

Phase timings (single commands; scopes differ by tool, see Setup scope; do not rank across tools):

| timing | outer p50 ms | outer p95 ms | inner p50 ms | inner p95 ms | ok/n |
|---|---|---|---|---|---|
| deps.first_checkout (single, outer) | 547.8 | | | | 1/1 |
| deps.second_checkout (single, outer) | 124.2 | | | | 1/1 |
| prepare.a (single, outer) | 260.4 | | | | 1/1 |
| prepare.b (single, outer) | 79.4 | | | | 1/1 |
| prepare.c (single, outer) | 75.4 | | | | 1/1 |
| prepare.d (single, outer) | 73.9 | | | | 1/1 |
| prepare.e (single, outer) | 63.8 | | | | 1/1 |
| ready.a (single, outer) | 991.5 | | | | 1/1 |
| ready.b (single, outer) | 5928.2 | | | | 1/1 |
| repeat.app_read | 191.733 | 239.188 | 133.767 | 175.432 | 20/20 |
| repeat.entry | 90.33 | 96.659 | 33.139 | 34.967 | 20/20 |
| setup.first_checkout (single, outer) | 1488.9 | | | | 1/1 |
| setup.second_checkout (single, outer) | 71.8 | | | | 1/1 |

`ready.<co>` sums start, native readiness and the first verified identity (app deps excluded).
First-checkout timings start from the image's cache state above; they are not universal cold installs.
