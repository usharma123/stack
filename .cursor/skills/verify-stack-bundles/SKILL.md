---
name: verify-stack-bundles
description: Model-check what a compile accepts from a local, Git or OCI bundle (compile, compile --locked and every locked operation, compile --update) and what it sends a registry, against edited local directories, moved tags, hand-edited stack.lock pins, changed or foreign cache trees, links and excluded names, paths.bin escapes, [lock] artifacts = "required", and registry redirects, challenges and token services. Use when changing src/source.rs, src/hash.rs, src/oci.rs, src/compose.rs bin_dir, manifest::parse_bundle, project::compile_checked or check_committed_artifacts, or artifacts::Policy::check_host.
---

From the repository root, run `.cursor/skills/verify-stack-bundles/check.sh`, or `verification/check.sh bundles`. It needs bash, Java 11+, git, curl, sha256sum or shasum, and Cargo. The runner downloads the TLA+ v1.7.4 jar to `target/verification/tla2tools.jar` and checks its SHA-256 on every run, a cached copy included. It checks `BundleIntegrity.tla` under every config in `expected.tsv` with one worker (about two minutes), then runs the Rust tests in `rust-tests.txt`: library tests by path, integration tests as `tests/<crate>.rs <name>`. Set `VERIFY_SKIP_RUST=1` to check the models only, `CARGO="cargo +1.93.1"` to choose the toolchain and `CARGO_BUILD_JOBS` to bound the build. Evidence (each TLC log, `summary.tsv`, `rust.log`, `source.log`) stays under the printed `target/verification/bundles.*` directory. Read every TLC result, a pass or a counterexample, with pstack `/statechart`.

The model decides abstract policy only: which values are trusted, which are checked, and where a request or credential may go. It contains no exploit and needs none; its Rust tests use loopback registries, dummy credentials and temporary directories.

## Trust

| Who | What | How the model treats it |
|---|---|---|
| Operator (environment) | `STACK_OCI_USERNAME`, `STACK_OCI_PASSWORD`, `STACK_OCI_REGISTRY` (the origin they are bound to), `STACK_OCI_AUTH_REALMS` (approved token services) | constants of a behavior: none, bound to the reference's registry `R`, bound elsewhere (`Y`), or set without a valid binding |
| Project | the bundle reference (so the registry origin `R`), `[lock] artifacts` and `platforms`, `stack.lock` | chosen; `stack.lock` is data, its pins checked before they name anything, and it can be edited by hand at any time |
| Untrusted | the bundle tree, `bundle.toml`, `paths.bin`; every registry answer (redirects, challenges and their realms, token replies, manifest bytes, the layer digest a manifest names, blobs); cache trees this Stack did not install | chosen adversarially, nondeterministically, at every step |
| Same user | the cache directory and the local bundle directory | edited between any two steps; a cached tree's evidence is not a signature (see `oci::Evidence`) |

## What it promises

What one compile accepts, from the snapshot it hashed. The other files of a bundle are read when used, from the local directory or the cache, with no immutable copy: a local bundle edited after a compile, or a cached tree changed by another process running as the same user, is seen as it is then, until the next compile hashes it again (`LIMIT-LocalToolsAtUse`; docs/user/bundles.md says so).

