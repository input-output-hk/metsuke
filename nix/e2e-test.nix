# The whole path once, on real parts: a Leios node's own Prometheus endpoint,
# the agent through its module, the server through its module, and Garage as
# the bucket. Scrape, spool, signed submission, verification, object, ACK. What
# a value should be is not asserted here — the recorded fixtures under
# crates/metsuke/tests/fixtures own that; this asserts that the parts meet.
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
  metricsPort = 12798;
  listenPort = 8080;
  s3Port = 3900;

  devnetSrc = "${leios}/demo/proto-devnet";
  poolKeys = "${devnetSrc}/config/pools-keys/pool1";

  # Recorded from poolKeys/cold.vkey, not derived: a bump of the Leios pin that
  # changed the demo's pool1 would silently move a derived value, where this
  # shows up as a diff. `poolIdRecorded` below is what asserts they still agree.
  poolId = "pool1awyxt3egwmunup7nmd2uznqr54p2lgdt3tvrqetj8geqgfz26x9";

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

  # Neither is ever presented: no db-sync answers here, and no developer asks.
  password = pkgs.writeText "password" "not-a-real-secret";

  awsEnvironment = pkgs.writeText "aws-environment" ''
    AWS_ACCESS_KEY_ID=${accessKeyId}
    AWS_SECRET_ACCESS_KEY=${secretAccessKey}
  '';

  # Every trace line the bucket holds, one per output line. The object is the
  # raw signed body and nothing else (ADR 0005), so reading it back is od, zstd
  # and jq: no metsuke code is on this side of the assertion, which is what lets
  # it say the archive carries the node's lines rather than that metsuke agrees
  # with itself. Objects that are not a trace-line envelope drop out at the
  # schema version.
  archivedLines = pkgs.writeShellScript "archived-trace-lines" ''
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
    find /tmp/archive -name '*.json.zst' -print0 |
      while IFS= read -r -d "" object; do
        # The header rides uncompressed in a leading skippable frame: its
        # length is the u32 at offset 4, and the JSON follows at offset 8. No
        # decompressor is involved in reading it, which is the point of the
        # frame. `zstd -dcq` then skips it and emits the payload alone — for
        # schema 2, the node's own trace lines.
        #
        # The offsets are `envelope::HEADER_OFFSET` restated in shell, because
        # this side of the assertion runs no metsuke code. Nothing keeps them
        # in step: a container whose prefix changes shape changes them here.
        length=$(od --address-radix=n --format=u4 --skip-bytes=4 --read-bytes=4 "$object" | tr -d ' ')
        header=$(tail -c +9 "$object" | head -c "$length")
        if [ "$(jq -r .schema_version <<<"$header")" = 2 ]; then
          zstd -dcq "$object"
        fi
      done
  '';

  # The proto-devnet demo's own configuration, with the trace backends replaced
  # by a loopback PrometheusSimple one and the machine-readable stdout one the
  # agent's trace collection reads back off the journal (ADR 0010). The genesis
  # files are taken as they are: nothing here forges, so the node sits at the
  # demo's own start time, serves the endpoint and writes its startup traces.
  #
  # Why a start, not a round: docs/adr/0010.
  nodeConfig =
    pkgs.runCommand "leios-node-config"
      {
        nativeBuildInputs = [
          pkgs.yq-go
          pkgs.jq
        ];
      }
      # Genesis beside config.json, the YAML through yq, the topology template
      # taken as it is: scripts/record-scrape-fixtures.sh says why each of the
      # three has to be done that way.
      ''
        mkdir -p $out
        cp ${devnetSrc}/config/genesis/*.json $out/
        yq -o=json . ${devnetSrc}/config/config.yaml |
          jq --arg backend "PrometheusSimple 127.0.0.1 ${toString metricsPort}" \
            '.TraceOptionNodeName = "e2e"
             | .TraceOptions."".backends = ["Stdout MachineFormat", $backend]' \
            >$out/config.json
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

    nodes.e2e = {
      imports = [
        agentModule
        serverModule
      ];

      virtualisation = {
        memorySize = 4096;
        cores = 4;
        # Garage refuses to start with less than a gigabyte free.
        diskSize = 4096;
      };

      environment.systemPackages = [
        serverPackage
        pkgs.awscli2
      ];

      systemd.services.cardano-node = {
        wantedBy = [ "multi-user.target" ];
        serviceConfig = {
          ExecStart = pkgs.lib.concatStringsSep " " [
            "${cardano-node}/bin/cardano-node run"
            "--config ${nodeConfig}/config.json"
            "--topology ${nodeConfig}/topology.json"
            "--database-path /var/lib/cardano-node/db"
            "--socket-path /run/cardano-node/node.socket"
            "--host-addr 127.0.0.1"
            "--port 3001"
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
        # The pool's cold key, as the demo ships it: what the server's ADR-0003
        # check resolves the pool id from.
        signingKeyFile = "${poolKeys}/cold.skey";
        restartSecs = 2;
        settings = {
          pool_id = poolId;
          metrics_url = "http://127.0.0.1:${toString metricsPort}/metrics";
          upload_url = "http://127.0.0.1:${toString listenPort}/v1/submit";
          sample_interval_secs = 1;
          # The first submissions meet a bucket that has no layout yet; this is
          # how long the retry that follows takes (ADR 0004).
          upload_interval_secs = 5;
          upload_jitter_max_secs = 0;
          # A name no test network can resolve would cost every sample the SNTP
          # timeout.
          sntp_servers = [ ];
          # The node writes its traces to the journal, so this is where the
          # group grant and the journalctl child are exercised on real parts.
          log.journal_unit = "cardano-node.service";
        };
      };

      services.metsuke-server = {
        enable = true;
        environmentFile = awsEnvironment;
        calidusPasswordFile = password;
        developerPasswordFile = password;
        restartSecs = 2;
        settings = {
          listen = "127.0.0.1:${toString listenPort}";
          archive.s3 = {
            inherit bucket region;
            endpoint = "http://127.0.0.1:${toString s3Port}";
            request_timeout_secs = 30;
            signature_validity_secs = 300;
            put_retries = 1;
            put_retry_backoff_ms = 500;
            list_max_pages = 1000;
          };
          ingest = {
            allowlist.${poolId} = "MUSA-0000";
            max_body_bytes = 1048576;
            max_header_bytes = 4096;
            max_decompressed_bytes = 4194304;
            rate_limit_uploads = 240;
            rate_limit_window_secs = 3600;
            max_timestamp_skew_secs = 300;
          };
          calidus = {
            # Nothing reaches db-sync here: the cold key answers ADR 0003 before
            # any chain question is asked.
            socket_dir = "/run/postgresql";
            dbname = "cexplorer";
            role = "metsuke_ro";
            query_timeout_secs = 30;
            shelley_genesis_path = "${devnetSrc}/config/genesis/shelley-genesis.json";
            resolution_ttl_secs = 3600;
            max_registrations = 10;
          };
          developer = {
            user = "metsuke-dev";
            list_max_rows = 1000;
          };
        };
      };
    };

    testScript = ''
      import json
      from collections import Counter
      from datetime import timedelta

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
              "journalctl -u metsuke.service | grep -q 'batch acked'",
              timeout = timedelta(minutes = 5),
          )
          listing = e2e.succeed(
              "set -a; . ${awsEnvironment}; AWS_DEFAULT_REGION=${region};"
              " aws --endpoint-url http://127.0.0.1:${toString s3Port}"
              " s3api list-objects-v2 --bucket ${bucket} --prefix ${keyPrefix}"
              " --query 'Contents[].Key' --output text"
          )
          assert "${keyPrefix}${poolId}/" in listing, listing

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

      with subtest("the node's own trace lines reach the archive"):
          # The agent follows from the journal's end, so this node's first
          # start was already past when it attached. Restarting the node is a
          # start it does not miss, and a start is where a node with no peers
          # says anything the selection rules want.
          e2e.succeed("systemctl restart cardano-node.service")
          e2e.wait_for_open_port(${toString metricsPort}, addr = "127.0.0.1")
          # Waiting on the Leios line waits on all of them: the tracing system
          # names itself before consensus starts, and a batch is taken oldest
          # first.
          e2e.wait_until_succeeds(
              "${archivedLines} | grep -qF 'Consensus.LeiosKernel.Msg'",
              timeout = timedelta(minutes = 5),
          )
          archived = [json.loads(line) for line in e2e.succeed("${archivedLines}").splitlines()]
          namespaces = Counter(line["ns"] for line in archived)
          # The namespace rule, on the one Leios namespace a node with no peers
          # reaches. Why a start, not a round: docs/adr/0010.
          assert namespaces["Consensus.LeiosKernel.Msg"] > 0, namespaces
          # The severity rule, which is the developers' other ask and reaches
          # past every namespace in the list.
          assert namespaces["Reflection.TracerConfigInfo"] > 0, namespaces
          # And it selected rather than forwarded: the node's Debug volume,
          # which nobody asked for, is not in the bucket.
          assert not [line for line in archived if line["sev"] == "Debug"], namespaces

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
{
  inherit test poolIdRecorded;
}
