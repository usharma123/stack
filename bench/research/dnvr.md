# dnvr

Research date: 2026-10-06. This note is implementation guidance for Opus 5.5, not a benchmark result. No services or timing trials were run.

## Identity and decision

Include dnvr as a direct Nix environment/service-module contender. The canonical repository is [dialohq/dnvr](https://github.com/dialohq/dnvr), confirmed by a successful shallow clone from that URL. The discovery search returned a GitHub mirror; the mirror is not the source used here. Inspected master commit [`a66c2bbabb67293812a5c39855ab0ecf6af21d41`](https://github.com/dialohq/dnvr/commit/a66c2bbabb67293812a5c39855ab0ecf6af21d41), dated 2026-09-08. Live GitHub releases and tags APIs both returned empty arrays, and `git ls-remote --tags --refs` returned no tags. Use the commit as its version. The sidebar's internal `0.1.0` package version is not a dnvr release.

dnvr generates Nix devShells with scripts, process modules, a tmux runner, and live runtime discovery. Its built-in presets are PostgreSQL and ClickHouse. Redis requires a user-authored process module. Its default `up` command always attaches a terminal dashboard, so it needs a PTY driver in an unattended benchmark. It has no built-in `up --detach`, `down`, or `restart` CLI dispatch at this revision. The dashboard supplies restart, interrupt, and stop actions. These are source findings, not observed runtime failures.

Source inspection covered the shell generator, process wrapper, state CLI, PostgreSQL preset, tmux runner, and Rust sidebar. Historical `eval/` recipes and the current `bench/fixtures/app` only supplied workload context.

## Implementation evidence

| Source permalink | Consequence for the adapter |
| --- | --- |
| [Flake library](https://github.com/dialohq/dnvr/blob/a66c2bbabb67293812a5c39855ab0ecf6af21d41/flake.nix), [lock](https://github.com/dialohq/dnvr/blob/a66c2bbabb67293812a5c39855ab0ecf6af21d41/flake.lock) | Supports plain flakes through `lib.mkDevShells`. The inspected lock fixes Nixpkgs at `062346a6d85bc4b49dfaa61c986e9c5be21217d1`. |
| [Shell module](https://github.com/dialohq/dnvr/blob/a66c2bbabb67293812a5c39855ab0ecf6af21d41/shell-module.nix) | Computes root with `git rev-parse --show-toplevel`, otherwise cwd. State is `.dnvr` under that root. Process names namespace data/discovery, but shells within one root share process runtime directories. |
| [Process wrapping and refs](https://github.com/dialohq/dnvr/blob/a66c2bbabb67293812a5c39855ab0ecf6af21d41/shell-module.nix#L230) | Launch and pid-file locks guard duplicate starts; runtime keys are cleared before claiming the lifetime lock. Whole-value `dnvr://process/key` refs wait before the consuming process executes, with a 120-second timeout. Unknown producers, self-refs and cycles fail evaluation. |
| [State CLI](https://github.com/dialohq/dnvr/blob/a66c2bbabb67293812a5c39855ab0ecf6af21d41/dnvr-state.nix) | `get` and `wait` require a held producer pid-file lock. Default explicit wait timeout is 30 seconds. `dump` shows raw files and can include stale keys. `pick-port` closes its bound socket before returning, so allocation has a documented bind race. |
| [PostgreSQL preset](https://github.com/dialohq/dnvr/blob/a66c2bbabb67293812a5c39855ab0ecf6af21d41/presets/postgres.nix), [readiness helper](https://github.com/dialohq/dnvr/blob/a66c2bbabb67293812a5c39855ab0ecf6af21d41/presets/lib.nix) | Supplies `initdb`, durable data directories, server startup, log capture, optional extensions and once-on-database-creation SQL. Publishes `database`, `url` and `socketUrl` after readiness and database creation. Earlier `port` and `socketDir` keys do not prove readiness. The preset's own readiness poll has no deadline while the server remains alive; the adapter must bound its wait. |
| [Tmux runner](https://github.com/dialohq/dnvr/blob/a66c2bbabb67293812a5c39855ab0ecf6af21d41/runners/tmux.nix#L172), [sidebar actions](https://github.com/dialohq/dnvr/blob/a66c2bbabb67293812a5c39855ab0ecf6af21d41/runners/tmux-sidebar/src/main.rs#L221) | Starts a detached session, then unconditionally attaches. Existing sessions reattach without replacing their service panes. `r` calls `respawn-pane -k`; `x` sends Ctrl-C; `Q` kills the session. Detach leaves processes running. |

Do not assume process-local ordinary env values are private: non-ref process env is merged into the shared environment. Shell env wins at shell entry; the up script re-exports process env. Ref env values stay scoped to their consumer. Use unique variable names in process modules and read live endpoints inside app scripts.

## Supported recipe

This source-reviewed recipe uses Python 3.13, PostgreSQL 17, uv and Redis 8. It is not execution-validated. Keep the framework's inspected Nixpkgs pin for the first condition. At that revision [`pkgs.redis` is 8.8.1](https://github.com/NixOS/nixpkgs/blob/062346a6d85bc4b49dfaa61c986e9c5be21217d1/pkgs/by-name/re/redis/package.nix), and [`postgresql_17` is explicit](https://github.com/NixOS/nixpkgs/blob/062346a6d85bc4b49dfaa61c986e9c5be21217d1/pkgs/servers/sql/postgresql/default.nix). Record actual patch versions on execution. For matched package versions across contenders, point `dnvr.inputs.nixpkgs` at the common chosen immutable revision and regenerate the lock; record that as a separate condition.

Save as `flake.nix` at the application checkout root:

```nix
{
  inputs.dnvr.url =
    "github:dialohq/dnvr/a66c2bbabb67293812a5c39855ab0ecf6af21d41";
  outputs = { dnvr, ... }:
    let
      systems = [ "aarch64-linux" "x86_64-linux"
                  "aarch64-darwin" "x86_64-darwin" ];
    in {
      devShells = builtins.listToAttrs (map (system: {
        name = system;
        value = dnvr.lib.mkDevShells {
          inherit system;
          imports = [ ({ pkgs, presets, dnvrState, ... }: {
            dnvr.shells.app = {
              packages = [ pkgs.python313 pkgs.uv pkgs.redis pkgs.tmux ];
              env = {
                UV_PYTHON_DOWNLOADS = "never";
                UV_PYTHON = "${pkgs.python313}/bin/python3";
              };
              processes.pg = {
                imports = [ presets.postgres ];
                package = pkgs.postgresql_17;
                database = "rwb";
                # Independent Unix socket dirs permit both checkouts to
                # use 5432 without competing for a TCP listener.
                listenAddresses = "";
                settings.cluster_name = "dnvr-rwb";
              };
              processes.redis.command = pkgs.writeShellApplication {
                name = "rwb-redis";
                runtimeInputs = [ pkgs.redis pkgs.coreutils pkgs.gnused dnvrState ];
                text = ''
                  dir="$DNVR_ROOT/.dnvr/redis-data"
                  mkdir -p "$dir"
                  port=$(dnvr-state pick-port)
                  redis-server --bind 127.0.0.1 --port "$port" \
                    --dir "$dir" --daemonize no --appendonly no --save "" &
                  pid=$!
                  cleanup() {
                    kill -TERM "$pid" 2>/dev/null || true
                    wait "$pid" 2>/dev/null || true
                  }
                  trap cleanup EXIT
                  trap 'exit 129' HUP
                  trap 'exit 130' INT
                  trap 'exit 143' TERM
                  ready=false
                  for _ in $(seq 1 300); do
                    kill -0 "$pid" 2>/dev/null || exit 1
                    actual=$(redis-cli -h 127.0.0.1 -p "$port" \
                      INFO server 2>/dev/null | tr -d '\r' \
                      | sed -n 's/^process_id://p' || true)
                    if [ "$actual" = "$pid" ] && \
                       [ "$(redis-cli -h 127.0.0.1 -p "$port" PING)" = PONG ]; then
                      ready=true
                      break
                    fi
                    sleep 0.1
                  done
                  "$ready" || exit 1
                  dnvr-state set port "$port"
                  dnvr-state set dataDir "$dir"
                  dnvr-state set url "redis://127.0.0.1:$port/0"
                  wait "$pid"
                '';
              };
              scripts.app = {
                description = "Run the fixture against live owned services";
                runtimeInputs = [ pkgs.uv dnvrState ];
                text = ''
                  set -euo pipefail
                  cd "$DNVR_ROOT"
                  export DATABASE_URL="$(dnvr-state wait pg.socketUrl --timeout 60)"
                  export REDIS_URL="$(dnvr-state wait redis.url --timeout 60)"
                  exec uv run --frozen --no-sync python -m rwbapp "$@"
                '';
              };
              scripts.integration.text = ''
                set -euo pipefail
                cd "$DNVR_ROOT"
                export DATABASE_URL="$(dnvr-state wait pg.socketUrl --timeout 60)"
                export REDIS_URL="$(dnvr-state wait redis.url --timeout 60)"
                exec uv run --frozen --no-sync pytest -q "$@"
              '';
            };
          }) ];
        };
      }) systems);
    };
}
```

The Redis module, its identity probe, cache policy and app endpoint wrapper are benchmark-owned glue. dnvr supplies generic process execution, runtime state/locks, scripts and the port helper. PostgreSQL initialization/readiness is native. This recipe deliberately treats Redis as a rebuildable cache; switching to AOF is a separate persistence condition.

Prepare each disposable checkout as an independent Git root, or as a real Git worktree. Plain nested fixture directories inside a shared Git repository collapse to one `DNVR_ROOT` and are not independent checkouts. Add generated Nix files to the disposable checkout's index because Git flake sources exclude untracked files:

```bash
set -euo pipefail
export NIX_CONFIG='experimental-features = nix-command flakes'
git add flake.nix pyproject.toml uv.lock rwbapp migrations tests
nix flake lock
git add flake.lock
nix develop .#app --no-update-lock-file --command bash -c '
  set -euo pipefail
  python --version
  uv --version
  postgres --version
  redis-server --version
  dnvr --help
  uv sync --frozen --python "$UV_PYTHON"
'
```

Use the shared fixture `uv.lock` bytes for every tool. `uv sync` installs application dependencies; dnvr does not supply a Python dependency resolver. App commands are reproducible Nix shell entry plus the checked-in script:

```bash
nix develop .#app --no-update-lock-file --command dnvr app identity
nix develop .#app --no-update-lock-file --command dnvr app migrate
nix develop .#app --no-update-lock-file --command dnvr app mark --checkout A
nix develop .#app --no-update-lock-file --command dnvr app crud --checkout A
nix develop .#app --no-update-lock-file --command dnvr app cache --checkout A
nix develop .#app --no-update-lock-file --command dnvr integration
```

## Noninteractive runner and cleanup

`nix develop .#app -c dnvr up` on pipe-only stdin is not a supported detached-start command. Source creates service panes before `attach-session`, so an attachment error may coexist with running services. Never treat its exit code alone as proof of successful startup or complete rollback.

Implement a recorded Python `pty.fork` driver in the adapter. Its child execs `nix develop .#app --no-update-lock-file --command dnvr up`; its parent drains and saves the PTY stream while a separate shell waits for `pg.socketUrl`, `redis.url` and fixture identity. After readiness, detach the client through the exact owned socket using:

```bash
nix develop .#app --no-update-lock-file --command bash -c '
  set -euo pipefail
  socket="$DNVR_STATE/runtime/tmux-app-up.sock"
  tmux -S "$socket" detach-client -s =dnvr
'
```

The PTY driver must have an overall startup deadline, collect the child exit, and run scoped cleanup on timeout/error. Keep PTY overhead in startup measurements. A custom process-compose or headless runner is an extension condition named `dnvr + custom runner`, with that runner's code counted as user work. It is not evidence for default-runner behavior.

For graceful teardown, enumerate process panes under that socket with `list-panes -s -t =dnvr -F '#{@dnvr_role} #{pane_id}'`, send `C-c` only to rows with role `process`, then poll `dnvr ps` until each service's lock is released. Query fixture identity separately to confirm endpoints refuse. Finally remove the dashboard using `tmux -S "$socket" kill-session -t =dnvr`. This scripted stop confirmation is required for the unattended condition. The dashboard's native `Q` action only calls `kill-session`; source inspection does not establish that every grandchild is reaped. The PostgreSQL preset handles EXIT/INT/TERM but has no explicit HUP trap, so verify postgres descendants and endpoints after killing a session.

Restart after that cleanup with the same PTY-start workflow. PostgreSQL's existing data directory survives. Redis restarts empty under this recipe. Do not delete `.dnvr` while any process lock is held. Remove only the benchmark's own checkout directory after verifying stopped processes. Never use default-socket `tmux kill-server` or global process matching.

Repeated `dnvr up` reattaches the existing session and does not rebuild running services. A changed configuration requires an explicit stop/restart before claiming the services use its new Nix closure. The app script resolves endpoints on every invocation; process refs resolve once before process exec and do not update an already running process's environment after a producer restart.

Shell entry can print a gum banner and writes a per-shell banner stamp under `.dnvr`. Retain complete shell output and extract the fixture JSON explicitly; do not interpret the first entry's banner as malformed application JSON. Record first activation separately from repeated entries.

## Fair tasks and evidence checks

| Task | Mode and evidence |
| --- | --- |
| Locked setup and copy | Native Nix lock plus shared uv lock. Copy source, config and locks into C, exclude `.dnvr` and `.venv`, add copied files to the index, use `--no-update-lock-file` and `uv sync --frozen`, compare lock hashes. |
| Startup/readiness | Native PostgreSQL preset, scripted Redis readiness, scripted PTY driver. Require fixture `identity`, not session creation, `dnvr ps`, early `socketDir` publication or raw `dump`. |
| Two checkouts | Native root-scoped state and socket dirs; native port helper used by scripted Redis. Compare source tokens, PG `data_directory` and `system_identifier`, Redis `run_id`, `dir` and port. A socket PG connection reports null `inet_server_port`; this is valid and must not fail a TCP-only identity assertion. |
| Migration, CRUD, cache and integration tests | Execute the same shared fixture via `dnvr app`/`dnvr integration`, verify JSON and migration versions. Set A/B markers, then prove each sees only its own marker. Distinct Redis logical databases alone are not independent Redis services. |
| Repeated commands | Measure fresh `nix develop -c true` and fresh `nix develop -c dnvr app read --checkout A` separately. Also allow a held-shell condition, labelled separately. Avoid running `uv sync` on each read. |
| Stop A while B lives | Scoped scripted teardown. Verify B's source/service identities remain unchanged and A's recorded endpoints refuse. Check process locks and OS children before data deletion. |
| Restart and persistence | PG markers and rows survive, PG data directory/system identifier stay the same, postmaster start changes. Redis run ID changes and application cache rebuild succeeds. |
| Invalid config/occupied port | Unknown package and invalid/cyclic refs should fail evaluation before launch. Redis pick-port is best-effort; validate launched identity and bounded failure on bind loss. A PG occupied-TCP-port scenario is not applicable to the socket-only condition. Use an explicit different PG TCP port per checkout for a separate TCP condition. |

The local macOS host has no `nix` executable. Read-only Docker image inspection found `ev-nix:latest`, ID `sha256:fbf87c18aced2d3c969df40d15b137943429882f4283fe256c2f481a7af17c6a`, arm64 Linux, created 2026-10-02. There is no dnvr image/tool version receipt here. `eval/images/Dockerfile.nix` installs Determinate Nix from an unpinned installer, so record the actual Nix distribution/version and image ID at run time. Existing images are availability evidence, not fresh results.

The framework and inspected packages target macOS/Linux through Nix. The supplied recipe declares both CPU architectures on both systems, but build availability and Python binary-wheel runtime compatibility need execution. Linux containers must have a usable Nix installation/store and PTY support. Run PostgreSQL as a non-root user, or use the preset's explicit `runAsUser` Linux root path and record the user-creation/permission changes. macOS normally uses the invoking non-root user. Keep socket paths short enough for OS Unix-socket limits. A host installation benchmark and a Linux-container benchmark are separate environments.
