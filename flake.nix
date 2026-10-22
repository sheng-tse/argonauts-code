# Builds Argonauts Code from source with Nix: `nix run github:sheng-tse/argonauts-code`, or
# `nix profile install github:sheng-tse/argonauts-code`. Only the argonauts binary is built, and the test suite
# isn't run, since it needs the agent CLIs' stand-ins and a terminal.
{
  description = "Orchestrate any mix of coding agents and models, from Claude Code and Codex to open-weight models on your own machine";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "aarch64-darwin"
        "x86_64-darwin"
        "x86_64-linux"
        "aarch64-linux"
      ];
      forEachSystem = build: nixpkgs.lib.genAttrs systems (system: build nixpkgs.legacyPackages.${system});
      workspace = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package;
    in
    {
      packages = forEachSystem (pkgs: rec {
        argonauts-code = pkgs.rustPlatform.buildRustPackage {
          pname = "argonauts-code";
          inherit (workspace) version;
          src = pkgs.lib.cleanSource ./.;
          cargoLock.lockFile = ./Cargo.lock;
          cargoBuildFlags = [
            "--package"
            "argonauts-code"
          ];
          doCheck = false;
          meta = {
            description = "Orchestrate any mix of coding agents and models";
            homepage = "https://github.com/sheng-tse/argonauts-code";
            license = pkgs.lib.licenses.asl20;
            mainProgram = "argonauts";
            platforms = systems;
          };
        };
        default = argonauts-code;
      });
    };
}
