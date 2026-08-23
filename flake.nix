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
    # Leios prototype the MusashiNet nodes run (same pin as cardano-playground).
    # Source only, never evaluated as a flake: scripts/record-scrape-fixtures.sh
    # records scrape fixtures from it, and the VM test will run it.
    cardano-node-leios = {
      url = "github:input-output-hk/ouroboros-leios?ref=refs/tags/prototype-2026w32";
      flake = false;
    };
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
          # Cargo sources plus the scrape fixtures include_str! compiles in.
          src = pkgs.lib.cleanSourceWith {
            src = ./.;
            filter = path: type: (craneLib.filterCargoSources path type) || pkgs.lib.hasSuffix ".prom" path;
            name = "source";
          };

          # Names the workspace derivations, and nothing else. The crates are
          # versioned apart (ADR 0006), so there is no
          # workspace.package.version for crane to find; passing this to every
          # crane call is what keeps it from warning about the absence.
          version = "0";

          commonArgs = {
            inherit src version;
            strictDeps = true;
          };

          cargoArtifacts = craneLib.buildDepsOnly commonArgs;

          # Builds a binary from a tree holding only the crates it compiles, so
          # an edit elsewhere in the workspace does not invalidate it. The
          # wildcard workspace.members in Cargo.toml is what lets a crate be
          # absent; a member cargo can see but whose sources are missing is an
          # error, so a crate is either whole here or gone.
          crateSrc =
            crates:
            pkgs.lib.fileset.toSource {
              root = ./.;
              fileset = pkgs.lib.fileset.unions (
                [
                  ./Cargo.toml
                  ./Cargo.lock
                ]
                ++ map craneLib.fileset.commonCargoSources crates
              );
            };

          # Tests run once, in checks.test. Crane defaults doCheck to true,
          # which would run the suite again inside each binary.
          binaryArgs = {
            inherit cargoArtifacts version;
            strictDeps = true;
            doCheck = false;
          };

          testAlone =
            crate:
            craneLib.cargoTest (
              commonArgs
              // {
                inherit cargoArtifacts;
                pname = "${crate}-alone";
                cargoExtraArgs = "--package ${crate}";
              }
            );
        in
        {
          packages = {
            metsuke = craneLib.buildPackage (
              binaryArgs
              // {
                src = crateSrc [
                  ./crates/metsuke-wire
                  ./crates/metsuke
                ];
                cargoExtraArgs = "--package metsuke";
              }
            );
            metsuke-server = craneLib.buildPackage (
              binaryArgs
              // {
                # build.rs reads the agent manifest for CLIENT_VERSION, so the
                # agent crate has to be here in full even though nothing links
                # against it.
                src = crateSrc [
                  ./crates/metsuke-wire
                  ./crates/metsuke
                  ./crates/metsuke-server
                ];
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

            # The workspace run unifies dependency features across members, so
            # a feature one binary must carry on its own can be supplied by the
            # other and still pass there (ticket metsuke-b4r).
            test-agent = testAlone "metsuke";
            test-server = testAlone "metsuke-server";

            audit = craneLib.cargoAudit {
              inherit src;
              inherit (inputs) advisory-db;
            };

            deny = craneLib.cargoDeny { inherit src version; };
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
