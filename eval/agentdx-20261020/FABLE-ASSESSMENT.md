# Stack after the AgentDX remediation: state, boundary, and what to sharpen

Author: Claude Fable 5.1, 2026-10-20. Assessment only; nothing here was implemented or
authorized by this document.

| | |
|---|---|
| Inspected source | `c73f38ac95577c69899c04ab1ca7f9f5f5055cab`, branch `fix/agentdx-findings-20261020`, pushed to `origin` at that SHA. No PR, merge or release |
| Product base | `8b3402fa1db8f11917b5ba33a333c9093545d9ce` (Stack 0.1.22 source; 0.1.22 was not on npm during the evaluations, and this branch publishes nothing) |
| Benchmark base | `065086dba914b886d1f46763a80b30c045e16021`; `bench/` unchanged since except `README.md` |
| Tested binary | `cargo build --locked` debug, sha256 `84f3c9064f5e7f17e2d0e3e754a8bb223d5b673225bcc10117aed43678973287` (tested head `34369ef`; the one commit after it, `c73f38a`, changes only `eval/` docs, so the inspected SHA runs this binary) |
| Independent approval | GPT-6 Astra, review of exactly `c73f38a`, no blocking findings: `/Users/utsavsharma/.t3/review-evidence/agentdx-product-c73f38a.fuKCEb/REVIEW.md` (outside the repository) |

Evidence used: [REMEDIATION-LEDGER.md](REMEDIATION-LEDGER.md), [REMEDIATION.md](REMEDIATION.md),
the unchanged [Opus](opus/REPORT.md) and [Astra](astra/REPORT.md) reports and
[SYNTHESIS.md](SYNTHESIS.md) (historical findings, not an open bug list), the benchmark
[README](../../bench/README.md) and [protocol](../../bench/AGENTDX-PROTOCOL.md), the round-5
[invariant table](../results/agentdx-product-corrections-r5-20261020/INVARIANTS.md) (git-ignored,
local), the independent pilot transcript review
(`/Users/utsavsharma/.t3/review-evidence/agentdx-product-a401cfe.rygjjib_/PILOT-REVIEW.md`), and the
source under [`src/`](../../src).

## 1. What Stack does today

Stack is a per-checkout owner of a project's tools, services and their endpoints. It borrows
installation from mise and process supervision from Pitchfork, and owns everything that makes
those safe for several concurrent agents: a port registry outside the ephemeral range
([`ports.rs`](../../src/ports.rs)), a checkout token derived from the directory's device, inode and
creation time ([`checkout.rs`](../../src/checkout.rs)), a session record with a configuration
fingerprint, lease and per-service verification ([`session.rs`](../../src/session.rs)), a data
directory ledger bound to resolved path and inode ([`datadirs.rs`](../../src/datadirs.rs)), and a
signal registry that passes cancellation on to the commands it runs
([`process.rs`](../../src/process.rs)). The CLI and the 14-tool stdio MCP server share one result
envelope ([`mcp.rs`](../../src/mcp.rs)).

The agent loop, end to end, with what each step decides for the agent:

1. `stack init` then `stack compile`: exact versions pinned, checksums embedded, ports assigned
   from the registry. The agent never picks a port, and the version choices were made once by
   whoever wrote `stack.toml`; the agent reuses the committed pins.
2. `stack up --ttl 30m`: gc of expired sessions, lock, locked compile, root and platform refusal,
   socket preflight, install, stop of a stale generation, port-conflict check, then starts in
   `depends_on` waves with each service's `setup` run just before it. Every service is verified
   (supervisor state, PID, port, TCP, Postgres or Redis identity or an opt-in probe) before the
   session is recorded. The readiness checks, `setup` commands and dependency order are the
   configuration author's, defined once in `stack.toml` or a bundle; the agent reuses that
   committed contract instead of writing a readiness loop or a migration chain per task. Stack
   itself chooses no application version, migration or readiness logic.
