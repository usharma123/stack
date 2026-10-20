# Plain Nix development shell (bench/research/nix.md): pinned toolchain only. Nix has no
# service supervisor; PostgreSQL/Redis are started by the benchmark's scripted glue
# (_shared/rwb-services.sh, pg_ctl + redis-server) inside this shell. Kept in its own
# directory so path-flake evaluation never copies checkout state (.venv, .rwb-state).
{
  description = "rwb: Python 3.13, uv, PostgreSQL 17, Redis from one pinned nixpkgs";
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/151fa4e8ddfdd8dd25d945ad94ed54a13de9f6e4";
  outputs = { self, nixpkgs }:
    let systems = [ "aarch64-linux" "x86_64-linux" "aarch64-darwin" "x86_64-darwin" ];
    in {
      devShells = nixpkgs.lib.genAttrs systems (system:
        let pkgs = nixpkgs.legacyPackages.${system};
        in {
          default = pkgs.mkShellNoCC {
            packages = [ pkgs.python313 pkgs.uv pkgs.postgresql_17 pkgs.redis ];
            UV_PYTHON = "${pkgs.python313}/bin/python3";
            UV_PYTHON_DOWNLOADS = "never";
          };
        });
    };
}
