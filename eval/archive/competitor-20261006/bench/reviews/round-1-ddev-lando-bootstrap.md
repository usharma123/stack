# Astra independent review, round 1

Verdict: REQUEST CHANGES. Two concrete findings remain. The original live blockers are not proven resolved; fresh retries are pending.

Reviewed checkout `/Users/utsavsharma/.t3/projects/stack`, HEAD `8952179f55989ceea23b6c0a7da166837b8af0b5`, including the six requested uncommitted files. Original evidence: `bench/reviews/sol-runtime-resume-results.md`, DDEV credential-helper failure and Lando CA-install/orchestrator/credential failures. No repository edits, commits, delegation, network requests, builds, Docker CLI calls, services, or benchmark runs were performed. All review artifacts are in `/tmp`. Prior attempts and unrelated runtime lanes were not acted on.

## Findings

### P1: Lando can run against a different daemon than preflight and cleanup

Location: `bench/rwb/adapters/lando.py:122-134`, especially the environment exports at line 128; shared comparison in `bench/rwb/adapters/ddev.py:45-52`.

Concrete trigger: launch with `DOCKER_HOST=unix:///selected-by-env.sock` or `DOCKER_CONTEXT=env-selected`, while the user's persisted `currentContext` is `desktop-linux` on another endpoint. The adapter preserves these overrides. Both sides of its preflight comparison inherit them, so the comparison passes, and `docker info` plus the shared-infrastructure snapshot address the overridden daemon. Lando then deletes these variables through `utils/build-config.js` and `utils/strip-env.js`. Compose resolves the copied persisted context in the private HOME instead. Adapter receipt and cleanup commands run in fresh shells that still inherit the override, so they can inspect or clean the wrong daemon and miss resources Lando created.

Offline reproduction: `/tmp/astra-ddev-lando-probes-r1.py`, output `/tmp/astra-ddev-lando-probes-r1.json`. Both override cases returned preflight exit 0. The first compared `default unix:///selected-by-env.sock` on both sides, but post-strip resolution was `desktop-linux unix:///Users/u/.docker/run/docker.sock`. The second compared `env-selected unix:///env-context.sock`, then resolved the same different desktop endpoint after stripping. The Docker double was extended with environment precedence from the vendored Docker CLI source; no daemon was contacted. Executing Lando's actual `strip-env.js` in a Node VM with only lodash iteration/string helpers substituted confirmed deletion of DOCKER_HOST, DOCKER_CONTEXT, DOCKER_CONFIG and RWB_USER_DOCKER_CONFIG. Receipt: `/tmp/astra-lando-strip-env-r1.json`.

Source corroboration: clean Lando checkout `7a87f80576c5cdb5c7d616108bc9aff81150d463`, `utils/build-config.js:66-73` and `utils/strip-env.js:7-10`. Clean DDEV checkout `5da91aeb9ebab0b0e66171c450b72099308d332c`, vendored `github.com/docker/cli/cli/command/cli.go:439-456`, confirms the override-versus-persisted-context distinction.

Suggested fix: reject unsupported daemon/TLS environment overrides before any daemon call, or materialize the effective selection into the private config and verify the endpoint under Lando's actual stripped environment. Use that same effective environment for receipts and cleanup. Add cases for DOCKER_HOST and DOCKER_CONTEXT; the current fake Docker ignores both and cannot catch this regression.

### P2: Dropping TLS material does not fail closed before contacting the daemon

Location: `bench/adapters/ddev/docker_client_config.py:45-48` and `bench/rwb/adapters/ddev.py:45-52`.

The helper copies a TLS context's endpoint metadata but silently omits its TLS directory. Comparing only name and host still succeeds. There is no guard rejecting an active context with TLS material before the subsequent `docker info` call. This contradicts the requested behavior that unsupported TLS contexts fail closed.

This is not merely an assumed Docker error: in DDEV's pinned vendored Docker CLI, `cli/context/store/tlsstore.go:62-70` treats an absent TLS directory as an empty successful result; `cli/context/tlsdata.go:50-70` returns nil TLS data; and `cli/context/docker/load.go:45-48` returns no TLS configuration when SkipTLSVerify is false. The vendored Moby client, `client/client.go:238-248`, then selects HTTP rather than HTTPS when there is no TLS config. With SkipTLSVerify true, the client instead constructs insecure TLS without the dropped client certificate. A conventional mutual-TLS daemon may reject the later request, but that is a server-side failure after the adapter has changed connection semantics, not an explicit preflight rejection.