3. `stack run test` or `stack exec --require-all -- cmd`: services are re-verified on every call,
   unverified endpoints are poisoned to `unverified.stack.invalid` rather than unset, and the
   execution is registered so gc and `restart` know the session is busy.
4. `stack restart --changed` after editing code a service loaded; `watch` reports, nothing reloads.
5. `stack down` when done. `stack gc` and `gc --reclaim-data` later, for what a removed
   checkout left.

Who should use it first: one developer or a small team running several local agents in Git
worktrees of a Postgres or Redis application on macOS arm64 or Linux arm64, and hosted agents
that run as an unprivileged user. Not yet: Intel Macs with services (no Pitchfork build), anyone
who needs a security boundary (Stack bounds processes, it does not sandbox them), and anyone
expecting Stack to create or remove worktrees (it never does, see
[worktrees.md](../../docs/user/worktrees.md)).

What is still unmeasured: whether agents finish with less human help. The only real-agent
evidence is six Opus 5.5 trials on an older binary (`1cad8602…`), three small synthetic apps,
both arms, one repetition, warm cache. All six solved the task in both arms, so the tasks do not
separate the arms; no human has reviewed a transcript, so no trial counts as autonomous under the
protocol; 242 s of agent time and $1.04 of CLI-reported list cost are not billing records. There
is no hosted measurement, no speedup, and no statistical inference anywhere in this evaluation.

## 2. What the fixes establish, and what they do not

The branch has 46 commits after the benchmark base. The ledger maps 40 of them to findings: 31
from the two hands-on sessions and the synthesis, then 9 corrections from four independent review
rounds (4, 2, 1, 2). The rest are evaluation documents. Gates at the tested head: 530 Rust tests
(13 ignored, of which 12 are inner halves run by their outer tests and one is a `procinfo` helper),
Clippy clean, 41 Node tests plus 1 skip, 2 real-mise boundary tests, 124 benchmark tests, 12 of
12 macOS e2e scenarios including OCI, six cold regression checks `FIXED` with controls, 8 of 8
validation cells valid. Linux evidence is local Docker LinuxKit arm64 only: 239 unit tests, 27
affected runtime tests, six regressions fixed, 11 e2e scenarios with OCI skipped. There is no
Intel or x64 runtime evidence and no hosted-runner run.

What the fixes establish, in the order an agent meets them:

- **The documented loop works.** `run` and MCP `stack_run` give the same verified loopback URL
  as `exec` (R1). Both pilot Stack trials ran `stack run test` without a workaround.
- **Determinism that used to depend on the shell.** New Postgres clusters are `UTF8` with
  `C.UTF-8` collation whatever the caller's locale, and Redis starts under a locale the host
  lacks (R3, N1). Task edits no longer invalidate the session (R6c). fnox is installed before
  secrets are resolved (R4).
- **A dependency contract.** `depends_on` orders starts, restarts dependents, and refuses a
  dependent restart whose dependency is not verifiably ready; `setup` runs before each start
  (R6a, R6b, F3, R3-F3).
- **Cancellation that holds under adversarial schedules.** External SIGINT, SIGTERM and SIGHUP
  reach the command's group; a signal during a `setup` cancels the start whatever the setup's
  exit; admission and retirement decide at one boundary under one lock, so a signal is never
  judged after the step it cancelled and never sent to a reused group ID (R2, F2, R3-F2, R4-F1,
  R5-F1, R5-F2). Four review rounds found the previous fix wrong in a narrower case each time;
  the invariant table states, per caller and phase, what holds and which test covers it.
- **Reclaim that cannot be redirected.** `gc --reclaim-data` removes only a directory `up`
  recorded, reached without following links, matched by device and inode, through the
  descriptors it checked (L1, F1).

What remains, sorted by kind, because they need different owners:

