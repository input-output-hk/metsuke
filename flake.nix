{
  description = "metsuke: telemetry agent and ingest server for the MusashiNet rewards program";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";
    treefmt-nix = {
      url = "github:numtide/treefmt-nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    git-hooks = {
      url = "github:cachix/git-hooks.nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane.url = "github:ipetkov/crane";
    advisory-db = {
      url = "github:rustsec/advisory-db";
      flake = false;
    };
  };

  outputs =
    inputs@{ flake-parts, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];

      imports = [
        inputs.treefmt-nix.flakeModule
        inputs.git-hooks.flakeModule
      ];

      perSystem =
        {
          config,
          pkgs,
          ...
        }:
        let
          craneLib = inputs.crane.mkLib pkgs;
          src = craneLib.cleanCargoSource ./.;

          commonArgs = {
            inherit src;
            strictDeps = true;
          };

          cargoArtifacts = craneLib.buildDepsOnly commonArgs;
        in
        {
          packages = {
            metsuke = craneLib.buildPackage (
              commonArgs
              // {
                inherit cargoArtifacts;
                cargoExtraArgs = "--package metsuke";
              }
            );
            metsuke-server = craneLib.buildPackage (
              commonArgs
              // {
                inherit cargoArtifacts;
                cargoExtraArgs = "--package metsuke-server";
              }
            );
            default = config.packages.metsuke;
          };

          checks = {
            inherit (config.packages) metsuke metsuke-server;

            clippy = craneLib.cargoClippy (
              commonArgs
              // {
                inherit cargoArtifacts;
                cargoClippyExtraArgs = "--all-targets -- --deny warnings";
              }
            );

            test = craneLib.cargoTest (commonArgs // { inherit cargoArtifacts; });

            audit = craneLib.cargoAudit {
              inherit src;
              inherit (inputs) advisory-db;
            };

            deny = craneLib.cargoDeny { inherit src; };
          };

          treefmt = {
            projectRootFile = "flake.nix";
            programs = {
              rustfmt.enable = true;
              nixfmt.enable = true;
              taplo.enable = true;
              deadnix.enable = true;
              statix.enable = true;
            };
          };

          # The sandboxed hook run cannot vendor crates for clippy; checks.clippy
          # and checks.treefmt already cover both hooks hermetically.
          pre-commit.check.enable = false;
          pre-commit.settings.hooks = {
            treefmt.enable = true;
            clippy = {
              enable = true;
              packageOverrides = {
                inherit (pkgs) cargo clippy;
              };
              settings.denyWarnings = true;
            };
          };

          devShells.default = craneLib.devShell {
            inherit (config) checks;
            packages = [
              pkgs.cargo-audit
              pkgs.cargo-deny
              config.treefmt.build.wrapper
            ];
            shellHook = config.pre-commit.installationScript;
          };
        };
    };
}
