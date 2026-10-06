# Organist research

Research date: 2026-10-06. Organist belongs in the Nix development-environment comparison. It declares packages, shells, generated files, and foreground services in Nickel. Its native service runner is Honcho, generated from `config.services`; treating it as plain Nix with no service support would miss a real feature. PostgreSQL/Redis initialization, readiness, endpoint allocation, persistent directories, and detached lifecycle control still require project scripts. [Service implementation](https://github.com/nickel-lang/organist/blob/a7e4e638cade5e7c4f36a129b80d91bf3538088e/lib/services.ncl)

## Pin and evidence boundary

- Inspected main commit `a7e4e638cade5e7c4f36a129b80d91bf3538088e`, committed 2025-08-12. The only upstream tag is `v0.1`, commit `29255e6bc712530d9ab3b0b397cb0b58ee34407c`. Upstream lists that first release as 2023-11-16 and documents subsequent breaking module-interface changes. Use the inspected main SHA for this configuration and label it a commit build, not a current stable release. [Release notes](https://github.com/nickel-lang/organist/blob/a7e4e638cade5e7c4f36a129b80d91bf3538088e/RELEASES.md)
- The live GitHub repository API reports `archived=false`, `disabled=false`, and `pushed_at=2025-11-21T00:18:13Z`; its releases endpoint returns an empty list. That push timestamp is repository activity, not a newer main implementation. Limited recent activity is grounds to report maintenance uncertainty, not to call the project dead. [Repository API](https://api.github.com/repos/nickel-lang/organist), [releases API](https://api.github.com/repos/nickel-lang/organist/releases)
- Executed evidence is shallow source inspection, tag/API queries, and read-only Docker image inspection. No environment evaluation, service lifecycle, integration run, or timings were executed for this note. The snippets below are an implementation recipe whose compatibility must pass provision/setup gates before measurement.
- Local `ev-nix` exists as Linux/arm64 image `sha256:fbf87c18aced2d3c969df40d15b137943429882f4283fe256c2f481a7af17c6a`. There is no host `nix` executable. Organist is a flake library, not a separate CLI requiring an image-installed Organist version. The image's exact Nix version has not been executed here; record `nix --version` in provisioning. Existing `ev-*` containers were left untouched.

## What is native

| Requirement | Classification and implementation evidence |
|---|---|
| Pinned package set and shell environment | Native through Nix inputs/`flake.lock`, `shells.*.packages`, and `shells.*.env`. Nickel package references resolve against flake inputs. [Nix importer](https://github.com/nickel-lang/organist/blob/a7e4e638cade5e7c4f36a129b80d91bf3538088e/lib/lib.nix#L92), [shell builder](https://github.com/nickel-lang/organist/blob/a7e4e638cade5e7c4f36a129b80d91bf3538088e/lib/nix-interop/builders.ncl#L73) |
| Services | Native declaration of arbitrary command strings and a generated `start-services` flake app. No service-specific PostgreSQL/Redis schema. [Service schema and generated command](https://github.com/nickel-lang/organist/blob/a7e4e638cade5e7c4f36a129b80d91bf3538088e/lib/services.ncl#L14) |
| Process supervision and logs | Native via Honcho. The selected Nixpkgs supplies Honcho 2.0.0. It supervises foreground child process groups, multiplexes output, stops siblings when any child exits, handles SIGINT/SIGTERM, and escalates to SIGKILL after five seconds. It does not restart failed children. [Package](https://github.com/NixOS/nixpkgs/blob/151fa4e8ddfdd8dd25d945ad94ed54a13de9f6e4/pkgs/by-name/ho/honcho/package.nix), [Honcho manager](https://github.com/nickstenning/honcho/blob/8af63177405a574612abf90ef55aaa68d76f7686/honcho/manager.py#L85), [group termination](https://github.com/nickstenning/honcho/blob/8af63177405a574612abf90ef55aaa68d76f7686/honcho/compat.py#L21) |
| Tasks | Custom flake apps/packages/checks are native extension points. A `ShellApplication` wraps authored Bash, adds runtime package PATH entries, and checks Bash syntax/ShellCheck. There is no native task dependency graph or service-dependent task contract. [Output schema](https://github.com/nickel-lang/organist/blob/a7e4e638cade5e7c4f36a129b80d91bf3538088e/lib/schema.ncl#L37), [script builder](https://github.com/nickel-lang/organist/blob/a7e4e638cade5e7c4f36a129b80d91bf3538088e/lib/nix-interop/builders.ncl#L96), [upstream app example](https://github.com/nickel-lang/organist/blob/a7e4e638cade5e7c4f36a129b80d91bf3538088e/project.ncl#L27) |
| Dependency installation | Nix tool packages are native. `uv sync` for this fixture's Python packages is project-authored, with the shared `uv.lock`. |
| Readiness, unique endpoints, instance guard | Scripted readiness and port assignments. No native ownership/URL validation or automatic per-checkout port allocation in the service schema. |
| Data persistence and restart | Scripted directory choice and initialization guard, then PostgreSQL/Redis server persistence. Restart the foreground Honcho session; there is no independent service restart command in this module. |
| Detached start/stop/status, leases | A benchmark-owned holder is scripted. No native detached daemon registry, lease tracking, or abandoned-session reap in the inspected module. |

## Concrete configuration

Use the same application fixture and `uv.lock` as other adapters. The following Nixpkgs snapshot provides Python **3.13.15**, PostgreSQL **17.11**, Redis **8.10.2**, and uv **0.12.22**. This meets the requested major families, but Python/uv patches differ from some existing adapters. Record actual versions and do not claim patch-identical toolchains. [Python source version](https://github.com/NixOS/nixpkgs/blob/151fa4e8ddfdd8dd25d945ad94ed54a13de9f6e4/pkgs/development/interpreters/python/default.nix#L57), [PostgreSQL 17](https://github.com/NixOS/nixpkgs/blob/151fa4e8ddfdd8dd25d945ad94ed54a13de9f6e4/pkgs/servers/sql/postgresql/17.nix), [Redis](https://github.com/NixOS/nixpkgs/blob/151fa4e8ddfdd8dd25d945ad94ed54a13de9f6e4/pkgs/by-name/re/redis/package.nix#L31), [uv](https://github.com/NixOS/nixpkgs/blob/151fa4e8ddfdd8dd25d945ad94ed54a13de9f6e4/pkgs/by-name/uv/uv/package.nix#L21)

`flake.nix`:

```nix
{
  inputs.nixpkgs.url =
    "github:NixOS/nixpkgs/151fa4e8ddfdd8dd25d945ad94ed54a13de9f6e4";
  inputs.organist.url =
    "github:nickel-lang/organist/a7e4e638cade5e7c4f36a129b80d91bf3538088e";
  outputs = { organist, ... } @ inputs:
    organist.flake.outputsFromNickel ./. inputs {};
}
```

Keep Organist's own Nixpkgs input pinned by its upstream lock. It supplies Nickel **1.7.0**, while the project's `nixpkgs` supplies the application tools. Do not add `inputs.organist.inputs.nixpkgs.follows = "nixpkgs"` without validating current Nickel compatibility. The older library lock does not prevent selecting modern application packages. [Organist lock](https://github.com/nickel-lang/organist/blob/a7e4e638cade5e7c4f36a129b80d91bf3538088e/flake.lock), [locked compiler](https://github.com/NixOS/nixpkgs/blob/a71e967ef3694799d0c418c98332f7ff4cc5f6af/pkgs/by-name/ni/nickel/package.nix#L11)

`project.ncl`, using the current template's module form:

```nickel
let inputs = import "./nickel.lock.ncl" in
let organist = inputs.organist in
let pkg = organist.import_nix in
organist.OrganistExpression
& organist.services
& {
  Schema,
  config | Schema = {
    shells = organist.shells.Bash,
    shells.build.packages = {
      python = pkg "nixpkgs#python313",
      uv = pkg "nixpkgs#uv",
      postgres = pkg "nixpkgs#postgresql_17",
      redis = pkg "nixpkgs#redis",
    },
    shells.build.env.UV_PYTHON = nix-s%"%{pkg "nixpkgs#python313"}/bin/python3"%,
    shells.build.env.UV_PYTHON_DOWNLOADS = "never",
    filegen_hook.enable = false,
    services = {
      postgres = nix-s%"exec %{pkg "nixpkgs#bash"}/bin/bash scripts/organist-services.sh postgres %{pkg "nixpkgs#postgresql_17"}/bin"%,
      redis = nix-s%"exec %{pkg "nixpkgs#bash"}/bin/bash scripts/organist-services.sh redis %{pkg "nixpkgs#redis"}/bin"%,
    },
  },
} | organist.modules.T
```

The build shell propagates to dev/default through the schema. Disable automatic file generation here and explicitly regenerate the Nickel import file during setup. This keeps shell-entry timing from including file writes. [Template](https://github.com/nickel-lang/organist/blob/a7e4e638cade5e7c4f36a129b80d91bf3538088e/templates/default/project.ncl), [file hook](https://github.com/nickel-lang/organist/blob/a7e4e638cade5e7c4f36a129b80d91bf3538088e/lib/files.ncl#L124)

Create `scripts/organist-env.sh`, sourced from the checkout root by every app or service command:

```bash
set -euo pipefail
source .rwb-organist/ports.sh
export PGDATA="$PWD/.rwb-organist/postgres"
export REDISDATA="$PWD/.rwb-organist/redis"
export DATABASE_URL="postgresql://postgres@127.0.0.1:${PGPORT}/postgres"
export REDIS_URL="redis://127.0.0.1:${REDISPORT}/0"
export RWB_CHECKOUT
```

Create `scripts/organist-services.sh`:

```bash
set -euo pipefail
source scripts/organist-env.sh
case "$1" in
  postgres)
    mkdir -p "$PGDATA"
    if [[ ! -f "$PGDATA/PG_VERSION" ]]; then
      "$2/initdb" -D "$PGDATA" -U postgres --auth=trust --no-locale
    fi
    exec "$2/postgres" -D "$PGDATA" -h 127.0.0.1 -p "$PGPORT" \
      -c unix_socket_directories='' -c fsync=on
    ;;
  redis)
    mkdir -p "$REDISDATA"
    exec "$2/redis-server" --bind 127.0.0.1 --port "$REDISPORT" \
      --daemonize no --dir "$REDISDATA" --appendonly yes \
      --appendfsync everysec --save ''
    ;;
  *) exit 2 ;;
esac
```

These state paths are absolute physical checkout paths if the adapter enters each checkout with `cd -P`. A and B must get separate machine-local `ports.sh`, for example A `PGPORT=45432`, `REDISPORT=46379`, `RWB_CHECKOUT=a`; B `45433`, `46380`, `b`. The adapter writes those assignments under each `.rwb-organist/` directory. They are deliberately uncommitted and are not package lock inputs. This allocation is scripted and static, not an Organist feature.

## Noninteractive commands and lifecycle

Run as non-root user `agent` in one disposable `ev-nix` container for A/B, so their loopback port conflicts remain real. The image entrypoint starts the Nix daemon without systemd. `initdb` rejects root. Enable Nix's `nix-command flakes` features through `NIX_CONFIG`; avoid a global host setting. Keep fixture/config/scripts tracked in the benchmark's disposable Git checkout and ignore `.rwb-organist/` and `.venv/`. Nix's Git flake capture then excludes runtime data. Do not point repeated evaluations at an unfiltered `path:` tree containing a live database: Organist's `callNickel` copies the source directory into a derivation. [Source capture and evaluation](https://github.com/nickel-lang/organist/blob/a7e4e638cade5e7c4f36a129b80d91bf3538088e/lib/lib.nix#L158)

```bash
export NIX_CONFIG='experimental-features = nix-command flakes'
nix --version
nix flake lock
nix run .#regenerate-lockfile
# Track generated flake.lock and nickel.lock.ncl in the disposable checkout.
nix develop .#dev --no-update-lock-file -c bash -c \
  'python3 --version; postgres --version; redis-server --version; uv --version'
nix develop .#dev --no-update-lock-file -c bash -c \
  'uv sync --locked --python "$UV_PYTHON"'
nix run .#start-services --no-update-lock-file -- check
```

`check` validates the generated Procfile only. It says nothing about running services. The native start command is blocking:

```bash
nix run .#start-services --no-update-lock-file -- start --env /dev/null
```

For the adapter, spawn that command through an owned holder, retain its PID and output log, and keep it alive between individual commands. `--env /dev/null` prevents Honcho loading a stray `.env`; service scripts read the explicit checkout file. Shutdown targets the owned Honcho process, waits for it, and checks both endpoints have stopped. Nix `run` normally execs its selected program; still prove the saved PID is Honcho in the implementation rather than guessing from the launcher. Do not daemonize PostgreSQL/Redis inside Honcho, because their launcher exiting causes native sibling teardown.

Run every following body through `nix develop .#dev --no-update-lock-file -c bash -c 'source scripts/organist-env.sh; BODY'`:

```bash
uv run --frozen --no-sync python -m rwbapp wait
uv run --frozen --no-sync python -m rwbapp identity
uv run --frozen --no-sync python -m rwbapp migrate
uv run --frozen --no-sync python -m rwbapp mark --checkout "$RWB_CHECKOUT"
uv run --frozen --no-sync python -m rwbapp crud --checkout "$RWB_CHECKOUT"
uv run --frozen --no-sync python -m rwbapp cache --checkout "$RWB_CHECKOUT"
uv run --frozen --no-sync pytest -q
uv run --frozen --no-sync python -m rwbapp persist --checkout "$RWB_CHECKOUT"
```

The app's retrying `wait` is scripted readiness, so `ready()` should return `None` under the adapter contract. No fixed sleep. Start A and B before their isolation checks, then run A `check --checkout a --forbid b` and B `check --checkout b --forbid a`. Stop/restart A without deleting its state while B remains usable; run A `persisted --checkout a`. Explicit Redis `SAVE` in the fixture plus AOF makes the clean-restart task concrete. Crash durability is a different task with the `everysec` policy. Honcho SIGTERM exit 143 is an expected requested-stop receipt, not a successful test exit; accept it only for deliberate teardown and verify no owned server remains.

For C, copy A's `flake.nix`, `flake.lock`, `project.ncl`, `nickel.lock.ncl`, scripts, app, and `uv.lock`; generate C's local ports/state file. Require `nix develop --no-update-lock-file` and `uv sync --locked`, and compare lock bytes before/after. Regenerating the Nickel import file is not a package update, but must also leave its bytes unchanged for a same-pin copy. `nickel.lock.ncl` contains Nix-store imports for editor/evaluator access; `flake.lock` is the actual source/version lock. [Lock generator and evaluator fallback](https://github.com/nickel-lang/organist/blob/a7e4e638cade5e7c4f36a129b80d91bf3538088e/lib/lib.nix#L25)

## Benchmark checks and common mistakes

- Capture application source token/path, Python executable/version, PostgreSQL `system_identifier`, port and `data_directory`, Redis `run_id`, port and `CONFIG GET dir`, plus declared URLs. Require different PG clusters and Redis runs for A/B, correct physical checkout directories, and only each checkout's own unprefixed marker. A green protocol probe alone can hit a foreign service.
- Measure cached `nix develop ... -c true`, repeated real app commands with services already running, full cold package resolution/build separately, dependency sync, migration/test completion, concurrent checkout isolation, restart persistence, and teardown. Retain warm/cold Nix store/cache state in metadata. Keep provisioning and Nickel compiler builds visible rather than burying them in command latency.
- Occupy a requested port and require the owned startup to fail. Organist has no automatic fallback; a successful connection to the occupying server must fail identity checks. Classify missing ownership guard/automatic allocation as unsupported native features, not an app-task failure if a truthful project script supplies them.
- The upstream PostgreSQL example unconditionally runs `initdb` and uses generic `postgresql`; copying it unchanged would fail restart and may choose a different major. Its test sleeps two seconds. The guarded initialization, explicit `postgresql_17`, and retrying application wait above are intentional authored improvements. [Upstream example](https://github.com/nickel-lang/organist/blob/a7e4e638cade5e7c4f36a129b80d91bf3538088e/examples/services/project.ncl), [upstream service test](https://github.com/nickel-lang/organist/blob/a7e4e638cade5e7c4f36a129b80d91bf3538088e/examples/services/test.sh)
- Some dependency-management prose still names `organist.contracts`/`organist.lib` despite the current exported `OrganistExpression`/`import_nix`. Use the inspected template/source API. Mixing the 0.1 tag with current snippets is a harness error. [Current exports](https://github.com/nickel-lang/organist/blob/a7e4e638cade5e7c4f36a129b80d91bf3538088e/lib/organist.ncl), [older prose](https://github.com/nickel-lang/organist/blob/a7e4e638cade5e7c4f36a129b80d91bf3538088e/doc/dependency-management.md)
- Organist uses flake-utils default systems, covering x86_64/aarch64 Linux and macOS. The selected services run as local processes on either OS; Unix signal/CWD/permissions still need target-specific validation. The available image validates only aarch64-linux once executed, not macOS or native Windows. [Flake output generator](https://github.com/nickel-lang/organist/blob/a7e4e638cade5e7c4f36a129b80d91bf3538088e/flake.nix#L28)

Implementation handoff: add an Organist adapter using `ev-nix`, `service-instance` isolation, native package/environment/service supervision, and explicitly scripted readiness, persistence layout, endpoint choice, and detached holder. Prefer this native Honcho workflow over replacing its service runner with process-compose. If setup exposes an upstream incompatibility, preserve the exact error and source pins as blocked evidence before considering a separately labeled patched variant.
