# Both units under the confinement nix/unit.nix states: each starts from a
# configuration its module rendered, does the one thing it may do, and holds
# none of the privileges ADR 0007 refuses. The `bare` node runs
# contrib/metsuke.service itself, so what a host that is not NixOS copies is
# executed and not only diffed. A submission travelling from one unit to the
# other is not here. That is e2e-test.nix.
#
# Every node here needs a booted machine to say anything. That the module
# renders a config the server accepts needs no machine, and is
# checks.server-config.
{
  pkgs,
  agentModule,
  serverModule,
  metrics,
  # The recorded Leios trace stream, replayed into the journal on the `tracing`
  # node below.
  traces,
  contribUnit,
  # The binary contrib/metsuke.service names at /usr/local/bin/metsuke. The
  # module nodes take theirs from the module's own default.
  agent,
}:
let
  metricsPort = 12798;
  listenPort = 8080;
  # What `signingKey` below hashes to: the agent refuses to start unless the
  # two agree (`identity::check_pool_id`), so a wrong value here fails the test
  # rather than travelling.
  poolId = "pool13vscgf9dwn0jt56u965wp99ychz6avktk3pyrye326f3xctz4nm";
  # Throwaway: what is exercised is that systemd hands the file over and the
  # agent parses it, not what it signs.
  signingKey = pkgs.writeText "pool.skey" (
    builtins.toJSON {
      type = "StakePoolSigningKey_ed25519";
      description = "";
      cborHex = "5820${pkgs.lib.strings.replicate 32 "07"}";
    }
  );
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
  # Nothing listens: the upload is expected to fail and the scrapes to stay
  # spooled, which is what ADR 0004 asks for.
  uploadUrl = "http://127.0.0.1:1/v1/submit";

  # The one archive kind a booted server is needed for. That the module renders
  # a config the server accepts is checks.server-config's, for both kinds and
  # without a VM; what only a machine can show is the confinement below, and
  # the credential systemd hands over to reach it.
  hubNode = {
    imports = [ serverModule ];

    environment.etc."metsuke-server/developer-password" = {
      source = password;
      mode = "0400";
    };

    services.metsuke-server = {
      enable = true;
      developerPasswordFile = "/etc/metsuke-server/developer-password";
      settings = {
        archive.filesystem.root = "${stateDirectory}/archive";
        listen = "127.0.0.1:${toString listenPort}";
        http = {
          idle_timeout_ms = 30000;
          read_timeout_ms = 60000;
          write_timeout_ms = 60000;
          max_concurrent_requests = 64;
        };
        ingest = {
          allowlist.${poolId} = "MUSA-0000";
          max_body_bytes = 1048576;
          max_header_bytes = 4096;
          rate_limit_uploads = 24;
          rate_limit_uploads_total = 240;
          rate_limit_window_secs = 3600;
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

  # No node here reaches another, and none reaches off the machine: every URL
  # above is loopback and `sntp_servers` is empty. A DHCP lease would still be
  # what `network-online.target` waits for, and dhcpcd's ARP probe of the
  # offered address costs ~5 s that both units then wait out before starting.
  defaults.networking.useDHCP = false;

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
        scrape_interval_secs = 1;
        # A name no test network can resolve would cost every scrape the SNTP
        # timeout.
        sntp_servers = [ ];
      };
    };
  };

  # The same agent with trace collection on: the one privilege ADR 0010 adds,
  # and nothing else moved. The unit it follows writes the recorded stream, so
  # what travels journald, journalctl, the selection rules and the spool is a
  # real node's own bytes on a machine with no node.
  nodes.tracing = {
    imports = [ agentModule ];

    systemd.services.metrics-endpoint = metricsEndpoint;

    # Started by the test script, not at boot: the agent follows from the
    # journal's end, so a replay that ran before it attached would be lines it
    # never saw.
    systemd.services.trace-replay = {
      serviceConfig = {
        Type = "oneshot";
        ExecStart = "${pkgs.coreutils}/bin/cat ${traces}";
      };
    };

    # The whole recording arrives in one burst, and journald's default burst
    # allowance is smaller than it: the rows the spool ends up with would
    # otherwise be however many survived the rate limiter.
    services.journald.rateLimitBurst = 0;

    environment.systemPackages = [ pkgs.sqlite ];

    environment.etc."metsuke/pool.skey" = {
      source = signingKey;
      mode = "0400";
    };

    services.metsuke = {
      enable = true;
      signingKeyFile = "/etc/metsuke/pool.skey";
      settings = {
        pool_id = poolId;
        metrics_url = metricsUrl;
        upload_url = uploadUrl;
        scrape_interval_secs = 1;
        sntp_servers = [ ];
        log.journal_unit = "trace-replay.service";
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
        scrape_interval_secs = 1
        sntp_servers = []
      '';
    };

    systemd.tmpfiles.rules = [
      "L+ /usr/local/bin/metsuke - - - - ${agent}/bin/metsuke"
    ];
  };

  nodes.hub = hubNode;

  testScript = ''
    # The four nodes are independent. Nothing here sends a submission from one
    # to another, which is e2e-test.nix's job. Booted on first reference they
    # boot one after another, and the boots are most of this test's runtime.
    start_all()

    def spooled(machine):
        """Scrapes reached the spool.

        -readonly is not a detail: this runs as root, and a plain sqlite3
        creates the file when it is not there yet. That would hand the service
        a root-owned empty database and it would fail its next open.
        """
        machine.wait_until_succeeds(
            "test 0 -lt \"$(sqlite3 -readonly /var/lib/metsuke/spool.sqlite 'select count(*) from scrapes')\""
        )

    def confined(machine, unit, state, reads_journal = False):
        """The privileges a unit has to lack, and the one path it may write.

        `reads_journal` is ADR 0010's whole privilege delta: the group, and
        the /proc journalctl reads the boot id out of. False is ADR 0007's
        posture, and both directives are read back either way. A grant
        nobody decided on and a grant that was decided and then dropped are
        the same red.
        """
        groups = "systemd-journal" if reads_journal else ""
        machine.wait_for_unit(unit)
        pid = machine.succeed(f"systemctl show --property=MainPID --value {unit}").strip()

        def field(name):
            return machine.succeed(f"grep '^{name}:' /proc/{pid}/status").split(":", 1)[1].split()

        # No group beyond the transient one DynamicUser gives the service as
        # its own identity, plus exactly the ones the unit asked for: any
        # other entry would be a grant on something that already existed.
        granted = {
            machine.succeed(f"getent group {name} | cut -d: -f3").strip()
            for name in groups.split()
        }
        assert set(field("Groups")) <= set(field("Gid")) | granted, field("Groups")
        assert granted <= set(field("Groups")), (granted, field("Groups"))
        # And the directive is present with that exact value. DynamicUser alone
        # already yields an empty Groups line, so the assertion above stays
        # green if SupplementaryGroups= is deleted from nix/unit.nix; only
        # reading the rendered unit catches that.
        machine.succeed(f"systemctl cat {unit} | grep -qx 'SupplementaryGroups={groups}'")
        subset = "all" if reads_journal else "pid"
        machine.succeed(f"systemctl cat {unit} | grep -qx 'ProcSubset={subset}'")
        for name in ["CapEff", "CapPrm", "CapBnd", "CapAmb"]:
            assert int(field(name)[0], 16) == 0, (name, field(name))

        # ProtectSystem=strict, seen from inside the mount namespace it got.
        machine.succeed(f"nsenter --target {pid} --mount -- touch {state}/writable")
        machine.fail(f"nsenter --target {pid} --mount -- touch /etc/probe")

    pool.wait_for_unit("metrics-endpoint.service")
    pool.wait_for_open_port(${toString metricsPort}, addr = "127.0.0.1")

    with subtest("the agent scrapes the loopback endpoint"):
        pool.wait_for_unit("metsuke.service")
        # It read the credential and named the endpoint it is scraping.
        pool.wait_until_succeeds(
            "journalctl -u metsuke.service | grep -q 'scraping http://127.0.0.1:${toString metricsPort}/metrics'"
        )
        # It scraped and spooled, which is the only thing it may write.
        spooled(pool)
        # The signing key stayed root-only, so it arrived as a credential.
        pool.succeed("test 400 -eq \"$(stat -c %a /etc/metsuke/pool.skey)\"")

    with subtest("the agent holds nothing ADR 0007 refuses"):
        confined(pool, "metsuke.service", "/var/lib/metsuke")
        # It reached the endpoint above over TCP alone: neither unit opens a
        # unix socket, so neither is granted AF_UNIX.
        pool.fail("systemctl cat metsuke.service | grep -q 'AF_UNIX'")

    with subtest("collecting trace lines adds the journal group and nothing else"):
        tracing.wait_for_unit("metsuke.service")
        tracing.wait_until_succeeds(
            "journalctl -u metsuke.service"
            " | grep -q 'collecting trace lines from trace-replay.service'"
        )
        # It started journalctl and journalctl did not refuse the journal.
        tracing.fail("journalctl -u metsuke.service | grep -q 'trace lines not collected'")
        confined(tracing, "metsuke.service", "/var/lib/metsuke", reads_journal = True)

    with subtest("what a node wrote to the journal reaches the spool unchanged"):
        # Type=oneshot, so this returns when the whole recording has been
        # written. What the agent has read by then is whatever it has read:
        # only the properties below are asserted, never a count, so there is
        # nothing here to lose a race with.
        tracing.succeed("systemctl start trace-replay.service")
        # Wait on a namespace the rewards program asked for by name: which
        # rules reach it is tests/logselect.rs, and what this waits for is that
        # a line survived journald and journalctl to be judged by them at all.
        # The dump is inside the retry because each attempt has to read the
        # rows the agent has written by then, not the ones it had at the first.
        tracing.wait_until_succeeds(
            "sqlite3 -readonly /var/lib/metsuke/spool.sqlite 'select line from log_lines'"
            " > /tmp/spooled"
            " && grep -q '\"ns\":\"Consensus.LeiosKernel.Certified\"' /tmp/spooled"
        )
        import json
        # A spooled row is the wire line, stamped on the way in (metsuke-jfb.11),
        # so the node's own object is what is left once metsuke's one reserved
        # key comes off.
        lines = [
            {key: value for key, value in json.loads(line).items() if key != "metsuke"}
            for line in tracing.succeed("cat /tmp/spooled").splitlines()
        ]
        recorded = [json.loads(line) for line in tracing.succeed("cat ${traces}").splitlines()]
        # Field for field, which is the whole promise: the developers compute
        # their own distributions from the node's own record, and a transport
        # that dropped or renamed a field would leave them computing from
        # metsuke's.
        assert all(line in recorded for line in lines), [
            line for line in lines if line not in recorded
        ]
        # And the rules still filtered on the way: the recording's volume is
        # Debug lines nobody asked for.
        assert not [line for line in lines if line.get("sev") == "Debug"], lines

    with subtest("the contrib unit runs the agent on a host that is not NixOS"):
        bare.wait_for_unit("metrics-endpoint.service")
        bare.wait_for_open_port(${toString metricsPort}, addr = "127.0.0.1")
        # /run rather than /etc, which is read-only here, and which is also
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
  '';
}
