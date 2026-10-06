# Sol adapter fix handoff

Read-only understanding for the sole Opus 5.5 implementer. Branch
`codex/realworld-competitor-bench`, observed HEAD
`eb7dade7b6dba6ca97e7d4e7451f4d07d8bfd2e8`, 2026-10-06.
Implement only these five verified Astra findings and their regression tests.
No new features, provisioning, real service runs, or core changes are needed.
`bench/ADAPTER-CONTRACT.md` reserves base/scenario/verify/transport/registry/fixture
changes to the core owner. Keep exact project filters and unrelated resources safe.

I inspected current source and reran the existing reproduction against both its
snapshot and this checkout. All seven demonstrated failures reproduce on this
checkout. The original script is `/tmp/rwb-astra-all-3jq0hsnd/repro_review.py`.
Its assertions describe the bugs, so an assertion failure after fixing a bug is
expected; convert the cases to proper regression assertions. The reported 103
passing adapter tests are prior Astra evidence, not a new test run here.

## 1. P1: Lando passes the wrong ownership token

Paths: `bench/rwb/adapters/lando.py:82-84`, inherited callers in
`bench/rwb/adapters/devcontainers.py:198-219`, guard in
`bench/adapters/devcontainers/compose_receipt.py:155-166`.

`20261006t120000-abc123` becomes project `rwb20261006t120000abc123a`.
Inherited `running`, `resources`, and `remove` still pass the raw run ID.
`_owned` correctly rejects the project, blocking process/resource listings and
fallback cleanup. The reproduction reports exactly that refusal.

Minimal fix: keep the raw `self.run_id` and current project names. Add a family
ownership-token property on `ContainerAdapter`, defaulting to `self.run_id`, and
use it only in the three owned-receipt callers. Override it in Lando with the
same lowercase alphanumeric normalization as `project()`, and have `project()`
reuse that property. No core hook is involved. Update the ownership description
in validity metadata for Lando if it still claims a literal raw run ID.
Keep `_owned` unchanged, including its minimum token length and refusal checks.

Tests in `test_container_adapters.py`: execute Lando's generated process,
resource, and fallback removal receipts with its normalized project in the
existing fake-Docker state. They must succeed with a punctuated run ID; removal
must leave foreign containers, volumes, networks and images untouched. Another
run's selector and a too-short normalized token must still fail before removal.
Keep raw tokens for Dev Containers, DevPod and DDEV. Existing line 293 assumes
every removal uses raw `RUN`; make that assertion adapter-specific and retain
the exact generated-selector assertions.

## 2. P2: shared stop success skips the appended checks

Paths: `bench/rwb/adapters/agent_env_common.py:119-125`,
`bench/rwb/adapters/berth.py:184-196`,
`bench/rwb/adapters/branchbox.py:191-197`.

`project_stopped()` returns an inline body with `exit 0` when Docker finds no
running containers. Both callers append checks, but this exit terminates their
whole shell. Both currently pass with zero volumes; Berth even passes without
its generated env file. Trace contains only the initial `docker ps`.

Minimal fix: replace success `exit 0` with a loop `break` plus a success flag
checked after the loop. Retain `set -euo pipefail`, failure on a Docker query,
the existing running-container filter, polling interval and timeout failure.
Successful polling must return control to the caller. Keep Berth's port/env
checks and both adapters' existing requirement of at least two volumes.
Only Berth and BranchBox call this helper.

Tests in `test_agent_env_adapters.py`: execute the actual generated bodies.
For both tools, no running containers plus zero or one volume must fail; two
retained volumes must succeed when the remaining conditions are valid. Assert
the trace reaches `volume ls`. Berth must fail for a missing env file and for
either published port still accepting connections; valid env and closed ports
must pass. Docker query failures and a container remaining through the polling
budget must fail. Use stub `seq`/`sleep` to make timeout cases immediate; a port
test can use a short-lived local socket fixture with teardown, not tool services.

## 3. P2: Tilt treats a failed stop query as absence

Path: `bench/rwb/adapters/tilt.py:198-201`.

`test -z "$(docker ps ...)"` succeeds when Docker exits 1 without stdout.
The reproduction exits 0 while stderr says the daemon cannot be reached.

Minimal fix: first assign the query result with an explicit failure guard,
then test that variable for emptiness. For example, `left=$(docker ps ...)
|| exit $?`, followed by the existing empty-result success branch. Keep
`-aq`, the exact Compose project label, the timeout and normal polling semantics.
Putting only `set -e` before the existing `test` does not fix this masking.

Tests in `test_additional_adapters.py`: failed query with empty stdout must fail;
failed query with nonempty stdout must also fail. Successful empty query must
pass; successful nonempty query must wait/fail, and a later successful empty
query must pass. Assert every query uses this checkout's exact project label.

## 4. P1: Tilt and Vagrant emit successful empty identities

Paths: `bench/rwb/adapters/tilt.py:188-196`,
`bench/adapters/vagrant/vagrant-resources.sh:27-31`.
Vagrant's adapter invokes the script at `bench/rwb/adapters/vagrant.py:156-157`.

`printf` returns success even when its command substitutions fail. Vagrant's
existing `set -euo pipefail` does not protect these substitutions. Both currently
exit 0 and emit an identity with all IDs empty on a Docker daemon failure.
Tilt also pipes its volume query through sorting/formatting. Vagrant's volume
loop can lose the first inspection failure when the second inspection succeeds.

