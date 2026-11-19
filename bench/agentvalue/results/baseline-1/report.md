# baseline-1 (agent_experiment)

| Measure | Stack | Baseline |
|---|---|---|
| Task success (hidden checks) | 15/15 (assigned 15) | 15/15 (assigned 15) |
| Gated success (task + teardown + isolation + clean CLI exit) | 15/15 (assigned 15) | 15/15 (assigned 15) |
| Agent CLI ended cleanly | 15/15 (assigned 15) | 15/15 (assigned 15) |
| Committed its work | 15/15 (assigned 15) | 15/15 (assigned 15) |
| Ran the tests | 15/15 (assigned 15) | 15/15 (assigned 15) |
| Teardown gate held | 15/15 (assigned 15) | 15/15 (assigned 15) |
| Isolation gate held | 15/15 (assigned 15) | 15/15 (assigned 15) |
| Used its own database | 15/15 (assigned 15) | 15/15 (assigned 15) |
| Timed out | 0/15 (assigned 15) | 0/15 (assigned 15) |
| Seconds to verified result | median 37.75 [IQR 32.36–44.91], range 23.86–57.42, n=15 | median 42.16 [IQR 35.29–47.72], range 27.48–56.93, n=15 |
| Seconds, all scored | median 37.75 [IQR 32.36–44.91], range 23.86–57.42, n=15 | median 42.16 [IQR 35.29–47.72], range 27.48–56.93, n=15 |
| Cost USD (CLI-reported, list) | median 0.103 [IQR 0.079–0.109], range 0.071–0.136, n=15 | median 0.089 [IQR 0.077–0.106], range 0.072–0.134, n=15 |
| Turns | median 7 [IQR 6.0–8.0], range 5–10, n=15 | median 6 [IQR 6.0–7.0], range 6–13, n=15 |
| Tool calls | median 6 [IQR 5.0–7.0], range 4–9, n=15 | median 5 [IQR 5.0–6.0], range 5–12, n=15 |
| Tool errors | median 0 [IQR 0.0–1.0], range 0–1, n=15 | median 0 [IQR 0.0–0.0], range 0–1, n=15 |
| Environment errors in tool output | median 0 [IQR 0.0–0.0], range 0–0, n=15 | median 0 [IQR 0.0–0.0], range 0–0, n=15 |
| Episodes with all agents gated-successful (of scored; assigned) | 5/5; 5 | 5/5; 5 |
| Broad kill commands | 0 | 0 |
| Benchmark-file access in traces | 0 | 0 |
| Not run / invalid agent slots | 0 / 0 | 0 / 0 |
| Gated success by position (first / second in pair) | 9/9 (assigned 9) / 6/6 (assigned 6) | 6/6 (assigned 6) / 9/9 (assigned 9) |

Missing runs: []; invalid runs: []; runs from another plan ignored: {}

Paired difference in gated success (stack − baseline, per-pair mean, bootstrap over pairs): {'pairs': 5, 'per_pair': [0.0, 0.0, 0.0, 0.0, 0.0], 'mean_diff': 0.0, 'ci95': [0.0, 0.0]}
Paired difference in task success: {'pairs': 5, 'per_pair': [0.0, 0.0, 0.0, 0.0, 0.0], 'mean_diff': 0.0, 'ci95': [0.0, 0.0]}
Discordant pairs: {}

| Pair | Arm | Pos | Task | Status | Pass | Teardown | Isolation | Seconds | Cost | Tools | Env err | Failed checks |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 0 | stack | 0 | invoice-notes | scored | True | True | True | 32.36 | 0.076 | 5 | 0 |  |
| 0 | stack | 0 | pay-receipts | scored | True | True | True | 37.17 | 0.1 | 6 | 0 |  |
| 0 | stack | 0 | customer-table | scored | True | True | True | 57.42 | 0.119 | 8 | 0 |  |
| 0 | baseline | 1 | invoice-notes | scored | True | True | True | 34.43 | 0.077 | 5 | 0 |  |
| 0 | baseline | 1 | pay-receipts | scored | True | True | True | 42.55 | 0.089 | 5 | 0 |  |
| 0 | baseline | 1 | customer-table | scored | True | True | True | 47.72 | 0.117 | 6 | 0 |  |
| 1 | baseline | 0 | invoice-notes | scored | True | True | True | 35.29 | 0.072 | 5 | 0 |  |
| 1 | baseline | 0 | pay-receipts | scored | True | True | True | 49.48 | 0.091 | 6 | 0 |  |
| 1 | baseline | 0 | customer-table | scored | True | True | True | 56.93 | 0.134 | 12 | 0 |  |
| 1 | stack | 1 | invoice-notes | scored | True | True | True | 30.98 | 0.079 | 4 | 0 |  |
| 1 | stack | 1 | pay-receipts | scored | True | True | True | 44.63 | 0.107 | 7 | 0 |  |
| 1 | stack | 1 | customer-table | scored | True | True | True | 53.03 | 0.136 | 9 | 0 |  |
| 2 | stack | 0 | invoice-notes | scored | True | True | True | 34.94 | 0.088 | 6 | 0 |  |
| 2 | stack | 0 | pay-receipts | scored | True | True | True | 51.18 | 0.103 | 6 | 0 |  |
| 2 | stack | 0 | customer-table | scored | True | True | True | 44.91 | 0.114 | 9 | 0 |  |
| 2 | baseline | 1 | invoice-notes | scored | True | True | True | 36.44 | 0.081 | 5 | 0 |  |
| 2 | baseline | 1 | pay-receipts | scored | True | True | True | 39.74 | 0.086 | 5 | 0 |  |
| 2 | baseline | 1 | customer-table | scored | True | True | True | 38.82 | 0.106 | 5 | 0 |  |
| 3 | baseline | 0 | invoice-notes | scored | True | True | True | 30.09 | 0.075 | 5 | 0 |  |
| 3 | baseline | 0 | pay-receipts | scored | True | True | True | 43.98 | 0.089 | 6 | 0 |  |
| 3 | baseline | 0 | customer-table | scored | True | True | True | 42.88 | 0.105 | 6 | 0 |  |
| 3 | stack | 1 | invoice-notes | scored | True | True | True | 30.84 | 0.075 | 5 | 0 |  |
| 3 | stack | 1 | pay-receipts | scored | True | True | True | 34.3 | 0.088 | 5 | 0 |  |
| 3 | stack | 1 | customer-table | scored | True | True | True | 39.92 | 0.109 | 6 | 0 |  |
| 4 | stack | 0 | invoice-notes | scored | True | True | True | 23.86 | 0.071 | 5 | 0 |  |
| 4 | stack | 0 | pay-receipts | scored | True | True | True | 41.42 | 0.104 | 7 | 0 |  |
| 4 | stack | 0 | customer-table | scored | True | True | True | 37.75 | 0.109 | 6 | 0 |  |
| 4 | baseline | 1 | invoice-notes | scored | True | True | True | 27.48 | 0.074 | 5 | 0 |  |
| 4 | baseline | 1 | pay-receipts | scored | True | True | True | 42.16 | 0.101 | 6 | 0 |  |
| 4 | baseline | 1 | customer-table | scored | True | True | True | 55.6 | 0.132 | 10 | 0 |  |
