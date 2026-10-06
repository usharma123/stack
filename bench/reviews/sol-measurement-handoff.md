# Final measurement handoff

Read-only design for Opus 5.5. No implementation, tests, services or measurements ran.
Snapshot 2026-10-06 14:51:31 UTC, HEAD `66d740b9f86df0edf2e5643ece3943c9a7ae8a07`,
branch `codex/realworld-competitor-bench`; container edits remain concurrent.
`run.py` SHA256 `92e0239e3b2f275bd54d12f2bb4b75862fc2621f97363c6cbb03d57c153b1651`;
`rwb/scenario.py` SHA256 `3e3d818783446672a4b4d3c627677b286213b4eb70fa6131bfdedacdb22db088`.
This does not duplicate the approved core review or the active container review.

## Decision

Use a parent-owned session manifest and a small `bench/report.py` selector/renderer.
Keep per-run `meta.reportable=False`: those files are diagnostic source evidence.
The final report records `reported_timings` and exclusion reasons separately. A runner
`--measurement` flag cannot establish serial execution, correct revision, review or
receipt integrity. No flag, path prefix, default 20 repeats or exit 0 should promote a run.
In the derived report, each attempt's `reportable` is true only when at least one
metric passes every eligibility gate; keep coverage and correctness statuses separate.

Before running, the parent declares a final session's ordered tool/variant/output paths
and sample policy. After checking results, the parent selects exact attempts and names
the applicable source/result review. The report command verifies that selection. Smoke
runs stay outside the session; reruns remain separate attempts with exclusion reasons.
The parent declaration attests that builds, installs and other benchmark work did not
run concurrently. Interval checks can detect overlapping selected runs, but cannot prove
the whole host was idle.

## Minimal implementation

1. `run.py`: record `started_unix_ns`, `finished_unix_ns`, `keep`, and requested Docker
   resources. UTC strings currently have only second precision, which cannot prove order
   for equal timestamps. Keep the current UTC labels for humans. Clear public
   `meta.timings` on invalid or provisioning-blocked runs; raw timings remain untouched
   in `steps.jsonl`. Make `render_summary` independently suppress numeric timing tables
   for those conditions, even if passed an older meta containing numbers.
2. One report module owns the explicit manifest schema, integrity/eligibility checks,
   receipt selection and final JSON/Markdown output. Reuse `verify.app_result` and
   `stats.summarize_ns`; do not create a second workload or service runner.
3. Explain in README that run summaries are diagnostics; manifest-selected reports are
   the reviewed final artifact. Keep the 26-entry frozen roster and all statuses. A
   missing or blocked lane gets a row with its evidence/reason, never an invented timing.

The manifest needs these fields, without a general scheduling framework:

| Location | Required data |
|---|---|
| Session | schema version, session id, purpose `final-measurement`, plan creation UTC, ordered frozen tool/variant roster, requested repeats/warmups, timing policy, parent serialization declaration and review reference |
| Protocol | reviewed harness commit plus file hashes, fixture hashes, allowed per-lane config/shared-glue hashes and versions/deviations, platform/transport and cache/resource policy |
| Attempt | tool/variant, exact result path, run id, selected/excluded/blocked/untested state, reason, result-review reference, hashes of meta/outcomes/steps and referenced logs/artifacts |
| Stack | product source revision or source-tree fingerprint, binary SHA256 and build receipt; harness commit alone does not identify the built product |
| Derived report | manifest hash, source attempt hashes, eligibility per metric, omitted reasons, full outcome rows, reported timings with evidence sequence numbers and exact sample counts |

Run metadata already contains host/image identity, harness files, fixture/config hashes,
pins/options, scopes and cache notes. Versions are in raw `tool-version` and per-checkout
`*-tool-versions` output, not a guaranteed parsed field. Shared files under
`adapters/_shared` are not covered by the adapter's own config tree; the session protocol
must hash that glue too. Do not require clean git state by itself: match the reviewed
bytes, including any approved dirty files. Unreviewed source changes require review and
a fresh affected run, not updating the expected fingerprint to force acceptance.

## Receipt and timing gates

- Require existing, parseable meta/outcomes/steps/logs; unique step sequence numbers,
  known statuses, no duplicate checks, valid referenced sequence numbers and existing
  referenced raw files. Verify all manifest hashes. A missing outcome is not unsupported.
- Require exact tool/variant/run id, reviewed harness/fixture/config/glue, and the
  declared sample policy. Verify actual version output and retain deviations. Check
  selected run intervals in manifest order for non-overlap. Refuse `--keep`, incomplete
  runs, harness errors, unclean teardown, invalid runs and provisioning-blocked runs
  for timing. Still include their status/evidence rows. `valid=True` alone is insufficient.
- Do not reject a trustworthy run just because one tool check fails. Retain the failure;
  omit that metric and any metric whose required checks fail/block. A per-check block is
  different from a whole provisioning-blocked run. Unsupported/not-applicable/observed
  cells retain their modes/reasons and carry no pass-derived performance value.
- Start with the existing four comparison metrics only: `first_task.a`, `first_task.b`,
  `repeat.entry`, `repeat.app_read`. Other phase numbers remain diagnostic and are not
  automatically promoted. This avoids treating setup scopes as equivalent or reading
  `ready.a` as an initial-start metric when restart overwrites that key.
