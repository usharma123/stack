# Native-extra adapters: handoff

Owner: native-extra implementation agent (Opus 5.5). Scope: Process Compose, services-flake,
pkgx/dev, dnvr, GNU Guix (frozen roster, `SCOPE.md`). Written against `ADAPTER-CONTRACT.md`
stable v1 and the `base.py`/`scenario.py` on disk on 2026-10-06. No core, registry, fixture or
research file was edited. Nothing here ran services, a lifecycle or a timing.

## Files

| Path | Purpose |
|---|---|
| `rwb/adapters/process_compose.py` | `ProcessComposeAdapter`; also `NixFlakeAdapter` (shared by the three Nix lanes), `NIX_PINS`, `short_runtime_dir`, `install_process_compose` |
| `rwb/adapters/services_flake.py` | `ServicesFlakeAdapter` |
| `rwb/adapters/pkgx.py` | `PkgxAdapter`, `install_pkgx`, `freeze_pantry` |
| `rwb/adapters/dnvr.py` | `DnvrAdapter` |
| `rwb/adapters/guix.py` | `GuixAdapter` |
| `adapters/process-compose/{toolchain/flake.nix,process-compose.yaml,rwb-pc.sh}` | toolchain flake, PC project, control glue |
| `adapters/services-flake/{services/flake.nix,rwb-sf.sh}` | service-module flake, control glue |
| `adapters/pkgx/pkgx.yaml` | dev environment, exact top-level pins |
| `adapters/dnvr/{dnvr/flake.nix,rwb-dnvr.sh}` | dnvr shell + Redis process module, PTY driver/stop |
| `adapters/guix/{channels.scm,manifest.scm,provision.sh}` | channel pin, manifest, in-container provisioning |
| `tests/test_native_extra_adapters.py` | 17 offline tests (fake scenario, bash -n, shellcheck, pins, preflight block) |

Registry entries already exist; class names match. pkgx and Guix use the shared
`_shared/rwb-env.sh` + `rwb-services.sh` (scripted). Process Compose uses `rwb-env.sh` only.
All write `bench.local.env` through `bench_local_env()`.

## Lanes and declared modes

| Adapter | Image | lock / frozen | services / detached | readiness | ports / data | stop confirm | status |
|---|---|---|---|---|---|---|---|
| process-compose | ev-nix | native (Nix toolchain flake) | native | native (`is-ready --wait`, 120 s outer deadline) | scripted / scripted | scripted | native JSON |
| services-flake | ev-nix | native | native | native (module probes + `is-ready --wait`) | scripted (`local.json`) / native | scripted | native JSON |
| pkgx | ev-base | unsupported / unsupported | scripted | scripted (app `wait`) | scripted | scripted | scripted JSON line |
| dnvr | ev-nix | native | native (persistent tmux session) | scripted, `start_waits_ready=True` | scripted (`local.json`) / native (`.dnvr`) | scripted | unsupported (text `dnvr ps`, kept as observed) |
| guix | ev-base | native (`describe -f channels` lock) | scripted | scripted (app `wait`) | scripted | scripted | scripted |

`wrong_instance_guard` is `unsupported` everywhere. Isolation boundary: `service-instance` for
all five (separate clusters/Redis processes in one container, distinct ports).

## Versions and deviations (actual runs must record the binaries' own output)

- Nix lanes share nixpkgs `151fa4e8` with the plain Nix research recipe: Python 3.13.15,
  PostgreSQL 17.11, Redis 8.10.2, uv 0.12.22 (source versions, not receipts). Process
  Compose binary v1.122.0 (archive sha256 `52fa7d5a…`, verified in a disposable container;
  binary reports commit `23b0aca`). services-flake bundles PC 1.122.0 from nixpkgs.
- services-flake is main `0ba7183` + process-compose-flake `464ff68` + flake-parts `024633c`,
  not release 0.4.0.
- dnvr is untagged commit `a66c2bb`. Its own lock pins nixpkgs `062346a6` (Redis 8.8.1); the
  adapter overrides with `dnvr.inputs.nixpkgs.follows` to the shared revision for version
  parity. That makes the sidebar/fblog build against a nixpkgs dnvr was not locked with;
  an evaluation/build failure there is a recipe risk, recorded as such.
