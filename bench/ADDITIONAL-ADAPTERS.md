# Additional adapters: Tilt, Organist, Vagrant

Owner: additional-adapter implementation agent (Opus 5.5), 2026-10-06. Contract: stable v1
(`ADAPTER-CONTRACT.md`, current `base.py`/`scenario.py`). Research inputs: `research/tilt.md`,
`research/organist.md`, `research/vagrant.md`.

No lifecycle, service start or timing was run. All three tools are **runnable** at the
preflight level checked below, so none is marked blocked. The parent's serialized run is the
first execution of the full workload.

## Files (all owned here)

| Path | Purpose |
|---|---|
| `rwb/adapters/tilt.py` | `TiltAdapter`, plus the shared host helpers `darwin_arm64_preflight`, `remove_owned` |
| `rwb/adapters/organist.py` | `OrganistAdapter` |
| `rwb/adapters/vagrant.py` | `VagrantAdapter` (imports the two helpers from `tilt.py`) |
| `adapters/tilt/` | `Tiltfile`, `compose.yaml`, `Dockerfile`, `.dockerignore` |
| `adapters/organist/` | `flake.nix`, `project.ncl`, `organist-services.sh` (scripted), `organist-holder.sh` (scripted) |
| `adapters/vagrant/` | `Vagrantfile`, `Dockerfile`, `.dockerignore`, `images.lock.json`, `vagrant-resources.sh` (scripted) |
| `tests/test_additional_adapters.py` | 17 offline tests (see Checkpoints) |

Organist also uses the shared `adapters/_shared/rwb-env.sh` (`shared_files`), unchanged.

## Integration

- Registry entries already exist: `tilt` → `rwb.adapters.tilt:TiltAdapter`,
  `organist` → `rwb.adapters.organist:OrganistAdapter`, `vagrant` → `rwb.adapters.vagrant:VagrantAdapter`.
  Each has one variant, `default`.
- Run: `python3 bench/run.py --tool tilt|organist|vagrant [--out …]`.
- Option `--option tools_dir=DIR` (Tilt, Vagrant only) reuses already-downloaded private binaries
  instead of downloading into the run work dir. Hashes are still verified. Expected layouts:
  `DIR/tilt` + `DIR/docker-compose`; and `DIR/vagrant/bin/vagrant` (expanded payload). This
  session left verified copies at `/tmp/rwb-dl-tilt` and `/tmp/rwb-dl-vagrant/tools`.
- Nothing is installed globally. Default provisioning downloads into `<run workdir>/tools`
  (about 30 MB of Tilt, 30 MB of Compose and a 56 MB Vagrant DMG), which is removed with the work dir.

## Per tool

### Tilt 0.37.8, local Docker Compose backend (no Kubernetes)

- Transport `host`; boundary `container`. One Compose project per checkout,
  `rwb-<run>-<co>`, with a private network and named volumes. No host ports are published, so
  `occupied_port` is `not_applicable`.
- Pins: Tilt `mac.arm64` archive sha256 `2d396b13…ff4484`, binary `190255a6…cfb79`. Private
  Compose v5.6.0 `darwin-aarch64` sha256 `bd714a42…43c9`, selected via `TILT_DOCKER_COMPOSE_CMD`
  rather than the Desktop plugin (v2.40.3). Images: python 3.13.16-slim, uv 0.12.23,
  postgres 17.6-alpine, redis 8.10.2-alpine, all digest-pinned and checked against the
  registries (anonymous manifest reads; python index has arm64). The pins also go into `meta.json`.
- State is per run: `TILT_DEV_DIR=<workdir>/tilt-dev`, `TILT_DISABLE_ANALYTICS=1`, and
  `docker_prune_settings(disable=True)`.
- Workflow:

  | contract | command |
  |---|---|
  | setup | `tilt alpha tiltfile-result` |
  | start | `tilt ci --port 0 --timeout 900s` (`start_waits_ready`) |
  | ready | health receipt (`docker inspect` health = healthy) |
  | enter | `compose -p P exec -T app bash -c …` |
  | status | `compose ps --all --format json` |
  | stop | `tilt down` (volumes kept) |
  | cleanup | `tilt down --delete-volumes` |

- `wait=True` makes Compose `--wait` gate on the PG/Redis healthchecks. Research shows Tilt's CI
  readiness alone treats a running Compose container as ready.
- Modes: services, detached, readiness, ports, data and stop are native. Status is scripted,
  because `tilt get` needs a live `tilt up` server. Lockfile, frozen setup and guard are unsupported.
