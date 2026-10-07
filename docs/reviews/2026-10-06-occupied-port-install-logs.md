# Review record: port conflicts, `install`, `logs` (branch `fix/occupied-port-install-logs`)

Reviewer: Astra (`gpt-6-astra`, reasoning high), two rounds against the full branch diff,
with its own reproductions against real mise 2026.9.18 and Pitchfork 2.29.0. Investigation of
Pitchfork log storage, `mise daemons logs`, `mise install` isolation, listener attribution and
Worktrunk hooks: Sol (`gpt-6.1-sol`, reasoning high), primary sources cited in the task.

## Round 1: CHANGES REQUIRED

1. **P2, blocking.** `down_locked` took `mise daemons --json`'s `port` (the configured
   allocation) as the owned listening port. During a generation change mise reports the new
   allocation with the old running PID, so a foreign listener on the new port was waited for
   after the old process stopped: `up` returned `stop_unconfirmed` after 20 s instead of
   `port_conflict`. Reproduced with real tools.
   Resolved in `bfe03d3`: owned ports come from where a daemon actually listens
   (`pitchfork status --json <id>` `active_port` when the configured port differs from the
   recorded one; recorded, else configured, port kept when that cannot be established). Fake
   supervisor exposes its listener PID and marks it stopped; regressions for `up` and `down`;
   e2e scenario 9 reproduces the sequence against real tools.
2. **P2, docs.** pilot-protocol.md's recovery path (`stack down` from a restored checkout) is
   impossible: a recreated directory is a different project (`session_conflict`). Resolved in
   `a278a9a`: stop the recorded supervisor daemon, retry GC.
3. **P3, docs.** worktrees.md claimed a failed `pre-start` leaves nothing started; start or
   verification failures can leave services under a launch record. Resolved in `a278a9a`.
4. **P3, docs.** commands.md described `logs_failed` too narrowly. Resolved in `a278a9a`.

No blocking findings in `install` semantics, CLI/MCP `tail` validation, subprocess capture
bounds, the frozen-copy adapter change, or commit separation.

## Round 2: APPROVE

- Round-1 reproduction on a fresh build: `up` returned `port_conflict` in 0.44 s with
  `stop_previous` ok and the old PID dead; direct `down` passed in 0.23 s listing the squatter.
- Port coincidence after `--reassign-ports` is possible in principle (the project's old
  reservations are released and deterministic allocation may pick one again when free); a
  still-listening old service prevents it through the bind check. Not a blocking failure in
  the reviewed lifecycle.
- Provider discovery without a record is lazy, cached per call, and falls back conservatively.
  The qualified id returned by `pitchfork status` is checked exactly. GC uses the session's
  provider record.
- All three documentation corrections match the implementation.
- **P3, non-blocking:** both generation-change fixtures reported an `active_port` equal to the
  recorded port, so the tests would pass if the supervisor's answer were discarded. Addressed
  after approval with two tests without a session record: one where only the active port
  separates the old service from a squatter on the configured port, one where no active port
  is available and the configured port is waited for (`stop_unconfirmed`), the squatter alive.

Benchmark evidence for the fix: [bench/measurements/stack-portfix-20261006](../../bench/measurements/stack-portfix-20261006/README.md).
