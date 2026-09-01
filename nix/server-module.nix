{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.metsuke-server;
  toml = pkgs.formats.toml { };
  unit = import ./unit.nix;
  unitName = "metsuke-server";
  stateDirectory = "metsuke-server";
  # ProtectSystem=strict leaves this the only path the unit may write, so a
  # path outside it renders a unit that dies on its first open.
  writable = "/var/lib/${stateDirectory}";
  # The config has to name the paths LoadCredential wrote to, so both halves
  # are derived from this one list and `unitName`.
  credentials = {
    developer-password = cfg.developerPasswordFile;
  };
  # Written out rather than $CREDENTIALS_DIRECTORY: this lands in a TOML file,
  # where nothing expands a variable.
  credentialPath = name: "/run/credentials/${unitName}.service/${name}";

  # The generator runs as a named user, which `unit.hardening`'s `user`
  # parameter says why, and the group is what carries the file to the server.
  rosterUnit = "metsuke-roster";
  rosterDirectory = "/var/lib/${rosterUnit}";
  rosterFile = "${rosterDirectory}/roster.json";

  # `query` then `generate`, the same two steps deploying.md documents by hand.
  # The chain's answer lands in the unit's own directory: it is an intermediate
  # nobody else reads, unlike the roster, which is replaced by rename.
  rosterScript = pkgs.writeShellApplication {
    name = "metsuke-roster-run";
    runtimeInputs = [
      cfg.roster.package
      cfg.roster.cardanoCli
    ];
    text = ''
      answer="$STATE_DIRECTORY/pool-state.json"
      metsuke-roster query ${lib.escapeShellArg cfg.roster.era} \
        --socket-path ${lib.escapeShellArg cfg.roster.socketPath} \
        ${
          if cfg.roster.network ? mainnet then
            "--mainnet"
          else
            "--testnet-magic ${toString cfg.roster.network.testnetMagic}"
        } > "$answer"
      metsuke-roster generate "$answer" ${lib.escapeShellArg rosterFile}
    '';
  };

  inherit (lib) mkOption types;

  # These carry no description: the option's name is the Rust field's name, and
  # `settings` below points at the file that owns what it means.
  positive = mkOption { type = types.ints.positive; };
  required = type: mkOption { inherit type; };

  # An attrTag cannot hold a bucket and a filesystem root at once, where a flat
  # submodule of nullable fields could and would only be refused at startup. It
  # renders as the `[archive.<kind>]` table `ArchiveConfig` deserializes.
  archiveType = types.attrTag {
    filesystem = mkOption {
      type = types.submodule {
        options.root = required types.str;
      };
    };
    s3 = mkOption {
      type = types.submodule {
        options = {
          bucket = required types.str;
          region = required types.str;
          endpoint = required types.str;
          request_timeout_ms = positive;
          signature_validity_secs = positive;
          put_retries = mkOption { type = types.ints.unsigned; };
          put_retry_backoff_ms = positive;
          list_max_pages = positive;
        };
      };
    };
  };

  # The one nullable setting is absent rather than null in the file: TOML has
  # no null, and the server reads absence as "no Leios keys" (ADR 0011).
  configFile = toml.generate "metsuke-server-config.toml" (
    cfg.settings
    // {
      ingest = lib.filterAttrs (_: value: value != null) cfg.settings.ingest;
    }
  );
