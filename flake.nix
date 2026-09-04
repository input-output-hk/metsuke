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

      flake.hydraJobs =
        let
          jobs = {
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
                pipeDropIn = ./contrib/node-pipe.conf;
                nodeCommand = (import ./nix/unit.nix).nodeCommandPlaceholder;
                agent = self.packages.${system}.metsuke;
              }
            );

            packages = lib.genAttrs systems (system: removeAttrs self.packages.${system} [ "default" ]);
            checks = lib.genAttrs systems (system: self.checks.${system});
            devShells = lib.genAttrs systems (system: self.devShells.${system});
          };

          # Every job above that this system has one of. Collected rather than
          # listed, so a job added to `jobs` is a job the aggregate covers and a
          # green tick keeps meaning the whole jobset.
          builtFor =
            system: lib.collect lib.isDerivation (lib.mapAttrs (_: bySystem: bySystem.${system}) jobs);

          aggregate =
            system: name: constituents:
            inputs.nixpkgs.legacyPackages.${system}.releaseTools.aggregate {
              inherit name constituents;
            };

          # One per system, so a failure says which arch without opening the
          # jobset, and the arches can be required separately while one is new.
          perSystem = lib.genAttrs systems (system: aggregate system "required-${system}" (builtFor system));
        in
        jobs
        // lib.mapAttrs' (system: job: lib.nameValuePair "required-${system}" job) perSystem
        // {
          # What a branch protects on: one status check standing for every
          # arch's aggregate, so a system added to `systems` cannot quietly
          # stop being required.
          required = aggregate (builtins.head systems) "required" (lib.attrValues perSystem);
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
          # Cargo sources, the fixtures and the server's page: the scrape
          # bodies, the trace recordings, the instructions markup and the icon
          # are compiled in with include_str!, the submission recordings and the
          # S3 cassette are read at test time.
          #
          # Under crates/ alone, because this filter is not gitignore-aware: a
          # devnet run leaves .hex files in the working tree, and matching
          # those by suffix anywhere would re-hash every derivation.
          extraSources = [
            ".prom"
            ".log"
            ".hex"
            ".http"
            ".svg"
            ".html"
            ".css"
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

          # What a shipped binary says it was built from
          # (crates/metsuke-wire/build.rs). A sandbox has no repository to
          # read, so the commit is passed in; a dirty tree says so, and a
          # source with no revision at all is honest about that too.
          buildRev = self.shortRev or self.dirtyShortRev or "unknown";

          # Tests run once, in checks.test. Crane defaults doCheck to true,
          # which would run the suite again inside each binary.
          binaryArgs = {
            inherit cargoArtifacts version;
            strictDeps = true;
            doCheck = false;
            METSUKE_REV = buildRev;
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
          # whole into the instructions page with include_str!, as is the icon
          # under assets/, so cargo sources alone do not build.
          serverFileset = pkgs.lib.fileset.unions [
            (pkgs.lib.fileset.fromSource (crateSrc [
              ./crates/metsuke-wire
              ./crates/metsuke
              ./crates/metsuke-server
            ]))
            ./contrib/config.example.toml
            ./contrib/config.minimal.toml
            ./contrib/config.pipe.toml
            ./contrib/config.journald.toml
            ./contrib/metsuke.service
            ./contrib/metsuke-journald.service
            ./contrib/node-pipe.conf
            ./crates/metsuke-server/assets
            # What the check step shows, recorded by the agent's own test.
            ./crates/metsuke/tests/fixtures/recordings/agent-journal.log
          ];

          unit = import ./nix/unit.nix;

          # One line per list element, as NixOS renders too: SystemCallFilter's
          # leading `~` inverts the line it opens, so a joined list would deny
          # nothing.
          directives = pkgs.lib.generators.toKeyValue { listsAsDuplicateKeys = true; };

          # The three files an operator brings, named once so a unit's prose and
          # its ExecStart cannot point at different paths. instructions.rs reads
          # the binary and the config back out of ExecStart.
          agentBinary = "/usr/local/bin/metsuke";
          agentConfig = "/etc/metsuke/config.toml";
          agentKey = "/etc/metsuke/bls.skey";

          # systemd reads the key as root and hands the service a copy only it
          # can read. Naming the key in config.toml instead needs it readable by
          # the unit's DynamicUser, which for a cold key means readable by
          # everyone, so the credential is what ships rather than an option the
          # header offers.
          credential = "LoadCredential=signing-key:${agentKey}";
          execStart = "${agentBinary} --config ${agentConfig} --signing-key \${CREDENTIALS_DIRECTORY}/signing-key";

          # One unit per log source, so an operator picks a file rather than a
          # set of directives to change. Everything below the header is shared,
          # and `readsTheJournal` is the only difference between the two.
          agentUnit =
            {
              name,
              header,
              readsTheJournal ? false,
            }:
            pkgs.writeText name ''
              ${header}

              [Unit]
              Description=metsuke telemetry agent
              After=network-online.target
              Wants=network-online.target

              [Service]
              ${credential}
              ExecStart=${execStart}
              Restart=always
              RestartSec=${toString unit.restartSecs}
              ${directives (
                unit.hardening {
                  stateDirectory = "metsuke";
                  inherit (unit) addressFamilies;
                  inherit readsTheJournal;
                }
              )}
              [Install]
              WantedBy=multi-user.target
            '';

          # What an operator brings, which is the same list whichever unit they
          # take. A list rather than a sentence: these are paths the unit will
          # look for, and one of them carries a mode that matters.
          bringTheseFiles = ''
            #   the binary at ${agentBinary}
            #   the configuration at ${agentConfig}
            #   the signing key at ${agentKey}, owned by root, mode 0400'';

          contribUnit = agentUnit {
            name = "metsuke.service";
            header = ''
              # Example hardened unit for a host that is not NixOS. Generated:
              # edit nix/unit.nix, then `nix build .#metsuke-unit` and commit
              # what it wrote here.
              #
              # Copy to /etc/systemd/system/metsuke.service, and bring:
              ${bringTheseFiles}
              #
              # Take contrib/config.minimal.toml as the configuration. This unit
              # collects metrics and reads no trace lines. For those,
              # contrib/metsuke-journald.service reads the node's journal and
              # contrib/node-pipe.conf reads its stdout; ADR 0010 has what each
              # one costs.'';
          };

          contribJournaldUnit = agentUnit {
            name = "metsuke-journald.service";
            readsTheJournal = true;
            header = ''
              # Example hardened unit for a host that is not NixOS, collecting
              # the node's trace lines from its journal. Generated: edit
              # nix/unit.nix, then `nix build .#metsuke-journald-unit` and commit
              # what it wrote here.
              #
              # contrib/metsuke.service plus the two directives journalctl
              # needs: the systemd-journal group, which reads every unit's
              # journal on the host, and ProcSubset=all, without which
              # journalctl exits before its first line.
              #
              # Copy to /etc/systemd/system/metsuke.service, and bring:
              ${bringTheseFiles}
              #
              # Take contrib/config.journald.toml as the configuration.
              # contrib/node-pipe.conf is the source that costs no group.'';
          };

          # Pipe mode is a change to the node's unit and not the agent's, since
          # the agent runs downstream of the node and has no unit of its own. A
          # drop-in rather than an edited unit, so the node's own packaging
          # still owns its file.
          contribPipeDropIn = pkgs.writeText "node-pipe.conf" ''
            # Example drop-in for the node's unit, which is where pipe mode
            # lives. Generated: edit nix/unit.nix, then
            # `nix build .#metsuke-pipe-dropin` and commit what it wrote here.
            #
            # Copy to /etc/systemd/system/<your-node>.service.d/metsuke.conf.
            # That directory is yours to make: `sudo mkdir -p` it, or let
            # `systemctl edit <your-node>` make it and paste this in. And
            # bring:
            ${bringTheseFiles}
            #
            # Take contrib/config.pipe.toml as the configuration.
            #
            # Replace ${unit.nodeCommandPlaceholder} below with the command your node's unit
            # already runs, which `systemctl cat <your-node>.service` prints.
            # The empty ExecStart= is what clears that command before this one
            # replaces it, and the shell is because systemd has no pipelines.
            #
            # The agent then runs as whatever user the node runs as, holds no
            # group and reads no journal, and writes its spool under the state
            # directory below. It passes every line through to its own stdout,
            # so the node's output still reaches the journal. ADR 0010 has what
            # each source costs.

            [Service]
            ${credential}
            StateDirectory=metsuke
            ExecStart=
            ExecStart=/bin/sh -c '${unit.nodeCommandPlaceholder} | ${execStart}'
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
            # `suiteTools`, exposed. docs/reading-the-archive.md tells a
            # developer to run both over a downloaded tree, and a sync's own
            # summary prints a duckdb line, so a host that has neither can
            # reach them by `nix run` rather than by cloning for a devShell.
            inherit (pkgs) duckdb zstd;

            metsuke = craneLib.buildPackage agentArgs;
            metsuke-unit = contribUnit;
            metsuke-journald-unit = contribJournaldUnit;
            metsuke-pipe-dropin = contribPipeDropIn;
            metsuke-allowlist = (import ./nix/allowlist.nix { inherit pkgs; }).package;
            metsuke-roster = (import ./nix/roster.nix { inherit pkgs; }).package;
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

            roster = (import ./nix/roster.nix { inherit pkgs; }).tests;

            server-config = import ./nix/server-config-test.nix {
              inherit pkgs;
              serverModule = self.nixosModules.metsuke-server;
              server = config.packages.metsuke-server;
            };

            roster-unit = import ./nix/roster-unit-test.nix {
              inherit pkgs;
              serverModule = self.nixosModules.metsuke-server;
              server = config.packages.metsuke-server;
              roster = config.packages.metsuke-roster;
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

            contrib-unit = pkgs.runCommand "contrib-units-are-current" { } ''
              stale() {
                echo "contrib/$1 is stale; its header says how"
                exit 1
              }
              diff -u ${./contrib/metsuke.service} ${contribUnit} \
                || stale metsuke.service
              diff -u ${./contrib/metsuke-journald.service} ${contribJournaldUnit} \
                || stale metsuke-journald.service
              diff -u ${./contrib/node-pipe.conf} ${contribPipeDropIn} \
                || stale node-pipe.conf
              touch $out
            '';

            # The pages tell an operator to build these by name, and
            # instructions.rs composes the rest of those commands, so both are
            # read. Nothing in the Rust tree can see whether an output still
            # exists, and the page's own `$ARCH` placeholder is why the suffix
            # must match at least one character: `metsuke-static-` alone is not
            # a package anyone can build.
            instructions-outputs = pkgs.runCommand "instructions-name-real-outputs" { } ''
              pages="${./crates/metsuke-server/assets/quickstart.html} ${./crates/metsuke-server/assets/details.html} ${./crates/metsuke-server/src/instructions.rs}"
              # Each grep is asserted non-empty first: a rename that also
              # reflowed the literal would otherwise leave a loop over nothing.
              # `|| true`: a grep that matches nothing exits 1, and under
              # `set -o pipefail` that would abort with an empty log instead of
              # the message below.
              packages=$(grep -oh 'metsuke-static-[a-z0-9_-][a-z0-9_-]*' $pages | sort -u || true)
              modules=$(grep -oh 'nixosModules\.[a-z-]*' $pages | cut -d. -f2 | sort -u || true)
              [ -n "$packages" ] || { echo "no page offers a build to run"; exit 1; }
              [ -n "$modules" ] || { echo "no page points at a module"; exit 1; }
              for name in $packages; do
                case " ${toString (builtins.attrNames config.packages)} " in
                  *" $name "*) ;;
                  *)
                    echo "a page offers $name, which this flake does not build"
                    exit 1
                    ;;
                esac
              done
              for name in $modules; do
                case " ${toString (builtins.attrNames self.nixosModules)} " in
                  *" $name "*) ;;
                  *)
                    echo "a page points at nixosModules.$name, which does not exist"
                    exit 1
                    ;;
                esac
              done
              touch $out
            '';

            # The same reason as the check above, for the documents the details
            # page links: the Rust source is filtered to the crates and
            # contrib, so a test there cannot see a document, and a renamed one
            # would leave a 404 on the page. Read off the template rather than
            # a render, because the prefix is what a render supplies.
            instructions-documents = pkgs.runCommand "instructions-link-real-documents" { } ''
              documents=${pkgs.lib.sourceFilesBySuffices ./. [ ".md" ]}
              page=${./crates/metsuke-server/assets/details.html}
              # `|| true` for the same reason the check above gives.
              files=$(grep -oh '{{DOCS_PREFIX}}[^"]*' $page |
                sed 's|^{{DOCS_PREFIX}}||' | sort -u || true)
              trees=$(grep -oh '{{REPOSITORY}}/tree/main/[^"]*' $page |
                sed 's|^{{REPOSITORY}}/tree/main/||' | sort -u || true)
              [ -n "$files" ] || { echo "the details page links no documents"; exit 1; }
              for path in $files; do
                [ -f "$documents/$path" ] || {
                  echo "the details page links $path, which is not a file in this repository"
                  exit 1
                }
              done
              for path in $trees; do
                [ -d "$documents/$path" ] || {
                  echo "the details page links $path as a directory, which it is not"
                  exit 1
                }
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
              entry = lib.mkForce (
                toString (
                  pkgs.writeShellScript "clippy-hook" ''
                    export PATH=${
                      lib.makeBinPath [
                        pkgs.cargo
                        pkgs.clippy
                        pkgs.rustc
                      ]
                    }:$PATH
                    # Named rather than left to PATH: an interactive devShell
                    # sources the user's own rc, so a rustup shim can sit ahead
                    # of this and hand cargo a different rustc than the clippy
                    # beside it.
                    export RUSTC=${lib.getExe' pkgs.rustc "rustc"}
                    if ! command -v cc >/dev/null; then
                      echo "clippy builds this workspace's build scripts and there is no cc here. Commit from the devShell: nix develop"
                      exit 1
                    fi
                    # Read off the run rather than predicted: what clippy needs
                    # cached is what it resolves for this target, and probing
                    # that with cargo fetch or cargo metadata over-resolves and
                    # refuses a cargo home that would have linted fine.
                    log=$(mktemp)
                    trap 'rm -f "$log"' EXIT
                    set -o pipefail
                    cargo-clippy clippy --all-targets --offline -- --deny warnings 2>&1 | tee "$log"
                    status=$?
                    # Both of cargo's offline complaints: a crate the lock names
                    # that is not cached, and one it wants to download.
                    if [ "$status" -ne 0 ] && grep -qF \
                      -e 'offline mode (via' \
                      -e 'but --offline was specified' "$log"; then
                      echo
                      echo "A crate this workspace locks is not in this cargo home, so that is not a lint. Warm it once: nix develop -c cargo fetch"
                    fi
                    if [ "$status" -ne 0 ] && grep -qF 'incompatible version of rustc' "$log"; then
                      echo
                      echo "The target dir holds artifacts from another rustc, so that is not a lint. Clear it once: nix develop -c cargo clean"
                    fi
                    exit "$status"
                  ''
                )
              );
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