- pkgx 2.11.0 (tar.xz sha256 `fb4b9c2b…`, verified), dev 1.8.1, Pantry frozen at
  `2df061bd` via `PKGX_PANTRY_DIR`. Pins python.org 3.13.15, uv 0.12.22, and the deviations
  **PostgreSQL 17.2.0** (only 17.0.0/17.2.0 published) and **Redis 8.10.0** (8.10.2 not
  published for linux/aarch64). No lockfile: transitive bottles are not pinned.
- Guix release 1.5.0 binary, channel `71d01018`: Python 3.13.13, **PostgreSQL 16.14**,
  **Redis 7.2.6**, uv 0.10.12. Not the canonical PG 17 / Redis 8 workload; must not be reported
  as an exact match. Pins carry `deviation`.
- Redis durability is `appendonly yes` + `appendfsync always` in all five (shared policy).

## Workflow notes per tool

- **Process Compose**: `up --detached` inside `nix develop` (manager inherits the toolchain
  PATH and endpoint variables). Each checkout gets its own UDS `/tmp/rwb-pc-<sha1(run)[:10]>/<co>.sock`.
  A second `start` while the manager answers is a logged no-op (`up` is not idempotent).
  `down` returns before teardown, so `rwb-pc.sh down` waits until the postgres/redis PIDs from
  the JSON list and the API are gone. YAML validated with `up --dry-run`
  (`Validated 4 configured processes from 2 files`, exit 0).
- **services-flake**: one group `services`; port pair, `cluster_name` and socket come from the
  uncommitted `services/local.json`. `no-server = false`, `use-uds`, unique socket. Default
  module probe timings (2 s delay, 10 s period) are kept. Data under `.rwb-state/sf/` of the
  checkout root (the wrapper resolves against CWD).
- **pkgx**: environment entered on every command with
  `dev_env="$(pkgx --quiet +pkgx.sh/dev=1.8.1 -- dev)" && eval "$dev_env"` (dev's documented
  temporary activation; non-tty stdout selects its dump path). Because dev ignores pkgx's
  exit status, `enter` requires `python3 uv postgres redis-server` afterwards and `setup`
  asserts exact versions. Each entry pays Deno + dev + pkgx resolution; that is the lane.
