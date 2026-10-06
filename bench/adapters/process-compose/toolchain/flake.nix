# Toolchain for the Process Compose lane (bench/research/process-compose.md).
# Process Compose installs nothing, so Nix supplies Python/uv/PostgreSQL/Redis at the
# nixpkgs revision shared with the plain Nix lane. Kept in its own directory so path-flake
# evaluation never copies checkout state (.venv, .rwb-state) into the store.
{
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/151fa4e8ddfdd8dd25d945ad94ed54a13de9f6e4";
  outputs = { self, nixpkgs }:
    let systems = [ "aarch64-linux" "x86_64-linux" "aarch64-darwin" "x86_64-darwin" ];
    in {
      devShells = nixpkgs.lib.genAttrs systems (system:
        let pkgs = nixpkgs.legacyPackages.${system};
        in {
          default = pkgs.mkShellNoCC {
            packages = [ pkgs.python313 pkgs.uv pkgs.postgresql_17 pkgs.redis pkgs.bash ];
            UV_PYTHON = "${pkgs.python313}/bin/python3";
            UV_PYTHON_DOWNLOADS = "never";
          };
        });
    };
}
