# Both units under the confinement nix/unit.nix states: each starts from a
# configuration its module rendered, does the one thing it may do, and holds
# none of the privileges ADR 0007 refuses. The `bare` node runs
# contrib/metsuke.service itself, so what a host that is not NixOS copies is
# executed and not only diffed. A submission travelling from one unit to the
# other is not here — that is e2e-test.nix.
{
  pkgs,
  agentModule,
  serverModule,
  metrics,
  contribUnit,
  # The binary contrib/metsuke.service names at /usr/local/bin/metsuke. The
  # module nodes take theirs from the module's own default.
  agent,
}:
let
  metricsPort = 12798;
  listenPort = 8080;
  # Bech32 over 28 zero bytes. Nothing here signs for it; it is the pool id
  # both halves have to accept at load.
  poolId = "pool1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq8a7a2d";
  # Throwaway: what is exercised is that systemd hands the file over and the
  # agent parses it, not what it signs.
  signingKey = pkgs.writeText "pool.skey" (
    builtins.toJSON {
      type = "StakePoolSigningKey_ed25519";
      description = "";
      cborHex = "5820${pkgs.lib.strings.replicate 32 "07"}";
    }
  );
  # The server reads k out of this and nothing else out of the file.
  shelleyGenesis = pkgs.writeText "shelley-genesis.json" (builtins.toJSON { securityParam = 108; });
  password = pkgs.writeText "password" "not-a-real-secret";

  # Named where both the config and the assertions can reach it.
  stateDirectory = "/var/lib/metsuke-server";

  # Stands in for cardano-node's PrometheusSimple endpoint: the body is a
  # recording of the Leios prototype's, so the agent parses what it will meet.
  metricsEndpoint = {
    wantedBy = [ "multi-user.target" ];
    serviceConfig.ExecStart =
      let
        served = pkgs.runCommand "metrics-endpoint-root" { } ''
          mkdir -p $out
          cp ${metrics} $out/metrics
        '';
      in
      "${pkgs.python3}/bin/python3 -m http.server ${toString metricsPort} --bind 127.0.0.1 --directory ${served}";
  };

  metricsUrl = "http://127.0.0.1:${toString metricsPort}/metrics";
  # Nothing listens: the upload is expected to fail and the samples to stay
  # spooled, which is what ADR 0004 asks for.
  uploadUrl = "http://127.0.0.1:1/v1/submit";

  # What `environmentFile` is for. No bucket is reached, so the values only
  # have to be present.
  awsEnvironment = pkgs.writeText "aws-environment" ''
    AWS_ACCESS_KEY_ID=not-a-real-key
    AWS_SECRET_ACCESS_KEY=not-a-real-secret
  '';

  # One server, one archive: the two nodes below differ in that and nothing
  # else, so every other limit is stated once.
  serverNode =
    {
      archive,
      environmentFile ? null,
    }:
    {
      imports = [ serverModule ];

      environment.etc = {
        "metsuke-server/calidus-password" = {
          source = password;
          mode = "0400";
        };
        "metsuke-server/developer-password" = {
          source = password;
          mode = "0400";
        };
      };

      services.metsuke-server = {
        enable = true;
        inherit environmentFile;
        calidusPasswordFile = "/etc/metsuke-server/calidus-password";
        developerPasswordFile = "/etc/metsuke-server/developer-password";
        settings = {
          inherit archive;
          listen = "127.0.0.1:${toString listenPort}";
          # Every limit the server refuses to start without, set through the
          # module: an option whose name has drifted from the Rust field
          # renders a key the server then refuses, which is what this listing
          # catches.
          ingest = {
            allowlist.${poolId} = "MUSA-0000";
            max_body_bytes = 1048576;
            max_decompressed_bytes = 4194304;
            rate_limit_uploads = 24;
            rate_limit_window_secs = 3600;
            max_timestamp_skew_secs = 300;
          };
          calidus = {
            socket_dir = "/run/postgresql";
            dbname = "cexplorer";
            role = "metsuke_ro";
            query_timeout_secs = 30;
            shelley_genesis_path = "${shelleyGenesis}";
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
in
pkgs.testers.runNixOSTest {
  name = "metsuke-units";

  nodes.pool = {
    imports = [ agentModule ];

    systemd.services.metrics-endpoint = metricsEndpoint;

    # Root-only, so the agent reaching it proves systemd passed it as a
    # credential rather than the service user reading the path.
    environment.etc."metsuke/pool.skey" = {
      source = signingKey;
      mode = "0400";
    };

    environment.systemPackages = [ pkgs.sqlite ];

    services.metsuke = {
      enable = true;
      signingKeyFile = "/etc/metsuke/pool.skey";
      settings = {
        pool_id = poolId;
        metrics_url = metricsUrl;
        upload_url = uploadUrl;
        sample_interval_secs = 1;
        # A name no test network can resolve would cost every sample the SNTP
        # timeout.
        sntp_servers = [ ];
      };
    };
  };

  # No module: the file an operator copies, at the two paths its header names,
  # started the way its header says to start it. The key is named in the
  # configuration rather than loaded as a credential, which is the half the
  # module's LoadCredential replaces and nothing else here executes.
  nodes.bare = {
    systemd.services.metrics-endpoint = metricsEndpoint;

    environment.systemPackages = [ pkgs.sqlite ];

    environment.etc = {
      "metsuke/metsuke.service".source = contribUnit;
      # Readable by the DynamicUser the unit runs as: with no credential in
      # play, the service opens this path itself.
      "metsuke/pool.skey" = {
        source = signingKey;
        mode = "0444";
      };
      "metsuke/config.toml".text = ''
        pool_id = "${poolId}"
        metrics_url = "${metricsUrl}"
        upload_url = "${uploadUrl}"
        signing_key = "/etc/metsuke/pool.skey"
        sample_interval_secs = 1
        sntp_servers = []
      '';
    };

    systemd.tmpfiles.rules = [
      "L+ /usr/local/bin/metsuke - - - - ${agent}/bin/metsuke"
    ];
  };

  nodes.hub = serverNode { archive.filesystem.root = "${stateDirectory}/archive"; };

  # The other archive kind. Nothing is served over it — startup touches no
  # bucket — so what this node proves is that the module renders an `[archive]`
  # the server accepts, which the filesystem node cannot show for the S3 fields
  # it does not set.
  nodes.hubs3 = serverNode {
    environmentFile = awsEnvironment;
    archive.s3 = {
      bucket = "cardano-playground-metsuke";
      region = "eu-central-1";
      endpoint = "http://127.0.0.1:1";
      request_timeout_secs = 30;
      signature_validity_secs = 300;
      put_retries = 1;
      put_retry_backoff_ms = 500;
      list_max_pages = 1000;
    };
  };

  testScript = ''
    def spooled(machine):
        """Samples reached the spool.

        -readonly is not a detail: this runs as root, and a plain sqlite3
        creates the file when it is not there yet. That would hand the service
        a root-owned empty database and it would fail its next open.
        """
        machine.wait_until_succeeds(
            "test 0 -lt \"$(sqlite3 -readonly /var/lib/metsuke/spool.sqlite 'select count(*) from samples')\""
        )

    def confined(machine, unit, state):
        """The privileges a unit has to lack, and the one path it may write."""
        machine.wait_for_unit(unit)
        pid = machine.succeed(f"systemctl show --property=MainPID --value {unit}").strip()

        def field(name):
            return machine.succeed(f"grep '^{name}:' /proc/{pid}/status").split(":", 1)[1].split()

        # No group beyond the transient one DynamicUser gives the service as
        # its own identity: any other entry would be a grant on something
        # that already existed.
        assert set(field("Groups")) <= set(field("Gid")), field("Groups")
        # And the directive that refuses one is present. DynamicUser alone
        # already yields an empty Groups line, so the assertion above stays
        # green if SupplementaryGroups= is deleted from nix/unit.nix; only
        # reading the rendered unit catches that.
        machine.succeed(f"systemctl cat {unit} | grep -qx 'SupplementaryGroups='")
        for name in ["CapEff", "CapPrm", "CapBnd", "CapAmb"]:
            assert int(field(name)[0], 16) == 0, (name, field(name))

        # ProtectSystem=strict, seen from inside the mount namespace it got.
        machine.succeed(f"nsenter --target {pid} --mount -- touch {state}/writable")
        machine.fail(f"nsenter --target {pid} --mount -- touch /etc/probe")

    pool.wait_for_unit("metrics-endpoint.service")
    pool.wait_for_open_port(${toString metricsPort}, addr = "127.0.0.1")

    with subtest("the agent samples the loopback endpoint"):
        pool.wait_for_unit("metsuke.service")
        # It read the credential and named the endpoint it is sampling.
        pool.wait_until_succeeds(
            "journalctl -u metsuke.service | grep -q 'sampling http://127.0.0.1:${toString metricsPort}/metrics'"
        )
        # It scraped and spooled, which is the only thing it may write.
        spooled(pool)
        # The signing key stayed root-only, so it arrived as a credential.
        pool.succeed("test 400 -eq \"$(stat -c %a /etc/metsuke/pool.skey)\"")

    with subtest("the agent holds nothing ADR 0007 refuses"):
        confined(pool, "metsuke.service", "/var/lib/metsuke")
        # It reached the endpoint above without AF_UNIX, which is what the
        # server needs for db-sync (ADR 0009) and the agent does not.
        pool.fail("systemctl cat metsuke.service | grep -q 'AF_UNIX'")

    with subtest("the contrib unit runs the agent on a host that is not NixOS"):
        bare.wait_for_unit("metrics-endpoint.service")
        bare.wait_for_open_port(${toString metricsPort}, addr = "127.0.0.1")
        # /run rather than /etc, which is read-only here — and which is also
        # why [Install] stays unexercised: `systemctl enable` writes there.
        bare.succeed("cp /etc/metsuke/metsuke.service /run/systemd/system/metsuke.service")
        bare.succeed("systemctl daemon-reload")
        bare.succeed("systemctl start metsuke.service")
        spooled(bare)
        confined(bare, "metsuke.service", "/var/lib/metsuke")

    with subtest("the server starts on the configuration its module rendered"):
        hub.wait_for_unit("metsuke-server.service")
        hub.wait_for_open_port(${toString listenPort}, addr = "127.0.0.1")
        hub.wait_until_succeeds(
            "journalctl -u metsuke-server.service | grep -q 'accepting 1 pools'"
        )

    with subtest("the server holds nothing ADR 0007 refuses"):
        confined(hub, "metsuke-server.service", "${stateDirectory}")

    with subtest("the same server starts on the module's S3 archive"):
        hubs3.wait_for_unit("metsuke-server.service")
        hubs3.wait_for_open_port(${toString listenPort}, addr = "127.0.0.1")
  '';
}
