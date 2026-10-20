# services-flake with process-compose-flake

Research date: 2026-10-06. This note provides an implementation recipe. No service startup, integration test, isolation, persistence, teardown, or timing measurement was run during research.

## Scope and versions

services-flake is a direct competitor for native reproducible development services. Its Nix modules configure PostgreSQL initialization, server commands, probes, restart policy, persistent directories, and multiple named instances. process-compose-flake turns those modules into locked Nix packages that invoke Process Compose. Python tooling comes from the same Nixpkgs input; application dependencies use the shared fixture's `uv.lock`. This is a distinct row from plain Nix with handwritten server scripts. [Service implementation](https://github.com/juspay/services-flake/tree/0ba7183cab54ffbd0be70cb95694f024701afd2b/nix/services), [wrapper implementation](https://github.com/Platonic-Systems/process-compose-flake/blob/464ff6880737f063c3f0d3d2c7781fda9190868f/nix/process-compose/default.nix).

Live official repository and release checks established:

| Component | Inspected identity |
| --- | --- |
| services-flake current main | `0ba7183cab54ffbd0be70cb95694f024701afd2b`, commit dated 2026-10-05 |
| Latest services-flake release | [0.4.0](https://github.com/juspay/services-flake/releases/tag/0.4.0), published 2024-12-10, commit `9cf03e68a1fe33822f1a444ea47a7a9bce15e01e` |
| process-compose-flake current main | `464ff6880737f063c3f0d3d2c7781fda9190868f` |
| Latest process-compose-flake release | [0.1.0](https://github.com/Platonic-Systems/process-compose-flake/releases/tag/0.1.0), published 2023-06-12 |
| Bundled Process Compose with the Nixpkgs pin below | [1.122.0 package definition](https://github.com/NixOS/nixpkgs/blob/151fa4e8ddfdd8dd25d945ad94ed54a13de9f6e4/pkgs/by-name/pr/process-compose/package.nix), source commit `673850dd20683ef14b33890444ca3416d02c751c` |
| Local `ev-nix:latest` | ARM64 Linux, image ID `sha256:fbf87c18aced2d3c969df40d15b137943429882f4283fe256c2f481a7af17c6a`, created 2026-10-02 |
| Executed image version check | `nix (Determinate Nix 3.23.0) 2.35.2` |

The recipe pins current module sources, including process-compose-flake's modern `cli` options. Report that source identity rather than labeling it services-flake 0.4.0. An optional tagged-release lane can replace only the services-flake input with its 0.4.0 commit; the PostgreSQL and Redis options used below exist there too. Do not pin process-compose-flake 0.1.0 and assume its API matches current main. The modules have no standalone services-flake binary to install in ev-nix.

Clones are under `/tmp/stack-bench-sources/services-flake`, `/tmp/stack-bench-sources/services-flake-process-compose`, and `/tmp/stack-bench-sources/services-flake-process-compose-cli`. I read service setup, module merging, wrappers, checks, CLI generation, and the exact bundled Process Compose detached/list/start/restart implementations. Source findings below are not execution receipts.

## Native features and benchmark work

| Requirement | Native support | Application or benchmark work |
| --- | --- | --- |
| Pinned tools | Nixpkgs package/store identity and `flake.lock` | Choose package attributes and record actual versions |
| PostgreSQL | `initdb`, initial databases/schema scripts, loopback configuration, `pg_isready`, five restart attempts, fast SIGINT shutdown | Migrations after initial creation; endpoint identity assertions |
| Redis | Configuration file, data directory, `PING` readiness, restart policy | Explicit AOF policy for the common persistence contract |
| Multiple instances | Attribute sets such as `services.postgres.pg` and separate process groups | Choose distinct TCP ports and manager sockets |
| Detached lifecycle | Process Compose `up -D`, `process start/stop/restart`, `down` | Enable its API, target the right socket, await actual readiness and closure |
| Python tests | Locked shell tools and commands, optional native flake check | `uv sync --frozen`, fixture migrations and integration tests |
| Config reuse | Export/import reusable `processComposeModules` | Copy application source, locks and config; exclude live state |

The service library supplies reusable Nix modules, not an ownership/lease registry or automatic free-port allocation. Independent checkouts share immutable packages but require separate writable paths and ports. Reusable groups can also be exported for another flake to import without importing a whole developer environment. [Multi-instance module implementation](https://github.com/juspay/services-flake/blob/0ba7183cab54ffbd0be70cb95694f024701afd2b/nix/lib.nix), [module sharing](https://github.com/juspay/services-flake/blob/0ba7183cab54ffbd0be70cb95694f024701afd2b/doc/share-services.md).

## Concrete locked recipe

Place this `flake.nix` beside the shared Python 3.13 fixture. Both independent checkouts receive exactly the same config and locks. A selects `services-a`/shell `a`; B selects `services-b`/shell `b`. The port pair is configuration chosen by the adapter, not dynamic allocation. Use benchmark-owned free port pairs or an isolated container namespace.

```nix
{
  description = "Python app with native services-flake PostgreSQL and Redis";
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/151fa4e8ddfdd8dd25d945ad94ed54a13de9f6e4";
    flake-parts.url = "github:hercules-ci/flake-parts/024633cd702b10285db5cb19b40ad48d2399ba60";
    process-compose-flake.url = "github:Platonic-Systems/process-compose-flake/464ff6880737f063c3f0d3d2c7781fda9190868f";
    services-flake.url = "github:juspay/services-flake/0ba7183cab54ffbd0be70cb95694f024701afd2b";
  };
  outputs = inputs: inputs.flake-parts.lib.mkFlake { inherit inputs; } {
    systems = [ "aarch64-linux" "x86_64-linux" "aarch64-darwin" "x86_64-darwin" ];
    imports = [ inputs.process-compose-flake.flakeModule ];
    perSystem = { config, pkgs, self', ... }:
      let
        ports = {
          a = { pg = 15432; redis = 16379; };
          b = { pg = 25432; redis = 26379; };
        };
        mkGroup = key: p: {
          imports = [ inputs.services-flake.processComposeModules.default ];
          cli.preHook = "mkdir -p .bench-services";
          cli.environment = {
            PC_DISABLE_TUI = true;
            PC_DISABLE_DOTENV = true;
          };
          cli.options = {
            # services-flake defaults this to true. Client control requires false.
            no-server = false;
            use-uds = true;
            unix-socket = ".bench-services/pc.sock";
            log-file = ".bench-services/manager.log";
          };
          services.postgres.pg = {
            enable = true;
            package = pkgs.postgresql_17;
            port = p.pg;
            listen_addresses = "127.0.0.1";
            socketDir = "";
            superuser = "bench";
            dataDir = "./.bench-services/postgres";
            initialDatabases = [ { name = "bench"; } ];
            settings.cluster_name = "services-flake-${key}";
          };
          services.redis.rd = {
            enable = true;
            package = pkgs.redis;
            port = p.redis;
            bind = "127.0.0.1";
            dataDir = "./.bench-services/redis";
            extraConfig = ''
              appendonly yes
              appendfsync always
              save ""
            '';
          };
        };
        mkShell = key: p: pkgs.mkShellNoCC {
          inputsFrom = [ config.process-compose."services-${key}".services.outputs.devShell ];
          packages = [ pkgs.python313 pkgs.uv self'.packages."services-${key}" ];
          UV_PYTHON_DOWNLOADS = "never";
          UV_PYTHON = "${pkgs.python313}/bin/python3";
          DATABASE_URL = "postgresql://bench@127.0.0.1:${toString p.pg}/bench";
          REDIS_URL = "redis://127.0.0.1:${toString p.redis}/0";
          RWB_CHECKOUT = key;
        };
      in {
        process-compose = {
          services-a = mkGroup "a" ports.a;
          services-b = mkGroup "b" ports.b;
        };
        devShells = builtins.mapAttrs mkShell ports;
      };
  };
}
```

Record Python, uv, PostgreSQL, Redis and Process Compose versions after realization. The package attributes pin patch versions through Nixpkgs, but exact version matching across competitors still needs the agreed package selections. The shared fixture requires Python 3.13. The AOF setting follows a synchronous acknowledged-write persistence condition; use the same setting in other recipes.

For each disposable fixture checkout, generate the lock once before the measured frozen workflow, and retain its hash. A Git-backed flake excludes untracked files, so track `flake.nix`, the generated `flake.lock`, and any files imported by Nix in that fixture. This is fixture setup, not authorization to stage this repository.

```bash
set -euo pipefail
export NIX_CONFIG='experimental-features = nix-command flakes'
git add flake.nix
nix flake lock
git add flake.lock
nix develop .#a --no-update-lock-file --command \
  uv sync --frozen --python python3.13
nix build .#services-a --no-update-lock-file --out-link .bench-services-package
```

Dependency installation and realization belong inside the fresh usable-checkout cost unless the condition explicitly declares them prewarmed. Keeping a prepared result symlink or the wrapper package in a shell is a supported warm workflow.

## Noninteractive lifecycle and repeated commands

Run the following from checkout A, as non-root `agent` in ev-nix. PostgreSQL refuses root. Each command can start in a fresh noninteractive shell; `nix develop` exports the configured URLs.

```bash
set -euo pipefail
export NIX_CONFIG='experimental-features = nix-command flakes'
nix run .#services-a --no-update-lock-file -- up -D
nix run .#services-a --no-update-lock-file -- process list -o json
nix develop .#a --no-update-lock-file --command \
  uv run --frozen python -m rwbapp wait --timeout 60
nix develop .#a --no-update-lock-file --command \
  uv run --frozen python -m rwbapp migrate
nix develop .#a --no-update-lock-file --command \
  uv run --frozen python -m rwbapp mark --checkout a
nix develop .#a --no-update-lock-file --command \
  uv run --frozen pytest -q
nix develop .#a --no-update-lock-file --command \
  uv run --frozen python -m rwbapp identity
```

`up -D` waits up to five seconds for the manager API only. It does not establish healthy databases. Poll `process list -o json` under the common outer deadline and require named `pg` and `rd` entries with `is_running: true`, `has_ready_probe: true`, `is_ready: "Ready"`, plus successful `pg-init`. Then require the application wait/query and identity checks. The PostgreSQL module depends on successful initialization; an application process declared inside the group can natively depend on `pg` and `rd` with `condition = "process_healthy"`. [Exact detached behavior](https://github.com/F1bonacc1/process-compose/blob/673850dd20683ef14b33890444ca3416d02c751c/src/cmd/project_runner_unix.go), [JSON list fields](https://github.com/F1bonacc1/process-compose/blob/673850dd20683ef14b33890444ca3416d02c751c/src/types/process.go#L278), [PostgreSQL startup and probes](https://github.com/juspay/services-flake/blob/0ba7183cab54ffbd0be70cb95694f024701afd2b/nix/services/postgres/default.nix).

Repeat tests/application commands while the detached manager remains alive. Measure `nix develop -c` entry separately from executing the same command repeatedly inside a held shell, and from calling `.bench-services-package/bin/services-a` for service control. Do not rerun `up -D` as a repeated-command substitute against an already-running socket.

Checkout B uses `services-b`, shell `b`, and `--checkout b`, all from B's working directory. Both checkouts may share package caches. Their CWD-relative data directories and control sockets are independent. Run them in the same container for the same-host explicit-port scenario, or report separate container identity for a per-container scenario.

```bash
# In checkout A, persist fixture data before restart.
nix develop .#a --no-update-lock-file --command \
  uv run --frozen python -m rwbapp persist --checkout a
nix run .#services-a --no-update-lock-file -- process restart pg
nix run .#services-a --no-update-lock-file -- process restart rd
nix develop .#a --no-update-lock-file --command \
  uv run --frozen python -m rwbapp wait --timeout 60
nix develop .#a --no-update-lock-file --command \
  uv run --frozen python -m rwbapp persisted --checkout a
nix run .#services-a --no-update-lock-file -- down
```

Restart is a control request; verify readiness and new server identity afterward. For full manager restart, call `down`, await API disappearance, child exit and closed service endpoints, then `up -D` and the readiness checks again. Data remains in `.bench-services`. Stopping A must leave B able to run tests and mutations. Erase only that checkout's owned data after verified stop when testing a clean reset. [Control commands](https://github.com/F1bonacc1/process-compose/tree/673850dd20683ef14b33890444ca3416d02c751c/src/cmd), [persistence/data-path contract](https://github.com/juspay/services-flake/blob/0ba7183cab54ffbd0be70cb95694f024701afd2b/doc/datadir.md).

## Platform constraints and likely harness mistakes

- The official modules target Linux and macOS. ev-nix runs native ARM64 Linux processes with Nix's daemon, no systemd service unit and no nested Docker dependency. Native Windows is outside this recipe; WSL is a Linux environment. Actual realization on all four listed Nix systems remains unverified here. [Official scope](https://github.com/juspay/services-flake/blob/0ba7183cab54ffbd0be70cb95694f024701afd2b/README.md).
- Run from each checkout root. `nix run /some/other/flake#services-a` resolves code there but relative service data still belongs to the caller's CWD. The native wrapper does not automatically change directory. Save an executable/store path and exact CWD before deleted-workspace cleanup tests; no built-in external ownership registry was found. [Wrapper](https://github.com/Platonic-Systems/process-compose-flake/blob/464ff6880737f063c3f0d3d2c7781fda9190868f/nix/process-compose/default.nix), [data directory guide](https://github.com/juspay/services-flake/blob/0ba7183cab54ffbd0be70cb95694f024701afd2b/doc/datadir.md).
- Keep the absolute manager UDS path short, under the platform Unix socket path limit. The relative `.bench-services/pc.sock` recipe assumes a short fixture root. For longer roots, override the socket option with a unique runner-owned short absolute path and use that same path for every control command. PostgreSQL TCP with empty `socketDir` avoids a second path constraint.
- Override `cli.options.no-server = false`. services-flake sets it to true by default. Without an API, detached startup's API wait and later `down`/client operations cannot work. Unique UDS avoids Process Compose's default API TCP port collision. [services-flake default](https://github.com/juspay/services-flake/blob/0ba7183cab54ffbd0be70cb95694f024701afd2b/nix/process-compose/defaults.nix), [CLI options](https://github.com/Platonic-Systems/process-compose-flake/blob/464ff6880737f063c3f0d3d2c7781fda9190868f/nix/process-compose/cli.nix).
- Native probes are `pg_isready` and Redis `PING`. A wrong healthy listener can satisfy a protocol probe. PostgreSQL initialization waits for its own temporary socket, but the server probe still needs an independent data/cluster identity assertion. Redis's TCP probe uses its port and the client's default host; keep bind loopback for this recipe. [Redis probe implementation](https://github.com/juspay/services-flake/blob/0ba7183cab54ffbd0be70cb95694f024701afd2b/nix/services/redis.nix).
- Initial SQL scripts run on first data-directory initialization. Editing initialization SQL does not migrate existing state. Use the fixture's explicit migration command for repeated development changes. A partial initialization failure may leave a directory, so a later init can take the existing-state path. Preserve logs and fail the task rather than interpreting mere directory existence as successful initialization. [Setup implementation](https://github.com/juspay/services-flake/blob/0ba7183cab54ffbd0be70cb95694f024701afd2b/nix/services/postgres/setup-script.nix).
- Defaults include a two-second initial probe delay and ten-second period. Keep the default lane intact; if testing a tuned lane, override the generated `settings.processes.pg/rd.readiness_probe` explicitly and disclose it for all tools.
- `nix flake check` is a supported alternative when a process named `test` exists. process-compose-flake enables that process in a separate config, sets exit-on-end and exit-on-skipped, and adds a derivation check. Its build sandbox/state model is distinct from persistent services in a checkout, and successful cached check realization is not a newly executed integration test. [Test config generation](https://github.com/Platonic-Systems/process-compose-flake/blob/464ff6880737f063c3f0d3d2c7781fda9190868f/nix/process-compose/settings/default.nix#L204), [check implementation](https://github.com/Platonic-Systems/process-compose-flake/blob/464ff6880737f063c3f0d3d2c7781fda9190868f/nix/process-compose/test.nix).

## Comparable tasks and exact checks

Use fresh usable checkout, repeated application commands, simultaneous A/B state isolation, interrupted service recovery, full manager restart, preserved data, copied locks/config, and complete cleanup. Keep the same application, Python minor, dependency lock, mutations, durability policy and common deadlines across tools. Do not make a Stack-specific bundle or lease command an automatic criterion.

Require fixture JSON `ok: true` and expected command/result fields, not exit status alone. After initial migration, run distinct A/B markers through the same SQL table and Redis key. Require PostgreSQL `SHOW data_directory` to resolve to that checkout's `.bench-services/postgres`, different PostgreSQL system identifiers, distinct Redis `run_id`, matching Redis `CONFIG GET dir`, expected `cluster_name`, port and version, and matching application URL endpoints. A PID in Process Compose can be a shell parent, so do not assume it equals the server PID without process-tree verification. Stop A and continue querying/mutating B. After restart, require new server PIDs/Redis run ID but unchanged PostgreSQL system identifier and fixture persisted rows/cache values.

For config reproduction, copy app sources, `pyproject.toml`, `uv.lock`, `flake.nix`, and `flake.lock` into clean C; select a nonconflicting explicit port pair if C runs concurrently. Exclude `.venv`, data, sockets and result links. Verify frozen lock hashes and package identities, and repeat the application scenario. Report the port override as necessary instance configuration, not an automatic services-flake feature. Keep old `eval` results historical; plain Nix's old skipped service cells say nothing about this native service-module workflow.

## Validation performed

Executed source/release queries, Docker image metadata inspection, a fresh disposable ev-nix version invocation, and an offline `nix-instantiate --parse` of the inspected PostgreSQL module. No existing container/service was touched. The extracted recipe passed offline Nix syntax parsing in ev-nix, and every Bash block passed `bash -n`. These checks are separate from module evaluation and realization; the implementation must still evaluate/build the locked flake and execute the lifecycle checks before reporting capability success or timings.
