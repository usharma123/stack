# Remaining runtime results

The nine requested remaining lanes were attempted in order, followed by a fresh Flox retry. Eight remaining lanes and Flox emitted complete scenario receipts. GitGrove failed before its first scenario receipt. Every invocation used 2 repeats, 1 warmup, keep=false, no adapter options and default resources. All results and added diagnostics are non-reportable. No final timing report, score or ranking was produced. Original failed/blocked outcomes are retained.

This was the sole live runtime executor in `/Users/utsavsharma/.t3/projects/stack`, using Sol 6.1 High. No delegation, commit, adapter/harness/fixture/product edit, global installation, shared service change or unrelated build was performed. Only lane provisioning/builds and owned cleanup ran. Runtime receipt/resource inspection scripts stayed under `/tmp`; results and this review are the deliverables. Existing Docker base layers/build cache were not pruned.

## Host, source and serialization

Host Darwin 24.6.0 arm64, harness Python 3.12.9. Docker desktop-linux, client 28.1.1 / daemon 29.1.3 linux/arm64. Host plugin Compose 2.40.3-desktop.1; Tilt uses a privately downloaded hash-checked Compose 5.6.0. Workz/Worktrunk application processes run natively on macOS and use Docker service containers. Berth/BranchBox/Tilt/Vagrant app processes run in Linux containers through host-transport CLIs. isola, Organist and Flox run through DockerTransport. These transport and source boundaries are distinct.

Baseline HEAD was `7a545d92c8ca12604fcc96425f64cf7bb6413b73`; the checkout changed during execution. Other authorized agents edited only the DevPod/DDEV/Lando lane code/config/tests. The per-run commit and complete byte hashes below and in each meta.json are the evidence; there was no frozen final-session source claim. Dirty=true can also reflect the preexisting untracked research/HANDOFF.md. No relevant lane, scenario, transport, fixture or shared-glue bytes changed in the final baseline comparison. Unrelated DDEV/Lando additions/changes and checkout commits are listed in the final resource/source snapshot.

Baseline and after-each-run inventories are under `bench/results/remaining-runtime-verification/`. `run-ledger.json` contains exact per-run pins, file maps, raw-file SHA-256 values, outcome counts, source identities, copied artifacts and receipt references. Standard invocation intervals are strictly serial and non-overlapping by recorded Unix nanoseconds. GitGrove's initializer ran between Worktrunk completion and isola startup and emitted no task/timing receipt.

Exact invocation shape, separately for each lane:

```sh
PYTHONDONTWRITEBYTECODE=1 python3 bench/run.py --tool <tool> --out bench/results/<fresh-name> --repeats 2 --warmups 1
```

| Tool | Exact result path | Run ID | Recorded outcome counts | Independent finding |
|---|---|---|---|---|
| workz | `bench/results/smoke-workz-1` | `20261006t160603-d11564` | `{"observed": 2, "pass": 27}` | See lane evidence; diagnostics only |
| worktrunk | `bench/results/smoke-worktrunk-1` | `20261006t160659-989c60` | `{"observed": 2, "pass": 27}` | See lane evidence; diagnostics only |
| git-grove | `bench/results/smoke-git-grove-1` | `20261006t160737-959f8c` | `{}` | Verified adapter startup defect; no workload |
| isola | `bench/results/smoke-isola-1` | `20261006t160819-4e45c2` | `{"not_applicable": 1, "observed": 3, "pass": 24, "unsupported": 2}` | See lane evidence; diagnostics only |
| berth | `bench/results/smoke-berth-1` | `20261006t160859-8a5d42` | `{"blocked": 1, "not_applicable": 1, "observed": 2, "pass": 23, "unsupported": 2}` | Main workload passes; bad_config environment-blocked |
| branchbox | `bench/results/smoke-branchbox-1` | `20261006t161017-b3e6f5` | `{"blocked": 1, "not_applicable": 1, "observed": 1, "pass": 23, "unsupported": 3}` | Main workload passes; bad_config environment-blocked |
| tilt | `bench/results/smoke-tilt-1` | `20261006t161123-8af6e8` | `{"not_applicable": 1, "observed": 2, "pass": 24, "unsupported": 2}` | Bad-config false positive and leaked event reader |
| organist | `bench/results/smoke-organist-1` | `20261006t161306-02645b` | `{"observed": 2, "pass": 27}` | See lane evidence; diagnostics only |
| vagrant | `bench/results/smoke-vagrant-1` | `20261006t161521-0018bf` | `{"not_applicable": 1, "observed": 2, "pass": 24, "unsupported": 2}` | Bad-config false positive; environment block |
| flox | `bench/results/smoke-flox-4` | `20261006t161857-c734dc` | `{"observed": 2, "pass": 27}` | See lane evidence; diagnostics only |

## workz

All 27 checks pass. Native worktree creation, navigation, port allocation and removal are realized. Python/uv, Compose services and data volumes are explicit scripting. The app runs on macOS in the actual native worktree; Docker port-map receipts prove its host URLs reach that checkout's PostgreSQL/Redis containers. A/B source tokens, distinct servers, migrations, CRUD/cache, tests, repeated start, stop, B survival and restart persistence are realized. The owned listener is verified after E allocation. Step 69 start returns exit 1 with Docker's address-in-use refusal at the allocated host PostgreSQL port 3040. No relocation is claimed.