| Kind | Items |
|---|---|
| Actual capability gaps | No database handoff or snapshot between checkouts (H1). Custom services without an identity probe are liveness-only. `status` does not flag a dependent started before its dependency's last start. No hosted, x64 or Intel macOS runtime evidence. No terminal-attached end-to-end test. |
| Deliberate safety defaults | Plain `down` keeps the shared supervisor (`--stop-supervisor` is opt-in; both pilot agents left one). Data reclaim is explicit only; quarantined `.replaced-*` data is never removed, and data recorded before this branch without a resolved location and inode is retained until a verified `up` records that binding. `gc` refuses to stop a deleted checkout's daemons by name. `restart` never starts a dependency it was not given. |
| Known internal limits | Waits for the signal thread are unbounded. The cancellation grace is timed from the run's first observation of a forwarded signal. A descendant that leaves its process group escapes group cleanup. A signal the kernel has not delivered when a command retires belongs to the next command or ends Stack. |
| External prerequisites | A published 0.1.22 or later. A hosted Linux runner. Pitchfork for Intel macOS. Root hosted images need an unprivileged user. |

Two caveats on the evidence itself. The six-trial pilot ran before the nine review corrections
and was not rerun; the final binary has only scripted and unit evidence for the cancellation
paths. And the pilot's isolation was requested, not proven: every init record lists 13 plugins
despite the safe-mode flags, and the `Read`/`Edit` denials are not a Bash sandbox. Nothing in
the transcripts shows plugin, source, grader or oracle access. That is an observation, not proof.

## 3. The product boundary, and which concepts to sharpen

**Position.** Stack should own exactly one thing: the execution environment attached to a
checkout, meaning its pinned tools, its services, their verified endpoints, and the lifetime
and reclaim of everything it started or wrote. It should not own Git worktrees, agents, or a
security boundary.

The evidence supports this boundary from both sides. Every failure the testers hit that cost
them a decision was inside it: a wrong URL, an orphaned command tree, a locale-dependent
cluster, a stale dependent, a port and data residue after removal, a lingering supervisor.
Nothing they hit was "Stack should have made my worktree": both testers made worktrees with
Git in seconds, the documented Worktrunk hooks are blocking pre-start and pre-remove calls
(documented, not run by either tester), and the harness and pilot kept Stack out of checkout
creation entirely. Owning worktrees would
duplicate Git and collide with the runners (Worktrunk, T3, agent CLIs) that already own them.
Containers buy isolation the user does not want to pay for in weight and spin-up, and would not
fix the issues above; they would hide them. Becoming an orchestrator would put Stack in
competition with the agent harness that calls it over MCP.

The cost of this boundary is that disposal depends on the caller: a hook that runs after a
worktree is deleted is too late, and a checkout removed while services run needs a person to
follow `gc_incomplete`'s inspect-then-stop steps. That cost is real and is the first roadmap
item below. It is a lifecycle gap inside the boundary, not a reason to widen it.

Four concepts, ranked. The first two are where the product's remaining friction lives; the
last two are already strong and need only small sharpening.

**A. One identity for every resource a checkout owns (highest).** Today three machine-state
records key the same checkout three ways: the session index by a hash of the path
(`state::project_key`) with device and inode stored inside; port reservations by path only
(`ports::Reservation.project`, pruned when the path no longer exists); the data ledger by path
plus the checkout token. The checkout token is the right identity, since it changes when a
directory is recreated at the same path and every service process carries it. Invariant to
state: every record Stack writes for a checkout, and every process it starts, is bound to the
checkout token, and any operation on a checkout resolves records by token before path.
User-facing consequence: `status`, `gc` and `down` can say "this checkout, not the one that was
here before", a recreated directory can never inherit ports or a session by accident, and
disposal becomes a function of identity rather than of a path that may be gone.

