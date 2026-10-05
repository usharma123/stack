{
  description = "Comparison toolchain; plain Nix has no built-in service supervisor";
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  outputs = { self, nixpkgs }: let
    system = "aarch64-linux";
    pkgs = nixpkgs.legacyPackages.${system};
  in {
    devShells.${system}.default = pkgs.mkShell {
      packages = [ pkgs.python313 pkgs.uv pkgs.postgresql_17 pkgs.redis ];
      UV_PYTHON_DOWNLOADS = "never";
    };
  };
}
