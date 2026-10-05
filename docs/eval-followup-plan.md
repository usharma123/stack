# Evaluation follow-up implementation plan

Source: `eval/results/stack-0.1.3/summary.json`, native and fresh-cache lock results,
and the current implementation. The 0.1.3 report and recorded runs are historical
evidence and must not be rewritten as results of these changes.

## Judgment

The Postgres/Redis bundle and session contract is demonstrated. The release is a
credible controlled-pilot base, but exact tool reproducibility, custom identity,
and unattended cleanup remain incomplete. Setup timings are single observations;
the October 2 competitor results cannot establish a speedup.

## Implementation sequence and acceptance

1. **Exact versions.** Extend the lock contract to record requested and resolved
   tool and preset-service versions, including provider tools. Render and install
   the resolved versions. Preserve pins on ordinary compile, resolve changes on
   explicit update, and reject missing or stale pins in locked mode. Define legacy
   lock migration and platform-specific resolution explicitly. Keep inspect
   read-only and avoid resolving versions on status/exec. Do not mistake a bundle
   commit or an exact version string for an artifact checksum guarantee. Test
   upstream movement with a controlled resolver and fresh cache, changed requests,
   failed resolution, migration, and generated service versions. Remove floating
   examples using verified available versions. Preserve install failure semantics
   or document and test any intentional earlier resolution error.

2. **Native lifecycle coverage and socket diagnostics.** Make service/session E2E
   scenarios portable and run native macOS plus Linux in CI. Include independent
   checkouts, foreign instance refusal, TTL/owner death/active exec, MCP, generation
   changes, and local OCI where feasible. Any separate OCI job must be explicit.
   Check the actual effective Pitchfork socket path in doctor and before install
   or service startup, accounting for environment overrides and byte limits.
   A 118-byte isolated provider-state path failed on macOS; ordinary long project
   paths were not shown to fail. Test path boundaries and Unicode byte lengths.

3. **Custom identity.** Introduce an opt-in reusable identity-probe contract in
   bundle services. Compare a live identity reached through the application's
   connection settings against expected instance identity. A readiness command
   exiting zero alone is insufficient. Use strict time/output bounds and process
   descendant cleanup. Keep services without probes explicitly liveness-only.
   Test a correct instance, a foreign healthy endpoint, malformed/missing output,
   timeout, output flooding, and generation changes. Document that bundles and
   their executable probes are trusted code, not a sandbox against malicious authors.

4. **Cleanup after deletion and unattended expiry.** Persist enough ownership and
   provider information in machine state to stop owned services after the project
   directory disappears. Reconcile live ownership before signalling; stale PIDs,
   PID reuse, foreign ports, or reused project paths must never authorize killing
   another process. Keep ownership records on uncertainty/failure and report
   gc_incomplete honestly. Do not recreate deleted project contents as a workaround.
   Add an explicit opt-in foreground periodic GC mode suitable for a supervisor;
   do not install an OS service automatically. Preserve active-exec protection,
   owner-death policy, lifecycle locking, and crash recovery. Test deletion, stale
   ownership, foreign replacement, concurrent renewal, and expiry without another up.

5. **Measured pilot.** Add a bounded repeatable pilot runner with concurrent isolated
   projects, bundled tasks, fresh/cached runs, and controlled runner/service failures.
   Record task outcomes, wrong-instance incidents, orphan counts, and latency samples
   with run identity and platform/provider metadata in a NEW output directory.
   Distinguish scripted concurrency from actual independent agent task completion.
   Run the local scripted pilot if dependencies are available. Do not invent agent
   productivity findings; document a reproducible real-agent protocol and any missing
   external execution as a remaining validation gate.

## Delivery and review loop

Claude Opus 5.5 implements the sequence, with regression tests and updated product
documentation. Preserve pre-existing dirty files, especially the corrected MCP
assertion and all evaluation artifacts. Do not commit, push, merge, or release.
Record changed files, exact checks, results, and unverified gates in
`docs/eval-followup-implementation.md`.

A separate Astra review examines the full final diff and acceptance evidence,
especially lock compatibility, provider semantics, identity and process ownership.
Concrete findings return to a fresh Claude task. A fresh Astra task reviews each
correction round with previous objections included. Stop only on evidence-backed
approval or an explicit unresolved blocker, never on a fabricated CI or pilot claim.

Local validation includes Rust tests and clippy, package tests where affected,
targeted new regressions, and available real native/Linux lifecycle runs. Adding a
CI job does not mean the remote job passed. Preserve the original benchmark data.
