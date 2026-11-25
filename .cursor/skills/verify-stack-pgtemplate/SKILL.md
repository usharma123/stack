---
name: verify-stack-pgtemplate
description: Check PostgreSQL template generation lookup when Stack's cache entry is discarded and rebuilt concurrently with a record read.
---

From the repository root, run `.cursor/skills/verify-stack-pgtemplate/check.sh`, or `verification/check.sh pgtemplate`, which runs it among Stack's other models (`verify-stack-session`, `verify-stack-snapshot`, `verify-stack-endpoints`, `verify-stack-bundles`, `verify-stack-ports-mcp`). It needs bash, Java 11+, git, curl, sha256sum or shasum, and Cargo. It uses the shared runner in `verification/lib.sh`: the TLA+ v1.7.4 jar is downloaded to `target/verification/tla2tools.jar` on first use and its SHA-256 checked on every run, a cached copy included; `Generation.tla` is checked under every config in `expected.tsv` with one worker, comparing each exit code, violated invariant and distinct-state count with the table; then the Rust test in `rust-tests.txt` runs. Set `VERIFY_SKIP_RUST=1` to check the model only, `CARGO="cargo +1.93.1"` to choose the toolchain and `CARGO_BUILD_JOBS` to bound the build. Evidence (each TLC log, `summary.tsv`, `rust.log`, `source.log`) stays under the printed `target/verification/pgtemplate.*` directory. Read the TLC results with pstack `/statechart`.

An accepted template record must come from the directory observed by both identity checks. If a stale record is accepted for a replacement directory, Stack can reject a valid template and fall back to a fresh PostgreSQL initialization.

| Model action | Rust operation in `src/pgtemplate.rs` |
|---|---|
| `LookBefore` and `LookAfter` | The two `identity` calls via `unmoved`, using device and inode |
| `ReadRecord` | The `read_record` closure passed by `generation` |
| `Discard` | `discard` renames the old directory away |
| `Publish` | `build` renames the completed directory into place |

The model explores one reader and at most one replacement of a published template. It distinguishes the original, absence, and the replacement. It assumes distinct directory identities across publication and an immutable, valid record in each published directory, with each lookup observing a single generation or absence. The check covers the ordering in `generation`. Clone integrity, retry and retirement, inode reuse, invalid records, and multi-step filesystem I/O within a record read are outside the model.

The normal model must pass (`FALSE.cfg`: exit 0, 23 distinct states). The early-read configuration must report the named invariant violation (`TRUE.cfg`: exit 12, `AcceptedRecordMatchesObservedDirectory`, at 25 distinct states). It reads the old record, then the original is discarded and replaced, then both identity checks see the replacement. `expected.tsv` holds these numbers, and the runner fails on any other exit code, invariant or count. The Rust test `a_template_replaced_while_its_record_is_read_is_read_again_under_the_lock` injects a replacement at each lookup stage and checks that Stack uses the valid replacement. Review the TLC trace alongside that test when diagnosing a counterexample.

The coverage was mapped to `e48d520712c125cf326e8013cdc3cf6051a76e41` and confirmed in sync at `e32c089`: `src/pgtemplate.rs` did not change between them, and its callers in `src/session.rs`, `src/session/gc_pass.rs` and `src/supervisor.rs` changed only around `collect`, which the model does not cover. Recheck the action table, the bounds, and the Rust regression if the covered operations change. Move the recorded commit forward after confirming the match and rerunning the check. Use `/maintain-formal-verification-skill` when reconciling the model with intervening changes.