- `first_task.<co>` needs its existing `ok=True` and all six gates passing:
  setup/start/deps/migrate/CRUD-cache/tests for that checkout. Its step ids must exist,
  have successful non-timeout receipts, include the declared setup/start/deps/identity
  and workload labels, and sum to `steps_ns`. Preserve `wall_ns` as a different
  measure. Record its prepared-checkout boundary and excluded preparation scope.
  Do not use a successful first task to imply the later isolation/persistence checks pass.
- Each repeat metric needs its outcome `pass`, all declared non-warmup samples present
  and successful, and its warmup receipts present/successful. Recompute summaries from
  those raw sample ids instead of trusting stored percentiles. For app reads, also
  reparse the JSON receipt and require `item.sku == "keeper-a"`; a raw exit 0 can accompany
  a semantic failure. Warmups never enter distributions. Missing inner timing is null,
  not zero; report inner availability/count separately from process failures.
  The concurrent-checkout condition additionally needs both checkout workloads and
  `isolation` passing before sampling. Otherwise omit the metric from that condition;
  surviving A-only samples do not establish the promised two-checkout workload.
- Preserve all raw command timings, including failed checks, only as diagnostic
  receipts. Final `reported_timings` contains eligible metrics only. Empty distributions
  or failed/blocked checks never receive a numeric p50/p95 or task time.

Output describes this particular host, image/cache state, transport and recipe.
First-task A/B are single observations, not startup distributions. Twenty repeated
entries/reads in one run yield descriptive nearest-rank p50/p95 for that run; they are
not 20 independent setup trials or proof of a performance advantage. No overall rank,
score, confidence claim, universal cold-install claim or native-macOS inference.

## Minimum offline tests

- An unselected smoke remains diagnostic, including a smoke with 20 repeats. A declared,
  reviewed, serial valid attempt emits only its eligible metrics.
- Reject overlapping runs, mismatched tool/revision/config/glue, tampered hashes,
  duplicate/missing steps, missing referenced logs, incomplete outcomes and `keep=True`.
- Invalid or provisioning-blocked meta containing attractive timing numbers produces
  no numeric report/summary. Clean blocked Guix still contributes a blocked roster row.
- One repeat sample fails, or exit 0 returns the wrong keeper: no timing for that check,
  failure remains visible, unrelated eligible metrics remain available. Failed warmup,
  missing samples and policy mismatch are excluded with explicit reasons.
- A first-task gate fails despite `ok=True`: exclude it. Recomputed task sum/sample
  percentiles must agree with raw receipts; warmups are excluded, absent inner values
  stay null. Reruns preserve both attempt directories and select only the declared one.

## Reproducible invocation

These commands are proposed, not executed. Create the session plan first with these
paths and the final reviewed source hashes. Each invocation is synchronous; record its
exit and continue to preserve coverage even when a lane is invalid. Do not use `&`,
parallel jobs, an active builder, or `--keep`.

```bash
RWB_SESSION="bench/results/final-20261006-reviewed-1"
RWB_STACK_BINARY="/tmp/stack-bench-build/target/release/stack"
RWB_STACK_SHA256="$(shasum -a 256 "$RWB_STACK_BINARY" | awk '{print $1}')"
mkdir -p "$RWB_SESSION"
run_final_attempt() {
  RWB_TOOL="$1"
  shift
  if python3 bench/run.py --tool "$RWB_TOOL" --out "$RWB_SESSION/$RWB_TOOL" \
      --repeats 20 --warmups 3 "$@"; then RWB_RC=0; else RWB_RC=$?; fi
  printf '%s\t%s\n' "$RWB_TOOL" "$RWB_RC" >> "$RWB_SESSION/exitcodes.tsv"
}
run_final_attempt stack --option "stack_binary=$RWB_STACK_BINARY" \
  --option "stack_sha256=$RWB_STACK_SHA256"
for RWB_TOOL_NAME in mise flox devbox devenv nix pixi compose; do
  run_final_attempt "$RWB_TOOL_NAME"
done
# Supply a release digest verified independently, not a hash invented for this run.
RWB_GUIX_VERIFIED_SHA256="<verified-64-hex-release-digest>"
run_final_attempt guix --option "guix_binary_sha256=$RWB_GUIX_VERIFIED_SHA256"
```

If no verified Guix digest exists, run Guix without that option to retain the actual
exit-77 blocked receipt; select it as evidence-only, with no timing. Do not pass
`guix_daemon_flags=--disable-chroot` implicitly. An explicitly chosen flag is a separate
named condition with its own pins. Additional 17 lanes use the same sequential command
shape, after their reviews, within the unchanged 26-entry plan.

After result review, proposed report command:

```bash
python3 bench/report.py --manifest bench/measurements/final-20261006-reviewed-1/manifest.json \
  --out bench/measurements/final-20261006-reviewed-1/report
```

Use a new output path. Do not rewrite source meta to set reportable or overwrite smoke
directories. Keep selected raw receipts and earlier failed attempts; the final delivery
must contain the selected evidence or a durable hash-verified bundle, not just links
into ignored `bench/results`. The final receipt names all 26 coverage states, the
selected attempts and review, eligible metrics, omitted reasons and evidence hashes.