- Attempt `bench/results/smoke-workz-1`, run `20261006t160603-d11564`, interval `2026-10-06T16:06:03Z` to `2026-10-06T16:06:39Z`.
- Source commit `7a545d92c8ca12604fcc96425f64cf7bb6413b73`, dirty `True`. Adapter SHA-256 `6f73ae72f4380e379413c18c62e1f7320cb34564e3214abf4e6198c2b68573fa`.
- Complete harness file-map SHA-256 `97f3e2167751ae0702ef526f2be3c9ae5cd9ff0638debc2c3b616f34c04c3df0`; config-map SHA-256 `1ea4849b92844e4016d2d62e3c614d4e4d42af8913a6f8a41cd4d062b66b1a97`. Map digest convention is sorted compact JSON. The underlying exact file hashes remain in meta.json and run-ledger.json.
- Transport `host`, isolation boundary `container`, image identity `host transport; workload image digests/IDs in receipts`.
- Recorded completed `True`, valid `True`, reportable `False`, cleanup problems `[]`, artifact errors `[]`. These flags do not override independent findings.
- Raw receipts `77`; referenced stdout/stderr files `154`. Every sequence and outcome evidence reference exists and every file has a retained SHA-256. Original raw hashes still match the after-run snapshot. Copied artifact files `0`.
- Receipt verification `bench/results/remaining-runtime-verification/smoke-workz-1-receipts.json`; resource verification `bench/results/remaining-runtime-verification/smoke-workz-1.json`.
- Retained `bad_config` = `pass`: setup exit 2 (intended diagnostic); evidence steps `[62, 63, 64]`.
- Retained `occupied_port` = `pass`: refused-at-start; evidence steps `[68, 69]`.
- Actual version receipt `bench/results/smoke-workz-1/logs/0003-tool-version.stdout` and `bench/results/smoke-workz-1/logs/0003-tool-version.stderr`, exit `0`.
- Actual version receipt `bench/results/smoke-workz-1/logs/0009-a-tool-versions.stdout` and `bench/results/smoke-workz-1/logs/0009-a-tool-versions.stderr`, exit `0`.
- A raw identity step `10`: Python `3.13.16`, PostgreSQL `17.6`, Redis `8.10.2`, source token `ccd5b0fd816d83e0`, module `/private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t160603-d11564-xqyc6ezc/w/rwbz--rwb-20261006t160603-d11564-a/rwbapp`. Token matches this checkout's prepare/setup body. Full server/URL/data/mount identity is retained in `bench/results/smoke-workz-1/logs/0010-a-start-identity.stdout` and the receipt verification JSON.
- B raw identity step `28`: Python `3.13.16`, PostgreSQL `17.6`, Redis `8.10.2`, source token `44d325380c81b0bd`, module `/private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t160603-d11564-xqyc6ezc/w/rwbz--rwb-20261006t160603-d11564-b/rwbapp`. Token matches this checkout's prepare/setup body. Full server/URL/data/mount identity is retained in `bench/results/smoke-workz-1/logs/0028-b-start-identity.stdout` and the receipt verification JSON.

Independent name/label/process/tempdir checks find no remaining run resource. The resource snapshot verifies the original 27 container IDs/images/states/start timestamps and matching network/volume inventories.

## worktrunk

All 27 checks pass. Native switch/create, hook and alias execution, hash_port values and foreground removal are realized; the commands in those hooks are declared scripting. A/B app code runs in the native worktree on macOS with verified Docker port maps. The occupied-port start returns its native exit 1 with the Docker bind refusal near port 11428. Native pre-remove hooks remove services and volumes before worktree removal.

- Attempt `bench/results/smoke-worktrunk-1`, run `20261006t160659-989c60`, interval `2026-10-06T16:06:59Z` to `2026-10-06T16:07:29Z`.
- Source commit `7a545d92c8ca12604fcc96425f64cf7bb6413b73`, dirty `True`. Adapter SHA-256 `0437feae392f7e83f0a9be828078e136d6e24546542869b130ea6dc9ee047f70`.
- Complete harness file-map SHA-256 `97f3e2167751ae0702ef526f2be3c9ae5cd9ff0638debc2c3b616f34c04c3df0`; config-map SHA-256 `960a5eaa55f1230c3074cf728fdc985e65fd8799f7c7ef1a8813a2662c76deaf`. Map digest convention is sorted compact JSON. The underlying exact file hashes remain in meta.json and run-ledger.json.
- Transport `host`, isolation boundary `container`, image identity `host transport; workload image digests/IDs in receipts`.
- Recorded completed `True`, valid `True`, reportable `False`, cleanup problems `[]`, artifact errors `[]`. These flags do not override independent findings.
- Raw receipts `77`; referenced stdout/stderr files `154`. Every sequence and outcome evidence reference exists and every file has a retained SHA-256. Original raw hashes still match the after-run snapshot. Copied artifact files `0`.
- Receipt verification `bench/results/remaining-runtime-verification/smoke-worktrunk-1-receipts.json`; resource verification `bench/results/remaining-runtime-verification/smoke-worktrunk-1.json`.
- Retained `bad_config` = `pass`: setup exit 2 (intended diagnostic); evidence steps `[62, 63, 64]`.
- Retained `occupied_port` = `pass`: refused-at-start; evidence steps `[68, 69]`.
- Actual version receipt `bench/results/smoke-worktrunk-1/logs/0003-tool-version.stdout` and `bench/results/smoke-worktrunk-1/logs/0003-tool-version.stderr`, exit `0`.
- Actual version receipt `bench/results/smoke-worktrunk-1/logs/0009-a-tool-versions.stdout` and `bench/results/smoke-worktrunk-1/logs/0009-a-tool-versions.stderr`, exit `0`.
- A raw identity step `10`: Python `3.13.16`, PostgreSQL `17.6`, Redis `8.10.2`, source token `1ba554b712d1b94a`, module `/private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t160659-989c60-vyhy7k7k/w/rwbt.rwb-20261006t160659-989c60-a/rwbapp`. Token matches this checkout's prepare/setup body. Full server/URL/data/mount identity is retained in `bench/results/smoke-worktrunk-1/logs/0010-a-start-identity.stdout` and the receipt verification JSON.
- B raw identity step `28`: Python `3.13.16`, PostgreSQL `17.6`, Redis `8.10.2`, source token `598a0bde82ccdc26`, module `/private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t160659-989c60-vyhy7k7k/w/rwbt.rwb-20261006t160659-989c60-b/rwbapp`. Token matches this checkout's prepare/setup body. Full server/URL/data/mount identity is retained in `bench/results/smoke-worktrunk-1/logs/0028-b-start-identity.stdout` and the receipt verification JSON.

