# services-flake + process-compose-flake (bench/research/services-flake.md).
# Native: PostgreSQL/Redis service modules (initdb, pg_isready / PING probes, restart policy,
# fast shutdown), Process Compose supervision and JSON status, flake.lock pinning.
# Project configuration: the per-checkout port pair, cluster name and control socket come
# from the uncommitted ./local.json written by the benchmark (services-flake has no port
# allocator). Data lives under the caller's CWD (the checkout root) in .rwb-state/sf.
{
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
        local = builtins.fromJSON (builtins.readFile ./local.json);
      in {
        process-compose.services = {
          imports = [ inputs.services-flake.processComposeModules.default ];
          cli.preHook = "mkdir -p .rwb-state/sf ${builtins.dirOf local.socket}";
          cli.environment = {
            PC_DISABLE_TUI = true;
            PC_DISABLE_DOTENV = true;
          };
          cli.options = {
            # services-flake defaults no-server to true; detached up and every client
            # command (list/down/is-ready) need the API, on a socket unique to this checkout.
            no-server = false;
            use-uds = true;
            unix-socket = local.socket;
            log-file = ".rwb-state/sf/process-compose.log";
          };
          # Process output (incl. PostgreSQL/Redis startup errors) is kept only in memory unless
          # the project sets log_location; relative to the caller's CWD (the checkout root).
          settings.log_location = ".rwb-state/sf/processes.log";
          services.postgres.pg = {
            enable = true;
            package = pkgs.postgresql_17;
            port = local.pg;
            listen_addresses = "127.0.0.1";
            socketDir = "";
            superuser = "postgres";
            dataDir = "./.rwb-state/sf/pg";
            settings.cluster_name = local.instance;
          };
          services.redis.rd = {
            enable = true;
            package = pkgs.redis;
            port = local.redis;
            bind = "127.0.0.1";
            dataDir = "./.rwb-state/sf/redis";
            # Common durability policy across lanes.
            extraConfig = ''
              appendonly yes
              appendfsync always
            '';
          };
        };
        devShells.default = pkgs.mkShellNoCC {
          inputsFrom = [ config.process-compose.services.services.outputs.devShell ];
          packages = [ pkgs.python313 pkgs.uv self'.packages.services ];
          UV_PYTHON_DOWNLOADS = "never";
          UV_PYTHON = "${pkgs.python313}/bin/python3";
          DATABASE_URL = "postgresql://postgres@127.0.0.1:${toString local.pg}/postgres";
          REDIS_URL = "redis://127.0.0.1:${toString local.redis}/0";
        };
      };
  };
}