**B. A disposal operation with a stated state machine (highest).** The lifecycle exists but is
spread over `down`, `down --stop-supervisor`, `gc`, `gc --reclaim-data` and the hook ordering
in the docs. States: `launching`, `verified`, `stale` (fingerprint changed), `busy`
(registered executions), `expired` (TTL idle or owner dead), `stopped` (services confirmed
gone, ports reserved, data kept), `released` (ports back, data reported), `disposed` (data and
mise per-project state gone, supervisor stopped if nothing else uses it). Invariant: each
transition is explicit, idempotent and safe after the directory is gone, and the only
destructive transition (`released` to `disposed`) needs an explicit flag. Consequence: a runner
or hook calls one operation at the end of a task and gets one answer about what is left; an
expired lease can carry a disposal policy chosen at `up`. The hard case is a checkout deleted
without `down`. `down` already stops every process carrying the token by PID and start time,
which is the identity-checked stop `gc` refuses to do by daemon name. Whether `gc` may use that
same token-bound stop for a gone checkout is a safety-default decision for the owner (section 7),
not a technical blocker.

**C. Verified endpoints and the dependency contract (strong, two small sharpenings).**
Fingerprinting, poisoning and identity checks are the product's best idea and survived both
testers intact. Two gaps: a custom service without a probe is labelled `liveness`, and
`--require` accepts it; and a dependent that started before its dependency's current process
is not flagged by `status` although `ServiceRecord.started_at_ms` makes the comparison cheap.
Consequence of closing both: "all green" stops meaning "an API holding a dead connection".

**D. CLI and MCP consistency (already good, one asymmetry).** One envelope, one `outputSchema`,
usage errors with hints on both surfaces. The remaining asymmetry is semantic: an attached
terminal command stays in Stack's group and receives only SIGTERM to its PID, while captured
and MCP commands get a group and all three signals. The docs state it; a test does not exercise it.

## 4. Native isolation and worktree ergonomics

**Collisions.** Disjoint ports, PIDs, data directories, identity tokens and seeded rows were
observed by both testers across concurrent worktrees, and e2e scenario 1 checks it. Not
observed: two independent agents at once on one host. The registry's ports are path-keyed, so a
recreated checkout at the same path reuses its predecessor's ports, which is harmless for ports
and is exactly why data is token-keyed instead.

**Disk accounting.** Tool installs are shared through mise and are not Stack's to reclaim.
Service data lives under mise's state directory per project hash; `gc` reports its size and
removes it only on request. Three things accumulate with no reclaim path at all: quarantined
`.replaced-<time>` data from recreated paths, data of checkouts that 0.1.22 ran and that were
deleted before this branch (the ledger learns a directory only from an `up`), and mise's small
per-project state files. Those are the disk-growth vectors on a host that
churns worktrees, and none is measured today beyond the benchmark's `disk_after_down` receipt.

**Resources attached to worktrees versus owned copies or snapshots.** The discussion considered
one visible folder with hidden snapshots. The evidence says: keep resources attached to the
Git worktree, do not build owned workspace copies now. The worktree holds uncommitted work and
is the user's; a Stack-owned copy would create a second place for that work to go stale, which
is the duplication the user dislikes, and would need a merge-back story Git already provides.
What agents actually asked for was data, not files: seeding one checkout's database from
another (H1), which both testers did through code and migrations. That is a bounded feature on
top of the data ledger, not a workspace model.

**Fast spin-up.** The dominant cold costs observed were first execution of newly installed
binaries on macOS (a native-arm ledger `up` of 381 s, then 91 s on the next run) and
`initdb`; the cold validation cells later that day started the service apps in 3 to 13 s once
the host had seen the binaries. No per-phase breakdown exists across many fresh
worktrees. Before any snapshot or template design, the cheap experiment is: ten fresh worktrees
of the ledger fixture, warm cache, `STACK_TIMINGS=1 stack up` each, then `down`, delete, `gc
--reclaim-data`, with per-phase times and a disk and port census after each step. That gives
the startup profile and the residue profile in one afternoon on the current binary and would
either justify a data-template feature (initdb once per Postgres version, copy per checkout)
or show the time is elsewhere.