Independent name/label/process/tempdir checks find no remaining run resource. The resource snapshot verifies the original 27 container IDs/images/states/start timestamps and matching network/volume inventories.

## git-grove

`bench/results/smoke-git-grove-1` is preserved with an empty logs directory and zero-byte steps.jsonl. meta.json, outcomes.json and summary.md were never written. The launcher returned exit 1 with `TypeError: Object of type method is not JSON serializable` at run.py:106. The created tempdir supplies run ID `20261006t160737-959f8c`; this is not a fabricated standard meta/receipt. GitGrove was not provisioned, so no actual GitGrove CLI or workload version is claimed. Intended npm pin is 0.1.0-alpha.1.8; expected CLI hash is b8cd71e6d268fc6578da93d8da293728085900e80978dd316e5e53654a426b12, not a realized binary.

Verified source cause: `bench/rwb/adapters/git_grove.py:74` defines `image(self, co)` and shadows the base adapter's metadata image field. run.py:100 stores the method in meta, then JSON serialization fails outside the runner's protected execution/teardown block. A read-only constructor/JSON reproduction confirms the method type and same error, with stdout/stderr/argv/exit retained in `git-grove-model-repro.*`. This reproduction ran no workload and supplies no timing.

Failure-time source snapshot HEAD `7a545d92c8ca12604fcc96425f64cf7bb6413b73`, adapter SHA-256 `a21d12140ec2bcefd14c9fb7c9224776d08f4aa7f7e82af610fcf042a92691ac`. Full failure-time harness/config/fixture/glue map is in `git-grove-startup-failure.json`.

Only the exact empty tempdir `/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t160737-959f8c-fqp9gibq` was removed with rmdir. UID 501, inode 265192066, empty contents and no live token process outside the inspection ancestry were rechecked immediately before removal. Absence was verified. No signal or recursive deletion was used. `git-grove-startup-receipt.json` retains the launcher observation and exact cleanup evidence. No standard raw command receipt was emitted by this failed initializer; the limitation is explicit.

## isola

All applicable checks pass. This is database isolation on benchmark-owned shared servers inside the disposable ev-base container, not separate-server isolation. A and B have the same PostgreSQL system identifier and Redis server run ID, but different PostgreSQL databases and Redis logical indexes 12 and 5. Accessory receipts match .env.isola and each Redis owner marker. isola down stops the keeper and retains reachable data endpoints by design; explicit stop/data-endpoint observations and restart/persistence receipts are present. isola destroy proves the recorded databases and Redis indexes are empty/gone before shared-server stop. Lock/frozen setup are unsupported and occupied_port is not applicable.

- Attempt `bench/results/smoke-isola-1`, run `20261006t160819-4e45c2`, interval `2026-10-06T16:08:19Z` to `2026-10-06T16:08:47Z`.
- Source commit `7a545d92c8ca12604fcc96425f64cf7bb6413b73`, dirty `True`. Adapter SHA-256 `e757a09afc8c29480c016c2782b8ecd19ba4cad8bc33ebfe98c12a8e5e4f238d`.
- Complete harness file-map SHA-256 `97f3e2167751ae0702ef526f2be3c9ae5cd9ff0638debc2c3b616f34c04c3df0`; config-map SHA-256 `cf3d7f74a951bb1af5de3420b07e13e6ca5a24811536813dce1277c062e2e00c`. Map digest convention is sorted compact JSON. The underlying exact file hashes remain in meta.json and run-ledger.json.
- Transport `docker`, isolation boundary `database`, image identity `sha256:748dca6569d1619558c68bcdda89b1770896995c3a286a26f72de572d55b7a2b`.
- Recorded completed `True`, valid `True`, reportable `False`, cleanup problems `[]`, artifact errors `[]`. These flags do not override independent findings.
- Raw receipts `64`; referenced stdout/stderr files `128`. Every sequence and outcome evidence reference exists and every file has a retained SHA-256. Original raw hashes still match the after-run snapshot. Copied artifact files `0`.
- Receipt verification `bench/results/remaining-runtime-verification/smoke-isola-1-receipts.json`; resource verification `bench/results/remaining-runtime-verification/smoke-isola-1.json`.
- Retained `bad_config` = `pass`: setup exit 0, start exit 1 (intended diagnostic); evidence steps `[55, 56, 57, 58]`.
- Retained `occupied_port` = `not_applicable`: isola allocates ports only for port-bearing services; PostgreSQL/Redis are shared servers, so a squatted checkout port tests nothing isola owns; evidence steps `[]`.
- Actual version receipt `bench/results/smoke-isola-1/logs/0005-tool-version.stdout` and `bench/results/smoke-isola-1/logs/0005-tool-version.stderr`, exit `0`.
- Actual version receipt `bench/results/smoke-isola-1/logs/0010-a-tool-versions.stdout` and `bench/results/smoke-isola-1/logs/0010-a-tool-versions.stderr`, exit `0`.
- A raw identity step `11`: Python `3.13.16`, PostgreSQL `17.11`, Redis `8.10.2`, source token `6bd178313d25d69d`, module `/home/agent/rwb/a/rwbapp`. Token matches this checkout's prepare/setup body. Full server/URL/data/mount identity is retained in `bench/results/smoke-isola-1/logs/0011-a-start-identity.stdout` and the receipt verification JSON.
- B raw identity step `27`: Python `3.13.16`, PostgreSQL `17.11`, Redis `8.10.2`, source token `c3a6ce56dcc76a4f`, module `/home/agent/rwb/b/rwbapp`. Token matches this checkout's prepare/setup body. Full server/URL/data/mount identity is retained in `bench/results/smoke-isola-1/logs/0027-b-start-identity.stdout` and the receipt verification JSON.

Independent name/label/process/tempdir checks find no remaining run resource. The resource snapshot verifies the original 27 container IDs/images/states/start timestamps and matching network/volume inventories.

## berth