| Invariant | Promise | Harm if broken |
|---|---|---|
| `ManifestIsHashed` | The manifest a compile uses is the `bundle.toml` bytes its content hash covered, however the file changes meanwhile. | stack.lock pins one manifest while another is used. |
| `NoUnhashedContent` | No accepted tree held a link or special file when hashed. | Content (even outside the tree) readable through the bundle, covered by no hash. |
| `BinInsideBundle` | Every `paths.bin` directory put on `PATH` is, when the compile composes, a real directory inside the bundle and not under `.git`/`.stack` in any spelling. | A directory outside the hashed bundle, or one hashing skips, on every command's `PATH`. |
| `OciTreeIsDigests` | An OCI compile accepts only the tree of the layer its manifest digest names. | A cached tree filled otherwise, or changed since, used as the pinned bundle. |
| `CacheHoldsNoExcluded` | An OCI compile never accepts a tree holding `.git` or `.stack` under any spelling a case-insensitive filesystem folds to. | Unhashed content in the shared cache, read as a repository or as Stack's state. |
| `OnlyDigestsNamePaths` | No value not checked to be a digest or commit id names a cache path or request. | A lock or manifest field steers a path or URL. |
| `LockedAcceptsTheLock` | A locked compile accepts exactly the pins and content hash of the stack.lock it read. | CI or an agent runs content nobody locked. |
| `PinsMoveOnlyByUpdate` | Only `--update` moves a locked pin. | A moved tag or branch silently changes what runs. |
| `UnlistedHostRefusedFirst` | Under `required`, a locked operation on a machine `[lock] platforms` does not list fetches nothing and never reaches mise. | Installs or runs with nothing in stack.lock vouching for this platform. |
| `RequiredMeansCovered` | Under `required`, mise is reached by a locked operation, and stack.lock written by a compile, only with every pin checked. | An unchecked download installed or committed. |
| `CredentialsOnlyWhereBound` | The operator's credentials go only to the origin they are bound to, and through it to its own or an approved token service. | A bundle reference, redirect or challenge collects the operator's registry credentials. |
| `AuthOnlyToItsOrigin` | An Authorization header goes only to the origin it was issued for, never across a redirect. | A registry token or Basic credential replayed to another server. |

## Action table (src/ at the sync commit)

| Model action | Rust |
|---|---|
| `Begin(mode)` | `project::compile_checked`: locked mode needs stack.lock (`lock_outdated`), `Policy::check_host` before any fetch or provider call (974bdf4), then `Source::fetch`'s pin: `fetch_git`/`fetch_oci` take stack.lock's commit or digest in `UseLock`/`Frozen` (`is_commit`, `oci::digest`, `lock_invalid`) and resolve otherwise (`git::resolve`, `Client::resolve`, which hashes the manifest bytes into the digest); `Frozen` never resolves |
| `NeedsRegistry` | what `fetch_oci` asks the registry: `Client::resolve` (tag), `Client::pull` into an absent slot, `Client::verify_cached` for a cached tree no lock pin or `Evidence::matches` vouches for |
| `RegistryRedirect`, `RegistryDone`, `RegistryFails` | `Client::send`: `redirects(0)` agent, explicit redirects (`challenged` reset, at most 5), `validate_transport`, the token attached only when its origin is the target's |
| `RegistryChallenge(kind, reply)` | `authenticate_with`: refused unless the challenged origin is `credential_origin`; Bearer: `trusted_realm` (own origin or `STACK_OCI_AUTH_REALMS`), `CredentialEnv::grant`, the token request with Basic credentials only under `Grant::Credentials`, a non-200 (redirect) reply refused; Basic: credentials only under `Grant::Credentials` |
| `Grant` | `CredentialEnv::grant` with `registry_origin`: `Anonymous`, `Credentials` for the bound origin, `Elsewhere` (anonymous), `oci_credentials_unbound` |
| `Fetch(layerOk)` | `fetch_path` / `fetch_git` → `git::materialize` → `hash::hash_keeping` (`list_files` refusing links, special files and non-UTF-8 names, folding excluded names; the kept `bundle.toml` bytes); `fetch_oci` → `Client::pull` (`layer`: manifest and blob each `verify_digest`ed, the layer digest checked by `oci::digest` before its request; `install_with` → `extract` refusing links and excluded names, `Evidence::write` before publication), `hash::first_excluded`, then reuse by lock pin and content hash, `Evidence::matches`, or `verify_cached`; `content_hash_mismatch`; local `Frozen` compares with stack.lock (`lock_outdated`) |
| `Parse` | `manifest::parse_bundle(fetched.manifest, ...)` |
| `Load` | `compose::LoadedBundle::new` → `bin_dir` |
| `Finish` | `compose` → `bin_dir` again, then (locked) `same_pins` (`lock_outdated`) and `check_committed_artifacts` before the preflight (`mise::check_requirements`) or the command reaches mise; (compile, update) `unchecked_targets` → `unlocked_error` before `lock::write` |
| `LocalEdit`, `UpstreamMoves`, `LockEdited`, `CacheChanged` | a person or process editing the local bundle (perhaps replacing its bin directory by a link), a tag or branch moved upstream, stack.lock edited, a cached tree changed by another process running as the user |

