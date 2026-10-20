# Astra independent R3 review

Verdict: NO FINDINGS in the reviewed DDEV/Lando implementation and updated container fixtures. The R2 P1 cleanup defect is fixed in offline execution. The reviewed full-suite snapshot passed all 378 tests, including the updated container fixtures. Fresh live retries remain pending.

Scope: `/Users/utsavsharma/.t3/projects/stack`, observed HEAD `6374702a3f8f76243ae1bb0bf49cd77486603f4e`. The literal supplied `stack6DDEV` directory does not exist; the prior R1 and R2 documents identify this checkout and its six bootstrap files. Reviewed those files plus the newly updated `bench/tests/test_container_adapters.py`. Read shared adapter/teardown code only to trace execution. Core and Tilt work was not edited or independently approved by this review.

Read-only checkout review. No source edits, commits, delegation, network, live Docker, builds, service starts, or benchmark runs. Review scripts, results, and the full-suite source snapshot are under `/tmp`. Tests use fake Docker/Lando and synthetic resources. The full suite also exercises short-lived test processes; these are not real workload services. The snapshot avoids repository writes from tests that explicitly compile Python files.

## Resolved R2 finding

The marker is outside the private Docker config. Preflight removes the prior marker before building config, compares the user/private context name and endpoint, performs Lando's engine-socket check, then seals selection before `docker info`. The inherited Docker override guard runs even earlier, before preflight writes; an override rejection therefore does not remove an existing marker, but also prevents all later bodies under that override from executing tools.

Every later daemon-using body receives `env(validated=True)`. Its helper verification reads the marker and private files without invoking Docker, and the shell converts verification failure to 77 before reaching the body. Named-context endpoint changes, missing or modified config, missing/malformed marker, and added TLS material fail closed. A valid seal survives a later info failure, so cleanup can still attempt the previously validated daemon.

Independently re-ran the original `/tmp/astra-ddev-lando-r2-cleanup-probe.py` against current source. Both adapters, each with active TLS keys or SkipTLSVerify, returned preflight 77 with no tool calls. All 12 later cleanup invocations returned 77 with zero Docker calls. Result: `/tmp/astra-r3-cleanup-probe.json`. This corroborates the supplied `/tmp/astra-r3p1-cleanup-probe-after.json` claim without treating the supplied result as a new independent run.

No blanket cleanup-77 success conversion was introduced. The actual shared teardown still records nonzero host cleanup and failed leftover queries as cleanup problems. The seal failure therefore cannot certify a clean daemon.

## Default-context and marker limits

Verified actual helper/adapter behavior with `/tmp/astra-r3-default-probe.py`; result `/tmp/astra-r3-default-probe.json`.

For both adapters, a default-context preflight seals `default unix:///var/run/docker.sock`; an unchanged seal passes silently. Changing only the marker's endpoint to `default unix:///different.sock` still passes verification. `docker_client_config.py:144-145` returns no stored host for default, and `:159-162` deliberately compares only its name and token shape. Consequently this is not an independently verified default endpoint after preflight, and the marker is not an authenticated tamper-proof receipt.

This probe does not demonstrate daemon redirection. The marker does not supply an endpoint to Docker. Docker resolves the built-in default from its CLI defaults and environment; supported bodies reject the daemon/TLS environment overrides and supply no host flag. The pinned Docker CLI source confirms Unix default `unix:///var/run/docker.sock` in `opts/hosts_unix.go`, selection through `resolveDefaultDockerEndpoint` and `getServerHost` in `cli/command/cli.go`. A marker-only string change therefore changes the claimed record, not the actual selection. This remains a limit, not a new reproduced wrong-daemon cleanup finding.

Adding mutable `buildx/activity` after sealing passes verification, as intended. Adding a TLS file under `contexts/tls` returns 77. Inherited DOCKER_HOST returns 77 before tools. The marker hashes config.json and contexts files, not all plugin state. It does not attest daemon identity, socket inode/realpath stability after preflight, arbitrary executable replacement, or hostile concurrent modification of the config and marker together.

## Regression coverage and fixtures

All 34 bootstrap tests independently passed in 60.22 seconds. Log: `/tmp/astra-r3-bootstrap.txt`. Coverage includes active TLS through later bodies, missing preflight, valid seal followed by info failure, silent successful verification, nine missing/tampered selection cases, rejected repeated TLS preflight, Lando engine mismatch, and named-context sealing mismatch, in addition to prior bootstrap guards.

The updated container fixtures arrived during review and are included in the full-suite snapshot. SHA-256: `d2dab88963ef2bf01b49b942dd60cdaa44467fb80805cf9ef94db470b3732c40`. They use the actual build/seal helper on temporary private configs instead of bypassing verification. Lando receipt fixtures now have a unique temporary root. Planned-body assertions expect `validated=False` only for the sealing preflight. Existing cleanup assertions still require owned removal, preservation of foreign resources, and nonzero results for query/removal/shared-cleanup failures. The reported earlier 367-pass/8-fail run is not being dismissed; the completed current full-suite result is required below.

