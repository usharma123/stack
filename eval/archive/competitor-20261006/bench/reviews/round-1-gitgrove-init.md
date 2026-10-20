# Astra review: GitGrove initialization, round 1

Verdict: REQUEST CHANGES. Three P2 findings. The GitGrove helper rename itself is correct, and all 15 submitted initialization tests pass.

## Scope and source identity

Read-only review of the current dirty checkout `/Users/utsavsharma/.t3/projects/stack`, specifically `bench/run.py`, `bench/rwb/adapters/git_grove.py`, and new `bench/tests/test_initialization_failures.py`. Read supporting Recorder, transport, Scenario, summary rendering, and the original runtime report. No repository edits, commits, delegation, Docker, services, or network calls.

SHA-256 values match the requested review snapshot:

- `bench/run.py`: `67d04e016691c9361ca52ce86196ac3ede4cab55d6cc3ead13b5a827c28c8d32`
- `bench/rwb/adapters/git_grove.py`: `ee21232280df95b7897e9e063000c120ae792e6c5f62c39e49587fbb90d5ed02`
- `bench/tests/test_initialization_failures.py`: `8634b5d2f773de3805851bc7bb5fa62fb85526670881ec0d586b27fd7667aac1`

## Findings

### P2: Fallback serialization repeats failures for circular containers and unsupported mapping keys

Location: `bench/run.py:192`, used at `bench/run.py:252`.

`json.dumps(..., default=mark)` only handles unsupported values passed to `default`. It does not handle circular lists/dictionaries or unsupported dictionary keys. If an adapter supplies either through metadata such as `pins`, the initial write fails and the fallback fails again before even outcomes.json is written. With a writable result directory, both cases leave only logs/ and steps.jsonl. The original exception and cleanup survive, but the promised invalid/incomplete/nonreportable metadata, error outcome, summary, and saved traceback are absent.

Reproduced through real `main()` using an otherwise ordinary host toy adapter with `pins['self'] = pins`, then with `pins = {('tool', 'version'): '1'}`. The exceptions were respectively `ValueError: Circular reference detected` and `TypeError: keys must be str, int, float, bool or None, not tuple`. Both received a `receipt NOT completed` note. No transport command ran.

Suggested fix: recursively sanitize containers, tracking active container identities and marking invalid keys/cycles explicitly. Also keep a minimal receipt built from trusted scalar fields as a fallback if copying arbitrary metadata still fails. Add end-to-end tests for both shapes. This is a failure-path guarantee issue; no currently registered adapter was observed to emit either shape.

### P2: SIGTERM during failure handling displaces the original initialization error

Location: `bench/run.py:244-249`; the same Exception-only handling appears at lines 267 and 272.

The installed SIGTERM handler raises KeyboardInterrupt. The initializer catches BaseException, but `initialization_failed()` catches only Exception around cleanup, recording, and closing. A SIGTERM while handling an earlier TypeError therefore escapes the handler and bypasses the caller's bare re-raise. If it arrives during cleanup, recording and Recorder.close are also skipped. This contradicts the explicit original-error-retention contract.

Reproduced by triggering the installed SIGTERM callback from the cleanup hook after the shadowed-image TypeError. The propagated exception was `KeyboardInterrupt: signal 15`, with TypeError only in its context; the result directory contained only logs/ and steps.jsonl. This deterministic test invokes the actual callback without sending an operating-system signal. The original remains visible as chained context, so it is displaced rather than erased entirely.

Suggested fix: defer further interrupts during bounded failure cleanup/receipt writing, or explicitly guard these recovery operations against BaseException, attach the secondary fault safely, and re-raise the original object. Ensure Recorder.close is protected by an outer finally. Test interruption separately during cleanup, receipt writes, and close.

### P2: The creation identity is not bound to the directory recursively removed

Location: `bench/run.py:216-226`.

The creation identity is checked with lstat, then `shutil.rmtree(workdir)` resolves the path again. Symlink-safe rmtree protects against following symlinks but does not bind its own initial lookup to the earlier creation identity. If a concurrent actor renames the owned directory and installs another real directory at the same path between those operations, rmtree deletes the replacement and reports successful owned cleanup.

Reproduced entirely under /tmp by wrapping the rmtree call: after the helper's ownership check, rename work to original, rename an unrelated directory containing a sentinel to work, then call the unmodified real shutil.rmtree. The unrelated sentinel was deleted, original remained, and the receipt said `action=removed`, `problems=[]`, `verified_absent=true`.

Suggested fix: use an opened directory descriptor, compare fstat against the creation identity, and perform descendant cleanup relative to that verified descriptor without following symlinks. Treat path replacement as a cleanup problem and do not recursively traverse a fresh path lookup. Add a deterministic replacement-between-check-and-delete regression. Exploitation requires concurrent ability to rename entries in the tempdir's parent; this is not a claim that an existing adapter currently performs that race. The current tests cover replacement before the check, not this window.

## Validation and limits

- `PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s bench/tests -p test_initialization_failures.py -v`: 15 passed.
- `PYTHONDONTWRITEBYTECODE=1 python3 /tmp/astra-gitgrove-init-probes.py`: reproduced both serializer cases, secondary interruption, and replacement race. Script retained for reproduction. All mutations were temporary scratch artifacts.
- All adapter/variant initial metadata serialization passes in the submitted test. GitGrove retains inherited `image=None`; setup, versions, and cleanup use the renamed app-image helper.
- Normal cleanup protects pre-existing symlinks/replacement directories, refuses unknown identity or recorded commands, and leaves internal symlink targets intact in the focused tests.
- No full-suite, mutation-baseline, live GitGrove, or live runtime validation performed. The parent's combined suite remains necessary. Other lanes' reported bad-config and Tilt process errors were not dismissed or adjudicated here.
- The minimal `init` dictionary is still constructed outside the initialization try block. No separate actionable current-adapter failure was established for that gap.

One-line verdict: Request changes for fallback serialization, original-error retention during interruption, and the directory identity race before approving the stated initialization guarantees.
