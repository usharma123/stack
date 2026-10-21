# Vagrant research

Research date: 2026-10-06. This is source and documentation research, not a timed run. No services, containers, VMs, installers, or host plugins were launched or installed.

## Recommendation and scope

Implement `vagrant-docker` as a host-transport adapter after Vagrant is provisioned. The built-in Docker provider can manage an application container plus PostgreSQL and Redis containers through the already reachable Docker Desktop daemon. It does not require a Vagrant box or a Vagrant-managed VM on this Mac. Keep `vagrant-vmware` as a separate, currently unprovisioned VM comparison. Its guest kernel, SSH, snapshots, box import and provisioning serve workloads outside ordinary local service management.

The old `eval/harness/render-comparison.py` calls Vagrant adjacent and says no VM benchmark was run. That is historical context, not evidence about this recipe. The current fixture and adapter contract support the Docker variant with `transport="host"`, `isolation_boundary="container"`, three machines per checkout and scripted application readiness.

## Versions and local availability

| Item | Read-only evidence |
|---|---|
| Official current stable Vagrant | [2.4.9 downloads](https://releases.hashicorp.com/vagrant/2.4.9/), including `darwin_arm64.dmg`, `darwin_amd64.dmg`, `linux_amd64.zip`; no Linux ARM64 release asset in that listing |
| Stable source inspected | [`97d5ea2501e81d05eacac7fa7be6fc6665c10b74`](https://github.com/hashicorp/vagrant/tree/97d5ea2501e81d05eacac7fa7be6fc6665c10b74), tag `v2.4.9` |
| Current main inspected | [`dc55920a284544919340b975098b4ac9005b08b0`](https://github.com/hashicorp/vagrant/tree/dc55920a284544919340b975098b4ac9005b08b0), reports `2.4.10.dev`; do not report this as the released CLI |
| Host | `uname -m`: `arm64`; macOS 15.7.2 |
| Vagrant | `command -v vagrant` returned no path; runtime/provider compatibility remains unexecuted |
| Docker | `/opt/homebrew/bin/docker`; `docker version --format ...` returned client 28.1.1, daemon 29.1.3; current context `desktop-linux` |
| VMware integration source | [`036db7a6eac280515af94f03d0e45b7717d86272`](https://github.com/hashicorp/vagrant-vmware-desktop/tree/036db7a6eac280515af94f03d0e45b7717d86272); includes Ruby plugin and Go utility; utility source version 1.0.24 |

Official installation uses the [HashiCorp package or installer](https://developer.hashicorp.com/vagrant/install); a RubyGem install is [unsupported](https://developer.hashicorp.com/vagrant/docs/installation). Do not run a host-wide install from the benchmark automatically. Provisioning must first make an explicit pinned Vagrant executable available and record `vagrant --version`, executable hash and installer checksum. A Linux ARM64 benchmark container cannot simply download a nonexistent official ARM64 zip. Emulating an AMD64 CLI or building source changes the comparison and must be labeled.

## Source and licensing boundaries

The current CLI and built-in Docker provider source are public under [BUSL-1.1](https://github.com/hashicorp/vagrant/blob/97d5ea2501e81d05eacac7fa7be6fc6665c10b74/LICENSE), not an OSI open-source license. The inspected license names Vagrant 2.4.3 or later, permits internal use, restricts specified paid competing hosted/embedded offerings, and changes each version to MPL 2.0 after its stated four-year period. A local evaluation does not establish permission to embed Vagrant in a distributed competitor.

The [VMware plugin and utility repository](https://github.com/hashicorp/vagrant-vmware-desktop/tree/036db7a6eac280515af94f03d0e45b7717d86272) is public and [MPL 2.0](https://github.com/hashicorp/vagrant-vmware-desktop/blob/036db7a6eac280515af94f03d0e45b7717d86272/LICENSE). Do not repeat old claims that this provider is closed or needs a paid Vagrant plugin license. VMware Fusion itself is a separately installed proprietary hypervisor. Broadcom says [Fusion and Workstation are free for commercial, educational and personal use](https://blogs.vmware.com/cloud-foundation/2024/11/11/vmware-fusion-and-workstation-are-now-free-for-all-users/). HashiCorp's provider overview still contains older purchase wording, so it is not authoritative current pricing evidence.

Docker Desktop is another separately installed dependency with its own [license terms](https://docs.docker.com/subscription-billing/desktop-license/). Docker provider behavior is visible in the Vagrant repository; Docker Desktop implementation and VMware Fusion implementation are outside that repository. This research does not establish the source/license boundary of every optional Vagrant plugin or the hosted HCP registry. Neither is necessary for the Docker recipe.

## Meaningful implementation evidence

All following links pin the released source. These are code inferences, not executed provider results.

- [`config.rb`](https://github.com/hashicorp/vagrant/blob/97d5ea2501e81d05eacac7fa7be6fc6665c10b74/plugins/providers/docker/config.rb#L251-L286) defaults `has_ssh=false`, `compose=false`, `pull=false`, `remains_running=true`, and `force_host_vm=false` on Darwin, Windows and Linux. [`provider.rb`](https://github.com/hashicorp/vagrant/blob/97d5ea2501e81d05eacac7fa7be6fc6665c10b74/plugins/providers/docker/provider.rb) checks `docker version` and uses the local driver unless host-VM mode is selected. Explicitly set `force_host_vm=false`; avoid the old `hashicorp/boot2docker` host box.
- [`create.rb`](https://github.com/hashicorp/vagrant/blob/97d5ea2501e81d05eacac7fa7be6fc6665c10b74/plugins/providers/docker/action/create.rb) passes custom names, volumes, environment, ports and `create_args` to the driver. Its default name uses checkout basename, machine name and a seconds timestamp. Supply names with a run ID and checkout ID to avoid same-basename collisions and identify owned resources.
- [`driver.rb`](https://github.com/hashicorp/vagrant/blob/97d5ea2501e81d05eacac7fa7be6fc6665c10b74/plugins/providers/docker/driver.rb) implements this with `docker run`, `docker start`, `docker stop`, and `docker rm -f -v`. User-specified named volumes survive container removal; anonymous volumes should not be treated as durable benchmark storage. The explicit named-volume removal in this recipe is separate scripted cleanup. Docker documents [volume lifecycle](https://docs.docker.com/engine/storage/volumes/).
- [`wait_for_running.rb`](https://github.com/hashicorp/vagrant/blob/97d5ea2501e81d05eacac7fa7be6fc6665c10b74/plugins/providers/docker/action/wait_for_running.rb) checks container running state. It does not wait for SQL connections, Redis commands, or Docker health status. Use the fixture's `wait` command and label readiness scripted.
- [`action.rb`](https://github.com/hashicorp/vagrant/blob/97d5ea2501e81d05eacac7fa7be6fc6665c10b74/plugins/providers/docker/action.rb) implements up, halt, reload and destroy. Suspend and package explicitly raise unsupported errors; there are no Docker snapshot actions. Reload can recreate a Dockerfile-built application container. Dependencies in its writable layer do not survive that recreation unless installed in the image or an owned volume.
- [`command/exec.rb`](https://github.com/hashicorp/vagrant/blob/97d5ea2501e81d05eacac7fa7be6fc6665c10b74/plugins/providers/docker/command/exec.rb#L70-L105) defaults to noninteractive execution. A stopped target prints a message, skips execution and returns zero. A running target's failing command raises through [`executor/local.rb`](https://github.com/hashicorp/vagrant/blob/97d5ea2501e81d05eacac7fa7be6fc6665c10b74/plugins/providers/docker/executor/local.rb). Gate success on parsed fixture JSON and running-container identity, not the Vagrant exit code alone.
- [`synced_folder.rb`](https://github.com/hashicorp/vagrant/blob/97d5ea2501e81d05eacac7fa7be6fc6665c10b74/plugins/providers/docker/synced_folder.rb) converts checkout folders to Docker bind mounts. Bind the correct checkout to `/app`; exclude `.vagrant` from reproducible source copies.

The official [Docker basics](https://developer.hashicorp.com/vagrant/docs/providers/docker/basics) document images, Dockerfile builds, synced folders and no required box. [Docker configuration](https://developer.hashicorp.com/vagrant/docs/providers/docker/configuration) documents the settings used below. A user-defined network and its cleanup here are ordinary Docker glue passed through `create_args`; Vagrant supplies machine lifecycle and command execution.

## Concrete Docker recipe for the current fixture

This recipe has not run. The implementer must serialize a smoke run once the CLI is available. Copy `bench/fixtures/app` into each run-owned checkout. Commit `Vagrantfile`, `Dockerfile`, the fixture's `pyproject.toml` and `uv.lock`, and an `images.lock.json`. The lock must contain real resolved `name@sha256:...` image references, with ARM64 manifests verified before starting. Do not substitute invented digests. The parent should resolve the same Python 3.13 patch, uv version, PostgreSQL 17.6 and Redis release used in the other container adapters. Record image IDs and architecture after creation.

`images.lock.json` has four keys: `python`, `uv`, `postgres`, `redis`. Python must be a Python 3.13 image with `/usr/local/bin/python3.13`; uv must be the official binary image exposing `/uv` and `/uvx`; PostgreSQL must use the 17-series data layout. The fixture requires Python `>=3.13,<3.14`, pins Python packages and pytest, and disables uv Python downloads.

`Dockerfile`:

```dockerfile
ARG PYTHON_IMAGE
ARG UV_IMAGE
FROM ${UV_IMAGE} AS uvbin
FROM ${PYTHON_IMAGE}
COPY --from=uvbin /uv /uvx /usr/local/bin/
ENV UV_PYTHON_DOWNLOADS=never UV_PROJECT_ENVIRONMENT=/opt/venv
WORKDIR /app
CMD ["sleep", "infinity"]
```

`Vagrantfile`:

```ruby
require "json"
pins = JSON.parse(File.read(File.join(__dir__, "images.lock.json")))
id = ENV.fetch("RWB_INSTANCE")
raise "bad instance" unless id.match?(/\Arwb-[a-z0-9-]+\z/)
pins.each_value { |v| raise "unpinned image" unless v.match?(/@sha256:[0-9a-f]{64}\z/) }
net = "#{id}-net"

Vagrant.configure("2") do |config|
  config.vm.synced_folder ".", "/vagrant", disabled: true

  config.vm.define "pg" do |m|
    m.vm.provider "docker" do |d|
      d.image = pins.fetch("postgres")
      d.name = "#{id}-pg"
      d.force_host_vm = false
      d.has_ssh = false
      d.stop_timeout = 30
      d.env = { "POSTGRES_USER" => "bench", "POSTGRES_PASSWORD" => "bench",
                "POSTGRES_DB" => "bench", "PGDATA" => "/var/lib/postgresql/data" }
      d.create_args = ["--network", net, "--network-alias", "pg",
                      "--mount", "type=volume,source=#{id}-pgdata,target=/var/lib/postgresql/data"]
    end
  end

  config.vm.define "redis" do |m|
    m.vm.provider "docker" do |d|
      d.image = pins.fetch("redis")
      d.name = "#{id}-redis"
      d.force_host_vm = false
      d.has_ssh = false
      d.stop_timeout = 30
      d.cmd = ["redis-server", "--dir", "/data", "--appendonly", "yes",
               "--appendfsync", "everysec"]
      d.create_args = ["--network", net, "--network-alias", "redis",
                      "--mount", "type=volume,source=#{id}-redisdata,target=/data"]
    end
  end

  config.vm.define "app", primary: true do |m|
    m.vm.synced_folder ".", "/app"
    m.vm.provider "docker" do |d|
      d.build_dir = "."
      d.build_args = ["--build-arg", "PYTHON_IMAGE=#{pins.fetch('python')}",
                      "--build-arg", "UV_IMAGE=#{pins.fetch('uv')}"]
      d.name = "#{id}-app"
      d.force_host_vm = false
      d.has_ssh = false
      d.create_args = ["--network", net]
      d.env = { "DATABASE_URL" => "postgresql://bench:bench@pg:5432/bench",
                "REDIS_URL" => "redis://redis:6379/0" }
    end
  end
end
```

The adapter writes an ignored `.bench.env` containing an exported `RWB_INSTANCE=rwb-<runid>-<checkout>`, with shell-quoted values. Every fresh noninteractive shell sources that file. Never let two checkout roots share `.vagrant` or local settings. Use `VAGRANT_CHECKPOINT_DISABLE=1` for both competitors' provenance/timing reproducibility where applicable, and capture the selected Docker context without changing it.

Run in checkout A, with its distinct source token already written by the harness:

```bash
set -euo pipefail
. ./.bench.env
export VAGRANT_CHECKPOINT_DISABLE=1
vagrant --version
docker network create --label "rwb.instance=$RWB_INSTANCE" "$RWB_INSTANCE-net"
docker volume create --label "rwb.instance=$RWB_INSTANCE" "$RWB_INSTANCE-pgdata"
docker volume create --label "rwb.instance=$RWB_INSTANCE" "$RWB_INSTANCE-redisdata"
vagrant up pg redis app --provider=docker --no-parallel
vagrant docker-exec --no-prefix app -- uv sync --frozen --python /usr/local/bin/python3.13
vagrant docker-exec --no-prefix app -- /opt/venv/bin/python -m rwbapp wait --timeout 60
vagrant docker-exec --no-prefix app -- /opt/venv/bin/python -m rwbapp migrate
vagrant docker-exec --no-prefix app -- /opt/venv/bin/python -m rwbapp mark --checkout a
vagrant docker-exec --no-prefix app -- /opt/venv/bin/python -m rwbapp crud --checkout a
vagrant docker-exec --no-prefix app -- /opt/venv/bin/python -m rwbapp cache --checkout a
vagrant docker-exec --no-prefix app -- env RWB_CHECKOUT=a /opt/venv/bin/python -m pytest -q -p no:cacheprovider
vagrant docker-exec --no-prefix app -- /opt/venv/bin/python -m rwbapp identity
```

The setup and dependency steps are separate. The image supplies the pinned interpreter and uv; `uv sync --frozen` installs the fixture lock. uv provides Python dependency reproducibility. `/opt/venv` is unique to each application container. Set `RWB_CHECKOUT` for pytest so its marker agrees with the checkout's earlier writes. `docker-exec` sends an argv vector, so shell operators require explicit `sh -c` with proper quoting; do not use an interactive TTY for timing or parse prefixed output as JSON.

Run B in a distinct root using the same committed image/package locks, a distinct `RWB_INSTANCE` and source token, and `--checkout b`. Networks give both projects the same internal DNS names and ports while pointing at different service containers and named volumes. This is container isolation. It does not require separate host ports. Copy C's committed configuration/lock and verify their hashes before and after `uv sync --frozen`; do not copy a preexisting virtualenv or `.vagrant` directory.

For stop/start persistence, first run `rwbapp persist --checkout a`, then `vagrant halt pg redis`. Confirm A's two service containers are stopped and B's identity/CRUD still work. Run `vagrant up pg redis --provider=docker --no-parallel`, `rwbapp wait`, `rwbapp persisted --checkout a` and `rwbapp check --checkout a --forbid b` through A's app container. Assert both `pg_keeper` and `redis_durable` in the receipt, even if the fixture exit status alone only enforces the PostgreSQL flag. Also test destroy/recreate while retaining named volumes as a distinct durability case. Do not claim crash consistency from orderly stop with Redis SAVE.

Final cleanup sources the same `.bench.env`, verifies the recorded container/network/volume names and ownership, runs `vagrant destroy -f app redis pg`, then explicitly removes only `"$RWB_INSTANCE-net"`, `"$RWB_INSTANCE-pgdata"` and `"$RWB_INSTANCE-redisdata"`. On partial setup failure, consult the recorded Docker IDs as well as `.vagrant` before scoped cleanup. Vagrant may remove its build image when destroying the app; never broadly prune images or Docker resources.

## VM variant and blockers

Vagrant supports VM provisioning, synced folders and noninteractive `vagrant ssh app -c '<command>'`. For an Apple ARM VM flavor, use an ARM64 Linux box built for `vmware_desktop`, exact box version and checksum, a matching Fusion installation, the [VMware utility ARM64 installer](https://developer.hashicorp.com/vagrant/install/vmware), and the [provider plugin](https://developer.hashicorp.com/vagrant/docs/providers/vmware/installation). Boxes are [provider-specific](https://developer.hashicorp.com/vagrant/docs/providers/basic_usage). Broadcom says [Fusion on Apple Silicon runs ARM64 guests](https://knowledge.broadcom.com/external/article/315602/compatibility-considerations-for-arm-gue.html). An x86 box from a getting-started example is not a supported ARM recipe.

The source-inspected VMware provider implements [snapshot save/restore/delete](https://github.com/hashicorp/vagrant-vmware-desktop/blob/036db7a6eac280515af94f03d0e45b7717d86272/lib/vagrant-vmware-desktop/action.rb#L223-L284). Commands are `vagrant snapshot save app baseline`, `vagrant snapshot restore app baseline`, and `vagrant snapshot delete app baseline`. A supported provider's snapshot is a separate VM capability; do not record Docker snapshot support. VM snapshots also do not roll back host bind-mounted checkout files. Keep database data on guest disks for the snapshot test and verify database/cache contents after restore.

No usable box/Fusion/utility/plugin combination was provisioned or verified in this research. Therefore the VM flavor is blocked on platform provisioning and box validation, not a failed application task. A generic VM recipe installing unpinned Python/PostgreSQL/Redis from the guest OS would invalidate this benchmark's version controls. Build or supply a pinned ARM64 guest image with those components and the fixture lock, or resolve exact provisioner packages before claiming a runnable VM variant. Vagrant plugin/utility code is reviewable, but guest/provider runtime compatibility requires a real serial smoke run.

## Benchmark classification and receipts

| Behavior | Classification for this recipe |
|---|---|
| Multi-machine up/halt/status/destroy; application exec | Native Vagrant Docker provider |
| Checkout container lifecycle and bind mounts | Native; naming policy comes from configuration |
| Python and dependency locking | Image pins plus uv; declared scripted composition |
| Service readiness | Scripted fixture connection checks |
| Network and durable-volume setup/removal | Scripted Docker resource operations |
| Independent service data | Container boundary with configured owned volumes |
| Frozen image/lock copy | Scripted copy and hash verification |
| VM SSH/snapshots | Native in compatible VM provider; unavailable in this Docker image recipe |
| Wrong-instance refusal or Stack lease ownership | Unsupported; ordinary Docker IDs are receipts, not equivalent guards |

Capture Vagrant version, source/config/package-lock hashes, Docker daemon/context version, container image IDs/architecture, exact container IDs, mount source/destination and named-volume IDs. Match `.vagrant/machines/{app,pg,redis}/docker/id` to inspected Docker IDs. Assert `State.Running` and the expected image/mount/network before executing fixture commands. Parse the fixture JSON, require `ok=true`, verify `/app/rwbapp` and the checkout source token. A/B PostgreSQL `system_identifier` and Redis `run_id` must differ; compare unprefixed markers and assert no cross-checkout data. Internal URLs should truthfully show port 5432 and 6379 rather than claiming host-port isolation.

Add host-port collision as an optional configuration variant using `config.vm.network :forwarded_port, guest: 5432, host: <checkout-port>, host_ip: '127.0.0.1', auto_correct: false`. Validate the resulting mapping from Docker inspection before declaring success. The base recipe's application uses an internal bridge and needs no published service port, so a forced host-port collision is not applicable to it. Vagrant's collision handling can remap opted-in ports; do not hard-code expected URLs while permitting auto-correction.

Report cold CLI/provider installation, image download/build, cold dependency install, warm dependency sync, repeated app commands and service restart separately. Docker Desktop startup/cache costs belong in the Docker flavor's setup envelope just as for Compose or Dev Containers. Compare the same fixture work and data isolation, and report VM boot/provisioning/snapshot work separately. No new performance, successful integration, or VM capability execution claim follows from this artifact.
