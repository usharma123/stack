# Astra independent R2 review

Verdict: REQUEST CHANGES. One verified P1 finding remains in failure cleanup. The original environment-override mismatch and active-TLS preflight findings are fixed at preflight, but TLS rejection does not remain fail-closed through teardown.

Scope: the six DDEV/Lando bootstrap files named in `bench/reviews/round-1-ddev-lando-bootstrap.md`, in `/Users/utsavsharma/.t3/projects/stack`. HEAD observed during R2: `9dd1c20cc03c069fe937a0435291492663c93ab7`. Shared teardown and receipt code was read only to trace the failure path. No repository edits, commits, delegation, network, builds, live Docker calls, services, or benchmark runs. Scratch scripts and results were written only under `/tmp`; Python bytecode writing was disabled for test runs. Concurrent run/base/scenario/Tilt work was not modified.

## P1: TLS-blocked preflight falls back to the default daemon during cleanup

Locations: `bench/rwb/adapters/ddev.py:109-118` and `bench/rwb/adapters/lando.py:144-154`. The helper rejects active TLS at `bench/adapters/ddev/docker_client_config.py:54-59`, before writing config at lines 89-97.

Concrete trigger: persisted `currentContext=tls-prod`, endpoint `tcp://tls.example.invalid:2376`, with active TLS key material or `SkipTLSVerify=true`, and no exported Docker overrides. Both adapters correctly exit 77 before any Docker call. They have created the destination directory but no `config.json`. Teardown still invokes `service_processes`, `cleanup_host`, and `host_resources`. Their env guard only rejects exported variables, so it passes. Each body sets DOCKER_CONFIG to the empty private directory. Docker consequently resolves `default unix:///var/run/docker.sock`, not the user's selected TLS endpoint.

The real receipt script then issues Docker queries on that unintended daemon. `cleanup_host` also enters the owned-resource removal path. The offline reproduction used an empty fake daemon, so it demonstrates wrong-daemon queries and entry into removal, not an actual deletion or unscoped resource deletion. Ownership filters remain in place. All three cleanup bodies returned 0 in this fixture, which can also falsely certify cleanup against the wrong endpoint.

Evidence: `/tmp/astra-ddev-lando-r2-cleanup-probe.py` and `.json`. For both active-key and SkipTLSVerify cases, preflight returned 77 with zero tool calls. DDEV then issued 10/64/32 Docker calls for service_processes/cleanup_host/host_resources; Lando issued 5/34/17. Each body resolved the default socket and returned 0. The script imports the supplied test harness, invokes the actual generated bodies and actual `compose_receipt.py`, and runs only fake Docker executables. Effective endpoint resolution is an explicit separate fake context-inspect check after each body.

Source path: `Scenario.cleanup` always probes service processes; `bench/run.py:300-309` always performs host cleanup and resource inspection for host adapters. Neither needs a successfully provisioned checkout. The new regression test covers rejected environment overrides through cleanup, but active-TLS tests stop after the preflight assertion.

Suggested fix: require successfully validated private daemon selection before any daemon-using receipt or cleanup body can run. A run-private readiness marker bound to the accepted config/endpoint, or an equivalent adapter-local validation guard, must also reject missing/incomplete configuration after failed preflight. Extend tests to invoke actual teardown bodies after active TLS and SkipTLSVerify rejection and assert zero Docker/Lando/DDEV calls. Preserve an honest nonzero cleanup result such as 77; do not blanket-convert cleanup 77 to success. Ensure any solution still cleans up resources after failures that occur after daemon selection was successfully validated.

## Checks and resolved points

