# Reviewed measurement session, 2026-10-06

[Results](report/report.md) contain 96 reviewed descriptive metrics across 24 workload lanes. [Astra review](../../reviews/astra-final-results-r1.md) documents independent verification, actual runtime versions, transport differences and all exclusions. [Sol execution record](../../reviews/sol-final-measurement-session.md) preserves the pre-review handoff.

DDEV is blocked by this adapter's preparation sequence, and Guix by container sandbox restrictions. Neither has startup or repeat timings. Stack, Process Compose and services-flake retain occupied-port failures. Seven bad-configuration checks remain blocked. Unsupported checks are not passes.

The host used warm caches and default resource limits, with unrelated services running. First-task values are single observations from prepared checkouts; the table reports summed task-step time and separately retains wall spans. Repeated commands have 20 samples after 3 excluded warmups, including transport overhead. Recipes, Python/PostgreSQL/Redis versions and isolation boundaries differ. Host CLI transport does not imply native host services. These results establish no overall ranking, cold-install comparison or confidence interval.

There were 27 attempts: 15 completed, one interrupted dnvr run, then 11 completed after recovery. The partial run is retained and excluded. The original plan and resumption supplement remain unchanged. All 27 preexisting containers and original networks/volumes survived unchanged.

## Restore and verify

Run from the repository root. Each archive has repository-relative paths. The JSON receipts contain archive and individual file SHA256 hashes. Both archives were reopened and every file compared with the source before delivery.

```sh
tar -xzf bench/measurements/final-20261006-reviewed-1/raw-results.tar.gz -C .
tar -xzf bench/measurements/final-20261006-reviewed-1/session-evidence.tar.gz -C .
python3 -B bench/report.py --manifest bench/measurements/final-20261006-reviewed-1/manifest.json --out /tmp/stack-benchmark-report-new --require-complete
```

Use a fresh output directory. Generated timestamps and absolute manifest paths can differ; approved metric values and omissions must agree. Raw evidence remains diagnostic, with `meta.reportable=false`. The separate manifest selects reviewed metrics. The evidence archive includes chronological inventories, interruption cleanup, original pending-review manifest, executor receipts and Astra audit receipts.