- **dnvr**: `rwb-dnvr.sh up` runs `dnvr up` (the real tmux runner) under util-linux
  `script -qfec` with a FIFO as the keyboard and the transcript in `.dnvr/logs/rwb-pty-*.log`;
  waits for an attached client, then `dnvr-state wait pg.url` (native preset key) and
  `redis.url` (published by the benchmark's Redis process after `INFO server` PID match), then
  sends Ctrl-G (dnvr's `detach-client` binding). Repeated `up` reattaches and detaches without
  touching processes. `down`: Ctrl-C to each `@dnvr_role=process` pane (the dashboard `x`),
  wait until `dnvr ps` shows nothing `running` (flock liveness), then `kill-session` (the
  dashboard `Q`). Never `kill-server`, never the default tmux socket.
- **Guix**: three provisioning steps run `adapters/guix/provision.sh` inside the run's own
  container: `preflight` (root, read-only), `install` (root: tarball → `/gnu` `/var/guix`,
  build users, container-local `guix-daemon`, substitute keys), `canary` (agent: one
  substitute fetch and one local derivation build). Entry is
  `guix time-machine -q -C channels.lock.scm -- shell -q --pure -m manifest.scm -- bash --noprofile --norc -c ...`.

## Integration hooks requested from core

1. **Blocked provisioning.** `provision.sh` exits **77** and prints `RWB-BLOCKED: <reason>` when
   an environment prerequisite is missing. Today `Scenario.provision_and_versions` raises
   `ProvisionError` and `run.py` records `provision` = `fail`. Requested: exit 77 →
   `provision` = `blocked` with the reason line as detail, remaining checks `blocked`, and
   the run still valid. Without this hook a Guix blocker is mislabelled as a failure.
2. **Options.** Guix needs `--option guix_binary_sha256=<hex>` (required),
   optional `guix_binary_url=` and `guix_daemon_flags=`. A non-empty daemon flag (for example
   `--disable-chroot`) changes the title and pins and must be reported as its own condition.
3. **dnvr start cost.** `start` includes PTY-driver overhead and the readiness wait
   (`start_waits_ready=True`, `ready()` is `None`). Please keep start/readiness time reported
   as one dnvr "start to ready" number with a footnote, not compared as bare start latency.
4. **Artifacts on failure (optional).** Useful logs live in the checkout:
   `.rwb-state/logs/*` (PC/pkgx/Guix), `.rwb-state/sf/process-compose.log`,
   `.dnvr/logs/` (dnvr PTY transcripts, postgres jsonlog). A core hook to copy a declared list
   of checkout paths into the result directory would preserve them; none exists today.
5. `dnvr` overrides `supervisor_processes()` to list tmux/sidebar/wrapper processes.
   `service_processes()` (core default) is unchanged for all five.

## Honest blockers and unverified behavior

- **Guix is expected to be blocked here.** No verified SHA-256 of
  `guix-binary-1.5.0.aarch64-linux.tar.xz` exists in this repository, and from this host
  `ftp.gnu.org:443` did not connect (curl timed out; research saw exit 28 twice). I did not
  invent a digest; the parent must supply one obtained and verified out of band (GPG
  signature against the Guix release key). Research also saw `unshare -Ur` EPERM in a default
  ev-base container. That probe alone does not prove `guix shell` impossible, so it is only
  recorded. The `canary` build decides: if the container-local daemon cannot set up its build
  sandbox, the result is `RWB-BLOCKED` with the daemon log, not a Guix failure.
  `--disable-chroot` is never used unless the parent passes it explicitly.
- **dnvr's unattended path is source-derived, not runtime-verified:** Ctrl-G through `script`
  stdin from a FIFO, tmux 3.x behavior with no `stty size` (dnvr falls back to 80x24), and the
  PG preset's shutdown on Ctrl-C (its wrapper traps INT) all need the first serialized run.
  `kill-session` alone (native `Q`) would send SIGHUP, which postgres treats as reload; the stop
  path therefore interrupts panes first.
- **services-flake**: process names `pg`/`rd` in `process list` and the wrapper's handling of
  global options before client subcommands come from source reading only.
- **pkgx**: `dev` also sniffs `pyproject.toml` (adds `pip.pypa.io`) and `uv.lock` (adds
  `astral.sh/uv *`); pkgx must intersect those with the exact pins. dev forces
  `CLICOLOR_FORCE=1` on its inner pkgx call; whether that colors the emitted env is unverified.
- **Process Compose / services-flake occupied port**: postgres bind failure plus
  `exit_on_failure`/restart policy should surface as readiness failure within the 120 s outer
  deadline; unverified.
- Nothing here measured setup, lifecycle or latency. No `eval/` result was reused.

## Checks performed

- `python3 -m unittest bench/tests/test_native_extra_adapters.py`: 17/17 pass. In the full
  discovery run the core contract test passes for all five of these adapters; the remaining
  failures at hand-off time are core's own toy scenario tests and the worktree group's
  `workz`/`worktrunk` adapters (concurrent work by other owners).
- Bad-config recipes match the core `bad_config_pattern` (`99\.99\.99|postgresql_99`):
  Nix lanes `postgresql_99`, pkgx `=99.99.99`, Guix `postgresql@99.99.99`.
- `shellcheck -S warning` clean on `rwb-pc.sh`, `rwb-sf.sh`, `rwb-dnvr.sh`, `provision.sh`.
- `python3 bench/run.py --tool <name> --dry-run` for all five: exit 0.
- Disposable `ev-nix` containers (`--rm`, unique `rwb-nativeextra-parse*` names):
  `nix-instantiate --parse` ok for all three flakes (Determinate Nix 3.23.0 / Nix 2.35.2);
  Process Compose archive digest ok, `version` v1.122.0 commit 23b0aca, YAML `--dry-run` ok,
  `--detached`, `is-ready --wait`, `process list -o json` flags present; pkgx archive digest
  ok, `pkgx 2.11.0`.
