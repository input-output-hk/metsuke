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
    inputs@{ self, flake-parts, ... }:
    let
      inherit (inputs.nixpkgs) lib;
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
    in
    flake-parts.lib.mkFlake { inherit inputs; } {
      inherit systems;

      imports = [
        inputs.treefmt-nix.flakeModule
        inputs.git-hooks.flakeModule
      ];

      # A hydraJob, not a check: runNixOSTest wants /dev/kvm, and `nix flake
      # check` has to stay runnable wherever the crates build. The end-to-end
      # test (ticket metsuke-4zo.14) lands beside it for the same reason.
      flake.hydraJobs.units = lib.genAttrs systems (
        system:
        import ./nix/unit-test.nix {
          pkgs = inputs.nixpkgs.legacyPackages.${system};
          agentModule = self.nixosModules.metsuke;
          serverModule = self.nixosModules.metsuke-server;
          metrics = ./crates/metsuke/tests/fixtures/recordings/leios-node.prom;
          contribUnit = ./contrib/metsuke.service;
          agent = self.packages.${system}.metsuke;
        }
      );

      flake.nixosModules = {
        metsuke =
          { lib, pkgs, ... }:
          {
            imports = [ ./nix/agent-module.nix ];
            services.metsuke.package = lib.mkDefault self.packages.${pkgs.stdenv.hostPlatform.system}.metsuke;
          };
        metsuke-server =
          { lib, pkgs, ... }:
          {
            imports = [ ./nix/server-module.nix ];
            services.metsuke-server.package =
              lib.mkDefault
                self.packages.${pkgs.stdenv.hostPlatform.system}.metsuke-server;
          };
      };

      perSystem =
        {
          config,
          pkgs,
          ...
        }:
        let
          craneLib = inputs.crane.mkLib pkgs;
          # Cargo sources, the shipped SQL, the fixtures and the test doubles:
          # the scrape bodies and the SQL are compiled in with include_str!,
          # the CIP-151 recordings and the psql double are run at test time.
          #
          # Under crates/ alone, because this filter is not gitignore-aware: a
          # devnet run leaves .hex and .csv files in the working tree, and
          # matching those by suffix anywhere would re-hash every derivation.
          extraSources = [
            ".prom"
            ".sql"
            ".hex"
            ".csv"
            ".sh"
          ];
          cratesDir = "${toString ./crates}/";
          src = pkgs.lib.cleanSourceWith {
            src = ./.;
            filter =
              path: type:
              (craneLib.filterCargoSources path type)
              || (
                pkgs.lib.hasPrefix cratesDir path
                && pkgs.lib.any (suffix: pkgs.lib.hasSuffix suffix path) extraSources
              );
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

          agentSrc = crateSrc [
            ./crates/metsuke-wire
            ./crates/metsuke
          ];

          agentArgs = binaryArgs // {
            src = agentSrc;
            cargoExtraArgs = "--package metsuke";
          };

          # The agent as one file an operator drops on a host that is not
          # NixOS. Cross rather than the native toolchain even for this
          # system's own architecture: musl is a different libc, so both
          # targets are the same code path.
          staticAgent =
            crossPkgs:
            let
              crossCrane = inputs.crane.mkLib crossPkgs;
              # This toolchain has to compile the dependencies again, so the
              # native artifacts come out.
              staticArgs = removeAttrs agentArgs [ "cargoArtifacts" ] // {
                # nixpkgs links its musl targets dynamically, so rustc's own
                # musl default is turned off before it reaches here. Asking
                # for it back is what leaves no interpreter in the binary.
                CARGO_BUILD_RUSTFLAGS = "-C target-feature=+crt-static";
              };
            in
            crossCrane.buildPackage (
              staticArgs
              // {
                cargoArtifacts = crossCrane.buildDepsOnly staticArgs;
              }
            );

          unit = import ./nix/unit.nix;

          # One line per list element, as NixOS renders too: SystemCallFilter's
          # leading `~` inverts the line it opens, so a joined list would deny
          # nothing.
          directives = pkgs.lib.generators.toKeyValue { listsAsDuplicateKeys = true; };

          contribUnit = pkgs.writeText "metsuke.service" ''
            # Example hardened unit for a host that is not NixOS. Generated:
            # edit nix/unit.nix, then `nix build .#metsuke-unit` and commit
            # what it wrote here.
            #
            # Copy to /etc/systemd/system/metsuke.service, with the binary at
            # /usr/local/bin/metsuke and the configuration at
            # /etc/metsuke/config.toml.
            #
            # Optional, and what the NixOS module does: keep the signing key
            # unreadable to the service user by loading it as a credential. Add
            #   LoadCredential=signing-key:/etc/metsuke/pool.skey
            # append
            #   --signing-key ''${CREDENTIALS_DIRECTORY}/signing-key
            # to ExecStart, and leave signing_key out of config.toml.

            [Unit]
            Description=metsuke telemetry agent
            After=network-online.target
            Wants=network-online.target

            [Service]
            ExecStart=/usr/local/bin/metsuke --config /etc/metsuke/config.toml
            Restart=always
            RestartSec=${toString unit.restartSecs}
            ${directives (
              unit.hardening {
                stateDirectory = "metsuke";
                addressFamilies = unit.agentAddressFamilies;
              }
            )}
            [Install]
            WantedBy=multi-user.target
          '';

          # What "static" has to mean for the operator dropping this on a host
          # whose libc is not ours: no interpreter to find and nothing to load.
          # readelf reads any architecture, so one derivation covers both.
          linksNothing =
            agent:
            pkgs.runCommand "${agent.name}-links-nothing"
              {
                nativeBuildInputs = [ pkgs.binutils ];
              }
              ''
                readelf --program-headers --dynamic ${agent}/bin/metsuke > sections
                if grep -Eq 'INTERP|NEEDED' sections; then
                  echo "${agent}/bin/metsuke is not static:"
                  grep -E 'INTERP|NEEDED' sections
                  exit 1
                fi
                touch $out
              '';
        in
        {
          packages = {
            metsuke = craneLib.buildPackage agentArgs;
            metsuke-unit = contribUnit;
            metsuke-static-x86_64-linux = staticAgent pkgs.pkgsCross.musl64;
            metsuke-static-aarch64-linux = staticAgent pkgs.pkgsCross.aarch64-multiplatform-musl;
            metsuke-server = craneLib.buildPackage (
              binaryArgs
              // {
                # build.rs reads the agent manifest for CLIENT_VERSION, so the
                # agent crate has to be here in full even though nothing links
                # against it.
                src = pkgs.lib.fileset.toSource {
                  root = ./.;
                  fileset =
                    pkgs.lib.fileset.union
                      (pkgs.lib.fileset.fromSource (crateSrc [
                        ./crates/metsuke-wire
                        ./crates/metsuke
                        ./crates/metsuke-server
                      ]))
                      # include_str!'d, so cargo sources alone do not build.
                      ./crates/metsuke-server/src/registrations.sql;
                };
                cargoExtraArgs = "--package metsuke-server";
              }
            );
            default = config.packages.metsuke;
          };

          checks = {
            inherit (config.packages) metsuke metsuke-server;

            static-x86_64-linux = linksNothing config.packages.metsuke-static-x86_64-linux;
            static-aarch64-linux = linksNothing config.packages.metsuke-static-aarch64-linux;

            contrib-unit = pkgs.runCommand "contrib-unit-is-current" { } ''
              diff -u ${./contrib/metsuke.service} ${contribUnit} \
                || { echo "contrib/metsuke.service is stale; its header says how"; exit 1; }
              touch $out
            '';

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