The default private cargo source build completed within this invocation before workload commands. Provisioning checks source commit 3b93287584dcc5c7c26298a62379d8ce001400ff and builds --locked --release with private CARGO_HOME/CARGO_TARGET_DIR. No separate build or prebuilt binary option was supplied. Main workload, source mount/code digest, isolation, stop/restart and persistence checks pass. bad_config is correctly recorded blocked: step 53 fails in Docker credential retrieval before the registry can reject the invalid PostgreSQL tag. This is an environment block, not a Berth product rejection. Native down plus exact-project host cleanup removes the projects, images and worktrees. Lock/frozen are unsupported; occupied_port is not applicable to the allocate-and-start operation.

- Attempt `bench/results/smoke-berth-1`, run `20261006t160859-8a5d42`, interval `2026-10-06T16:08:59Z` to `2026-10-06T16:09:53Z`.
- Source commit `7a545d92c8ca12604fcc96425f64cf7bb6413b73`, dirty `True`. Adapter SHA-256 `36fd41abcd229bbe4c17b34f99bff11e8cfb2e0d3052cb6f9d34d95a1bc6a738`.
- Complete harness file-map SHA-256 `97f3e2167751ae0702ef526f2be3c9ae5cd9ff0638debc2c3b616f34c04c3df0`; config-map SHA-256 `ec5bbd58ea93488ac4d677c59d6b8754d76632eac95378850a852d356dec0757`. Map digest convention is sorted compact JSON. The underlying exact file hashes remain in meta.json and run-ledger.json.
- Transport `host`, isolation boundary `container`, image identity `host transport; workload image digests/IDs in receipts`.
- Recorded completed `True`, valid `True`, reportable `False`, cleanup problems `[]`, artifact errors `[]`. These flags do not override independent findings.
- Raw receipts `60`; referenced stdout/stderr files `120`. Every sequence and outcome evidence reference exists and every file has a retained SHA-256. Original raw hashes still match the after-run snapshot. Copied artifact files `0`.
- Receipt verification `bench/results/remaining-runtime-verification/smoke-berth-1-receipts.json`; resource verification `bench/results/remaining-runtime-verification/smoke-berth-1.json`.
- Retained `bad_config` = `blocked`: setup exit 1: failure output lacks the intended diagnostic /99\.99\.99|postgresql_99/; evidence steps `[52, 53, 54]`.
- Retained `occupied_port` = `not_applicable`: Berth binds 127.0.0.1:0 for a free port and starts Compose in the same `up`; there is no planned port to occupy before start, only a race; evidence steps `[]`.
- Actual version receipt `bench/results/smoke-berth-1/logs/0002-tool-version.stdout` and `bench/results/smoke-berth-1/logs/0002-tool-version.stderr`, exit `0`.
- Actual version receipt `bench/results/smoke-berth-1/logs/0007-a-tool-versions.stdout` and `bench/results/smoke-berth-1/logs/0007-a-tool-versions.stderr`, exit `0`.
- A raw identity step `8`: Python `3.13.16`, PostgreSQL `17.11`, Redis `8.10.2`, source token `3ca6991ad29c09f9`, module `/workspace/rwbapp`. Token matches this checkout's prepare/setup body. Full server/URL/data/mount identity is retained in `bench/results/smoke-berth-1/logs/0008-a-start-identity.stdout` and the receipt verification JSON.
- B raw identity step `24`: Python `3.13.16`, PostgreSQL `17.11`, Redis `8.10.2`, source token `57d5b6c56a8cfa1f`, module `/workspace/rwbapp`. Token matches this checkout's prepare/setup body. Full server/URL/data/mount identity is retained in `bench/results/smoke-berth-1/logs/0024-b-start-identity.stdout` and the receipt verification JSON.

Independent name/label/process/tempdir checks find no remaining run resource. The resource snapshot verifies the original 27 container IDs/images/states/start timestamps and matching network/volume inventories.

## branchbox

Main workload, source mount/code digest, isolation, stop/restart and persistence checks pass. Native feature start creates/configures the worktree; native devcontainer build/up/exec/down runs the application. Source receipts prove the appropriate A/B worktree under /workspaces. bad_config is correctly recorded blocked: step 53 reports Docker credential-helper failure, with no intended invalid-tag refusal. No BranchBox product failure follows from that prerequisite. Native temporary branchbox-devcontainer files are independently absent, recorded in branchbox-native-temp-absence.json. Lock/frozen and structured status are unsupported; occupied_port is not applicable because no host ports are published.

- Attempt `bench/results/smoke-branchbox-1`, run `20261006t161017-b3e6f5`, interval `2026-10-06T16:10:17Z` to `2026-10-06T16:10:55Z`.
- Source commit `7a545d92c8ca12604fcc96425f64cf7bb6413b73`, dirty `True`. Adapter SHA-256 `d8bf759fa54e25a468e1e6dc7889b8f7eaadc23fb900fef3898783386327ed1d`.
- Complete harness file-map SHA-256 `97f3e2167751ae0702ef526f2be3c9ae5cd9ff0638debc2c3b616f34c04c3df0`; config-map SHA-256 `a110c2b5d36bdb82a82b4b9110fece31dfdfaf1b8e06f34ddf13d75bd1bc73d6`. Map digest convention is sorted compact JSON. The underlying exact file hashes remain in meta.json and run-ledger.json.
- Transport `host`, isolation boundary `container`, image identity `host transport; workload image digests/IDs in receipts`.
- Recorded completed `True`, valid `True`, reportable `False`, cleanup problems `[]`, artifact errors `[]`. These flags do not override independent findings.
- Raw receipts `61`; referenced stdout/stderr files `122`. Every sequence and outcome evidence reference exists and every file has a retained SHA-256. Original raw hashes still match the after-run snapshot. Copied artifact files `0`.
- Receipt verification `bench/results/remaining-runtime-verification/smoke-branchbox-1-receipts.json`; resource verification `bench/results/remaining-runtime-verification/smoke-branchbox-1.json`.
- Retained `bad_config` = `blocked`: setup exit 0, start exit 1: failure output lacks the intended diagnostic /99\.99\.99|postgresql_99/; evidence steps `[51, 52, 53, 54]`.
- Retained `occupied_port` = `not_applicable`: the recipe publishes no host ports; services are reachable only inside each workspace's own Compose network; evidence steps `[]`.
- Actual version receipt `bench/results/smoke-branchbox-1/logs/0002-tool-version.stdout` and `bench/results/smoke-branchbox-1/logs/0002-tool-version.stderr`, exit `0`.
- Actual version receipt `bench/results/smoke-branchbox-1/logs/0007-a-tool-versions.stdout` and `bench/results/smoke-branchbox-1/logs/0007-a-tool-versions.stderr`, exit `0`.
- A raw identity step `8`: Python `3.13.16`, PostgreSQL `17.11`, Redis `8.10.2`, source token `22ca55e73a3042f0`, module `/workspaces/rwb-20261006t161017b3e6f5-a/rwbapp`. Token matches this checkout's prepare/setup body. Full server/URL/data/mount identity is retained in `bench/results/smoke-branchbox-1/logs/0008-a-start-identity.stdout` and the receipt verification JSON.
- B raw identity step `24`: Python `3.13.16`, PostgreSQL `17.11`, Redis `8.10.2`, source token `c09e8cec70fa83a5`, module `/workspaces/rwb-20261006t161017b3e6f5-b/rwbapp`. Token matches this checkout's prepare/setup body. Full server/URL/data/mount identity is retained in `bench/results/smoke-branchbox-1/logs/0024-b-start-identity.stdout` and the receipt verification JSON.

