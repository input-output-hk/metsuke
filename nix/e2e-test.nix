# The whole path once, on real parts: a Leios node's own Prometheus endpoint,
# the agent through its module, the server through its module, and Garage as
# the bucket. Scrape, spool, signed submission, verification, object, ACK. What
# a value should be is not asserted here. The recorded fixtures under
# crates/metsuke/tests/fixtures own that; this asserts that the parts meet.
#
# The node's tracing is the recorded pre-production configuration, and the
# instructions page's node-config snippets are merged into it rather than pasted
# over it, which is what the page tells an operator to do.
#
# One machine, because the agent refuses a plaintext upload_url that is not
# loopback (crates/metsuke/src/endpoint.rs) and there is no certificate here to
# make it https.
#
# Two derivations come out rather than one: `poolIdRecorded` answers the same
# question about the same pin without a VM or /dev/kvm, so it can fail on a
# host that cannot run `test` at all.
{
  pkgs,
  agentModule,
  serverModule,
  serverPackage,
  cardano-node,
  # Only for poolIdRecorded, which is why it never reaches the guest.
  cardano-cli,
  # The Leios source tree, for the proto-devnet configuration and its pool keys.
  leios,
}:
let
  inherit (pkgs) lib;

  # What this configuration is: docs/research/cardano-node-11-tracing.md,
  # section 1. Recorded by scripts/record-preprod-node-config.sh.
  preprodConfigFile = ./fixtures/leios-preprod-node-config.json;
  preprodConfig = lib.importJSON preprodConfigFile;

  # The endpoint that recording already opens: an operator reaches step 4 with a
  # `PrometheusSimple` backend already in place.
  metricsPort =
    let
      backends = preprodConfig.TraceOptions."".backends;
      named = lib.filter (lib.hasPrefix "PrometheusSimple ") backends;
      words = lib.splitString " " (lib.head named);
    in
    assert lib.assertMsg (
      named != [ ]
    ) "${toString preprodConfigFile} opens no PrometheusSimple endpoint to scrape";
    lib.toInt (lib.last words);

  # crates/metsuke-server/src/instructions.rs reads it from here too.
  pagePort =
    let
      example = fromTOML (builtins.readFile ../contrib/config.example.toml);
      matched = builtins.match "https?://[^:/]+:([0-9]+)(/.*)?" example.metrics_url;
    in
    assert lib.assertMsg (
      matched != null
    ) "the shipped example config's metrics_url names no port: ${example.metrics_url}";
    lib.toInt (lib.head matched);

  listenPort = 8080;
  s3Port = 3900;

  # Applying the page's snippets is an edit to the file the node reads, and a
  # store path cannot be edited. Seeded from `nodeConfig` at boot.
  configPath = "/var/lib/cardano-node-config/config.json";

  devnetSrc = "${leios}/demo/proto-devnet";
  poolKeys = "${devnetSrc}/config/pools-keys/pool1";

  # The guest boots here rather than at the host's clock, so the node forges.
  # pool1's opcert is issued for KES period 0 and the demo's genesis is years
  # old, so at any real clock the node is far past `maxKESEvolutions` and
  # produces nothing, and further every day. A clock the test owns also drops
  # the wall clock as an input.
  genesisStart = (lib.importJSON "${devnetSrc}/config/genesis/byron-genesis.json").startTime;

  # Leader about one slot in five for pool1's third of the stake
  # (`1 - (1 - f) ^ σ`), so a block inside seconds rather than inside a Poisson
  # tail. Not 1: every slot having a leader is further from anything an operator
  # runs than it needs to be.
  activeSlots = "0.5";

  # Recorded from poolKeys/cold.vkey, not derived: a bump of the Leios pin that
  # changed the demo's pool1 would silently move a derived value, where this
  # shows up as a diff. `poolIdRecorded` below is what asserts they still agree.
  poolId = "pool1awyxt3egwmunup7nmd2uznqr54p2lgdt3tvrqetj8geqgfz26x9";

  # The guest's hostname is its node name below, and the agent is given no
  # agent_id, so this is the slug it stamps every line with.
  agentId = "e2e";

  # A second machine reporting for the same pool, which is the case a per-pool
  # replay counter used to reject (metsuke-jfb.4). Named rather than slugged
  # from a hostname, because both agents run on the one guest a loopback
  # upload_url allows.
  secondAgentId = "e2e-two";

  bucket = "metsuke";
  # `KEY_PREFIX` in crates/metsuke-server/src/archive.rs, which owns what an
  # object key looks like.
  keyPrefix = "v1/";
  # Garage's own name for the single region it serves; the server signs against
  # it, so the two have to be the same word.
  region = "garage";
  keyName = "metsuke";
  # Imported rather than created, so the credentials exist before the cluster
  # does and the server's environmentFile can be a file rather than something
  # the test script writes. Throwaway: this bucket lives for one boot.
  accessKeyId = "GK1122334455667788990011aa";
  secretAccessKey = "0011223344556677889900112233445566778899001122334455667788990011";
  rpcSecret = "5c1915fa04d0b6739675c61bf5907eb0fe3d9c69850c83820f51b4d25d13868c";

  # The developer account the download subtest authenticates as.
  developerUser = "metsuke-dev";
  developerPassword = "not-a-real-secret";
  password = pkgs.writeText "password" developerPassword;

  awsEnvironment = pkgs.writeText "aws-environment" ''
    AWS_ACCESS_KEY_ID=${accessKeyId}
    AWS_SECRET_ACCESS_KEY=${secretAccessKey}
  '';

  # Every payload line the bucket holds for one schema, one per output line. The
  # object is the raw signed body and nothing else (ADR 0005), so reading it back
  # is od, zstd and jq: no metsuke code is on this side of the assertion, which
  # is what lets it say the archive carries the node's own metrics and lines
  # rather than that metsuke agrees with itself. Objects declaring another schema
  # drop out at the version.
  archivedPayload =
    schema:
    pkgs.writeShellScript "archived-payload-v${toString schema}" ''
      set -euo pipefail
      export PATH=${
        pkgs.lib.makeBinPath [
          pkgs.awscli2
          pkgs.zstd
          pkgs.jq
          pkgs.coreutils
          pkgs.findutils
        ]
      }
      set -a
      . ${awsEnvironment}
      set +a
      export AWS_DEFAULT_REGION=${region}
      rm -rf /tmp/archive
      mkdir -p /tmp/archive
      aws --endpoint-url http://127.0.0.1:${toString s3Port} \
        s3 sync s3://${bucket}/${keyPrefix} /tmp/archive >/dev/null
      find /tmp/archive -name '*.jsonl.zst' -print0 |
        while IFS= read -r -d "" object; do
          # The header rides uncompressed in a leading skippable frame: its
          # length is the u32 at offset 4, and the JSON follows at offset 8. No
          # decompressor is involved in reading it, which is the point of the
          # frame. `zstd -dcq` then skips it and emits the payload alone: for
          # schema 1 the scrape rows, for schema 2 the node's trace lines, each
          # with its provenance.
          #
          # The offsets are `envelope::HEADER_OFFSET` restated in shell, because
          # this side of the assertion runs no metsuke code. Nothing keeps them
          # in step: a container whose prefix changes shape changes them here.
          length=$(od --address-radix=n --format=u4 --skip-bytes=4 --read-bytes=4 "$object" | tr -d ' ')
          header=$(tail -c +9 "$object" | head -c "$length")
          if [ "$(jq -r .schema_version <<<"$header")" = ${toString schema} ]; then
            zstd -dcq "$object"
          fi
        done
    '';

  archivedScrapes = archivedPayload 1;
  archivedLines = archivedPayload 2;

  # The chain is the demo's and the tracing is the recording's, so the node boots
  # as an operator who has not reached step 4 yet.
  #
  # `activeSlots` is what makes it lead often enough to be certain of a block: at
  # the demo's own 0.05, pool1's third of the stake leads about one slot in
  # sixty, and a five-minute window was a coin toss this test lost. Shelley wants
  # `epochLength == 10k/f`, so the two move together. The same gate
  # devnet/flake.nix's `devnet-setup` applies, which is a fifth copy of what
  # metsuke-4zo.60 tracks.
  nodeConfig =
    pkgs.runCommand "leios-node-config"
      {
        nativeBuildInputs = [
          pkgs.yq-go
          pkgs.jq
        ];
      }
      # Genesis beside config.json because the demo's config addresses those
      # files as ./<era>-genesis.json; the YAML through yq because cardano-node
      # reads JSON too and plain jq cannot otherwise address the empty-string
      # TraceOptions key; the topology template as it is because this node has
      # no peers to fill into it. Genesis paths go absolute because the node
      # reads a copy of this config from `configPath`, not from here.
      ''
        mkdir -p $out
        cp ${devnetSrc}/config/genesis/*.json $out/
        chmod u+w $out/*.json
        jq --argjson f ${activeSlots} \
          '.activeSlotsCoeff = $f | .epochLength = (10 * .securityParam / $f | floor)' \
          ${devnetSrc}/config/genesis/shelley-genesis.json >$out/shelley-genesis.json
        jq -e '.epochLength == (10 * .securityParam / .activeSlotsCoeff)' \
          $out/shelley-genesis.json >/dev/null || {
          echo "error: epochLength is not 10k/f in $out/shelley-genesis.json" >&2
          exit 1
        }
        yq -o=json . ${devnetSrc}/config/config.yaml >devnet.json
        jq -s --arg dir "$out" \
          '.[0] as $devnet
           | .[1] as $spo
           | ($spo | with_entries(select(.key | startswith("TraceOption")))) as $tracing
           | ($devnet | with_entries(select(.key | startswith("TraceOption") | not)))
           + $tracing
           + { TraceOptionNodeName: "e2e" }
           | with_entries(
               if (.key | endswith("GenesisFile"))
               then .value = "\($dir)/\(.value)"
               else . end)' \
          devnet.json ${preprodConfigFile} >$out/config.json
        jq '.' ${devnetSrc}/config/topology.template.json >$out/topology.json
      '';

  poolIdRecorded =
    pkgs.runCommand "pool-id-is-the-pinned-one" { nativeBuildInputs = [ cardano-cli ]; }
      ''
        derived=$(cardano-cli latest stake-pool id --output-bech32 \
          --cold-verification-key-file ${poolKeys}/cold.vkey)
        [ "$derived" = "${poolId}" ] || {
          echo "nix/e2e-test.nix records ${poolId}; the pinned tag's pool1 is $derived"
          exit 1
        }
        touch $out
      '';

  test = pkgs.testers.runNixOSTest {
    name = "metsuke-e2e";

    nodes.e2e =
      { config, ... }:
      let
        # The second agent's own config: same pool, same key, a different
        # machine name and its own spool. Run as a plain unit rather than a
        # second module instance. What the module renders for one agent is
        # covered by `metsuke.service`, and this is about the server.
        secondConfig = (pkgs.formats.toml { }).generate "metsuke-two.toml" {
          pool_id = poolId;
          agent_id = secondAgentId;
          metrics_url = "http://127.0.0.1:${toString metricsPort}/metrics";
          upload_url = "http://127.0.0.1:${toString listenPort}/v1/submit";
          scrape_interval_secs = 1;
          upload_interval_secs = 5;
          upload_jitter_max_secs = 0;
          sntp_servers = [ ];
          spool_path = "/var/lib/metsuke-two/spool.sqlite";
        };
      in
      {
        imports = [
          agentModule
          serverModule
        ];

        systemd.services.metsuke-two = {
          description = "metsuke telemetry agent, second machine";
          wantedBy = [ "multi-user.target" ];
          serviceConfig = {
            ExecStart = pkgs.lib.concatStringsSep " " [
              "${config.services.metsuke.package}/bin/metsuke"
              "--config ${secondConfig}"
              "--signing-key ${poolKeys}/cold.skey"
            ];
            StateDirectory = "metsuke-two";
            Restart = "always";
            RestartSec = 2;
          };
        };

        virtualisation = {
          memorySize = 4096;
          cores = 4;
          # Garage refuses to start with less than a gigabyte free.
          diskSize = 4096;
        };

        environment.systemPackages = [
          serverPackage
          pkgs.awscli2
          pkgs.curl
        ];

        # `C+` at boot only, so the merged configuration the test script writes
        # survives the restart it then does.
        systemd.tmpfiles.rules = [
          "d ${dirOf configPath} 0755 root root -"
          "C+ ${configPath} 0644 root root - ${nodeConfig}/config.json"
        ];

        systemd.services.genesis-clock = {
          before = [ "cardano-node.service" ];
          requiredBy = [ "cardano-node.service" ];
          serviceConfig = {
            Type = "oneshot";
            RemainAfterExit = true;
            ExecStart = "${pkgs.coreutils}/bin/date -s @${toString (genesisStart + 60)}";
          };
        };
        services.timesyncd.enable = false;

        systemd.services.cardano-node = {
          wantedBy = [ "multi-user.target" ];
          serviceConfig = {
            # The keys are world-readable in the store and cardano-node refuses
            # one that is; RuntimeDirectory exists before this runs, and it runs
            # as the same dynamic user the node does.
            ExecStartPre = pkgs.lib.concatStringsSep " " [
              "${pkgs.coreutils}/bin/install -m0400 -t /run/cardano-node"
              "${poolKeys}/vrf.skey"
              "${poolKeys}/kes.skey"
              "${poolKeys}/bls.skey"
              "${poolKeys}/opcert.cert"
            ];
            ExecStart = pkgs.lib.concatStringsSep " " [
              "${cardano-node}/bin/cardano-node run"
              "--config ${configPath}"
              "--topology ${nodeConfig}/topology.json"
              "--database-path /var/lib/cardano-node/db"
              "--socket-path /run/cardano-node/node.socket"
              "--host-addr 127.0.0.1"
              "--port 3001"
              # Forging, so block adoption happens at all: two of the three
              # namespaces the agent ships selecting are adoption events, and a
              # node that produces no block emits neither (metsuke-jfb.20).
              "--shelley-vrf-key /run/cardano-node/vrf.skey"
              "--shelley-kes-key /run/cardano-node/kes.skey"
              "--shelley-bls-key /run/cardano-node/bls.skey"
              "--shelley-operational-certificate /run/cardano-node/opcert.cert"
            ];
            # The Leios ledger's path in config.json is relative, so it lands here
            # rather than in the root directory.
            WorkingDirectory = "/var/lib/cardano-node";
            StateDirectory = "cardano-node";
            RuntimeDirectory = "cardano-node";
            DynamicUser = true;
          };
        };

        services.garage = {
          enable = true;
          package = pkgs.garage;
          settings = {
            replication_factor = 1;
            db_engine = "sqlite";
            rpc_bind_addr = "127.0.0.1:3901";
            rpc_public_addr = "127.0.0.1:3901";
            rpc_secret = rpcSecret;
            s3_api = {
              s3_region = region;
              api_bind_addr = "127.0.0.1:${toString s3Port}";
            };
          };
        };

        services.metsuke = {
          enable = true;
          # The pool's cold key, as the demo ships it: the pool id the server
          # files under is the hash of this key.
          signingKeyFile = "${poolKeys}/cold.skey";
          restartSecs = 2;
          settings = {
            pool_id = poolId;
            metrics_url = "http://127.0.0.1:${toString metricsPort}/metrics";
            upload_url = "http://127.0.0.1:${toString listenPort}/v1/submit";
            scrape_interval_secs = 1;
            # The first submissions meet a bucket that has no layout yet; this is
            # how long the retry that follows takes (ADR 0004).
            upload_interval_secs = 5;
            upload_jitter_max_secs = 0;
            # A name no test network can resolve would cost every scrape the SNTP
            # timeout.
            sntp_servers = [ ];
            # The node writes its traces to the journal, so this is where the
            # group grant and the journalctl child are exercised on real parts.
            log.journal_unit = "cardano-node.service";
          };
        };

        services.metsuke-server = {
          enable = true;
          environmentFile = "${awsEnvironment}";
          developerPasswordFile = "${password}";
          restartSecs = 2;
          settings = {
            listen = "127.0.0.1:${toString listenPort}";
            http = {
              idle_timeout_ms = 30000;
              read_timeout_ms = 60000;
              write_timeout_ms = 60000;
              max_concurrent_requests = 64;
            };
            archive.s3 = {
              inherit bucket region;
              endpoint = "http://127.0.0.1:${toString s3Port}";
              request_timeout_ms = 30000;
              signature_validity_secs = 300;
              put_retries = 1;
              put_retry_backoff_ms = 500;
              list_max_pages = 1000;
            };
            ingest = {
              allowlist.${poolId} = "MUSA-0000";
              max_body_bytes = 4194304;
              max_header_bytes = 4096;
              max_timestamp_skew_secs = 300;
              rate_limit_uploads = 240;
              rate_limit_uploads_total = 2400;
              rate_limit_window_secs = 3600;
            };
            developer = {
              user = developerUser;
              list_max_rows = 1000;
            };
          };
        };
      };

    testScript = ''
      import html
      import json
      from collections import Counter
      from datetime import timedelta


      # The recording's `"maxFrequency": 2.0` echoes back as 2.
      def asNodeRenders(value):
          if isinstance(value, dict):
              return {key: asNodeRenders(inner) for key, inner in value.items()}
          if isinstance(value, float) and value.is_integer():
              return int(value)
          return value


      # The namespaces this node reaches of the ones the shipped agent selects:
      # one Leios line, which is a start rather than a round (docs/adr/0010), and
      # both block adoption events, which is why the node forges.
      selected = [
          "Consensus.LeiosKernel.Msg",
          "ChainDB.AddBlockEvent.AddedToCurrentChain",
          "Forge.Loop.AdoptedBlock",
      ]

      start_all()

      with subtest("the bucket the server was configured for exists"):
          e2e.wait_for_unit("garage.service")
          e2e.wait_for_open_port(${toString s3Port}, addr = "127.0.0.1")
          node_id = e2e.succeed("garage node id -q").split("@")[0].strip()
          e2e.succeed(f"garage layout assign -z e2e -c 1G {node_id}")
          e2e.succeed("garage layout apply --version 1")
          e2e.succeed("garage bucket create ${bucket}")
          e2e.succeed(
              "garage key import --yes -n ${keyName} ${accessKeyId} ${secretAccessKey}"
          )
          e2e.succeed("garage bucket allow --read --write ${bucket} --key ${keyName}")

      with subtest("the node serves its own loopback Prometheus endpoint"):
          e2e.wait_for_unit("cardano-node.service")
          e2e.wait_for_open_port(${toString metricsPort}, addr = "127.0.0.1")

      with subtest("the server accepts the allowlisted pool"):
          e2e.wait_for_unit("metsuke-server.service")
          e2e.wait_for_open_port(${toString listenPort}, addr = "127.0.0.1")
          e2e.wait_until_succeeds(
              "journalctl -u metsuke-server.service | grep -q 'accepting 1 pools'"
          )

      with subtest("a scrape reaches the bucket as a signed object, and is acked"):
          e2e.wait_for_unit("metsuke.service")
          # The whole path: the agent scraped the node, signed the submission,
          # the server resolved the cold key to the pool id, PUT the bytes, and
          # answered.
          e2e.wait_until_succeeds(
              "journalctl -u metsuke.service | grep -qE 'submission [0-9]+ accepted'",
              timeout = timedelta(minutes = 5),
          )
          listing = e2e.succeed(
              "set -a; . ${awsEnvironment}; AWS_DEFAULT_REGION=${region};"
              " aws --endpoint-url http://127.0.0.1:${toString s3Port}"
              " s3api list-objects-v2 --bucket ${bucket} --prefix ${keyPrefix}"
              " --query 'Contents[].Key' --output text"
          )
          assert "-${poolId}-${agentId}-metrics.jsonl.zst" in listing, listing
          # And the object holds the scrape rather than only being named for it:
          # a metric the node's own endpoint states, read back with zstd and jq
          # and no metsuke code. build_info because the node serves it from its
          # first body, before it has a chain to report a height for.
          # `grep -c` for the same reason the trace-line subtest below gives.
          e2e.wait_until_succeeds(
              "${archivedScrapes} |"
              " grep -cF '\"name\":\"cardano_node_metrics_cardano_build_info\"'"
              " >/dev/null",
              timeout = timedelta(minutes = 5),
          )

      with subtest("a second machine reporting for the same pool also lands"):
          # The motivating bug: one pool, two agents. Nothing here is keyed per
          # pool alone, so both objects are in the bucket under their own agent
          # id (metsuke-jfb.4).
          e2e.wait_for_unit("metsuke-two.service")
          e2e.wait_until_succeeds(
              "journalctl -u metsuke-two.service | grep -qE 'submission [0-9]+ accepted'",
              timeout = timedelta(minutes = 5),
          )
          listing = e2e.succeed(
              "set -a; . ${awsEnvironment}; AWS_DEFAULT_REGION=${region};"
              " aws --endpoint-url http://127.0.0.1:${toString s3Port}"
              " s3api list-objects-v2 --bucket ${bucket} --prefix ${keyPrefix}"
              " --query 'Contents[].Key' --output text"
          )
          for agent in ["${agentId}", "${secondAgentId}"]:
              assert f"-${poolId}-{agent}-metrics.jsonl.zst" in listing, listing

      with subtest("the agent reads the node's journal"):
          # The grant and the child, before any line has to have travelled
          # them: a failure here and a failure below are different repairs.
          e2e.succeed(
              "systemctl cat metsuke.service | grep -qx 'SupplementaryGroups=systemd-journal'"
          )
          e2e.wait_until_succeeds(
              "journalctl -u metsuke.service"
              " | grep -q 'collecting trace lines from cardano-node.service'"
          )
          e2e.fail("journalctl -u metsuke.service | grep -q 'trace lines not collected'")

      with subtest("the page's node-config snippets merge into an SPO's own config"):
          # Off the served page rather than restated here, so what this applies
          # is what an operator pastes.
          page = e2e.succeed("curl -sS http://127.0.0.1:${toString listenPort}/")
          snippets = []
          for block in page.split("<pre>")[1:]:
              try:
                  parsed = json.loads(html.unescape(block.split("</pre>")[0]))
              except json.JSONDecodeError:
                  continue
              if "TraceOptions" in parsed:
                  snippets.append(parsed["TraceOptions"])
          assert len(snippets) == 2, page
          backend_step, trace_step = snippets

          def kind(backend):
              return backend.split(" ")[0]

          def apply_steps(options):
              # The page's steps 4 and 5 as their prose reads.
              options = json.loads(json.dumps(options))
              replaced = {kind(backend) for backend in backend_step[""]["backends"]}
              options[""]["backends"] = [
                  backend
                  for backend in options[""]["backends"]
                  if kind(backend) not in replaced
              ] + backend_step[""]["backends"]
              options.update(trace_step)
              return options

          def assert_merge_kept(before, after):
              # What merging means and pasting over does not: the operator's own
              # keys survive it (metsuke-jfb.21).
              untouched = {
                  key: value
                  for key, value in before.items()
                  if key != "" and key not in trace_step
              }
              assert len(untouched) > 1, untouched
              for key, value in untouched.items():
                  assert after[key] == value, (key, after[key], value)
              for key in ["severity", "detail"]:
                  assert after[""][key] == before[""][key], after[""]
              for backend in before[""]["backends"]:
                  if kind(backend) not in {kind(b) for b in backend_step[""]["backends"]}:
                      assert backend in after[""]["backends"], after[""]
              for backend in backend_step[""]["backends"]:
                  assert backend in after[""]["backends"], after[""]
              # And no kind is named twice: the node reads only the first.
              kinds = [kind(backend) for backend in after[""]["backends"]]
              assert len(kinds) == len(set(kinds)), after[""]
              assert trace_step, "the trace snippet named no namespace"
              for namespace, entry in trace_step.items():
                  assert after[namespace] == entry, (namespace, after[namespace])
              return untouched

          recorded = json.loads(e2e.succeed("cat ${preprodConfigFile}"))["TraceOptions"]
          config = json.loads(e2e.succeed("cat ${configPath}"))
          assert asNodeRenders(config["TraceOptions"]) == asNodeRenders(recorded), (
              "the node did not boot the recorded tracing"
          )

          merged = apply_steps(recorded)
          untouched = assert_merge_kept(recorded, merged)
          config["TraceOptions"] = merged
          e2e.succeed(
              "cat > ${configPath} <<'MERGED'\n" + json.dumps(config, indent=2) + "\nMERGED"
          )

          # Against the recording's own root both replacements are no-ops -- its
          # Stdout backend is already MachineFormat and its PrometheusSimple is
          # already on the page's port -- so a merge that only appended would
          # pass. This root makes both happen (metsuke-jfb.23).
          def diverge(backend):
              if kind(backend) == "Stdout":
                  return "Stdout HumanFormatColoured"
              if kind(backend) == "PrometheusSimple":
                  return " ".join(backend.split(" ")[:-1] + ["19999"])
              return backend

          divergent = json.loads(json.dumps(recorded))
          divergent[""]["backends"] = [
              diverge(backend) for backend in divergent[""]["backends"]
          ]
          for backend in backend_step[""]["backends"]:
              assert backend not in divergent[""]["backends"], divergent[""]
          assert_merge_kept(divergent, apply_steps(divergent))

      with subtest("the node runs what the merge produced"):
          # The node's own reading, not this script's: it echoes the TraceOptions
          # it resolved on every start. Scoped to one invocation, so the
          # pre-merge start's report cannot answer for the post-merge one.
          #
          # This restart is also the start the trace subtest below reads back:
          # the agent follows from the journal's end, so the node's first start
          # was already past when it attached, and a start is where a node with
          # no peers says anything the selection rules want.
          # Before the restart takes it away: the recording on its own already
          # emits every namespace the agent selects, which is what
          # docs/research/cardano-node-11-tracing.md claims of the config SPOs
          # run. Read off the boot invocation, which ran the recording unmerged.
          boot = e2e.succeed(
              "systemctl show -p InvocationID --value cardano-node.service"
          ).strip()
          for namespace in selected:
              e2e.wait_until_succeeds(
                  f"journalctl _SYSTEMD_INVOCATION_ID={boot} -o cat"
                  f" | grep -qF '\"ns\":\"{namespace}\"'",
                  timeout = timedelta(minutes = 2),
              )

          e2e.succeed("systemctl restart cardano-node.service")
          e2e.wait_for_open_port(${toString metricsPort}, addr = "127.0.0.1")

          # The endpoint the page's own backend line opens serves the names the
          # agent reads. Until the merge, the recording's `PrometheusSimple
          # suffix ...` was what every metrics subtest above scraped, and the
          # flag word decides whether a name carries its type suffix -- so
          # without this, nothing tests the form the page ships.
          # crates/metsuke/src/scrape.rs owns which names those are.
          e2e.wait_until_succeeds(
              "curl -sS http://127.0.0.1:${toString metricsPort}/metrics"
              " | grep -c cardano_node_metrics_blockNum_int >/dev/null"
          )

          invocation = e2e.succeed(
              "systemctl show -p InvocationID --value cardano-node.service"
          ).strip()
          journal = f"journalctl _SYSTEMD_INVOCATION_ID={invocation} -o cat"
          e2e.wait_until_succeeds(f"{journal} | grep -q Reflection.TracerConfigInfo")

          report = e2e.succeed(
              f"{journal} | grep '\"ns\":\"Reflection.TracerConfigInfo\"'"
          ).splitlines()
          resolved = json.loads(report[-1])["data"]["conf"]["Options"]

          # Backends as a set: the echo is the parsed config re-rendered, and
          # nothing here depends on its order.
          assert sorted(resolved[""]["backends"]) == sorted(merged[""]["backends"]), (
              resolved[""]
          )
          for key, value in merged.items():
              if key == "":
                  value = {k: v for k, v in value.items() if k != "backends"}
                  got = {k: v for k, v in resolved[""].items() if k != "backends"}
              else:
                  got = resolved.get(key)
              assert asNodeRenders(got) == asNodeRenders(value), (key, got, value)

          # A namespace this node version does not have is reported as illegal
          # rather than refusing the start, so without this a snippet naming one
          # would be silent (metsuke-jfb.20). The trace is written only when
          # there are complaints, so no line is the clean case.
          warnings = e2e.succeed(
              f"{journal} | grep '\"ns\":\"Reflection.TracerConsistencyWarnings\"' || true"
          )
          for namespace in trace_step:
              assert f"Illegal namespace {namespace}" not in warnings, warnings

          # A key the operator had and step 5 does not name still reaches a
          # backend, which the echo above cannot say: `Startup.DiffusionInit` is
          # Info in the recording and fires on every start. A prefix, not an
          # exact ns: an entry governs its subtree, and what the node writes is
          # `Startup.DiffusionInit.ListeningServerSocket` and its siblings.
          survivor = "Startup.DiffusionInit"
          assert survivor in untouched, untouched
          e2e.wait_until_succeeds(
              f"{journal} | grep -qF '\"ns\":\"{survivor}.'",
              timeout = timedelta(minutes = 2),
          )

          # A silence the operator set is still a silence.
          silenced = [
              key for key, value in untouched.items() if value.get("severity") == "Silence"
          ]
          assert silenced, recorded
          for namespace in silenced:
              e2e.fail(f"{journal} | grep -q '\"ns\":\"{namespace}'")

      with subtest("the node's own trace lines reach the archive"):
          # `grep -c`, not `grep -q`: -q exits on the first match, and the
          # SIGPIPE that gives zstd trips `archivedLines`' own pipefail, so a
          # present line reports absent. -c reads the stream to its end and
          # still exits non-zero on no match.
          for namespace in selected:
              e2e.wait_until_succeeds(
                  f"${archivedLines} | grep -cF '\"ns\":\"{namespace}\"' >/dev/null",
                  timeout = timedelta(minutes = 5),
              )
          archived = [json.loads(line) for line in e2e.succeed("${archivedLines}").splitlines()]
          namespaces = Counter(line["ns"] for line in archived)
          for namespace in selected:
              assert namespaces[namespace] > 0, namespaces
          # Nothing outside the configured namespaces rides along, whatever
          # severity it carries: Reflection is under no configured namespace, so
          # severity alone selects nothing.
          assert not [ns for ns in namespaces if ns.startswith("Reflection.")], namespaces
          # And it selected rather than forwarded: the node's Debug volume,
          # which nobody asked for, is not in the bucket.
          assert not [line for line in archived if line["sev"] == "Debug"], namespaces
          # Every line says which pool and machine wrote it (metsuke-jfb.3).
          assert all(
              line["metsuke"]["pool_id"] == "${poolId}"
              and line["metsuke"]["agent_id"] == "${agentId}"
              for line in archived
          ), archived[0]

      with subtest("a developer downloads a stored object through the server's own routes"):
          # Garage is a real S3, not the tiny_http double `tests/s3.rs` GETs
          # against, so this is the one place `ObjectStream.length` and
          # `ArchiveError::EndpointUnusable` run against the class of endpoint
          # that could break them (metsuke-jfb.28).
          auth = "${developerUser}:${developerPassword}"
          listing = json.loads(
              e2e.succeed(
                  f"curl -sSf -u {auth}"
                  " http://127.0.0.1:${toString listenPort}/v1/submissions"
              )
          )
          assert listing["keys"], listing
          key = listing["keys"][0]
          e2e.succeed(
              f"curl -sSf -u {auth}"
              f" 'http://127.0.0.1:${toString listenPort}/v1/object?key={key}'"
              " -o /tmp/downloaded.zst"
          )
          e2e.succeed(
              "set -a; . ${awsEnvironment}; AWS_DEFAULT_REGION=${region};"
              f" aws --endpoint-url http://127.0.0.1:${toString s3Port}"
              f" s3 cp s3://${bucket}/{key} /tmp/direct.zst"
          )
          # Byte-identical to the copy taken straight off the bucket: the
          # download route hands back the stored bytes verbatim
          # (`archive::Bytes`), and the subtest below has `verify::verify`
          # signature-check an S3 fetch of the same archive, so identical
          # bytes here means the download route hands out bytes that verify.
          e2e.succeed("cmp /tmp/downloaded.zst /tmp/direct.zst")

      with subtest("the stored bytes verify against the key that signed them"):
          # Last, and after the units stop: this opens the server's index as
          # root, and the service could not read what root left behind.
          e2e.succeed("systemctl stop metsuke.service metsuke-server.service")
          config = e2e.succeed(
              "systemctl cat metsuke-server.service | grep -oP '(?<=--config )\\S+'"
          ).strip()
          # `succeed` is the whole assertion: the command exits non-zero on an
          # archive that verified nothing as well as on one that failed to.
          e2e.succeed(
              f"set -a; . ${awsEnvironment}; metsuke-server --config {config} verify-archive"
          )
    '';
  };
in
# One agent scrapes one URL across the pre-merge node and the post-merge one, so
# this test can only run while the two ports agree. Divergence is no operator's
# problem, since step 4 tells them which to keep. It is this test that would
# then need two URLs.
assert lib.assertMsg (metricsPort == pagePort) (
  "the recording opens port ${toString metricsPort} and the page's snippet opens "
  + "${toString pagePort}: nix/e2e-test.nix scrapes one port across both"
);
{
  inherit test poolIdRecorded;
}