## Bounds and case distinctions

- `Compiles = 3` compiles and `Events = 2` events per behavior, one `[[use]]` bundle, versions 1 and 2 of its content (a local edit or a moved ref). There are 2,016 initial states.
- Transport: local directory, Git, OCI. The tree holds a link or special file or not; `paths.bin` is a plain relative directory, a `..` path, or `.Stack/bin`; a link replaces it only after a hash saw the tree (`LocalEdit`), since a link already there is refused by hashing first. An OCI layer can hold a directory folding to `.git`/`.stack`.
- stack.lock: none, or pinning version 1; it can be edited to a pin that is not a commit id or digest. The OCI cache slot of the locked digest starts absent, installed with evidence, filled with the same content without evidence, filled with other content, or holding an excluded directory; another process can change it.
- `[lock]`: best effort; `required` on a listed machine with every pin checked (`requiredCovered`, the case where locked operations must go ahead); `required` with a pin unchecked; or `required` on a machine `platforms` does not list.
- Registry: one conversation per fetch, starting at `R`, with at most one redirect (to `R` or another origin `Y`), one challenge per target (Basic, or Bearer with realm `R`, approved `A`, or unapproved `U`), and a token service that answers with a token or a redirect (to `Z`). Operator credentials: none, bound to `R`, bound to `Y`, or unbound.

## Atomicity assumptions and what is outside

- One action is one step of the code: `hash_keeping`'s listing and reads are one step (a tree swapped between them by the same user is outside the trust model, which already lets that user edit the files), as are a pull's download, extraction and publication under the install lock, and one request with its reply.
- One registry conversation stands for all of a fetch's requests (resolve, manifest, blob): they share one `Client`, its `credential_origin` and token.
- Outside: `stack publish` (`Client::push`, upload locations; covered by `absolute_upload_locations_do_not_receive_registry_authorization`), plain-HTTP opt-in and downgrades (`validate_transport`, unit-tested), expansion limits and gzip framing (`extract`, unit-tested), Git's own object integrity and `dir=` subdirectories (`subdirectory`, unit-tested), several bundles and `duplicate_bundle`, compose conflicts, mise's artifact locking itself (`lock_artifacts`), and what tools do at run time.

## Faults (historical bugs) and expected results

`FALSE.cfg` passes every invariant (1,785,966 distinct states). Each `TRUE-*.cfg` puts one bug back and must end with TLC exit 12 and the named violation. `LIMIT-LocalToolsAtUse.cfg` has no fault: it shows that a local bundle edited after a compile is read as edited, which the model does not promise. `expected.tsv` pins every config's exit code, invariant and distinct-state count.

The refusal invariants under `required` (`UnlistedHostRefusedFirst`, `RequiredMeansCovered`) would also hold in a model that refused every locked operation under `required`. Two configs rule that out. `WITNESS-RequiredLockedRuns.cfg` checks `RequiredLockedNeverRuns`, which is not a promise but its opposite, and must violate it (exit 12, 332,645 distinct states): a locked operation under `requiredCovered` reaches mise. Its counterexample is a locked compile of a local bundle on a listed machine with every pin checked, which `tests/runtime.rs` `every_locked_operation_runs_under_required_on_a_listed_platform_with_every_pin_checked` runs for real: install (reaching `mise install --locked`), up, exec, run, status, inspect and `compile --locked`, from the CLI and over MCP, each going ahead without rewriting stack.lock. `WITNESS-RefuseRequired.cfg` puts back that over-refusal (`RefuseRequired`) and passes the same check (exit 0), so the witness row would fail if the model, or a change to it, refused them all.

