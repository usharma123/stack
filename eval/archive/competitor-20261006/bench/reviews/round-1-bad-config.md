# Bad-config evidence review, round 1

Verdict: REQUEST CHANGES. One verified P1 finding remains in the continuation rule.

## Finding

### P1: An unrelated diagnostic plus indented progress still produces a pass

Location: `bench/rwb/verify.py:280-283`.

`wrapped` treats any refusal line ending in a word or comma as a phrase continued by the next indented line. That accepts ordinary complete diagnostics without punctuation and does not exclude progress or argv text on the following line.

Reproduced input on stderr of a refused setup step:

```text
error: redis package not found
    Pulling postgres:99.99.99
```

`bad_config_evidence` returns `intended` with `error: redis package not found Pulling postgres:99.99.99`. The full fake Scenario returns `bad_config = pass`, detail `setup exit 1 (intended diagnostic)`. Nothing here says the requested postgres version was refused.

Prepending `error getting credentials - err: exit status 1` produces the same pass. The fabricated continuation becomes the last intended diagnostic and replaces the credential prerequisite. This defeats the core false-positive fix for indented or interleaved multi-service output. These are synthetic counterexamples, not claims that these exact bytes appear in the retained logs.

Suggested fix: restrict continuation matching to a recognized unfinished refusal phrase, such as Pixi's `No candidates were found for`, with a following package/version continuation. Exclude a fresh progress/command/log record. Preserve the observed Pixi wrap, and add classifier plus Scenario regressions for both examples above. Do not loosen the tag requirement to restore mise's historical pass.

## Verified behavior

- All 22 embedded cases classify as expected. Tilt, Vagrant, DevPod, Berth and BranchBox return prerequisite; mise-2 returns no intended evidence; the other 16 return intended evidence.
- All 44 referenced raw stdout/stderr files exist locally. The provenance test verified each full-file SHA-256 and embedded bytes. devenv-2 embeds only the final 1200 stderr bytes, while its full raw file hash is checked.
- Tilt and Vagrant raw outputs also pass through Scenario and produce blocked. The retained compose refusal produces pass.
- ANSI stripping preserves devenv's genuine missing-attribute diagnostic. Pixi's observed continuation and isola's loopback connection refusal remain supported.
- Same-line prerequisites take priority. A terminal prerequisite in either stream blocks. Within one stream, a genuine later tag-associated refusal supersedes an earlier prerequisite, as the requested retry rule specifies. The finding above lets an unrelated continuation falsely qualify as that later refusal.
- `testing.py:180-187` supplies synthetic default refusal text only in FakeTransport. Explicit `world.outputs` replaces both output streams afterward, and mutators run after replacement. The reproduced negative cases supplied explicit output and were not masked by that fallback. The production Scenario does not import FakeTransport. Generic fake adapter success is still not evidence of real diagnostic compatibility; the retained-log tests supply that evidence for the covered cases.
- The original Sol report identifies the Tilt/Vagrant credential false positives and says to retain original outcomes. This review made no result changes and makes no claim that historical results have been corrected.

## Tests and evidence

Ran:

```sh
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest bench.tests.test_bad_config_evidence -v
```

Result: 16 tests passed, no skips. Separately ran offline classifier and full fake-Scenario probes for the two counterexamples, a legitimate Pixi wrap, and a genuine credential-then-registry-refusal retry. The latter two passed as intended. The two counterexamples incorrectly passed.

Probe inputs, outputs, all 22 replay classifications, and the 44 raw-file hashes are saved in `/tmp/astra-bad-config-r1-evidence.json`. The 44 files were hashed again before writing this report; all matched the earlier snapshot.

The reported 350-test full-suite result was not independently rerun. Suite inspection shows socket listeners and live process tests, so running indiscriminate discovery would exceed this review's no-services/network boundary.

## Fingerprints and scope

Checkout: `/Users/utsavsharma/.t3/projects/stack`.
HEAD at inspection: `201ad6ef66d5f15e0a13dcb00977d021245fe3ac`.
Reviewed dirty working-tree bytes, not just HEAD. SHA-256 values below were unchanged between initial fingerprinting and report preparation.

| File | SHA-256 |
| --- | --- |
| `bench/rwb/verify.py` | `aa242abc21696b56ae6483669c268d900f3bd626c1ed1e769f25f88c76b94b93` |
| `bench/rwb/adapters/base.py` | `b4462a9b7ac54425d2d877ee8d2679ef963951d6797704e19bf0a04db4c5775e` |
| `bench/rwb/scenario.py` | `a4e2c00016164775dc9064e412723f95995c88817daa73c1b44b2d95878508c3` |
| `bench/rwb/testing.py` | `41079e92a3c0e4d936a0f5b559b221ce78c4106dfba933e82b4679cc346f628e` |
| `bench/tests/test_bad_config_evidence.py` | `ff21ef452b96e3760c88c0e92e312eb06de4bd3f4ad74f316126cd52d68b8213` |

## Limits

Read-only repository review. Writes were confined to the two `/tmp` review artifacts. No edits, commits, delegation, Docker calls, services, or network calls. Other agents' run/Tilt/DDEV work was not reviewed. Retained raw evidence establishes these past diagnostics only; no new runtime compatibility claim is made. The supplied retained-log test uses an abbreviated devenv stderr body, so the classifier replay for that case is a tail replay rather than classification of its full stream.
