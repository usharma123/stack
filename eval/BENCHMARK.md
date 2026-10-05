# Command-entry benchmark

`harness/paired-host.py` compares the released 0.1.4 host binary with an explicitly supplied candidate. It launches both directly on the same host, without Docker transport. Run it once the candidate is built and other performance work has stopped. Its host lock prevents overlapping copies of this harness, not unrelated builds or benchmarks.

```sh
python3 eval/harness/paired-host.py \
  --baseline eval/results/latest-2026-10-05/package/package/binaries/darwin-arm64/stack \
  --baseline-build-info eval/results/latest-2026-10-05/package/package/build-info.json \
  --candidate target/release/stack \
  --mise eval/results/latest-2026-10-05/bin/mac/mise \
  --pairs 50 --warmup 5 --order alternating \
  --out eval/results/paired-host-NEW-RUN
```

Select `darwin-x64`, `linux-arm64` or `linux-x64` and the matching `bin/linux/mise` on other supported Unix hosts. The baseline must match the included release metadata hash and version 0.1.4. This checks artifact identity, not independent publisher attestation. Use `--order random --seed 20261005` for reproducible randomized pair order. `--cases tool-only` selects a short service-free smoke run.

Each variant and case gets a disposable project, HOME, mise install/cache/config/state, Pitchfork state, and Stack state/cache. The tool-only fixture requests uv 0.12.23. The verified fixture adds PostgreSQL 17 and Redis 8 presets. Setup explicitly installs uv because `exec -- true` can leave it uninstalled, resolves the other dependencies and starts services before sampling. This can take minutes on a fresh isolated HOME and needs network access and a working mise backend. The tool-only case requires uv; verified requires uv, Pitchfork, postgres, psql, redis-server and redis-cli. Failed or missing executable lookups invalidate setup, even if both variants lack the same tool.

Provider identities must match between variants for each case. Direct executables retain raw SHA-256 checks. Recognized mise Conda wrappers retain their path and raw hash, plus the underlying executable path/hash and the wrapper text/hash normalized only for the verified isolated install prefix. The wrapper must match the observed template exactly, with its environment exports and exec target pointing to that same install. Activation scripts sourced by the wrapper are hashed too. Unknown wrapper logic, changed underlying binaries, changed activation scripts or escaped install paths fail verification. Artifact receipts record the literal and resolved prefix, replacement token and occurrence count so a raw wrapper hash difference remains auditable.

Conda also relocates install-prefix strings inside PostgreSQL's macOS executables and regenerates their ad-hoc signatures. For these executables, the raw hash is retained and a second hash canonicalizes only exact install prefixes in printable, null-padded strings within `__TEXT,__cstring`. The signature must pass `/usr/bin/codesign --verify --strict`; only its SHA-256 CodeDirectory page digests are zeroed for this comparison. All other binary bytes and signature metadata remain in the normalized hash. Receipts list each changed string's byte range, literal value and normalized value, and each signature digest range. Unsupported relocation formats fail closed. `evidence/provider-wrapper-identity-2026-10-05.json` records read-only checks against two existing installations with equal resulting identities for postgres, psql, redis-server and redis-cli, despite different raw wrapper hashes. This is setup evidence, not a performance run.

The measured commands are `stack exec -- true` and `stack exec --require-all -- true` against live services. Timing uses `perf_counter_ns` immediately before host process launch until child exit. It includes process creation, Stack verification, mise activation and `true`; setup, service identity probes and receipt writes are excluded. Warmups run in pairs and remain in raw receipts, but do not enter reported distributions. Alternating order swaps the first variant on every pair. Each randomized pair chooses its order using the recorded seed.

Before and after verified sampling, the application URLs must reach the PostgreSQL and Redis data directories returned by that variant's session. The variants have separate service processes and data. Cleanup calls `down` per isolated project, checks confirmation, and stops only each isolated Pitchfork supervisor using the executable path and hash retained during setup. Missing or changed supervisor executables and failed stop commands invalidate cleanup. Exceptions, timeouts, SIGINT and SIGTERM pass through cleanup. A command timeout kills its process group, then bounds output draining and process reaping so a detached descendant holding a pipe cannot delay cleanup indefinitely. A cleanup failure invalidates the run and retains its temporary state for diagnosis. Do not stop any global supervisor or unrelated service to repair a benchmark.

The new output directory contains metadata, binary/backend hashes, installed-tool receipts, fixture files and lock hashes, full stdout/stderr, append-only events with exit outcomes and nanosecond timings, and a summary. Existing output directories are refused. p50/p95 use nearest-rank percentiles of successful measured samples. Failures are counted separately. Paired deltas include only pairs where both commands succeeded and use candidate minus baseline milliseconds. Any setup, identity, warmup, sample or cleanup failure invalidates the run. A single warmed host run gives descriptive latency evidence, not an agent-productivity or universal performance claim.

Run the offline checks without installing tools or starting services:

```sh
python3 -m unittest discover -s eval/harness -p test_paired_host.py -v
python3 -m compileall -q eval/harness
python3 eval/harness/verify-comparison.py eval/results/latest-2026-10-05
```

# Historical comparison

`evidence/released-0.1.4-context.json` is a small extract from the October 5 comparison, with hashes pointing back to the untouched local summary and report. Raw receipts and binaries remain under ignored `eval/results/`; the report remains at `eval/stack-0.1.4-report.html`. Do not commit that raw directory or downloaded binaries.

The historical runners require prepared `ev-*` ARM64 Linux images and downloaded tools. `current-benchmark.py` measures Docker exec transport as part of command entry. Setup steps differ between tools and must not be ranked. Correctly configured mise, Flox, Devbox and devenv recipes isolated services; initial configuration mistakes do not establish fundamental defects. Nix covers a toolchain shell, Compose covers private service networks without the Python fixture, and Pixi uses explicitly scripted manual service lifecycle.

Use a new results directory with the prepared package and tools to replay those runners. `release-native.py` and `release-contract.py` now choose the matching host artifact. `verify-comparison.py` checks saved historical claims read-only. `render-comparison.py RESULTS --report PATH` renders derived outputs and refuses existing report/summary files unless `--overwrite` is explicitly supplied. Rendering does not rerun workloads or refresh original evidence.
