# pilot-1 (agent_experiment)

| Measure | Stack | Baseline |
|---|---|---|
| Task success (hidden checks) | 3/3 (assigned 3) | 3/3 (assigned 3) |
| Gated success (task + teardown + isolation + clean CLI exit) | 3/3 (assigned 3) | 3/3 (assigned 3) |
| Agent CLI ended cleanly | 3/3 (assigned 3) | 3/3 (assigned 3) |
| Committed its work | 3/3 (assigned 3) | 3/3 (assigned 3) |
| Ran the tests | 3/3 (assigned 3) | 3/3 (assigned 3) |
| Teardown gate held | 3/3 (assigned 3) | 3/3 (assigned 3) |
| Isolation gate held | 3/3 (assigned 3) | 3/3 (assigned 3) |
| Used its own database | 3/3 (assigned 3) | 3/3 (assigned 3) |
| Timed out | 0/3 (assigned 3) | 0/3 (assigned 3) |
| Seconds to verified result | median 42.11 [IQR 30.47–51.33], range 30.47–51.33, n=3 | median 44.44 [IQR 35.9–75.89], range 35.9–75.89, n=3 |
| Seconds, all scored | median 42.11 [IQR 30.47–51.33], range 30.47–51.33, n=3 | median 44.44 [IQR 35.9–75.89], range 35.9–75.89, n=3 |
| Cost USD (CLI-reported, list) | median 0.097 [IQR 0.09–0.122], range 0.09–0.122, n=3 | median 0.089 [IQR 0.076–0.166], range 0.076–0.166, n=3 |
| Turns | median 7 [IQR 6.0–8.0], range 6–8, n=3 | median 6 [IQR 6.0–14.0], range 6–14, n=3 |
| Tool calls | median 6 [IQR 5.0–7.0], range 5–7, n=3 | median 5 [IQR 5.0–13.0], range 5–13, n=3 |
| Tool errors | median 0 [IQR 0.0–1.0], range 0–1, n=3 | median 0 [IQR 0.0–1.0], range 0–1, n=3 |
| Environment errors in tool output | median 0 [IQR 0.0–0.0], range 0–0, n=3 | median 0 [IQR 0.0–0.0], range 0–0, n=3 |
| Episodes with all agents gated-successful (of scored; assigned) | 1/1; 1 | 1/1; 1 |
| Broad kill commands | 0 | 0 |
| Benchmark-file access in traces | 0 | 0 |
| Not run / invalid agent slots | 0 / 0 | 0 / 0 |
| Gated success by position (first / second in pair) | 3/3 (assigned 3) / 0/0 (assigned 0) | 0/0 (assigned 0) / 3/3 (assigned 3) |

Missing runs: []; invalid runs: []; runs from another plan ignored: {}

Paired difference in gated success (stack − baseline, per-pair mean, bootstrap over pairs): {'pairs': 1, 'per_pair': [0.0], 'mean_diff': 0.0, 'ci95': None}
Paired difference in task success: {'pairs': 1, 'per_pair': [0.0], 'mean_diff': 0.0, 'ci95': None}
Discordant pairs: {}

| Pair | Arm | Pos | Task | Status | Pass | Teardown | Isolation | Seconds | Cost | Tools | Env err | Failed checks |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 0 | stack | 0 | invoice-notes | scored | True | True | True | 30.47 | 0.09 | 5 | 0 |  |
| 0 | stack | 0 | pay-receipts | scored | True | True | True | 51.33 | 0.097 | 6 | 0 |  |
| 0 | stack | 0 | customer-table | scored | True | True | True | 42.11 | 0.122 | 7 | 0 |  |
| 0 | baseline | 1 | invoice-notes | scored | True | True | True | 35.9 | 0.076 | 5 | 0 |  |
| 0 | baseline | 1 | pay-receipts | scored | True | True | True | 44.44 | 0.089 | 5 | 0 |  |
| 0 | baseline | 1 | customer-table | scored | True | True | True | 75.89 | 0.166 | 13 | 0 |  |