in
{
  options.services.metsuke-server = {
    enable = lib.mkEnableOption "the metsuke ingest server";

    package = lib.mkOption {
      type = lib.types.package;
      description = "The server to run.";
    };

    developerPasswordFile = lib.mkOption {
      # `str` rather than `path`, so a nix path literal cannot type check here.
      # Interpolating one copies the secret into /nix/store, where it is world
      # readable and travels in every closure this system is pushed to. A
      # runtime path given as a string is what sops-nix and agenix hand back,
      # and it is also what lets the secret be replaced without a rebuild.
      type = lib.types.str;
      description = ''
        The developer account's password, alone in a file. Read by systemd as
        root, so the deployed secret stays unreadable to the service user.
      '';
    };

    environmentFile = lib.mkOption {
      # `str` for the same reason `developerPasswordFile` is one, and it holds
      # the same kind of secret.
      type = lib.types.nullOr lib.types.str;
      default = null;
      description = ''
        Passed to systemd as `EnvironmentFile`. An S3 archive reads
        AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY from the environment
        (ticket metsuke-4zo.50), and this is where they come from; a
        filesystem archive needs none.
      '';
    };

    restartSecs = lib.mkOption {
      type = lib.types.ints.unsigned;
      default = unit.restartSecs;
      description = "How long systemd waits before restarting a stopped server.";
    };

    roster = mkOption {
      description = ''
        The timer that writes the Leios key roster this server reads (ADR
        0011). It lives here rather than in a module of its own because the
        file is local to the server and nothing else reads it: one module owns
        both units, the group that hands the file over, and the assertion that
        the server is pointed at what the timer writes.

        The host needs a cardano-node socket. Which era answers and which
        network it is are facts about that node, so they are stated here and
        not guessed.
      '';
      default = { };
      type = types.submodule {
        options = {
          enable = lib.mkEnableOption "the Leios key roster timer";

          package = mkOption {
            type = types.package;
            description = "The `metsuke-roster` generator to run.";
          };

          cardanoCli = mkOption {
            type = types.package;
            description = ''
              The cardano-cli the generator shells out to. No default: which
              build speaks the era below is a fact about the network, which is
              why `nix/roster.nix` does not wrap one in either.
            '';
          };

          era = required types.str;

          network = mkOption {
            description = ''
              Which network the node answers for, as the two flags the
              generator takes. An attrTag because a magic and mainnet are not
              both true, and a submodule of nullable fields would let them be.
            '';
            type = types.attrTag {
              mainnet = mkOption { type = types.submodule { }; };
              testnetMagic = mkOption { type = types.ints.unsigned; };
            };
          };

          socketPath = mkOption {
            type = types.str;
            description = ''
              The node socket the generator queries. On a cardano-parts host
              that is `config.services.cardano-node.socketPath 0`; this module
              takes it as a string so nothing here depends on that module.
            '';
          };

          socketGroup = mkOption {
            type = types.str;
            description = ''
              The group that may read `socketPath`, which the generator joins.
              `config.services.cardano-node.socketGroup` on a cardano-parts
              host.
            '';
          };

          file = mkOption {
            type = types.str;
            default = rosterFile;
            readOnly = true;
            description = ''
              Where the timer writes, which is the only path this server may be
              pointed at. Read-only so `settings.ingest.leios_roster` can name
              it instead of a second copy of the literal.
            '';
          };

          interval = mkOption {
            type = types.str;
            description = ''
              How often the roster is regenerated, as a systemd time span. What
              the cadence costs is ADR 0011's stale-roster consequence; it is an
              interval, so it is configuration rather than a constant.

              Monotonic (`OnBootSec` and `OnUnitActiveSec`), not a calendar
              expression: a calendar timer is armed against the wall clock, so
              an NTP step or any other jump moves the next tick by however far
              the clock moved. Nothing here wants to run at a particular time of
              day, only often enough.
            '';
          };

          accuracySec = mkOption {
            type = types.str;
            default = "1s";
            description = ''
              How far systemd may slide a tick to batch wakeups. Pinned tight
              rather than left at systemd's own one minute, which would widen
              the ceiling `interval` sets by a minute without saying so. Widen
              it deliberately where the cadence is hours and the wakeups are
              worth more than the precision.
            '';
          };
        };
      };
    };

    settings = mkOption {
      description = ''
        The server's configuration file, one option per field of
        `crates/metsuke-server/src/config.rs`, which owns what each value
        means. Only the option defaulted below has one; every other field is
        set here or evaluation fails naming it.
      '';
      type = types.submodule {
        options = {
          listen = required types.str;

          http = mkOption {
            type = types.submodule {
              options = {
                idle_timeout_ms = positive;
                read_timeout_ms = positive;
                write_timeout_ms = positive;
                max_concurrent_requests = positive;
              };
            };
          };

          archive = mkOption {
            type = archiveType;
          };

          ingest = mkOption {
            type = types.submodule {
              options = {
                allowlist = mkOption {
                  type = types.attrsOf types.str;
                  description = "As the offline allowlist generator emits them.";
                };
                leios_roster = mkOption {
                  type = types.nullOr types.str;
                  default = null;
                  description = ''
                    Where the Leios key roster is read from, as the roster
                    generator writes it. The one setting that may be absent:
                    null takes cold-key submissions only (ADR 0011).
                  '';
                };
                max_body_bytes = positive;
                max_header_bytes = positive;
                max_timestamp_skew_secs = positive;
                rate_limit_uploads = positive;
                rate_limit_uploads_total = positive;
                rate_limit_window_secs = positive;
              };
            };
          };

          developer = mkOption {
            type = types.submodule {
              options = {
                user = required types.str;
                password_file = mkOption {
                  type = types.str;
                  default = credentialPath "developer-password";
                  description = "Defaults to where `developerPasswordFile` is loaded to.";
                };
                list_max_rows = positive;
              };
            };
          };
        };
      };
    };
  };

  config = lib.mkIf cfg.enable {
    # The group the roster crosses on: the generator owns it and the server is
    # in it, so nothing else on the host reads which keys the chain registers
    # for whom.
    users.groups = lib.optionalAttrs cfg.roster.enable { ${rosterUnit} = { }; };
    users.users = lib.optionalAttrs cfg.roster.enable {
      ${rosterUnit} = {
        isSystemUser = true;
        group = rosterUnit;
        description = "metsuke Leios key roster generator";
      };
    };

    systemd.timers = lib.optionalAttrs cfg.roster.enable {
      ${rosterUnit} = {
        description = "regenerate the Leios key roster";
        wantedBy = [ "timers.target" ];
        timerConfig = {
          # From boot and then from each run, so a host that was down through a
          # tick regenerates on the way up rather than waiting out the interval
          # holding the stale roster that refuses whoever rotated. That is what
          # `Persistent` buys on a calendar timer, without the wall clock.
          OnBootSec = cfg.roster.interval;
          OnUnitActiveSec = cfg.roster.interval;
          AccuracySec = cfg.roster.accuracySec;
        };
      };
    };

    systemd.services = {
      ${unitName} = {
        description = "metsuke ingest server";
        wantedBy = [ "multi-user.target" ];
        # The roster is wanted, not required: the server refuses to start
        # without a readable one, so the ordering is what keeps a boot from
        # crash-looping until the first timer tick. A generator that fails
        # anyway (no node yet) leaves the server to retry, which is the same
        # behaviour as having no ordering at all.
        after = [ "network-online.target" ] ++ lib.optional cfg.roster.enable "${rosterUnit}.service";
        wants = [ "network-online.target" ] ++ lib.optional cfg.roster.enable "${rosterUnit}.service";

        serviceConfig = {
          ExecStart = lib.concatStringsSep " " [
            "${cfg.package}/bin/metsuke-server"
            "--config"
            configFile
          ];
          LoadCredential = lib.mapAttrsToList (name: source: "${name}:${source}") credentials;
          Restart = "always";
          RestartSec = cfg.restartSecs;
        }
        // lib.optionalAttrs (cfg.environmentFile != null) {
          EnvironmentFile = cfg.environmentFile;
        }
        // unit.hardening {
          inherit stateDirectory;
          inherit (unit) addressFamilies;
          # The one group the server is in, and only where a roster is
          # generated: it is how it reads a file the generator owns.
          groups = lib.optional cfg.roster.enable rosterUnit;
        };
      };
    }
    // lib.optionalAttrs cfg.roster.enable {
      ${rosterUnit} = {
        description = "generate the Leios key roster";

        # No start rate limit. A oneshot has no restart loop for the default
        # five-in-ten-seconds to protect against, and the timer is what paces
        # this: a start refused for arriving too soon would leave the roster
        # stale for the window, which is the failure ADR 0011 wants bounded.
        startLimitBurst = 0;

        # Ordered after nothing: it reaches one local socket, so it wants no
        # network, and the node it queries is a unit this module does not name.
        # A run before the node answers fails and the timer takes the next one.
        serviceConfig = {
          Type = "oneshot";
          ExecStart = lib.getExe rosterScript;
        }
        // unit.hardening {
          stateDirectory = rosterUnit;
          addressFamilies = unit.socketOnly;
          user = rosterUnit;
          groups = [ cfg.roster.socketGroup ];
          # 0640 on the roster, so the server's group can read it.
          umask = "0027";
        };
      };
    };

    warnings =
      # A warning and not an assertion, and nothing in this repository reaches
      # it: a deployment that refreshes the roster by some other means cannot
      # enable the timer, because the assertion below allows only the path the
      # timer writes. Refusing it outright would refuse that deployment; saying
      # nothing would let the stale-roster failure ADR 0011 accepts arrive
      # without anyone choosing it.
      lib.optional (cfg.settings.ingest.leios_roster != null && !cfg.roster.enable) ''
        services.metsuke-server.settings.ingest.leios_roster is set while
        services.metsuke-server.roster.enable is not, so nothing on this host
        refreshes that file. A roster nobody regenerates refuses every pool that
        rotates its Leios key (ADR 0011).
      ''
      # The config test and a single-host development deployment both use this
      # backend deliberately, and refusing it would leave them with nothing.
      # What it costs is not obvious from the option name, so it is said here as
      # well as at startup.
      ++ lib.optional (cfg.settings.archive ? filesystem) ''
        services.metsuke-server.settings.archive.filesystem stores the submission
        bytes alone and drops the key and signature they were checked with, so
        nothing can verify that archive afterwards, verify-archive refuses it, and
        every download reaches a consumer unattested. S3 is what production runs
        (ADR 0005).
      '';

    assertions = [
      {
        assertion = !cfg.roster.enable || cfg.settings.ingest.leios_roster == rosterFile;
        message = "services.metsuke-server.settings.ingest.leios_roster has to be ${rosterFile} when roster.enable is set: that is the only path the timer writes, and a server pointed anywhere else reads a roster nothing refreshes.";
      }
      {
        assertion = !(cfg.settings.archive ? s3) || cfg.environmentFile != null;
        message = "services.metsuke-server.environmentFile is required by an S3 archive: that is where the server reads AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY from.";
      }
      {
        assertion = lib.hasPrefix "/" cfg.developerPasswordFile;
        message = "services.metsuke-server.developerPasswordFile has to be an absolute path: LoadCredential reads a relative one as the name of a credential inherited from the manager, which is not a file on this host.";
      }
      {
        assertion =
          !(cfg.settings.archive ? filesystem)
          || lib.hasPrefix "${writable}/" cfg.settings.archive.filesystem.root;
        message = "services.metsuke-server.settings.archive.filesystem.root has to be under ${writable}: the unit may write nothing else.";
      }
    ];
  };
}