Independent name/label/process/tempdir checks find no remaining run resource. The resource snapshot verifies the original 27 container IDs/images/states/start timestamps and matching network/volume inventories.

## tilt

Main workload, source, separate-container isolation, stop/restart and persistence checks pass. Tilt ci and tilt down use pinned standalone Compose 5.6.0. Two independently verified defects exclude this attempt from any final selection. First, bad_config is recorded pass although step 57 only shows the requested tag in progress/command text and then Docker credential-helper failure. No registry invalid-tag refusal occurred; independent classification is environment-blocked with a false-positive benchmark gate. Second, post-run process inspection finds PID 68320, a run-owned Compose events --json reader from checkout D, after the harness reports clean teardown and deletes its workdir. supervisor_processes searches only the private /tilt executable and misses private /docker-compose. Original meta.valid=true, outcomes and cleanup_problems=[] remain unchanged. The exact live event-reader identity was rechecked and SIGTERM sent only to that PID; its absence was verified before Organist started.

- Attempt `bench/results/smoke-tilt-1`, run `20261006t161123-8af6e8`, interval `2026-10-06T16:11:23Z` to `2026-10-06T16:12:06Z`.
- Source commit `8952179f55989ceea23b6c0a7da166837b8af0b5`, dirty `True`. Adapter SHA-256 `9e94572fe4ae285bd9b7bdcfade1673f920c3cd11ea82a47713ad4a036f488b6`.
- Complete harness file-map SHA-256 `97f3e2167751ae0702ef526f2be3c9ae5cd9ff0638debc2c3b616f34c04c3df0`; config-map SHA-256 `20b394d9d0054a242e795e104dfce4e821b368243503f0503bd5d61799e1e73f`. Map digest convention is sorted compact JSON. The underlying exact file hashes remain in meta.json and run-ledger.json.
- Transport `host`, isolation boundary `container`, image identity `host transport; workload image digests/IDs in receipts`.
- Recorded completed `True`, valid `True`, reportable `False`, cleanup problems `[]`, artifact errors `[]`. These flags do not override independent findings.
- Raw receipts `65`; referenced stdout/stderr files `130`. Every sequence and outcome evidence reference exists and every file has a retained SHA-256. Original raw hashes still match the after-run snapshot. Copied artifact files `0`.
- Receipt verification `bench/results/remaining-runtime-verification/smoke-tilt-1-receipts.json`; resource verification `bench/results/remaining-runtime-verification/smoke-tilt-1.json`.
- Retained `bad_config` = `pass`: setup exit 0, start exit 1 (intended diagnostic); evidence steps `[55, 56, 57, 58]`.
- Retained `occupied_port` = `not_applicable`: services publish no host ports; each project reaches postgres:5432 and redis:6379 on its own Compose network; evidence steps `[]`.
- Actual version receipt `bench/results/smoke-tilt-1/logs/0002-tool-version.stdout` and `bench/results/smoke-tilt-1/logs/0002-tool-version.stderr`, exit `0`.
- Actual version receipt `bench/results/smoke-tilt-1/logs/0008-a-tool-versions.stdout` and `bench/results/smoke-tilt-1/logs/0008-a-tool-versions.stderr`, exit `0`.
- A raw identity step `9`: Python `3.13.16`, PostgreSQL `17.6`, Redis `8.10.2`, source token `dea83712bcef660d`, module `/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t161123-8af6e8-n94mftjj/w/a/rwbapp`. Token matches this checkout's prepare/setup body. Full server/URL/data/mount identity is retained in `bench/results/smoke-tilt-1/logs/0009-a-start-identity.stdout` and the receipt verification JSON.
- B raw identity step `26`: Python `3.13.16`, PostgreSQL `17.6`, Redis `8.10.2`, source token `2a3df6575ac7e1d6`, module `/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t161123-8af6e8-n94mftjj/w/b/rwbapp`. Token matches this checkout's prepare/setup body. Full server/URL/data/mount identity is retained in `bench/results/smoke-tilt-1/logs/0026-b-start-identity.stdout` and the receipt verification JSON.

Docker resources and the host tempdir were absent after harness teardown, but the independent live-process check found the owned Compose event reader. The after-tilt-process-cleanup snapshot proves absence after exact manual correction. The original resource snapshot preserves the leak.

## organist