Offline reproduction in the same probe artifact: both DDEV and Lando accepted a `tls-prod` context at `tcp://tls.example.invalid:2376` whose source fixture contains a private TLS key; both comparisons passed and reached the fake info call. The fake daemon success is only evidence of absent rejection, not proof of a real TLS connection succeeding. The transport downgrade is established separately by the pinned source above.

Suggested fix: detect and reject unsupported TLS material/settings for the effective active context before any Docker daemon operation. Account for inherited DOCKER_TLS, DOCKER_TLS_VERIFY and DOCKER_CERT_PATH as well. Test that such a context exits before info even when its name and host match. The existing mismatch test substitutes another hostname and therefore does not exercise this case.

## Verified changes and boundaries

- All 18 new bootstrap tests passed, including private config credential exclusion, user fixture preservation, post-plugin config checks, missing-key whole-config rejection, exact Compose path, and tampered-copy rejection.
- All 41 existing container-adapter tests passed. Total independently run: 59 tests. The reported 301-test full suite and prior mutation runs were not independently repeated.
- `git diff --check` passed. Pinned DDEV and Lando source checkouts were clean at the commits above.
- DDEV initializes docker/cli with environment-aware options in `pkg/dockerutil/docker_manager.go:61-80`; the vendored config implementation reads DOCKER_CONFIG. The private config therefore addresses the original credential-helper mechanism for the ordinary local-context case.
- Lando's pinned setup hooks return early for the configured skipInstallCa and false buildEngine/buildx/orchestrator flags. `lib/lando.js` gates plugin installation on installPlugins. The supplied config matches these source controls.
- The verified Compose asset is copied, not symlinked, to the exact private `bin/docker-compose-v2.40.3` path chosen by `get-compose-x`. The copy is hashed before rename. The merged-config check follows plugin installation. Version commands record the selected path and hash. These are source/offline confirmations, not a receipt from a fresh real Lando start.
- `check_config.py` does not print the full config/environment when the requested key is absent. The actual Lando JSON formatter emits compact JSON and falls back to the whole config on a missing path; the parser's rejection covers that behavior. No secret leaked in the supplied sentinel tests.
- Private HOME changes Lando's `/user` mount to the run-private home and makes its `.ssh` scan use the private directory, created by Lando core. The host user's SSH key files are no longer exposed through `/user/.ssh`. This changes normal Lando behavior and must remain documented in benchmark validity notes. HOME is not literally empty after bootstrap: it contains `.docker` metadata and Lando creates `.ssh`. SSH_AUTH_SOCK is still inherited; the private HOME change alone is not a guarantee that all agent access is disabled.
- Registry auths/credential-helper keys and TLS key files are omitted by the helper in the tested standard Docker layout. Context metadata is copied wholesale, not field-filtered to endpoint names/hosts; descriptions and other metadata remain. This is not a general secret-sanitization guarantee for arbitrary metadata or symlinked input. No additional exploit finding is asserted without a concrete normal-workflow trigger.
- Live Docker startup, image pulls, CA behavior, workload identity, and post-run cleanup were deliberately not tested. Keep the original failed attempts intact and require fresh runtime receipts after fixes.

## SHA-256 fingerprints

```text
dfc5f14efd514f48e33ab8b5911c6f75f194b23730e4a1490bf4c62f4a6f619e  bench/rwb/adapters/ddev.py
cbb8b91f08b771a60d4b0ed0f4aff7d8955302a00b60cdf3129d8c02e40ac845  bench/rwb/adapters/lando.py
01f293996af55c7a95abd43881c8834b7c46a732f2e8cd8564512bdef4fdc632  bench/adapters/ddev/docker_client_config.py
609854be27ce1ec794e6792edfd048718b472dc0e1e22ea76282fa6eb8d49d36  bench/adapters/lando/config.yml
a944323e69a528fedf2c6bb10abc747078520037bb1cfef8e9de6bb9ee3d838b  bench/adapters/lando/check_config.py
5eab8ab70088332662826dc42c00a4e1e29c59b2566376302e087bc78035c469  bench/tests/test_ddev_lando_bootstrap.py
8d979805f45be51507aa7d85e077a7919cc5d4553ccc2b64be727f94a488c97b  /tmp/astra-ddev-lando-probes-r1.py
92dda31dbaced13f7032a982c1f8fc9eb8af734a583a6a95ec4a233dd37b4410  /tmp/astra-ddev-lando-probes-r1.json
f76380adf382908b835c5bd7b70239e25cde78afc5df00e3c0dfab436c6e68c4  /tmp/astra-lando-strip-env-r1.json
```
