# Evaluation follow-up review corrections

This records implementation changes after the original R1 review. It does not replace the
historical evaluation or claim another independent review approved the final tree.

| Finding | Correction |
| --- | --- |
| R1, GC generation race | Removed gone-project stop-by-name. Pitchfork lacks atomic compare-and-stop, so live/uncertain services retain ownership with `gc_incomplete`. Even matching PID/port observations cannot authorize a later stop. Missing ports fail closed. |
| R2, provider aliases | Resolution, install, daemon calls and nested `stack exec` share an isolated config boundary. Only generated Stack configuration loads. Real pinned mise regression covers inherited, global, project and parent aliases, including backend aliases and a resolver cache under configured parents. |
| R3, harness isolation | Clear inherited Stack/Pitchfork/mise selectors, explicitly isolate state/cache/config paths, and invoke only the isolated supervisor binary with an explicit state directory during broad cleanup. Pilot installs/cache remain under the HOME erased by its fresh phase. |
| R4, invalid pins | Validate reused pins offline and validate resolver responses. Reject floating, empty, path/system/ref selectors and incomplete known-provider versions without modifying output or contacting the resolver. Preserve PostgreSQL/jq two-part releases and named/calendar releases. |
| R5, partial starts | Persist available qualified IDs before start and observed PID/ID/port data after start failures and every status observation, before environment/readiness calls can fail. Propagate ownership-save errors. |
| R6, effective sockets | Overlay HOME, XDG_STATE_HOME and PITCHFORK_STATE_DIR from the provider environment. Resolve relative state paths from the provider cwd. Honor explicit state overrides for root. Doctor marks templated paths as unchecked. |
| R7, transitional states | Only confirmed stopped/missing daemons after a completed launch and dead recorded PID permit release. Starting, stopping, errored, unknown, malformed and incomplete-launch states retain ownership. |
| R8, preset scope | Pin all five known presets, default omitted versions to a resolved latest release, resolve prefix/sub selectors, and reject unsupported presets in locked mode. |

## Validation and limits

Rust tests, strict Clippy, package/promotion/harness tests, ShellCheck and actionlint pass locally.
The real-mise configuration test passes with the checksum-verified 2026.9.18 macOS ARM binary
and also runs in the native service CI jobs. Tests include corrupted pins, default preset
locking, partial startup failures, effective socket paths, replacement-process survival,
missing ports and nonterminal supervisor states. Hosted CI is required before merge.

Gone-project cleanup is intentionally conservative. Use `stack down` before deleting a
checkout. If already deleted, identify and stop the intended supervisor daemon explicitly,
then retry GC. Incomplete launches with uncertain identities may require manual reconciliation.
The new behavior does not claim automatic safe stopping without provider support.

The original benchmark files remain unchanged. The scripted pilot is not an agent-productivity
study. Its historical zero-orphan count covers PIDs observed in successful launch responses,
not a complete supervisor census after every failure. The pre-existing port allocator can
still overlap OS ephemeral client ports; the recorded failure's exact cause remains unproven.
Neither limitation is presented as fixed by these review corrections.
