# Retired: 26-tool competitor benchmark (2026-10-06)

This directory keeps the former `bench/` tree as historical evidence. It is no longer the
active benchmark. The active benchmark is [`bench/`](../../../bench/README.md), which compares
Stack against an existing project setup on matched agent tasks.

## What is here

`bench/` contains all 275 files that were tracked under `bench/` at
`8b3402fa1db8f11917b5ba33a333c9093545d9ce` (Stack 0.1.22). The archive commit moves them
byte for byte (git records every file as a 100% rename), so every blob hash in old manifests,
receipts and reviews is unchanged. That includes
`bench/measurements/final-20261006-reviewed-1/raw-results.tar.gz` and its receipt.

The benchmark answered a different question: whether 26 environment tools could run one
scripted Python/PostgreSQL/Redis workload in two checkouts, with per-tool glue. It had no agent,
so it says nothing about agent task completion or human intervention.

## Replaying it

The old runner, tests and manifests resolve paths against the repository root as `bench/...`.
Do not run them from this archive path; the recorded paths and hashes would not line up. Use
a checkout of the last commit where the tree was active:

```sh
git clone --no-local . /tmp/rwb-replay && cd /tmp/rwb-replay
git checkout 8b3402fa1db8f11917b5ba33a333c9093545d9ce
python3 -m unittest discover -s bench/tests
```

The full replay needs the Docker images and pinned tools listed in `bench/README.md`.

Offline unit-test status at that commit, before retirement:

- In a git checkout: 382 tests, 1 failure, 1 skip. The failure
  (`test_core.StackFrozenSetupTest.test_failed_partial_c_setup_is_still_cleaned_up`) expects
  `frozen_setup_starts_services` to be `True` while the adapter declares `False`. It predates
  the retirement.
- In a `git archive` extraction without `.git`: the same, plus 1 error in
  `test_reporting.ManifestTest.test_template_uses_current_bytes_and_no_review`, because the
  template reads the harness commit from git.

The archived tests are not part of any CI workflow, and retirement removes no required check.