**Concrete lifecycle direction, as a proposal.** Attach a disposal policy to the lease at `up`
(`stop` or `dispose` on expiry), make `gc` reclaim by token rather than path, and add one
end-of-task operation that performs `stopped` to `released` always and to `disposed` with a
flag. None of this is process isolation, and none of it should be described as such.

## 5. Roadmap

Immediate, in order. Each is small and has a test that could fail.

1. **Disposal as one operation, with the supervisor inside the lease.** Why: both pilot Stack
   agents ended correctly by the docs and still left a supervisor; removal after `down` still
   needs a second command. First deliverable: `down --dispose` (or `gc --checkout <path>`) that
   stops services, releases ports, reclaims recorded data, stops the supervisor when idle, and
   reports each step. Acceptance: extend `regressions.py removed-checkout-reclaim` so that after
   the operation the census of ports, data, mise state and supervisor is empty, in both the
   deleted-before and deleted-after orderings, on macOS and LinuxKit. Tradeoff: a destructive
   flag that an agent can misuse; keep the default conservative.
2. **A disposal and cancellation task family in the benchmark, plus two concurrent agents.**
   Why: the primary goal is finishing without help, and the hardest help requests come from
   recovery and cleanup, which no task exercises. First deliverable: one task per family
   (interrupted agent, deleted checkout, killed dependency) with the same hidden-check grading,
   and one plan that runs two trials at once on a host. Acceptance: validation cells valid in
   both arms. Tradeoff: changing `bench/` invalidates the approved plan hashes; keep the old
   evidence and re-approve.
3. **Hosted Linux x64 execution of the native e2e suite.** Why: every Linux claim is LinuxKit
   arm64 on one Mac. First deliverable: a CI job on a hosted runner executing `tests/e2e/native.sh`
   as an unprivileged user. Acceptance: 11 of 12 scenarios (OCI needs a registry). Tradeoff: cost
   and flakiness of hosted runners; keep it separate from the merge gate at first.
4. **Dependent staleness and a stricter `--require`.** Why: section 3C. First deliverable:
   `status` reports `older_than_dependency` from `started_at_ms`; `--require` on a
   liveness-only service is a warning in the result. Acceptance: runtime test with a real
   Postgres kill and `up` recovery showing the API flagged. Tradeoff: more fields in the envelope.
5. **Startup and residue profile.** The experiment in section 4, as a committed script and a
   results directory. Acceptance: a table of per-phase medians across ten worktrees. No code change.
6. **Rerun the pilot on the final binary with human review of all six trials, both arms.** Why:
   the only real-agent evidence predates nine corrections, and a matched comparison needs every
   trial of every pair reviewed. Acceptance: `review.json` for every trial, report `complete: true`.

Later, as research with a decision first: H1 data handoff (template from a stopped service
versus live snapshot; section 7); bounding the signal-thread waits with a diagnostic rather
than silence; a terminal-attached end-to-end test; a Compose or devcontainer control arm.