All 27 checks pass. Native Nickel/Nix locks and service definitions are realized; the Honcho holder, port/data layout, readiness and stop verification are declared scripting. Frozen lock copies and A/C version equality pass. The owned listener is verified at step 60; step 61 returns native/scripted start exit 1 with Honcho forwarding PostgreSQL Address already in use and port 25436, without an outer timeout. Three Honcho log artifacts are retained for A/B/E. Default image Nix emits auto-disabling sandboxing because namespaces are unavailable and sandbox-fallback is enabled. This observed fallback is explicit environment evidence; the executor supplied no --no-sandbox, sandbox=false, privileged container or global Nix change. Process-scoped lazy-trees=false is the adapter's existing compatibility setting.

- Attempt `bench/results/smoke-organist-1`, run `20261006t161306-02645b`, interval `2026-10-06T16:13:06Z` to `2026-10-06T16:14:56Z`.
- Source commit `8952179f55989ceea23b6c0a7da166837b8af0b5`, dirty `True`. Adapter SHA-256 `9695335efb28c9ca2285b09e1ff2afe72804290200e1fc784196e55f25be1f80`.
- Complete harness file-map SHA-256 `94e97c377ff561dc0254c65f084f05b7ea29126274b5443a83e1b09cf5b219fe`; config-map SHA-256 `9ecb9d9f1edf41b8b140384eb2e533d30e8a759c2e7e13fab3e30bdf20a5eed2`. Map digest convention is sorted compact JSON. The underlying exact file hashes remain in meta.json and run-ledger.json.
- Transport `docker`, isolation boundary `service-instance`, image identity `sha256:fbf87c18aced2d3c969df40d15b137943429882f4283fe256c2f481a7af17c6a`.
- Recorded completed `True`, valid `True`, reportable `False`, cleanup problems `[]`, artifact errors `[]`. These flags do not override independent findings.
- Raw receipts `76`; referenced stdout/stderr files `152`. Every sequence and outcome evidence reference exists and every file has a retained SHA-256. Original raw hashes still match the after-run snapshot. Copied artifact files `3`.
- Receipt verification `bench/results/remaining-runtime-verification/smoke-organist-1-receipts.json`; resource verification `bench/results/remaining-runtime-verification/smoke-organist-1.json`.
- Retained `bad_config` = `pass`: setup exit 1 (intended diagnostic); evidence steps `[55, 56, 57]`.
- Retained `occupied_port` = `pass`: refused-at-start; evidence steps `[60, 61]`.
- Actual version receipt `bench/results/smoke-organist-1/logs/0004-tool-version.stdout` and `bench/results/smoke-organist-1/logs/0004-tool-version.stderr`, exit `0`.
- Actual version receipt `bench/results/smoke-organist-1/logs/0010-a-tool-versions.stdout` and `bench/results/smoke-organist-1/logs/0010-a-tool-versions.stderr`, exit `0`.
- A raw identity step `11`: Python `3.13.15`, PostgreSQL `17.11`, Redis `8.10.2`, source token `59e195f20f19f777`, module `/home/agent/rwb/a/rwbapp`. Token matches this checkout's prepare/setup body. Full server/URL/data/mount identity is retained in `bench/results/smoke-organist-1/logs/0011-a-start-identity.stdout` and the receipt verification JSON.
- B raw identity step `25`: Python `3.13.15`, PostgreSQL `17.11`, Redis `8.10.2`, source token `f343d8e69bc4b025`, module `/home/agent/rwb/b/rwbapp`. Token matches this checkout's prepare/setup body. Full server/URL/data/mount identity is retained in `bench/results/smoke-organist-1/logs/0025-b-start-identity.stdout` and the receipt verification JSON.

Independent name/label/process/tempdir checks find no remaining run resource. The resource snapshot verifies the original 27 container IDs/images/states/start timestamps and matching network/volume inventories.

## vagrant

Private official-DMG checksum verification, read-only attach, package expansion, detach and relocated CLI version checks succeeded. Vagrant 2.4.9 and embedded Ruby 3.3.8 run from the private tools directory with private VAGRANT_HOME. Receipt bodies and stdout show --provider=docker --no-parallel and force_host_vm=false; no VM or global installer is invoked. Main workload, source, separate-container isolation, stop/restart and persistence checks pass. bad_config is recorded pass although step 54 echoes postgres:99.99.99-alpine and then Docker credential-helper failure. Unable to find the image locally is not a registry rejection of the invalid tag. Independent classification is environment-blocked with the same false-positive gate as Tilt. Original outcomes remain unchanged. hdiutil snapshots before provision, after provision and after completion contain zero mounted images. Machine ID artifacts are retained. Lock/frozen are unsupported; occupied_port is not applicable.

- Attempt `bench/results/smoke-vagrant-1`, run `20261006t161521-0018bf`, interval `2026-10-06T16:15:21Z` to `2026-10-06T16:18:42Z`.
- Source commit `8952179f55989ceea23b6c0a7da166837b8af0b5`, dirty `True`. Adapter SHA-256 `ba90da50e33dcbed2c8f8b575ddab79a88f2f2ff1b60f7fafed1fdb59403e126`.
- Complete harness file-map SHA-256 `94e97c377ff561dc0254c65f084f05b7ea29126274b5443a83e1b09cf5b219fe`; config-map SHA-256 `07648febce04a01209a292acbe0e5cf23e8fcce49459af9c392521f104f0ca61`. Map digest convention is sorted compact JSON. The underlying exact file hashes remain in meta.json and run-ledger.json.
- Transport `host`, isolation boundary `container`, image identity `host transport; workload image digests/IDs in receipts`.
- Recorded completed `True`, valid `True`, reportable `False`, cleanup problems `[]`, artifact errors `[]`. These flags do not override independent findings.
- Raw receipts `65`; referenced stdout/stderr files `130`. Every sequence and outcome evidence reference exists and every file has a retained SHA-256. Original raw hashes still match the after-run snapshot. Copied artifact files `38`.
- Receipt verification `bench/results/remaining-runtime-verification/smoke-vagrant-1-receipts.json`; resource verification `bench/results/remaining-runtime-verification/smoke-vagrant-1.json`.
- Retained `bad_config` = `pass`: setup exit 0, start exit 1 (intended diagnostic); evidence steps `[52, 53, 54, 55]`.
- Retained `occupied_port` = `not_applicable`: services publish no host ports; each checkout reaches pg:5432 and redis:6379 on its own Docker network; evidence steps `[]`.
- Actual version receipt `bench/results/smoke-vagrant-1/logs/0002-tool-version.stdout` and `bench/results/smoke-vagrant-1/logs/0002-tool-version.stderr`, exit `0`.
- Actual version receipt `bench/results/smoke-vagrant-1/logs/0007-a-tool-versions.stdout` and `bench/results/smoke-vagrant-1/logs/0007-a-tool-versions.stderr`, exit `0`.
- A raw identity step `8`: Python `3.13.16`, PostgreSQL `17.6`, Redis `8.10.2`, source token `9dbcf08414bc3192`, module `/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t161521-0018bf-wkbk4i5s/w/a/rwbapp`. Token matches this checkout's prepare/setup body. Full server/URL/data/mount identity is retained in `bench/results/smoke-vagrant-1/logs/0008-a-start-identity.stdout` and the receipt verification JSON.
- B raw identity step `24`: Python `3.13.16`, PostgreSQL `17.6`, Redis `8.10.2`, source token `331e50f2d18e299d`, module `/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t161521-0018bf-wkbk4i5s/w/b/rwbapp`. Token matches this checkout's prepare/setup body. Full server/URL/data/mount identity is retained in `bench/results/smoke-vagrant-1/logs/0024-b-start-identity.stdout` and the receipt verification JSON.

