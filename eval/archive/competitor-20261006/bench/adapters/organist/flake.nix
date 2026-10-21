{
  # Organist has no current release (only the obsolete 2023 v0.1 tag); pin current main.
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/151fa4e8ddfdd8dd25d945ad94ed54a13de9f6e4";
  inputs.organist.url = "github:nickel-lang/organist/a7e4e638cade5e7c4f36a129b80d91bf3538088e";

  outputs = { organist, ... } @ inputs:
    organist.flake.outputsFromNickel ./. inputs {};
}