Avoid building now: worktree creation or removal inside Stack; container or VM isolation;
an agent runner or task queue; automatic reload on `watch`; a background daemon (keep `gc
--watch` under the user's supervisor); task caching; proxying mise, Pitchfork or fnox MCP servers.

## 6. Making the benchmark measure the primary goal

Keep two ledgers that never mix: product acceptance (the green gates above, which stay green
per commit) and agent outcome evidence (trials, reviews, uncertainty). The harness already
enforces that separation in its report derivation; the gap is in what it is fed.

- **Tasks.** Three synthetic apps hit a ceiling: every trial passed in both arms. Add existing
  projects with real history (an open-source Postgres or Redis application the agent has not
  seen, with its own scripts as the control), and families where setup decisions actually
  differ: recovery after a killed service, external cancellation mid-task, disposal after the
  task, two agents on one host, a cold hosted bootstrap. Tasks should fail without the fix
  and pass with it in both arms, as now.
- **Controls.** The existing arm is glue written for the benchmark. For real projects, the
  control is the project's own setup, including Compose where that is what the project ships.
  Match cache mode, network and locale per pair as the plan already does, and record
  download waits separately from agent time.
- **Metrics that speak to help.** Count help requests and stops on a blocker, retries, invalid
  tool calls, docs and source lookups, recovery commands, what was left running, and disk and
  ports after `down`. The harness records most of these; the report should surface them per
  pair rather than only autonomy.
- **Review provenance.** The human-review gate is a policy choice and a sound one for any
  autonomy claim: the agent's own narrative and an AI reviewer's reading are both outputs of
  models whose errors correlate. Keep it. What can change without bypassing it: publish a
  machine-derived tier ("verified, no help channel, nothing left running, unreviewed")
  explicitly labelled, use AI review as triage that flags trials for human attention, and
  budget human review for a sample rather than every trial once the sample size grows. Do not
  let an AI review satisfy `review.json`.
- **Samples and uncertainty.** Pre-register repetitions from pilot variance, resample whole
  tasks, report bootstrap intervals per task family, and refuse to publish a difference
  without them. The protocol already says this; implement the interval computation so the
  report cannot omit it.
- **Platform.** One macOS host is one host. A hosted Linux x64 run of the same plan is the
  minimum before any claim about hosted agents.

## 7. Questions only the owner can answer

1. **Default supervisor lifetime.** Should plain `down` stop a supervisor that Stack started
   for a root nobody else uses? Keeping it shared is correct on a developer's machine with many
   projects; stopping it is what every per-agent root wants and what both pilot agents failed
   to do. Options: keep opt-in; stop by default when the supervisor's state directory is under
   a Stack-owned per-root path; or attach it to the lease. The second is the narrow change.
2. **May `gc` stop a deleted checkout's processes by token?** Today it refuses by design and
   asks a person. `down` already stops token carriers by PID and start time. Allowing `gc` the
   same identity-checked stop removes the last human step from disposal; the risk is a wrong
   kill if the token identity is ever weaker than assumed (filesystems without creation times).
3. **H1 data handoff: which semantics?** Three options with different ownership rules:
   none (status quo, handoff through migrations); a template copied from a stopped service's
   data directory into a new checkout, marker rewritten to the new token; a live snapshot of
   a running database. The template is cheap and composes with the ledger and quarantine rules.
   Live snapshots need provider-specific copy semantics and would blur "whose data is this".
4. **Disposal policy on lease expiry.** Stop only (current) or dispose, chosen at `up`. This
   decides whether an abandoned agent root reclaims its own disk.
5. **Quarantine retention.** `.replaced-*` data is never removed. Keep that, or allow
   `gc --reclaim-data --quarantined` with an explicit age.
6. **Liveness-only services.** Should `--require` and `run` accept a custom service that has
   no identity probe, or should bundles be nudged to ship one?
7. **Release.** 0.1.22 was unpublished during every evaluation. Whether to publish this branch
   as 0.1.23 after merge, and on which platforms, is outside this branch. It gates any claim
   about a published install; hosted measurement with a source-built binary stays valid when it
   is labelled as such.

## Follow-up after this assessment (2026-10-21)

This assessment describes `c73f38a` and is otherwise unchanged. On question 1 of section 7,
the owner chose to stop the supervisor automatically. Since `b80318c`, plain `down` stops the
supervisor once Stack can show it started it and nothing else uses it. That is a narrower test
than the "state directory under a Stack-owned per-root path" option above, and it applies to
any state directory. It is recorded in the
[ledger follow-up](REMEDIATION-LEDGER.md#follow-up-2026-10-21-plain-down-stops-the-supervisor-stack-started-owner-decision-on-o1).
The other questions, and the disposal and lease items of the roadmap, are still open.