Two guards hold a locked compile to stack.lock: `Begin` takes its pin and never resolves, and `Finish` compares the pins (`same_pins`). In the baseline either alone suffices. `REDUNDANT-NoSamePins.cfg` removes `same_pins` and keeps `LockedAcceptsTheLock`, with exactly the baseline's distinct states, since `Fetch`'s hash checks and the pin `Begin` took leave it nothing to catch. The mutants therefore remove the pin choice: `TRUE-IgnoreLockPin` lets compile resolve the moved tag, and `TRUE-LockedSkipsSamePins` removes both guards from locked mode.

| Config | Bug put back | Violation |
|---|---|---|
| `TRUE-SkipLinks` | 612d24a: hashing skips links and special files instead of refusing them | `NoUnhashedContent` |
| `TRUE-LexicalBin` | 612d24a: `paths.bin` containment checked lexically after following links | `BinInsideBundle` |
| `TRUE-NoComposeRecheck` | 612d24a: `paths.bin` checked at load only, not again at compose | `BinInsideBundle` |
| `TRUE-ExactExcluded-bin` | 9a97f0e: excluded names compared exactly (`.Stack/bin` on `PATH`) | `BinInsideBundle` |
| `TRUE-ExactExcluded-cache` | 9a97f0e: the same, in OCI extraction and cached trees | `CacheHoldsNoExcluded` |
| `TRUE-ReuseByName` | 9a97f0e: a cached OCI tree reused by its directory name | `OciTreeIsDigests` |
| `TRUE-UncheckedDigests` | 9a97f0e: lock and layer digests name paths and requests unchecked | `OnlyDigestsNamePaths` |
| `TRUE-ReadManifestAgain` | 9a97f0e: `bundle.toml` read again after hashing | `ManifestIsHashed` |
| `TRUE-CredentialsToAnyChallenger` | 08600bb: credentials offered to whichever registry challenged | `CredentialsOnlyWhereBound` |
| `TRUE-TokenFollowsRedirect` | b727b0d: the token request follows a token service's redirect | `CredentialsOnlyWhereBound` |
| `TRUE-AuthFollowsRedirects` | b727b0d: the HTTP agent follows redirects, carrying Authorization | `AuthOnlyToItsOrigin` |
| `TRUE-HostCheckOnlyInInstall` | 974bdf4: only install and up refuse an unlisted platform | `UnlistedHostRefusedFirst` |
| `TRUE-CoverageOnlyInInstall` | design guard (7eabc0e): locked operations other than install skip `check_committed_artifacts` | `RequiredMeansCovered` |
| `TRUE-IgnoreLockPin` | design guard: compile and locked mode resolve the ref or tag instead of taking stack.lock's pin (`moved_tag_does_not_change_a_locked_project`) | `PinsMoveOnlyByUpdate` |
| `TRUE-LockedSkipsSamePins` | design guard: that, with locked mode also skipping `same_pins` (`locked_mode_refuses_to_change_the_lock`) | `LockedAcceptsTheLock` |
| `LIMIT-LocalToolsAtUse` | none: a local bundle edited after its compile | `ToolsUsedAreHashed` |
| `WITNESS-RequiredLockedRuns` | none: a locked operation goes ahead under `required` | `RequiredLockedNeverRuns` (must be violated) |
| `WITNESS-RefuseRequired` | over-refusal: every locked operation under `required` refused | none (passes the witness, so the witness above is what notices) |
| `REDUNDANT-NoSamePins` | locked mode skips `same_pins` alone | none (`LockedAcceptsTheLock` holds) |