- Independently ran all 26 bootstrap tests and all 41 container-adapter tests: 67 passed. Logs: `/tmp/astra-ddev-lando-r2-bootstrap.txt` and `/tmp/astra-ddev-lando-r2-container.txt`.
- Re-ran `/tmp/rwb-r1-fix/probe_tls_active.py`. Both adapters reject actual active TLS material and SkipTLSVerify with 77 before info. Ordinary desktop fixtures pass. Receipt: `/tmp/astra-ddev-lando-r2-tls.json`. The old R1 TLS fixture was not used as evidence for active TLS.
- The guard checks presence through printenv, so even an empty exported override is rejected. Both adapter env prefixes and the helper reject HOST, CONTEXT, TLS, TLS_VERIFY, CERT_PATH. Supplied tests cover later receipt, install and cleanup bodies with overrides.
- Only active context metadata is newly copied. Inactive TLS material does not block ordinary desktop contexts. Credential auth/helper config and TLS files are not copied in the standard fixture; this is not a general arbitrary-metadata sanitization claim.
- Lando's socket check runs before info and requires the context unix endpoint's realpath to equal the engine socket realpath. This matches pinned `utils/get-engine-config.js` default `/var/run/docker.sock`. Wrong-socket and TCP endpoint tests pass. This comparison is static and is not a daemon identity receipt.
- The pinned Lando source checkout is clean at `7a87f80576c5cdb5c7d616108bc9aff81150d463`. Inspected get-engine-config, get-compose-x, lando-run-setup, build-config and setup guards. The skip-CA/build-engine/buildx/orchestrator/plugin controls, private HOME, copied and verified exact Compose path, post-plugin config checks, and version/hash receipts remain consistent with that source and focused tests.
- Private HOME validity notes correctly describe the changed `/user` mount, private `.ssh` scan, and inherited SSH_AUTH_SOCK. No claim of disabling all SSH agent access is warranted.
- `git diff --check` passed.

## Limits

The full suite was not independently re-run. The task brief reports 323 passes and one failure in concurrently edited initialization-failure tests; that remains an unresolved full-suite result, not a dismissed failure or a green final gate. Fresh live retries are pending. This review does not establish real startup, pulls, CA behavior, workload identity, or live cleanup success. No recommendation to reinterpret every cleanup exit 77 as success is made.

## SHA-256 fingerprints

```text
7bec55c2e5fa55f637c167a533f3ea1366d9c68903c3fbc65c0e1c5fc3266bc1  bench/rwb/adapters/ddev.py
a141b3d7a8d63d6de17a3d0a9aa20797d970666da5f23112ae11e9ce2db426e7  bench/rwb/adapters/lando.py
798132010ba7532c770e85a173d443412f31deebd60a98b63ec674b576031eca  bench/adapters/ddev/docker_client_config.py
609854be27ce1ec794e6792edfd048718b472dc0e1e22ea76282fa6eb8d49d36  bench/adapters/lando/config.yml
a944323e69a528fedf2c6bb10abc747078520037bb1cfef8e9de6bb9ee3d838b  bench/adapters/lando/check_config.py
78e7b0b61922bfceb3865d40423723b14f86adaeb1ae183b4e1919a8e89e88c0  bench/tests/test_ddev_lando_bootstrap.py
900eba5479b55e07194f7109fc84a40494c7607c1be5a15b0ea1198e558752cb  /tmp/astra-ddev-lando-r2-cleanup-probe.py
f5cadd5850da85ad13739a1905a497c4a136c0a48f017b25c44a058c257cbded  /tmp/astra-ddev-lando-r2-cleanup-probe.json
d92e92c0ec26fb0828e6f3eba2a5db67e7049840dda983fd468bc0d191cfcb18  /tmp/rwb-r1-fix/probe_tls_active.py
99df535d2e0c930e1629139a6b03bce65ba32f15088df3dda13e2ab4b6a50a1c  /tmp/astra-ddev-lando-r2-tls.json
ceebb5930480595b4b55b0ff4565816849f83986e8951c90cf5fbd60b2028c90  /tmp/astra-ddev-lando-r2-bootstrap.txt
43820ef293cbf53c064838ee0780174d2f4f3d2981607891d5907578d26665ae  /tmp/astra-ddev-lando-r2-container.txt
```