- Deviation from research: the app container is a toolchain image (Python + uv only). The checkout
  is **bind-mounted at its own absolute path**, not `COPY`ed to `/app`. As a result:
  - the common `uv sync --frozen` deps step runs as a real, measured step;
  - the source-path gate checks the real checkout path;
  - the base `app/deps/pytest` bodies are used unchanged.

  The uv cache is a run-owned host dir shared by A..E, so B is warm. `.dockerignore` limits the
  build context (and Tilt's file watch) to the Dockerfile.

### Organist (current main `a7e4e638`, 2025-08-12; only tag v0.1 is obsolete)

- Transport `docker` on `ev-nix` as `agent`; boundary `service-instance`. Pins: nixpkgs
  `151fa4e8` (Python 3.13, uv, PostgreSQL 17.11, Redis 8.10.2); Organist's own lock supplies
  Nickel 1.7.0.
- Native: lockfile (`flake.lock`, plus the generated `nickel.lock.ncl`), frozen setup
  (`--no-update-lock-file`), and services (`config.services` → generated Procfile →
  `nix run .#start-services` under Honcho 2.0.0).
- Scripted, because Honcho is foreground-only with no status, readiness or port/data allocation:
  - `organist-holder.sh` keeps Honcho detached. It records its PID, and stop signals only that
    PID after confirming it is Honcho with this checkout as its cwd. It then waits and checks
    that the ports closed.
  - `organist-services.sh` handles guarded `initdb`, ports and the Redis AOF `always` policy.
  - Readiness is the app's `wait`.
- Checkouts are git repos (`prepare` runs `git init/add/commit`) and runtime state is
  `.gitignore`d, because Nix evaluates only tracked files and Organist copies the source tree.
  The generated locks are `git add`ed in setup.
- Compatibility setting `NIX_CONFIG='lazy-trees = false'` (process-scoped, recorded in pins).
  Evidence: in a disposable `ev-nix` container, `nix run .#regenerate-lockfile` failed with
  `error: path '/nix/store/…-source' is not valid` under the image's Determinate Nix default
  (lazy trees). It succeeded with lazy trees off.
- The holder reports start success only when PostgreSQL's own `postmaster.pid` status reads
  `ready` and Honcho is alive, so a squatter on the port cannot look like a successful start.
  Honcho exits when any child exits, which the holder reports as a start failure with the log.
- Artifacts: `.rwb-state/logs` (Honcho log).

### Vagrant 2.4.9, built-in Docker provider (no box, no VM, `force_host_vm=false`)

- Transport `host`; boundary `container`. Machines `pg`, `redis` and `app` per checkout, named
  `rwb-<run>-<co>-*` and labelled `rwb.run`.
- **Not blocked.** There is no Linux arm64 release, and the official macOS installer writes to
  `/opt` and `/usr/local`. Provisioning instead:
  1. verifies the official `darwin_arm64.dmg` (sha256 `8de08bd4…d9d6`, universal);
  2. `hdiutil attach -readonly -nobrowse` at a private mount point;
  3. `pkgutil --expand-full` into the run's tools dir, then detaches;
  4. verifies the launcher (sha256 `102bbe83…0d41`) and `vagrant --version` = `Vagrant 2.4.9`.

  The relocated install (embedded Ruby 3.3.8) passed `vagrant validate` and `vagrant status
  --machine-readable` for a Docker-provider Vagrantfile. `VAGRANT_HOME` is per run.
- Workflow:

  | contract | command |
  |---|---|
  | setup | `vagrant validate` |
  | start | `vagrant-resources.sh create` + `vagrant up pg redis app --provider=docker --no-parallel` |
  | enter | `vagrant docker-exec --no-prefix app -- bash -c …` |
  | status | `vagrant status --machine-readable` (native) |
  | stop | `vagrant halt pg redis` |
  | cleanup | `vagrant destroy -f` + scripted network/volume removal |

- `docker-exec` behaviour, from the 2.4.9 source:
  - stdout and stderr are merged on success;
  - a failing command becomes Vagrant exit 1, so the app's exit code is lost;
  - a stopped target prints a notice and exits 0.

  Stop therefore halts only the service machines. The app container stays up, so post-stop
  identity fails truthfully. All checks parse JSON receipts.
- Readiness is scripted: the provider only waits for `State.Running`. Per-checkout data is
  scripted, because the network and named volumes come from `vagrant-resources.sh` with
  ownership-label checks.
- Image builds: Vagrant parses both the BuildKit and containerd-store outputs, and `-t rwb-<run>-<co>-app` makes the build image cleanable.
- Same bind-mount design as Tilt. Artifacts: `.vagrant/machines` (machine IDs to match against containers).

## Expected outcomes to watch in the serialized run (not yet observed)

- `bad_config`: D asks for `postgres:99.99.99-alpine` (Tilt, Vagrant) or `nixpkgs#postgresql_99` (Organist).
  - Organist and Vagrant should refuse cleanly: the eval error, or `docker run` failing before
    `redis` is created under `--no-parallel`.
  - Tilt may start D's Redis while PostgreSQL's pull fails. Neither Tilt nor Compose rolls back,
    so that is a legitimate `fail` ("new service processes left running"), not a harness fault.
- Setup scope differs (`setup_scope`). For both container tools, image pulls and builds fall in
  `start.a`. Compare `first_task.*`, not `setup.*`.
- Host-transport bodies run under macOS `/bin/bash` 3.2. The transport's `$EPOCHREALTIME`
  marker is empty there, so Tilt and Vagrant record outer timings only. Bodies avoid bash-4
  features (tested with `/bin/bash -n`).
- Docker credential helper: research noted `docker buildx imagetools` failing in the host
  helper. If a pull or build fails for that reason, it is an environment block for the parent
  to record, not a tool result.

## Core-hook notes (requests only; no shared file was edited)

1. `FakeTransport` prints no `*-instance-identity` receipt. Every container-boundary adapter
   therefore gets `isolation: fail` in the shared contract test, which only asserts "no error".
   Suggestion: default fake receipts that differ per checkout. My tests inject them via `world.outputs`.
2. Host transport on macOS has no inner timing (bash 3.2). Consider running the wrapper with a
   newer bash if one is available, or report "outer only" explicitly for host adapters.
3. `occupy()` already falls back to `nohup` without `setsid`. No change needed; noted for reviewers.

## Preflight evidence executed (no lifecycle, no timings)

- Registry digests for all four images, and release checksums for Tilt, Compose and Vagrant (all matched).
- Tilt, using the adapter's real bodies on host:
  - `provision` (hash path) and `versions` passed;
  - `prepare` + `setup` for A, and for D with the break applied, passed;
  - the evaluated graph has `postgres`, `redis`, and `app` depending on both; `wait: true`;
    project `rwb-<run>-a`; image selector `rwb-<run>-a-app`;
  - the Compose v5.6.0 model has the bind source and target equal to the checkout path.
- Vagrant, same steps: `vagrant validate` passed for A and D.
- Organist, in a disposable `rwb-probe-organist-*` container (removed afterwards):
  - adapter `prepare` → `nix flake lock` → `regenerate-lockfile` (needs lazy trees off, above);
  - `nix eval` of the dev shell drvPath, and the apps `regenerate-files`, `regenerate-lockfile`,
    `start-services`;
  - `honcho check`: "Valid procfile detected (postgres, redis)", resolving postgresql-17.11 and redis-8.10.2;
  - Honcho runs children in its cwd (`cwd=None`), which keeps `./organist-services.sh` correct.
- No Docker resources remain from these checks. The only leftovers are this agent's scratch
  dirs outside the repo: `/tmp/rwb-dl-tilt`, `/tmp/rwb-dl-vagrant`, `/tmp/rwb-research-organist`
  and `/tmp/rwb-scripts`. They are safe to delete; keep the first two to use `tools_dir`.

## Checkpoints

- [x] Adapters aligned to the contract v1 hooks: `start_waits_ready`, `shared_files`,
      `bench_local_env`, `artifacts`, `setup_scope`, `cache_note`, `bad_config_pattern`, and
      provisioning-blocked exit 77 / `RWB-BLOCKED:` for missing platform, Docker daemon,
      hdiutil/pkgutil or Nix daemon.
- [x] `python3 -m unittest bench/tests/test_additional_adapters.py` (17 tests) covers:
  - registration, declared modes and boundaries;
  - the full fake scenario per tool, with expected statuses;
  - identical instance receipts failing isolation;
  - `bash -n` of every generated body (host bodies against `/bin/bash` 3.2);
  - run-scoped Docker queries; digest-pin consistency across files; the shared Redis policy;
  - the breakers executed on copies; local env round-trip;
  - `remove_owned` against a `docker` stub, removing only owned items and running nothing on empty input;
  - the holder never signalling a PID that is not its Honcho.
- [x] Full suite `python3 -m unittest discover -s bench/tests`: 162 tests. The last run had one
      failure, core's `test_core.ParentFindingsTest.test_p4_occupied_port_needs_conflict_diagnostic`.
      It runs on the core `Toy` adapter, and core was changing during hand-off. All Tilt, Organist
      and Vagrant tests pass, including the shared contract test over them.
- [x] `run.py --dry-run` succeeds for all three.
- [ ] Parent-serialized real runs (not done here, by instruction).