## Rust regressions

`rust-tests.txt` lists the tests that run the real functions behind the key transitions:

- `hash::tests::*` for `Fetch`'s hashing (links, special files, non-UTF-8 names refused; excluded names folded; legitimate trees hash as before).
- `source::tests::the_manifest_parsed_is_the_one_hashed` for `Parse`; `a_local_bundle_edited_after_its_lock_is_outdated_until_compiled_again` for local `Fetch` in locked and ordinary mode; `only_commit_ids_name_git_bundle_checkouts` and `git_sources_reject_unknown_repeated_and_escaping_parameters` for `Begin`'s pin checks.
- `compose::tests::*` for `Load` and `Finish`'s `bin_dir`, including a bin directory replaced after loading.
- `oci::tests::*` for `Grant`, `trusted_realm`, `registry_origin`, `authenticate_with` (Basic only at the configured origin), digest and reference grammars, transport checks, `extract` and `install_with`.
- `artifacts::tests::required_policies_refuse_an_unlisted_runtime_platform_and_best_effort_ones_never_do` for `check_host`.
- `tests/compile.rs` (links in Git and local bundles, `paths.bin`, a moved tag, locked mode), `tests/oci_pull.rs` (layers with links or excluded directories, a link added to a cached tree, evidence-only reuse, a cached tree holding an excluded directory, only digests naming paths and requests), `tests/oci_auth.rs` (unapproved and approved token services, a token service's redirect, origin binding however spelled, unbound credentials failing closed, anonymous flows beside bound credentials, an approved realm of an unbound registry, a selected registry redirecting into the bound origin, and an authorized request redirected elsewhere carrying no Authorization), `tests/artifacts.rs` (`required_coverage_fails_before_anything_is_written`) and `tests/runtime.rs` (`every_locked_operation_refuses_an_unlisted_platform_under_required_and_ordinary_compile_does_not` and, for the witness, `every_locked_operation_runs_under_required_on_a_listed_platform_with_every_pin_checked`, CLI and MCP).

## Reading a result

A counterexample is a real bug only if each step can happen in the code. Replay it against the action table, and as a test of the real function where you can. If a step cannot happen, the model is wrong: fix the model, never weaken an invariant. Modeling errors found while writing it: `BinInsideBundle` first judged the value the code checked rather than what was on disk at compose, so `TRUE-NoComposeRecheck` passed; and a link-replaced bin directory had to come only after a hash saw the tree, since hashing refuses a link first. No counterexample of the baseline described a Rust defect.

## Coverage record

Covered files: `src/project.rs` (`compile`, `compile_checked`, `check_committed_artifacts`), `src/source.rs` (`fetch_git`, `fetch_oci`, `fetch_path`, `is_commit`), `src/hash.rs` (`hash_keeping`, `list_files`, `excluded`, `first_excluded`), `src/manifest.rs` (`parse_bundle`), `src/compose.rs` (`LoadedBundle::new`, `bin_dir`, `compose`'s bin paths), `src/oci.rs` (`Client::send`, `authenticate_with`, `CredentialEnv::grant`, `registry_origin`, `trusted_realm`, `resolve`, `pull`, `verify_cached`, `layer`, `digest`, `verify_digest`, `install_with`, `extract`, `Evidence`), `src/artifacts.rs` (`Policy::check_host`, `unchecked_targets`, `unlocked_error`).

The model was confirmed against these files at `402090b`; since 1710fbf, the last change to them, only a test was added (`src/source.rs`, 402090b). None of them changed by `f2d4fe7`, where the `requiredCovered` policy, the witness and the pin mutants were added. If any covered function changes, recheck the action table, the bounds, `expected.tsv` and the Rust list. Move the sync commit forward only after confirming that the model describes the new code and rerunning the check. Use `/maintain-formal-verification-skill` to reconcile intervening changes. If a config is intended to change, update its row in `expected.tsv` and say why in the commit.