Independent name/label/process/tempdir checks find no remaining run resource. The resource snapshot verifies the original 27 container IDs/images/states/start timestamps and matching network/volume inventories.

## flox

All 27 checks pass on the fresh retry under the current scenario/verify conflict gate, using the unmodified adapter and ev-flox image. Step 60 verifies the owned listener; step 63 app readiness returns exit 3, unreachable after its 120-second application deadline, with outer timed_out=false. Step 64 native status/logs returns exit 0 and reports PostgreSQL Completed, exit_code=1, Address already in use and port 25436. occupied_port passes on that actual native evidence, not merely the failed readiness. All 38 copied artifact files are retained. Actual Python 3.13.15, uv 0.12.17, PostgreSQL 17.11 and Redis 8.10.2 are recorded. The FloxHub login warning does not block these local environments. Holder activation is declared scripting around native Flox service management; source, lock, isolation, stop/restart and persistence conclusions come from this fresh run rather than the old three attempts.

- Attempt `bench/results/smoke-flox-4`, run `20261006t161857-c734dc`, interval `2026-10-06T16:18:57Z` to `2026-10-06T16:21:46Z`.
- Source commit `8952179f55989ceea23b6c0a7da166837b8af0b5`, dirty `True`. Adapter SHA-256 `73b0b4faead558f8af3985262fb2dd3b8d74684034848d15bbf353c6c59c6bac`.
- Complete harness file-map SHA-256 `c2c0b9ac7811e41e2d0a5b153a8d6a840650277b5d1db5a14cf2945ae2a942ab`; config-map SHA-256 `1ee96b4f903c15ba18901be166811a64324fb248922575da718c7bf8fb4db89f`. Map digest convention is sorted compact JSON. The underlying exact file hashes remain in meta.json and run-ledger.json.
- Transport `docker`, isolation boundary `service-instance`, image identity `sha256:61613637e02c263efc9a16e13f76c823a8d9210a3e920449bbe85207d95c5e66`.
- Recorded completed `True`, valid `True`, reportable `False`, cleanup problems `[]`, artifact errors `[]`. These flags do not override independent findings.
- Raw receipts `89`; referenced stdout/stderr files `178`. Every sequence and outcome evidence reference exists and every file has a retained SHA-256. Original raw hashes still match the after-run snapshot. Copied artifact files `38`.
- Receipt verification `bench/results/remaining-runtime-verification/smoke-flox-4-receipts.json`; resource verification `bench/results/remaining-runtime-verification/smoke-flox-4.json`.
- Retained `bad_config` = `pass`: setup exit 1 (intended diagnostic); evidence steps `[55, 56, 57]`.
- Retained `occupied_port` = `pass`: detected-at-readiness; evidence steps `[60, 61, 62, 63, 64]`.
- Actual version receipt `bench/results/smoke-flox-4/logs/0004-tool-version.stdout` and `bench/results/smoke-flox-4/logs/0004-tool-version.stderr`, exit `0`.
- Actual version receipt `bench/results/smoke-flox-4/logs/0010-a-tool-versions.stdout` and `bench/results/smoke-flox-4/logs/0010-a-tool-versions.stderr`, exit `0`.
- A raw identity step `11`: Python `3.13.15`, PostgreSQL `17.11`, Redis `8.10.2`, source token `b1bcc7d76f01b9e8`, module `/home/agent/rwb/a/rwbapp`. Token matches this checkout's prepare/setup body. Full server/URL/data/mount identity is retained in `bench/results/smoke-flox-4/logs/0011-a-start-identity.stdout` and the receipt verification JSON.
- B raw identity step `25`: Python `3.13.15`, PostgreSQL `17.11`, Redis `8.10.2`, source token `eb29a6c634e9af6f`, module `/home/agent/rwb/b/rwbapp`. Token matches this checkout's prepare/setup body. Full server/URL/data/mount identity is retained in `bench/results/smoke-flox-4/logs/0025-b-start-identity.stdout` and the receipt verification JSON.

Independent name/label/process/tempdir checks find no remaining run resource. The resource snapshot verifies the original 27 container IDs/images/states/start timestamps and matching network/volume inventories.

## Versions, pins and deviations