Minimal fix: gather each raw Docker result in a separate assignment with an
explicit nonzero-status guard before emitting JSON. Tilt needs separate
Postgres, Redis, volume and network query assignments; format volumes only
after the raw query succeeds. Vagrant needs separate pg, redis, network and
each of its two named-volume inspections. Reject empty required IDs/names even
when Docker exits 0. Keep existing JSON keys and string-valued volume fields,
deterministic volume formatting, exact Tilt project/service label filters, and
exact Vagrant instance resource names. Emit the receipt only after all checks
pass. Avoid a new receipt format or changing Vagrant create/remove behavior.

Tests in `test_additional_adapters.py`: parameterize every individual Docker
query failing while all later queries would succeed, including Vagrant's first
volume inspect. Each case must return nonzero and emit no success JSON. Repeat
with exit 0 plus an empty required result. Valid complete results must preserve
the existing parseable JSON schema and distinguish A from B. Check exact
filters/names in the call log and that identity performs no mutation.

## 5. P2: worktree cleanup and verification lose earlier query failures

Paths: `bench/rwb/adapters/worktree_common.py:346-370` and `382-386`.
Affected shared family is Workz, Worktrunk and Git Grove.

Cleanup groups three Docker listings in a command-substitution pipeline. Bash
does not reliably inherit `errexit` there; the group's last successful command
hides an earlier failed query even with outer `pipefail`. Verification puts
three substitutions in one assignment, whose status is the final substitution's
status. Both currently return 0 when `docker ps` fails and later queries succeed.

Minimal fix: collect container, volume and network query results in separate
guarded assignments. In cleanup, do this before any deletion, then combine the
successful outputs through `printf` and `sort -u`. Preserve the existing exact
project regex, label-filtered removal, order, and image ownership filtering.
In verification, guard each query independently and check that all three
successful outputs are empty. Do not weaken failure to an empty result or add
`|| true`. Existing regex tests should continue to pass unchanged.

Tests in `test_worktree_adapters.py`: fail each of the three discovery queries
individually while later queries succeed; cleanup must fail and log no removal.
For verification, each query failure must fail, successful empty results must
pass, and a leftover of any one kind must fail. Execute cleanup with owned
hyphen/underscore projects plus foreign and lookalike projects, then assert only
exact owned projects are removed. Cover all three adapters' shared-body usage.

## Validation commands

Run from `/Users/utsavsharma/.t3/projects/stack` after implementation:

```sh
python3 -m unittest bench/tests/test_container_adapters.py bench/tests/test_agent_env_adapters.py bench/tests/test_additional_adapters.py bench/tests/test_worktree_adapters.py -v
bash -n bench/adapters/vagrant/vagrant-resources.sh
python3 -m unittest discover -s bench/tests -v
git diff --check
```

The new cases must run generated Bash and the Vagrant script against executable
Docker stubs in a temporary directory. Fake scenario receipts and syntax checks
alone cannot expose these failures. Use an explicit stub-only PATH and logs;
never allow a test to invoke the real Docker CLI. Do not run timed benchmarks.

To replay the current failure demonstration against this checkout without
editing the snapshot script:

```sh
python3 - <<'PY'
from pathlib import Path
p = Path('/tmp/rwb-astra-all-3jq0hsnd/repro_review.py')
s = p.read_text().replace('root=pathlib.Path(__file__).resolve().parent',
                         "root=pathlib.Path('/Users/utsavsharma/.t3/projects/stack')")
exec(compile(s, str(p), 'exec'), {'__file__': str(p), '__name__': '__main__'})
PY
```

## Source SHA-256 at inspection

```text
faf63ea1b564cb951936ba80b2ed7f475ade5ca8481896ca204ae18387730bc5  bench/ADAPTER-CONTRACT.md
6448d5406dbbdd602d95d5e216069693f30909263ed735c6422c75cce3e4e83c  bench/rwb/adapters/lando.py
60041bd4d44f1707c4c5993df1d6c01b8f79e870e920feffd9972762627de61a  bench/rwb/adapters/devcontainers.py
e04505467ebf3ab6dfd9a124690cd0b2067e6d422ad08d63004d3353f4b56f0a  bench/adapters/devcontainers/compose_receipt.py
15e4d4d62fe2839265804cdee89bbc5ca4669972814bde4ba93a803fa47344b6  bench/rwb/adapters/agent_env_common.py
84d44bc60e5df234b6db90830406f3d898ec962ad4f8e2e5e8a4192e5801fd35  bench/rwb/adapters/berth.py
996fce60080f5774baac8ea53ec74652a27f8de3772f98f4805b1ddcd02fee98  bench/rwb/adapters/branchbox.py
4e4755b4cebe75e9d3ef4719e1d59aa728a3ff5a51830c4e7035046f74102592  bench/rwb/adapters/tilt.py
d057a6686b699e47548b99f14f4a46a60b5558679c58927d43b440d46695c540  bench/adapters/vagrant/vagrant-resources.sh
ba90da50e33dcbed2c8f8b575ddab79a88f2f2ff1b60f7fafed1fdb59403e126  bench/rwb/adapters/vagrant.py
f3ef08c536ffc8e8685844a9a06f13d2e294b2fdbf28ae3f40d780cfee5eef55  bench/rwb/adapters/worktree_common.py
```
