# dnvr (dialohq/dnvr a66c2bb, untagged) for the shared fixture (bench/research/dnvr.md).
# Native: devShell generation, the PostgreSQL preset (initdb, readiness, `url` published only
# once the server accepts connections), per-process runtime state with flock liveness,
# `dnvr ps`, and the persistent tmux runner (`dnvr up`).
# Benchmark-owned: the Redis process module (dnvr has no Redis preset) and its readiness
# publication, the per-checkout port pair from the uncommitted ./local.json, and the PTY
# driver/stop scripts at the checkout root (rwb-dnvr.sh).
# dnvr.inputs.nixpkgs follows the nixpkgs revision shared by every Nix lane, so package
# versions match those lanes; dnvr's own lock pins 062346a6 instead (recorded deviation).
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/151fa4e8ddfdd8dd25d945ad94ed54a13de9f6e4";
    dnvr.url = "github:dialohq/dnvr/a66c2bbabb67293812a5c39855ab0ecf6af21d41";
    dnvr.inputs.nixpkgs.follows = "nixpkgs";
  };
  outputs = { dnvr, ... }:
    let
      systems = [ "aarch64-linux" "x86_64-linux" "aarch64-darwin" "x86_64-darwin" ];
      local = builtins.fromJSON (builtins.readFile ./local.json);
    in {
      devShells = builtins.listToAttrs (map (system: {
        name = system;
        value = dnvr.lib.mkDevShells {
          inherit system;
          imports = [ ({ pkgs, presets, dnvrState, ... }: {
            dnvr.shells.rwb = { config, ... }: {
              description = "rwb fixture: postgres 17 + redis";
              # pkgs.redis: the same package as rwb-redis's runtime input, on PATH so the
              # requested `redis-server --version` receipt can run in the devshell.
              packages = [ pkgs.python313 pkgs.uv pkgs.redis pkgs.tmux pkgs.util-linux ];
              env = {
                UV_PYTHON_DOWNLOADS = "never";
                UV_PYTHON = "${pkgs.python313}/bin/python3";
                # Eval-static endpoints (dnvr's "static values" tier): usable from any
                # shell entry, refused while the group is down.
                DATABASE_URL = config.processes.pg.url;
                REDIS_URL = "redis://127.0.0.1:${toString local.redis}/0";
              };
              processes.pg = {
                imports = [ presets.postgres ];
                package = pkgs.postgresql_17;
                database = "postgres";
                port = local.pg;
                listenAddresses = "127.0.0.1";
                initdbArgs = [ "--auth=trust" "--encoding=UTF8" "--locale=C" ];
                settings.cluster_name = local.instance;
              };
              processes.redis.command = pkgs.writeShellApplication {
                name = "rwb-redis";
                runtimeInputs = [ pkgs.redis pkgs.coreutils pkgs.gnused dnvrState ];
                text = ''
                  dir="$DNVR_ROOT/.dnvr/redis-data"
                  port=${toString local.redis}
                  mkdir -p "$dir"
                  redis-server --bind 127.0.0.1 --port "$port" --dir "$dir" \
                    --daemonize no --appendonly yes --appendfsync always &
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
                    actual=$(redis-cli -h 127.0.0.1 -p "$port" INFO server 2>/dev/null \
                      | tr -d '\r' | sed -n 's/^process_id://p' || true)
                    if [ "$actual" = "$pid" ]; then
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
            };
          }) ];
        };
      }) systems);
    };
}
