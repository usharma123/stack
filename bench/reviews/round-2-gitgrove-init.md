# Astra R2 independent review

Verdict: REQUEST CHANGES. One P2 finding remains in the claimed bounded serializer. The three round-one counterexamples are fixed in fresh reproductions. The GitGrove helper rename is correct.

## Scope and source identity

Read-only review of `/Users/utsavsharma/.t3/projects/stack`, scoped to `bench/run.py`, `bench/rwb/adapters/git_grove.py`, and `bench/tests/test_initialization_failures.py`, using `bench/reviews/round-1-gitgrove-init.md` for the original findings. No repository edits, commits, delegation, Docker, services, or network access. Scratch artifacts and test outputs are under `/tmp`.

SHA-256 values, checked before and after validation, match the requested snapshot:

- `bench/run.py`: `e0c7c1006a88910f060dc4a1f19e9872a379e6bf8cecfae9c7565e17c3649e00`
- `bench/rwb/adapters/git_grove.py`: `ee21232280df95b7897e9e063000c120ae792e6c5f62c39e49587fbb90d5ed02`
- `bench/tests/test_initialization_failures.py`: `ccb53c421a1f6306ee02659ab700ffd356a67464290f14b103f70c91a297d508`

Neither `/tmp/astra-gitgrove-init-probes.py` nor its post-edit R2 copy was read, executed, or treated as independent evidence. I wrote `/tmp/astra-gitgrove-init-r2-independent.py` from the review's counterexamples and the current source. Its SHA-256 is `1c16bfdb37aa03f96103714bb4c6a7c4c369ce4cb44d73cdd1b510e13a4bbd1a`.

## Finding

### P2: The value budget does not bound container copying, output size, or mapping-key processing

Location: `bench/run.py:245-252`, with the budget check at `bench/run.py:223-225`.

After JSON_NODES is exhausted, `walk()` returns an omission marker for each subsequent value, but both container loops continue through every remaining entry. Both also eagerly materialize the entire input container before iteration. Mapping keys still run through `key_text()` and collision handling even after the budget is exhausted.

Fresh offline reproductions with the actual 20,000-value setting:

- `jsonable([None] * 60000)` produces 60,000 output entries, including 40,001 omission strings.
- A mapping with 20,000 ordinary values followed by a custom key still calls that key's `__repr__`, despite the exhausted budget.
- Through real `main()`, adapter pins containing 60,000 nulls followed by an unsupported object trigger the initial TypeError. The failure receipt then retains all 60,001 list entries. Original exception identity, closure, and cleanup remain correct in this case.

Thus broad malformed metadata can force recovery to allocate and serialize an arbitrarily large result instead of respecting the claimed bound. This can amplify memory/disk use and delay completion while SIGINT/SIGTERM are deferred. The minimal receipt is only attempted after an exception, so it does not limit a successful but oversized conversion. No registered adapter was shown to emit metadata of this size; this finding concerns the explicit bounded-recovery contract.

Suggested fix: iterate through base-type iterators without `list(...)` materialization, check the remaining budget before processing each key/value, and stop each container once it is exhausted. Emit one remainder marker rather than one marker per omitted entry. Add assertions that copied entry counts remain bounded as input width increases and that keys beyond the cutoff are not rendered. The current `test_depth_and_size_are_bounded` checks the last marker but does not test a size bound.

## Round-one findings and other reviewed behavior

- Circular metadata and tuple mapping keys now produce invalid, incomplete, nonreportable receipts containing an error outcome, summary, and traceback. Fresh end-to-end probes assert that the propagated exception is the exact object passed into recovery.
- Deferred SIGTERM and SIGINT callbacks during cleanup, receipt writing, and close preserve that original exception object. Direct secondary KeyboardInterrupt injections in each phase also preserve it. Recorder.close is called and closes its stream. Failed cleanup is reported and leaves the directory; a first receipt-write interruption reaches the minimal fallback.
- A forced full-copy failure produces the minimal receipt and retains the original exception. Submitted tests also cover omission of an untrusted title and persistent write failure.
- A replacement real directory inserted after the outer ownership check but before the verified open survives with its sentinel. A replacement inserted after the verified open also survives; only the original directory's contents are removed through the descriptor. Both receipts report that the path was left, with a problem and no successful absence claim.
- The descriptor-relative child traversal, no-follow opens, depth guard, and capability refusal avoid the original recursive fresh-path deletion. The acknowledged final identity-check/rmdir race can remove an empty replacement directory; it cannot recursively delete a replacement's contents. This acknowledged limit is not an additional finding.
- GitGrove inherits `image=None`; setup, versions, and cleanup consistently use `app_image`. No unrelated normal-cleanup change is required by this review.

## Validation and limits

- `PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s bench/tests -p test_initialization_failures.py -v`: 26 passed.
- `PYTHONDONTWRITEBYTECODE=1 python3 /tmp/astra-gitgrove-init-r2-independent.py`: 10 probe methods passed, including phase subcases. Three methods deliberately assert the observed budget defect, so this result is evidence for the finding, not a passing bound requirement. Output saved in `/tmp/astra-gitgrove-init-r2-independent.log`.
- Independent end-to-end probes prohibit transport execution and substitute a fixed value for Git metadata reads. Tests write only temporary results.
- No full-suite rerun or live runtime validation. The parent's reported DDEV/Lando fixture failures remain explicitly unresolved and are being handled by another owner; they are not dismissed or independently adjudicated here.
- Signal testing uses actual installed callbacks and direct exception injection, not operating-system signal delivery. Arbitrary custom repr/str execution is not proven time-bounded. The documented signal window after handler restoration remains. No claim of exhaustive concurrency verification.

One-line verdict: Request changes to enforce the serializer's value budget; the three original R1 reproductions pass with the fixes.
