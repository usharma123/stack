let inputs = import "./nickel.lock.ncl" in
let organist = inputs.organist in
let pkg = organist.import_nix in

organist.OrganistExpression
& organist.services
& {
  Schema,
  config | Schema
    = {
      shells = organist.shells.Bash,

      shells.build.packages = {
        python = pkg "nixpkgs#python313",
        uv = pkg "nixpkgs#uv",
        postgres = pkg "nixpkgs#postgresql_17",
        redis = pkg "nixpkgs#redis",
      },
      shells.build.env.UV_PYTHON_DOWNLOADS = "never",

      # Shell entry must not write files; nickel.lock.ncl is regenerated explicitly at setup.
      filegen_hook.enable = false,

      # Native Organist services: compiled into a Procfile run by Honcho
      # (`nix run .#start-services -- start`). The launcher script is benchmark glue.
      services = {
        postgres = nix-s%"exec %{pkg "nixpkgs#bash"}/bin/bash ./organist-services.sh postgres %{pkg "nixpkgs#postgresql_17"}/bin"%,
        redis = nix-s%"exec %{pkg "nixpkgs#bash"}/bin/bash ./organist-services.sh redis %{pkg "nixpkgs#redis"}/bin"%,
      },
    },
}
  | organist.modules.T