Full-suite result: **378 passed in 115.66 seconds**, with no exclusions. Log: `/tmp/astra-r3-full-with-git.txt`. Command:

```sh
GIT_DIR=/Users/utsavsharma/.t3/projects/stack/.git GIT_OPTIONAL_LOCKS=0 PYTHONDONTWRITEBYTECODE=1 python3 -m pytest -p no:cacheprovider /tmp/astra-r3-review-snapshot/bench/tests -q
```

The first snapshot run had 377 passes and one failure in `ManifestTest.test_template_uses_current_bytes_and_no_review`, because the copied tree lacked Git metadata and the template's commit was None. That test passed independently in the original checkout, then the complete snapshot suite passed with the original Git metadata made available to its read-only Git commands. Logs retained: `/tmp/astra-r3-full.txt` and `/tmp/astra-r3-manifest-checkout.txt`. No assertion or source patch was used to obtain the passing result.

`git diff --check` passed. The six bootstrap files and updated container fixtures remained identical to the test snapshot at final comparison. Core owner edits changed run.py during the test run, and HEAD advanced to `8e40e3f13c2cc0feaac912de6d4c2457e6662111`. The 378-pass result applies to the frozen snapshot, including run.py hash `e0c7c1006a88910f060dc4a1f19e9872a379e6bf8cecfae9c7565e17c3649e00`, not those later core edits. `/tmp/astra-r3-full-snapshot-manifest.json` records all snapshot file hashes.

## Preserved controls and live limits

The Docker override guard, rejection of active TLS and SkipTLSVerify, credential/helper omission, private Lando HOME, skip-CA/autosetup controls, post-plugin merged-config checks, verified Compose copy and exact versioned path, and existing asset/plugin/image pins remain in place. The pinned source checkouts were verified clean at DDEV `5da91aeb9ebab0b0e66171c450b72099308d332c` and Lando `7a87f80576c5cdb5c7d616108bc9aff81150d463`. Lando's engine default remains `/var/run/docker.sock`; its Docker environment stripping is still why the private HOME and override rejection matter.

Private HOME retains the documented `/user` and `.ssh` behavior change. SSH_AUTH_SOCK remains inherited. No claim of disabling all SSH agent access is made. Metadata is copied rather than comprehensively sanitized; this review does not establish arbitrary metadata secrecy.

No fresh live startup, image pull, system CA, workload identity, or real cleanup receipt was produced. The original live failures must stay recorded; this offline review does not close those runtime gates.

## SHA-256 fingerprints

```text
c1c3c128a5884679aeb147c4f717100d8d7120278b5334fd8a828f619c882211  bench/rwb/adapters/ddev.py
0312832e8780e20bb3a5fd7a5d45f1a42f3392cb9871d970a531a56f1c826fe2  bench/rwb/adapters/lando.py
95edf0682c74eb194f6933a36db04ec61a5b45abacdf66dbbfaa7c93832ef74a  bench/adapters/ddev/docker_client_config.py
609854be27ce1ec794e6792edfd048718b472dc0e1e22ea76282fa6eb8d49d36  bench/adapters/lando/config.yml
a944323e69a528fedf2c6bb10abc747078520037bb1cfef8e9de6bb9ee3d838b  bench/adapters/lando/check_config.py
7fe0ce3e415f7c2f55933e6d2f78a3efe531ca8d4d80b9dfda7c0dd42ec49196  bench/tests/test_ddev_lando_bootstrap.py
d2dab88963ef2bf01b49b942dd60cdaa44467fb80805cf9ef94db470b3732c40  bench/tests/test_container_adapters.py
e0c7c1006a88910f060dc4a1f19e9872a379e6bf8cecfae9c7565e17c3649e00  bench/run.py
900eba5479b55e07194f7109fc84a40494c7607c1be5a15b0ea1198e558752cb  /tmp/astra-ddev-lando-r2-cleanup-probe.py
59bf12af2009827c9f5db03d7b1eee7acb1ebb6ac8e01e52f03c8544d86c04ad  /tmp/astra-r3-cleanup-probe.json
629de0f550793a93f5b5a113924dc1064a66a675a174103bfb361fe942ce539f  /tmp/astra-r3-default-probe.py
20217c67cab2bfd566ca70b344016efa579d1a79e90748195dc1e8881594305e  /tmp/astra-r3-default-probe.json
2ac80224d3543d5238af3c0af3013ffd00e896dd1d6cc917f9e917324aac272a  /tmp/astra-r3-bootstrap.txt
9987b9723c5fc9b9b6e87489ebcc76d6ca5d586bd6e99fc1882e3703e3d44e2a  /tmp/astra-r3-full.txt
4e8b6c5a89da44bf05d17100903172be04033067c113a6831bdfafbd42b7da52  /tmp/astra-r3-manifest-checkout.txt
15ecf4a29a36b488db99d272dddcd4fe07fe4a8ff1743970e9fbd90c2944692b  /tmp/astra-r3-full-with-git.txt
d8678ff72cd3eac7278b5251c9e0272a93a4122860fd39788a0a4cb1d2072169  /tmp/astra-r3-full-snapshot-manifest.json
```
