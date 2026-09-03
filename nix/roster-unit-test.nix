# What the roster timer's unit is allowed, read off the module's own rendering.
# An evaluation and not a VM: every claim here is a systemd directive, and a
# directive that renders wrong is wrong before anything boots.
#
# The confinement of the units that run continuously is nix/unit-test.nix's, in
# a VM, because what those assert is that a running unit cannot reach what it
# must not. This one covers the generator, which cannot run without a node.
{
  pkgs,
  serverModule,
  server,
  roster,
}:
let
  poolId = "pool13vscgf9dwn0jt56u965wp99ychz6avktk3pyrye326f3xctz4nm";
  password = pkgs.writeText "developer-password" ''
    metsuke-dev = "not-a-real-secret"
  '';

  machine = pkgs.nixos (
    { config, ... }:
    {
      imports = [ serverModule ];
      services.metsuke-server = {
        enable = true;
        package = server;
        developerPasswordFile = "${password}";
        roster = {
          enable = true;
          package = roster;
          # Nothing runs, so which cardano-cli this is does not matter here; that
          # it has to be named at all is what the option asserts.
          cardanoCli = pkgs.coreutils;
          era = "conway";
          network.testnetMagic = 42;
          socketPath = "/run/cardano-node/node.socket";
          socketGroup = "cardano-node";
          interval = "1h";
        };
        settings = {
          listen = "127.0.0.1:0";
          public_url = "https://metsuke.example.org";
          archive.filesystem.root = "/var/lib/metsuke-server/archive";
          http = {
            idle_timeout_ms = 30000;
            read_timeout_ms = 60000;
            write_timeout_ms = 60000;
            max_concurrent_requests = 64;
          };
          ingest = {
            allowlist.${poolId} = "MUSA-0000";
            # The path the timer writes, read off the option that owns it.
            leios_roster = config.services.metsuke-server.roster.file;
            max_body_bytes = 4194304;
            max_header_bytes = 4096;
            max_timestamp_skew_secs = 300;
            rate_limit_uploads = 24;
            rate_limit_uploads_total = 240;
            rate_limit_window_secs = 3600;
          };
          developer = {
            list_max_rows = 1000;
            password_file = "${password}";
          };
        };
      };
    }
  );

  generator = machine.config.systemd.services.metsuke-roster.serviceConfig;
  generatorUnit = machine.config.systemd.units."metsuke-roster.service".text;
  ingest = machine.config.systemd.services.metsuke-server.serviceConfig;
  timer = machine.config.systemd.timers.metsuke-roster.timerConfig;

  # Each pair is a directive and what it has to be, so a failure names the
  # directive rather than a diff of two whole units.
  claims = {
    "generator User" = {
      found = generator.User or null;
      wanted = "metsuke-roster";
    };
    "generator DynamicUser" = {
      found = generator.DynamicUser;
      wanted = false;
    };
    "generator SupplementaryGroups" = {
      found = generator.SupplementaryGroups;
      wanted = "cardano-node";
    };
    "generator UMask" = {
      found = generator.UMask;
      wanted = "0027";
    };
    # One local socket. No AF_INET: the generator talks to a node beside it and
    # reaches no network of its own.
    "generator RestrictAddressFamilies" = {
      found = generator.RestrictAddressFamilies;
      wanted = [
        "AF_UNIX"
        "AF_NETLINK"
      ];
    };
    "generator capabilities" = {
      found = generator.CapabilityBoundingSet;
      wanted = "";
    };
    # Read off the rendered unit and not the option: `StartLimitBurst` is a
    # [Unit] directive, so setting it in `serviceConfig` would render a line
    # systemd ignores.
    "generator StartLimitBurst" = {
      found = pkgs.lib.hasInfix "StartLimitBurst=0" generatorUnit;
      wanted = true;
    };
    "server SupplementaryGroups" = {
      found = ingest.SupplementaryGroups;
      wanted = "metsuke-roster";
    };
    "server DynamicUser" = {
      found = ingest.DynamicUser;
      wanted = true;
    };
    # No AF_UNIX for the server: it reads the roster as a file, not a socket.
    "server RestrictAddressFamilies" = {
      found = ingest.RestrictAddressFamilies;
      wanted = [
        "AF_INET"
        "AF_INET6"
        "AF_NETLINK"
      ];
    };
    # Monotonic, not a calendar: a wall-clock timer moves by however far the
    # clock jumps, which the e2e's guest does deliberately.
    "timer OnBootSec" = {
      found = timer.OnBootSec;
      wanted = "1h";
    };
    "timer OnUnitActiveSec" = {
      found = timer.OnUnitActiveSec;
      wanted = "1h";
    };
    "timer OnCalendar" = {
      found = timer.OnCalendar or null;
      wanted = null;
    };
    "timer AccuracySec" = {
      found = timer.AccuracySec;
      wanted = "1s";
    };
  };

  wrong = pkgs.lib.filterAttrs (_: claim: claim.found != claim.wanted) claims;
  report = pkgs.lib.mapAttrsToList (
    name: claim:
    "${name}: rendered ${builtins.toJSON claim.found}, wanted ${builtins.toJSON claim.wanted}"
  ) wrong;
in
# Failing inside the build and not with a `throw`: an evaluation-time throw
# aborts `nix flake check` and `nix flake show` before any other check builds,
# so one wrong directive here would hide every other failure in the flake.
pkgs.runCommand "metsuke-roster-unit" { } ''
  ${
    if wrong == { } then
      "touch $out"
    else
      ''
        echo "the roster units render what nobody asked for:" >&2
        ${pkgs.lib.concatMapStringsSep "\n" (line: "echo ${pkgs.lib.escapeShellArg line} >&2") report}
        exit 1
      ''
  }
''
