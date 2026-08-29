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

      flake.hydraJobs = {
        # A hydraJob, not a check: runNixOSTest wants /dev/kvm, and `nix flake
        # check` has to stay runnable wherever the crates build. The end-to-end
        # job is devnet/flake.nix's, which is where the node it needs is pinned.
        units = lib.genAttrs systems (
          system:
          import ./nix/unit-test.nix {
            pkgs = inputs.nixpkgs.legacyPackages.${system};
            agentModule = self.nixosModules.metsuke;
            serverModule = self.nixosModules.metsuke-server;
            metrics = ./crates/metsuke/tests/fixtures/recordings/leios-node.prom;
            traces = ./crates/metsuke/tests/fixtures/recordings/leios-node-traces.log;
            contribUnit = ./contrib/metsuke.service;
            agent = self.packages.${system}.metsuke;
          }
        );

        packages = lib.genAttrs systems (system: removeAttrs self.packages.${system} [ "default" ]);
        checks = lib.genAttrs systems (system: self.checks.${system});
        devShells = lib.genAttrs systems (system: self.devShells.${system});
      };

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
          # Cargo sources and the fixtures: the scrape bodies and the trace
          # recordings are compiled in with include_str!, the submission
          # recordings and the S3 cassette are read at test time.
          #
          # Under crates/ alone, because this filter is not gitignore-aware: a
          # devnet run leaves .hex files in the working tree, and matching
          # those by suffix anywhere would re-hash every derivation.
          extraSources = [
            ".prom"
            ".log"
            ".hex"
            ".http"
          ];
          cratesDir = "${toString ./crates}/";
          # The shipped config and unit, which both crates compile in whole:
          # the agent's config test and the server's instructions page.
          contribDir = "${toString ./contrib}/";
          src = pkgs.lib.cleanSourceWith {
            src = ./.;
            filter =
              path: type:
              (craneLib.filterCargoSources path type)
              || pkgs.lib.hasPrefix contribDir path
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

          # What the suites reach for beside the crates: `checks.test` says why
          # each is here. Every way the suite is run needs them: the sandboxed
          # checks, a per-crate run, and `just test` in the devShell.
          suiteTools = [
            pkgs.duckdb
            pkgs.zstd
          ];

          testAlone =
            crate:
            craneLib.cargoTest (
              commonArgs
              // {
                inherit cargoArtifacts;
                pname = "${crate}-alone";
                cargoExtraArgs = "--package ${crate}";
                nativeCheckInputs = suiteTools;
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

          # The server's tree. build.rs reads the agent manifest for
          # CLIENT_VERSION, so the agent crate has to be here in full even
          # though nothing links against it; the two contrib files are carried
          # whole into the instructions page with include_str!, so cargo
          # sources alone do not build.
          serverFileset = pkgs.lib.fileset.unions [
            (pkgs.lib.fileset.fromSource (crateSrc [
              ./crates/metsuke-wire
              ./crates/metsuke
              ./crates/metsuke-server
            ]))
            ./contrib/config.example.toml
            ./contrib/metsuke.service
          ];

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
                inherit (unit) addressFamilies;
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
            metsuke-allowlist = (import ./nix/allowlist.nix { inherit pkgs; }).package;
            # The developer's pull tool, which links the wire crate alone. The
            # server tree is here because cargo loads every workspace member's
            # manifest, and this crate dev-depends on the server: sources it
            # cannot find are an error before anything is built.
            metsuke-fetch = craneLib.buildPackage (
              binaryArgs
              // {
                src = pkgs.lib.fileset.toSource {
                  root = ./.;
                  fileset = pkgs.lib.fileset.unions [
                    serverFileset
                    (pkgs.lib.fileset.fromSource (crateSrc [ ./crates/metsuke-fetch ]))
                  ];
                };
                cargoExtraArgs = "--package metsuke-fetch";
              }
            );
            metsuke-static-x86_64-linux = staticAgent pkgs.pkgsCross.musl64;
            metsuke-static-aarch64-linux = staticAgent pkgs.pkgsCross.aarch64-multiplatform-musl;
            metsuke-server = craneLib.buildPackage (
              binaryArgs
              // {
                src = pkgs.lib.fileset.toSource {
                  root = ./.;
                  fileset = serverFileset;
                };
                cargoExtraArgs = "--package metsuke-server";
              }
            );
            default = config.packages.metsuke;
          };

          checks = {
            inherit (config.packages) metsuke metsuke-server metsuke-fetch;

            allowlist = (import ./nix/allowlist.nix { inherit pkgs; }).tests;

            server-config = import ./nix/server-config-test.nix {
              inherit pkgs;
              serverModule = self.nixosModules.metsuke-server;
              server = config.packages.metsuke-server;
            };

            static-x86_64-linux = linksNothing config.packages.metsuke-static-x86_64-linux;
            static-aarch64-linux = linksNothing config.packages.metsuke-static-aarch64-linux;

            # Two locks name the same Leios tag: this one is what
            # scripts/record-scrape-fixtures.sh records against, devnet's is
            # what the devnet and the end-to-end job run. Bumping one alone
            # would leave the fixtures describing a node no test runs.
            leios-pin =
              let
                devnet = (lib.importJSON ./devnet/flake.lock).nodes.leios.locked.rev;
                here = inputs.cardano-node-leios.rev;
              in
              pkgs.runCommand "leios-pins-agree" { } ''
                [ "${here}" = "${devnet}" ] || {
                  echo "flake.lock pins leios ${here}; devnet/flake.lock pins ${devnet}"
                  exit 1
                }
                touch $out
              '';

            contrib-unit = pkgs.runCommand "contrib-unit-is-current" { } ''
              diff -u ${./contrib/metsuke.service} ${contribUnit} \
                || { echo "contrib/metsuke.service is stale; its header says how"; exit 1; }
              touch $out
            '';

            # The instructions page tells an operator to build these by name,
            # and nothing in the Rust tree can see whether they still exist.
            instructions-outputs = pkgs.runCommand "instructions-name-real-outputs" { } ''
              page=${./crates/metsuke-server/src/instructions.rs}
              # Each grep is asserted non-empty first: a rename that also
              # reflowed the literal would otherwise leave a loop over nothing.
              # `|| true`: a grep that matches nothing exits 1, and under
              # `set -o pipefail` that would abort with an empty log instead of
              # the message below.
              packages=$(grep -o 'metsuke-static-[a-z0-9_-]*' $page | sort -u || true)
              modules=$(grep -o 'nixosModules\.[a-z-]*' $page | cut -d. -f2 | sort -u || true)
              [ -n "$packages" ] || { echo "instructions.rs offers no build to run"; exit 1; }
              [ -n "$modules" ] || { echo "instructions.rs points at no module"; exit 1; }
              for name in $packages; do
                case " ${toString (builtins.attrNames config.packages)} " in
                  *" $name "*) ;;
                  *)
                    echo "instructions.rs offers $name, which this flake does not build"
                    exit 1
                    ;;
                esac
              done
              for name in $modules; do
                case " ${toString (builtins.attrNames self.nixosModules)} " in
                  *" $name "*) ;;
                  *)
                    echo "instructions.rs points at nixosModules.$name, which does not exist"
                    exit 1
                    ;;
                esac
              done
              touch $out
            '';

            clippy = craneLib.cargoClippy (
              commonArgs
              // {
                inherit cargoArtifacts;
                cargoClippyExtraArgs = "--all-targets -- --deny warnings";
              }
            );

            # zstd: the wire suite asserts a recording decompresses through the
            # real CLI, because the claim is about conforming decompressors
            # rather than about the crate. duckdb: the fetch suite asserts a
            # synced object is read where it landed, which is a claim about
            # duckdb and not about the container.
            test = craneLib.cargoTest (
              commonArgs
              // {
                inherit cargoArtifacts;
                nativeCheckInputs = suiteTools;
              }
            );

            # The workspace run unifies dependency features across members, so
            # a feature one binary must carry on its own can be supplied by the
            # other and still pass there (ticket metsuke-b4r).
            test-agent = testAlone "metsuke";
            test-server = testAlone "metsuke-server";
            test-fetch = testAlone "metsuke-fetch";

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
            packages = suiteTools ++ [
              pkgs.cargo-audit
              pkgs.cargo-deny
              pkgs.cargo-llvm-cov
              config.treefmt.build.wrapper
              # The justfile's recipes and the reporter they call.
              pkgs.just
              pkgs.nushell
            ];

            # cargo-llvm-cov looks for these next to the rustc that built the
            # instrumented binaries; the nixpkgs toolchain ships them apart.
            LLVM_COV = "${pkgs.rustc.llvmPackages.llvm}/bin/llvm-cov";
            LLVM_PROFDATA = "${pkgs.rustc.llvmPackages.llvm}/bin/llvm-profdata";

            shellHook = config.pre-commit.installationScript;
          };
        };
    };
}
