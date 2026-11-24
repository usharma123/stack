---
name: verify-stack-pgtemplate
description: Check PostgreSQL template generation lookup when Stack's cache entry is discarded and rebuilt concurrently with a record read.
---

From the repository root, run `.cursor/skills/verify-stack-pgtemplate/check.sh`. This needs Java, Cargo, ripgrep, curl, and shasum. The runner downloads and checksums the TLA+ v1.7.4 release jar on first use. Read the TLC results with pstack `/statechart`. Logs survive under the printed `target/verification/pgtemplate.*` directory.

An accepted template record must come from the directory observed by both identity checks. If a stale record is accepted for a replacement directory, Stack can reject a valid template and fall back to a fresh PostgreSQL initialization.

| Model action | Rust operation in `src/pgtemplate.rs` |
|---|---|
| `LookBefore` and `LookAfter` | The two `identity` calls via `unmoved`, using device and inode |
| `ReadRecord` | The `read_record` closure passed by `generation` |
| `Discard` | `discard` renames the old directory away |
| `Publish` | `build` renames the completed directory into place |

The model explores one reader and at most one replacement of a published template. It distinguishes the original, absence, and the replacement. It assumes distinct directory identities across publication and an immutable, valid record in each published directory, with each lookup observing a single generation or absence. The check covers the ordering in `generation`. Clone integrity, retry and retirement, inode reuse, invalid records, and multi-step filesystem I/O within a record read are outside the model.

The normal model should pass. The early-read configuration should report the named invariant violation. It reads the old record, then the original is discarded and replaced, then both identity checks see the replacement. The Rust test `a_template_replaced_while_its_record_is_read_is_read_again_under_the_lock` injects a replacement at each lookup stage and checks that Stack uses the valid replacement. Review the TLC trace alongside that test when diagnosing a counterexample.

The coverage was mapped to `e48d520712c125cf326e8013cdc3cf6051a76e41`. Recheck the action table, the bounds, and the Rust regression if the covered operations change. Move the recorded commit forward after confirming the match and rerunning the check. Use `/maintain-formal-verification-skill` when reconciling the model with intervening changes.
