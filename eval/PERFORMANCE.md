# Verified command-entry latency

On October 5, 2026, the candidate reduced median command-entry time with verified PostgreSQL and Redis from 175.8 ms to 108.0 ms, a 38.5% reduction on one macOS ARM64 host. Identity verification still runs for every command.

| Case | Released 0.1.4 p50 | Candidate p50 | Released p95 | Candidate p95 |
| --- | ---: | ---: | ---: | ---: |
| Tool-only | 38.3 ms | 37.1 ms | 43.3 ms | 47.4 ms |
| Verified PostgreSQL + Redis | 175.8 ms | 108.0 ms | 202.0 ms | 136.5 ms |

Each case used 50 alternating baseline/candidate pairs after five warmup pairs. All 200 measured commands succeeded. Service identities matched before and after sampling, provider comparisons passed, and cleanup reported no errors. The verified median paired difference was -66.2 ms. Tool-only timing was roughly unchanged, with a higher candidate p95; this is not a claim that every command became faster.

The host was Darwin 24.6.0 ARM64 with 14 logical CPUs. Each variant used separate HOME, provider state, service processes, and data directories. Timing includes direct host process creation through child exit, excludes setup, and includes no Docker transport. Other applications were not CPU-isolated. This is a warmed, single-host measurement, not a cold-install, agent-productivity, cross-platform, or competitor-win claim.

The baseline is the published 0.1.4 artifact from source `2e4eea4293a4001ce98a5fd374b83a1476b8ec52`. The candidate was built from the working-tree changes in this PR. Binary hashes, provider identities, individual timings, validation results, and hashes of the full local receipts are in [the evidence file](evidence/paired-host-2026-10-05.json). Follow [BENCHMARK.md](BENCHMARK.md) to reproduce the run.

The first attempt stopped before measurements because generated provider wrappers contained different installation paths. Its failure remains in the evidence. The corrected harness compares exact wrapper logic and underlying executable content, accounting explicitly for verified Conda Mach-O relocation. It retains the raw hashes and normalization details.

## Implementation

Subprocess capture wakes on pipe readiness instead of fixed polling delays. PostgreSQL and Redis probes use bounded capture, and independent service checks run on at most eight threads. Mise environment and daemon queries run concurrently. On non-Linux Unix, PID liveness uses `kill(2)` directly. Existing identity, generation, lease, and endpoint-poisoning checks remain active.

Set `STACK_TIMINGS=1` to print phase timings to stderr. For example, `STACK_TIMINGS=1 stack exec --require-all -- true` reports compilation, provider queries, verification, and lease work. JSON stdout remains separate. Leave timings disabled for comparative wall-clock measurements.

## Validation

The candidate passed 115 Rust tests, Clippy with warnings denied, 20 Node tests, the explicit real-mise configuration-boundary test, and 21 offline benchmark/renderer tests. Seven native lifecycle scenarios passed; the OCI scenario passed separately against a disposable local registry. The four-project stress pilot completed 16 of 16 tasks, handled four injected failures, and reported zero wrong-instance connections and zero orphaned services. Pilot timing is not compared here because that run overlapped other validation work.

The repository has pre-existing rustfmt differences; this change does not claim a clean repository-wide formatting check. Linux and Intel macOS execution remain for CI.