Actual pinned/realized tool identity: workz 0.11.0 binary 72994c049c43989e4ec868dd3741389548f70aa15342acd34cd88349c5feef97; Worktrunk 0.80.0 binary 0708ca37fc39f9fa48edc1af500a2ff3664ec0155f63909425b02994f29f0fd1; isola 0.4.1 commit af852ae57c6d107e09daaacf8d744cc09e18fbd0 binary fa0588f916f915f0bb62bcb0eba05ab965adba35219400b519d7c9c0bff60423; Berth source 3b93287584dcc5c7c26298a62379d8ce001400ff reports 0.1.0, built binary c47d52f239b053ab29927a68dd11ddb63a81769eeb4e65ad346fa81048c68b8c; BranchBox 0.13.4 binary a8597b6a072e2490452ec5284a37f4255153886fd11ddffd833a34aa991b4968. Each archive hash and pin is recorded in meta/step bodies and guarded by successful provisioning receipts.

Tilt v0.37.8 binary 190255a6e64023b4cfe7a2bbecb34b41113d8c5db7d74f74555b8827e38cfb79; its Compose 5.6.0 binary bd714a42b46e51757fc1121085b3096b9064e3f80d3ca5a991e33f44df4f43c9. Organist source a7e4e638cade5e7c4f36a129b80d91bf3538088e and workload nixpkgs 151fa4e8ddfdd8dd25d945ad94ed54a13de9f6e4; actual Nix in Organist/Flox is Determinate 3.23.0 / Nix 2.35.2. Vagrant 2.4.9 official DMG 8de08bd435ef8ae0fc5fbd6acefa9c68e62fb898c5ae0fbdacd26853bea9d4d6, launcher 102bbe8336c246c3a647c2374d5ed9ecad42c8cd7fb886ee7f8aec4d14290d41; embedded Ruby 3.3.8 arm64-darwin. Flox actual version is 1.17.0-g486737b. GitGrove has only an intended pin, not a realized identity.

Python/uv are 3.13.16 / 0.12.23 for workz, Worktrunk, isola, Berth, BranchBox, Tilt and Vagrant. Organist uses 3.13.15 / 0.12.22. Flox uses 3.13.15 / 0.12.17, PostgreSQL 17.11 and Redis 8.10.2; its actual package set is retained in the A version receipt. Python patch and uv deviations remain explicit. PostgreSQL is 17.6 for workz, Worktrunk, Tilt and Vagrant, a deviation from the 17.11 canonical recipe; it is 17.11 for isola, Berth, BranchBox and Organist. Redis is 8.10.2 in those lanes. Exact package sets, source/fixture/glue/config hashes and image digests remain in each result. Cache is the existing Docker/Nix image/store plus per-run cache declared by the adapter; no universal cold-cache claim is made.

## Exact cleanup additions

GitGrove initialization failed before protected teardown, leaving only an empty tempdir. Ownership recheck and exact rmdir are recorded in `git-grove-startup-receipt.json`; the result directory is retained. One earlier guard attempt stopped without mutation because its process-token scan also matched its own inspection shell. The successful recheck excludes only the inspection ancestry and finds no live run-token process.

Tilt initially left PID 68320 with PPID 1 and PGID 68311, started 2026-10-06 12:12:03 local. Its full argv used the exact private Compose executable and checkout D project/source path from run 20261006t161123-8af6e8, ending events --json. The full live ps identity was captured in `tilt-live-event-process.json`. Immediately before signalling, a fresh ps result exactly matched PID, start timestamp, command and project. Only SIGTERM to PID 68320 was sent; no process-group or archived-PID signal occurred. ps returned absent afterwards. `tilt-event-process-cleanup.json` and `after-tilt-process-cleanup.json` preserve that correction. The original run still records its false clean-teardown claim and is not repaired into a final attempt.

Vagrant mounts are absent in all three hdiutil plist snapshots. BranchBox-generated native temporary files are absent in its separate receipt. Cleanup never targeted preexisting containers, arbitrary names, shared layers, Docker/Nix caches or globally installed tools.

## Bounded Opus handoff

1. GitGrove: rename the per-checkout image-name helper in git_grove.py and update its callers so Adapter.image remains JSON-serializable metadata. Add a constructor/metadata regression that exercises the real initial metadata shape, then a fresh GitGrove diagnostic after review. Also ensure initialization failures retain a failure receipt and exact tempdir cleanup; run.py currently writes initial metadata before its try/finally. Do not append to or delete smoke-git-grove-1.

2. Tilt and Vagrant invalid-image evidence: the generic bad_config_pattern at base.py:120 and scenario.py:484-502 accepts the requested tag anywhere in output. Tilt step 57 and Vagrant step 54 prove a credential error can satisfy it through progress/argv text. Require terminal evidence of the intended registry invalid-tag/package refusal, with the requested tag associated with that refusal, and reject credential/network prerequisites as environment blocks. Preserve the original pass outcomes and use fresh directories after reviewed fixes. The credential issue itself should follow the already-reviewed private Docker-config approach, retaining daemon context without changing ~/.docker or installing globally. Berth/BranchBox have the same environment limitation but their current checks correctly remain blocked.

3. Tilt cleanup: cover the owned Compose event-reader as well as the private Tilt process. Track/recheck the actual live process ownership and terminate/wait only for that run's process when necessary. Include it in cleanup verification so workdir removal cannot hide it. Add a regression for failed ci leaving a Compose events descendant and a fresh runtime run after review. Keep no shared-daemon/global cleanup shortcut.

4. Final measurement prerequisites: freeze reviewed bytes and the actual toolchain/platform/cache/sandbox deviations in a parent-created session manifest before new final runs. These smokes, the initializer reproduction and manual cleanup diagnostics remain excluded from final timings. Organist's default sandbox fallback is explicit and needs a suitable enforcing environment if the final protocol requires build sandboxing. No product fixes or code changes were made by this executor.

## Final verification

`bench/results/remaining-runtime-verification/final-state.json` records final source/process/resource state. `run-ledger.json` records raw result hashes and exact receipts. All original 27 containers retain IDs, images, states and start timestamps; network and volume inventories match baseline. All requested run tokens have no remaining Docker container/network/volume/image, host tempdir or live host process after the two exact cleanup additions. No benchmark executor remains at handback. No raw result directory was overwritten, relabelled or deleted. This review records diagnostic execution and next fixes; it grants no final measurement or product pass beyond the cited checks.
