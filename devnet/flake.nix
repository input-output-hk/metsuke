{
  description = "Local Leios devnet: node, Postgres, db-sync, and a metadata submitter";

  # Kept out of ../flake.nix on purpose. leios and db-sync are both haskell.nix
  # projects, and their transitive inputs dwarf the workspace's own lock. The
  # workspace flake stays crane-only so an ordinary `nix flake check` there does
  # not evaluate a Haskell project it never builds.
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";
    process-compose-flake.url = "github:Platonic-Systems/process-compose-flake";
    services-flake.url = "github:juspay/services-flake";

    # Same revision as ../flake.lock's cardano-node-leios, as a flake this
    # time: the process-compose stack needs the node and cli packages at
    # evaluation, not a source tree.
    leios.url = "github:input-output-hk/ouroboros-leios?ref=refs/tags/prototype-2026w32";

    # The db-sync leios1-dbsync-a-1 runs; cardano-playground pins this branch.
    cardano-db-sync-leios.url = "github:IntersectMBO/cardano-db-sync/jl/leios-prototype";

    # The reference CIP-151 implementation. Nothing in cardano-cli can produce
    # a label-867 witness -- it cannot sign arbitrary bytes at all -- and a
    # blob our own encoder produced would make a verification test agree with
    # itself. Upstream ships no lockfile, so bun.lock and bun.nix beside this
    # flake are ours and pin what it builds against.
    cardano-signer-src = {
      url = "github:gitmachtl/cardano-signer";
      flake = false;
    };
    bun2nix = {
      url = "github:nix-community/bun2nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    inputs@{ flake-parts, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      systems = [ "x86_64-linux" ];

      imports = [ inputs.process-compose-flake.flakeModule ];

      perSystem =
        { pkgs, system, ... }:
        let
          nodePkgs = inputs.leios.inputs.cardano-node-leios.packages.${system};
          inherit (nodePkgs) cardano-node;
          inherit (nodePkgs) cardano-cli;
          cardano-db-sync =
            inputs.cardano-db-sync-leios.packages.${system}."cardano-db-sync:exe:cardano-db-sync";
          dbSyncSchema = "${inputs.cardano-db-sync-leios}/schema";
          devnetSrc = "${inputs.leios}/demo/proto-devnet";

          # Everything the stack writes. Wiped by `scripts/devnet.sh up` before
          # every run: a re-stamped genesis invalidates any state kept from the
          # last one.
          workdir = "./.devnet";

          # Postgres names, matching what leios1-dbsync-a-1 runs, so a query
          # recorded here is a query against production's schema owner.
          database = "cexplorer";
          owner = "cexplorer";
          # Read-only, and the server connects as this (ADR 0008).
          reader = "metsuke_ro";

          socketDir = "$PWD/${workdir}/postgres";

          # Node params. Local, single-forger, so nothing here ever rolls back
          # and the devnet's own k is bookkeeping: it is shrunk only to buy a
          # short epoch, keeping Shelley's epochLength = 10k/f. The k that
          # matters to the server is the one in the network's genesis, not
          # this one -- docs/research/leios-devnet.md.
          securityParam = 6;
          epochLength = 1200;
          metricsPort = 12798;

          bun2nix = inputs.bun2nix.packages.${system}.default;

          # Upstream keeps package.json beside the script and ships no
          # lockfile, so the build tree is its src plus ours.
          signerSrc = pkgs.runCommand "cardano-signer-src" { } ''
            mkdir -p $out
            cp ${inputs.cardano-signer-src}/src/package.json $out/
            cp ${inputs.cardano-signer-src}/src/cardano-signer.js $out/
            cp ${./cardano-signer/bun.lock} $out/bun.lock
          '';

          cardano-signer = bun2nix.mkDerivation {
            pname = "cardano-signer";
            version = "1.35.0";
            src = signerSrc;
            bunDeps = bun2nix.fetchBunDeps { bunNix = ./cardano-signer/bun.nix; };
            nativeBuildInputs = [ pkgs.makeBinaryWrapper ];
            dontBuild = true;
            installPhase = ''
              runHook preInstall
              mkdir -p $out/lib/cardano-signer $out/bin
              cp -r . $out/lib/cardano-signer
              makeBinaryWrapper ${pkgs.bun}/bin/bun $out/bin/cardano-signer \
                --add-flags "run --prefer-offline --no-install $out/lib/cardano-signer/cardano-signer.js"
              runHook postInstall
            '';
          };

          setup = pkgs.writeShellApplication {
            name = "devnet-setup";
            runtimeInputs = [
              pkgs.jq
              pkgs.yq-go
              pkgs.coreutils
            ];
            text = ''
              # Every process in the stack addresses its state as a relative
              # path, so a run from the wrong directory would quietly build a
              # second devnet somewhere else.
              [ -e ./flake.nix ] && [ -e ./db-sync-config.json ] || {
                echo "error: run this from the devnet flake directory, or via scripts/devnet.sh" >&2
                exit 1
              }

              work="$PWD/${workdir}"
              mkdir -p "$work"

              # Genesis with a fresh start time, so the node forges from slot 0
              # now. The node config addresses genesis as ./<era>-genesis.json,
              # so they sit next to config.json.
              cp "${devnetSrc}/config/genesis/"*.json "$work/"
              chmod u+w "$work/"*.json

              start_epoch=$(date +%s)
              start_iso=$(date -u -d "@$start_epoch" +"%Y-%m-%dT%H:%M:%SZ")

              jq --argjson time "$start_epoch" --argjson k ${toString securityParam} \
                '.startTime = $time | .protocolConsts.k = $k' \
                "${devnetSrc}/config/genesis/byron-genesis.json" >"$work/byron-genesis.json"

              jq --arg time "$start_iso" \
                --argjson k ${toString securityParam} \
                --argjson epoch ${toString epochLength} \
                '.systemStart = $time | .securityParam = $k | .epochLength = $epoch' \
                "${devnetSrc}/config/genesis/shelley-genesis.json" >"$work/shelley-genesis.json"

              # Shelley wants epochLength == 10k/activeSlotsCoeff. Asserted
              # rather than trusted: the two are set independently above.
              jq -e '.epochLength == (10 * .securityParam / .activeSlotsCoeff)' \
                "$work/shelley-genesis.json" >/dev/null || {
                echo "error: epochLength is not 10k/f in $work/shelley-genesis.json" >&2
                exit 1
              }

              # The demo's node config, with the Prometheus backend confined to
              # loopback (ADR 0007) and JSON rather than YAML so plain jq can
              # address the empty-string TraceOptions key.
              yq -o=json . "${devnetSrc}/config/config.yaml" |
                jq --arg backend "PrometheusSimple 127.0.0.1 ${toString metricsPort}" \
                  '.TraceOptionNodeName = "devnet"
                   | .ConsensusMode = "PraosMode"
                   | .TraceOptions."".backends = ["Stdout MachineFormat", $backend]' \
                  >"$work/config.json"

              # The demo config carries no genesis hashes. cardano-node does
              # not mind; db-sync refuses to parse a node config without
              # ByronGenesisHash, and the files were just rewritten, so the
              # hashes have to be computed here rather than copied.
              set_hash() {
                jq --arg key "$1" --arg hash "$2" '.[$key] = $hash' \
                  "$work/config.json" >"$work/config.next.json"
                mv "$work/config.next.json" "$work/config.json"
              }

              # A command substitution inside an argument list cannot fail the
              # script: errexit does not see it, so the hash would be set to
              # the empty string and the node would die much later with a
              # mismatch. Every hash is assigned on its own line.

              # Byron's genesis hash is taken over its canonical encoding, not
              # over the file, so `hash genesis-file` answers a different
              # question there and the node rejects it as a mismatch.
              byron_hash=$("${cardano-cli}/bin/cardano-cli" byron genesis \
                print-genesis-hash --genesis-json "$work/byron-genesis.json")
              set_hash ByronGenesisHash "$byron_hash"

              for era in Shelley Alonzo Conway Dijkstra; do
                lower=$(echo "$era" | tr '[:upper:]' '[:lower:]')
                era_hash=$("${cardano-cli}/bin/cardano-cli" hash genesis-file \
                  --genesis "$work/$lower-genesis.json")
                set_hash "''${era}GenesisHash" "$era_hash"
              done

              # No peers: one forger produces its own chain, which is all any
              # of this records.
              jq '.' "${devnetSrc}/config/topology.template.json" >"$work/topology.json"

              # NodeConfigFile in here is relative, so the file only works
              # beside the config.json written above.
              cp "${./db-sync-config.json}" "$work/db-sync-config.json"

              cp -r "${devnetSrc}/config/pools-keys/pool1" "$work/pool1"
              cp -r "${devnetSrc}/config/utxo-keys/utxo1" "$work/utxo1"
              chmod -R u+w "$work/pool1" "$work/utxo1"
              chmod 400 "$work/pool1/"*.skey "$work/utxo1/"*.skey

              # db-sync reads its Postgres credentials from here and nowhere
              # else. Socket connection, so the password is never used.
              printf '%s:5432:%s:%s:*\n' "${socketDir}" "${database}" "${owner}" >"$work/pgpass"
              chmod 600 "$work/pgpass"

              echo "devnet: genesis stamped at $start_iso, k=${toString securityParam}, epochLength=${toString epochLength}"
            '';
          };

          # Both halves of the stack come from separate upstreams. A recording
          # that cannot be attributed to a revision is not a recording.
          revisions = pkgs.writeShellApplication {
            name = "devnet-revisions";
            text = ''
              echo "leios:        ${inputs.leios.rev}"
              echo "cardano-node: $(${cardano-node}/bin/cardano-node --version | head -1)"
              echo "cardano-cli:  $(${cardano-cli}/bin/cardano-cli --version | head -1)"
              echo "db-sync:      ${inputs.cardano-db-sync-leios.rev}"
              echo "schema:       ${dbSyncSchema}"
            '';
          };

          # Shared preamble for the submitter commands: a funded address, its
          # key, and a node to talk to.
          submitterEnv = ''
            # How long a submission waits for its own change output. A limit,
            # so it is configuration.
            AWAIT_TRIES="''${AWAIT_TRIES:-120}"
            AWAIT_INTERVAL="''${AWAIT_INTERVAL:-5}"

            export CARDANO_NODE_SOCKET_PATH="$PWD/${workdir}/node.socket"
            export CARDANO_NODE_NETWORK_ID=164
            cli="${cardano-cli}/bin/cardano-cli"
            work="$PWD/${workdir}"
            payment="$work/utxo1/utxo"
            addr=$("$cli" dijkstra address build --payment-verification-key-file "$payment.vkey")

            utxos() { "$cli" dijkstra query utxo --address "$addr" --output-json; }

            # The largest unspent input, or a failure. Assigned on its own line
            # by every caller: inside an argument list a failing substitution
            # is invisible to errexit and reaches cardano-cli as `null`.
            largest_input() {
              utxos | jq -er 'to_entries | max_by(.value.value.lovelace) | .key'
            }

            # A utxo query reads the ledger at the tip and knows nothing of the
            # mempool, so back-to-back submissions would both pick the same
            # input and the second would be rejected as already spent. Every
            # submission waits for its own change output to land.
            await() {
              for _ in $(seq "$AWAIT_TRIES"); do
                # Run outside the `if`, so a dead node is an error rather than
                # a condition that stays false until the loop gives up.
                seen=$(utxos) || return 1
                if jq -e --arg t "$1" 'keys | any(startswith($t))' >/dev/null <<<"$seen"; then
                  return 0
                fi
                sleep "$AWAIT_INTERVAL"
              done
              echo "error: $1 not on chain after $((AWAIT_TRIES * AWAIT_INTERVAL))s" >&2
              return 1
            }
          '';

          # Any label, on a self-send. What db-sync files as a tx_metadata row
          # against a transaction with no certificate.
          submitMetadata = pkgs.writeShellApplication {
            name = "devnet-submit-metadata";
            runtimeInputs = [
              pkgs.jq
              pkgs.coreutils
            ];
            text = ''
              label="''${1:?usage: submit-metadata <label> <json-file>}"
              blob="''${2:?usage: submit-metadata <label> <json-file>}"
              ${submitterEnv}

              jq -e --arg l "$label" 'has($l)' "$blob" >/dev/null || {
                echo "error: $blob has no top-level \"$label\" key" >&2
                exit 1
              }

              tmp=$(mktemp -d)
              trap 'rm -rf "$tmp"' EXIT

              # Only the label that was asked for. A file holding both a 674
              # and an 867 would otherwise put both on chain, and the caller
              # would have no way to tell from the command they ran.
              jq --arg l "$label" '{($l): .[$l]}' "$blob" >"$tmp/metadata.json"

              txin=$(largest_input)
              "$cli" dijkstra transaction build \
                --tx-in "$txin" \
                --change-address "$addr" \
                --metadata-json-file "$tmp/metadata.json" \
                --out-file "$tmp/tx.raw"

              "$cli" dijkstra transaction sign \
                --tx-body-file "$tmp/tx.raw" \
                --signing-key-file "$payment.skey" \
                --out-file "$tmp/tx.signed"

              "$cli" dijkstra transaction submit --tx-file "$tmp/tx.signed"
              txid=$("$cli" dijkstra transaction txid --tx-file "$tmp/tx.signed" | jq -r .txhash)
              await "$txid"
              echo "$txid"
            '';
          };

          # A pool re-registration certificate carrying metadata. The
          # allowlist query joins pool_update.registered_tx_id to tx_metadata,
          # so a label-674 blob only lands where that join can see it if it
          # rides a registration certificate rather than a bare tx.
          registerPool = pkgs.writeShellApplication {
            name = "devnet-register-pool";
            runtimeInputs = [
              pkgs.jq
              pkgs.coreutils
            ];
            text = ''
              blob="''${1:?usage: register-pool <json-file>}"
              ${submitterEnv}
              pool="$work/pool1"

              tmp=$(mktemp -d)
              trap 'rm -rf "$tmp"' EXIT

              min_cost=$("$cli" dijkstra query protocol-parameters | jq -er .minPoolCost)

              "$cli" dijkstra stake-pool registration-certificate \
                --cold-verification-key-file "$pool/cold.vkey" \
                --vrf-verification-key-file "$pool/vrf.vkey" \
                --bls-signing-key-file "$pool/bls.skey" \
                --pool-pledge 0 \
                --pool-cost "$min_cost" \
                --pool-margin 0 \
                --pool-reward-account-verification-key-file "$pool/staking-reward.vkey" \
                --pool-owner-stake-verification-key-file "$pool/staking-reward.vkey" \
                --testnet-magic 164 \
                --out-file "$tmp/pool.cert"

              txin=$(largest_input)
              "$cli" dijkstra transaction build \
                --tx-in "$txin" \
                --change-address "$addr" \
                --certificate-file "$tmp/pool.cert" \
                --metadata-json-file "$blob" \
                --out-file "$tmp/tx.raw"

              "$cli" dijkstra transaction sign \
                --tx-body-file "$tmp/tx.raw" \
                --signing-key-file "$payment.skey" \
                --signing-key-file "$pool/cold.skey" \
                --signing-key-file "$pool/staking-reward.skey" \
                --out-file "$tmp/tx.signed"

              "$cli" dijkstra transaction submit --tx-file "$tmp/tx.signed"
              txid=$("$cli" dijkstra transaction txid --tx-file "$tmp/tx.signed" | jq -r .txhash)
              await "$txid"
              echo "$txid"
            '';
          };
        in
        {
          packages = {
            inherit
              cardano-cli
              cardano-node
              cardano-db-sync
              ;
            inherit cardano-signer;
            devnet-setup = setup;
            devnet-revisions = revisions;
            devnet-submit-metadata = submitMetadata;
            devnet-register-pool = registerPool;
          };

          process-compose.devnet =
            { ... }:
            {
              imports = [ inputs.services-flake.processComposeModules.default ];

              # The server is what `process-compose down` talks to. Without it
              # there is no way to stop the stack except signalling something,
              # and the only thing to signal is the supervisor itself.
              # services-flake defaults it off, so this override is what keeps
              # teardown from becoming a kill. scripts/devnet.sh scopes it to a
              # unix socket in the work directory: neither a port nor global.
              cli.options = {
                no-server = false;
                ordered-shutdown = true;
              };

              services.postgres.pg = {
                enable = true;
                inherit socketDir;
                # Socket only. There is nothing here for the network to reach,
                # the server connects by socket directory anyway, and binding
                # 5432 collides with whatever Postgres the machine already runs.
                listen_addresses = "";
                dataDir = "${workdir}/postgres-data";
                initialDatabases = [ { name = database; } ];
                initialScript.after = ''
                  CREATE USER ${owner};
                  ALTER DATABASE ${database} OWNER TO ${owner};
                  CREATE USER ${reader};
                  GRANT CONNECT ON DATABASE ${database} TO ${reader};
                '';
              };

              settings.processes = {
                setup.command = setup;

                revisions = {
                  command = revisions;
                  depends_on.setup.condition = "process_completed_successfully";
                };

                node = {
                  command = pkgs.writeShellApplication {
                    name = "devnet-node";
                    text = ''
                      work="$PWD/${workdir}"
                      # cwd is the workdir: the config's relative leios.db
                      # lands there rather than in the repo.
                      cd "$work"
                      exec ${cardano-node}/bin/cardano-node run \
                        --config "$work/config.json" \
                        --topology "$work/topology.json" \
                        --database-path "$work/db" \
                        --socket-path "$work/node.socket" \
                        --host-addr 127.0.0.1 \
                        --port 3001 \
                        --shelley-vrf-key "$work/pool1/vrf.skey" \
                        --shelley-kes-key "$work/pool1/kes.skey" \
                        --shelley-bls-key "$work/pool1/bls.skey" \
                        --shelley-operational-certificate "$work/pool1/opcert.cert"
                    '';
                  };
                  depends_on.setup.condition = "process_completed_successfully";
                  readiness_probe = {
                    exec.command = "test -S ${workdir}/node.socket";
                    initial_delay_seconds = 5;
                    period_seconds = 5;
                    timeout_seconds = 5;
                    failure_threshold = 60;
                  };
                };

                # db-sync migrates the schema on first start, so every table it
                # creates postdates initialScript. Default privileges are what
                # reaches them; a GRANT at init time would reach nothing.
                grant-reader = {
                  command = pkgs.writeShellApplication {
                    name = "devnet-grant-reader";
                    runtimeInputs = [ pkgs.postgresql ];
                    text = ''
                      psql -h "${socketDir}" -d ${database} -U ${owner} <<'SQL'
                      GRANT USAGE ON SCHEMA public TO ${reader};
                      GRANT SELECT ON ALL TABLES IN SCHEMA public TO ${reader};
                      ALTER DEFAULT PRIVILEGES FOR ROLE ${owner} IN SCHEMA public
                        GRANT SELECT ON TABLES TO ${reader};
                      SQL
                    '';
                  };
                  depends_on.db-sync.condition = "process_started";
                };

                db-sync = {
                  command = pkgs.writeShellApplication {
                    name = "devnet-db-sync";
                    text = ''
                      work="$PWD/${workdir}"
                      export PGPASSFILE="$work/pgpass"
                      # NodeConfigFile in db-sync-config.json is relative.
                      cd "$work"
                      exec ${cardano-db-sync}/bin/cardano-db-sync \
                        --config "$work/db-sync-config.json" \
                        --socket-path "$work/node.socket" \
                        --state-dir "$work/dbsync-ledger" \
                        --schema-dir ${dbSyncSchema}
                    '';
                  };
                  depends_on = {
                    pg.condition = "process_healthy";
                    node.condition = "process_healthy";
                  };
                };

                access-instructions = {
                  command = pkgs.writeShellApplication {
                    name = "devnet-access-instructions";
                    text = ''
                      cat <<EOF
                      psql:   psql -h $PWD/${workdir}/postgres -U ${reader} -d ${database}
                      owner:  psql -h $PWD/${workdir}/postgres -U ${owner} -d ${database}
                      cli:    export CARDANO_NODE_SOCKET_PATH=$PWD/${workdir}/node.socket
                              export CARDANO_NODE_NETWORK_ID=164
                      submit: scripts/devnet.sh submit-metadata <label> <json-file>
                              scripts/devnet.sh register-pool <json-file>
                      EOF
                      while true; do sleep 60; done
                    '';
                  };
                  depends_on.db-sync.condition = "process_started";
                };
              };
            };
        };
    };
}
