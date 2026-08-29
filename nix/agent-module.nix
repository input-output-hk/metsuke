{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.metsuke;
  toml = pkgs.formats.toml { };
  unit = import ./unit.nix;
  # The unit puts the key under this name and the agent is told to look for it
  # under the same one.
  credential = "signing-key";

  stateDirectory = "metsuke";
  # ProtectSystem=strict leaves this the only path the unit may write, so a
  # spool_path outside it renders a unit that dies on its first open.
  writable = "/var/lib/${stateDirectory}";

  inherit (lib) mkOption types;

  # Neither carries a description: the option's name is the Rust field's name,
  # and `settings` below points at the file that owns what it means.
  required = type: mkOption { inherit type; };

  # A defaulted field of `crates/metsuke/src/config.rs`. Null leaves it out of
  # the rendered TOML, so the shipped default applies and its digits stay in
  # the crate that ships them.
  shipped =
    type:
    mkOption {
      type = types.nullOr type;
      default = null;
    };

  set = lib.filterAttrs (_: value: value != null);

  # `[log]` is a table, so its own unset fields have to be dropped before it is
  # rendered; `lib.filterAttrs` does not descend.
  configFile = toml.generate "metsuke-config.toml" (
    set (removeAttrs cfg.settings [ "log" ])
    // lib.optionalAttrs (cfg.settings.log != null) { log = set cfg.settings.log; }
  );
in
{
  options.services.metsuke = {
    enable = lib.mkEnableOption "the metsuke telemetry agent";

    package = lib.mkOption {
      type = lib.types.package;
      description = "The agent to run.";
    };

    signingKeyFile = lib.mkOption {
      # `str` rather than `path`, so a nix path literal cannot type check here.
      # Interpolating one copies the key into /nix/store, where it is world
      # readable and travels in every closure this system is pushed to. A
      # runtime path given as a string is what sops-nix and agenix hand back,
      # and it is also what lets the key be replaced without a rebuild.
      type = lib.types.str;
      description = ''
        The pool cold signing key, in cardano-cli TextEnvelope form. The agent
        refuses to start unless it hashes to the configured pool id.
        Read by systemd as root and handed to the agent as a credential, so
        the file itself stays unreadable to the service user.
      '';
    };

    restartSecs = lib.mkOption {
      type = lib.types.ints.unsigned;
      default = unit.restartSecs;
      description = "How long systemd waits before restarting a stopped agent.";
    };

    settings = mkOption {
      description = ''
        The agent's configuration file, one option per field of
        `crates/metsuke/src/config.rs` bar `signing_key`, which this module
        owns: the key arrives as a systemd credential. The first three have no
        default and are set here or evaluation fails naming them.
      '';
      type = types.submodule {
        options = {
          pool_id = required types.str;
          metrics_url = required types.str;
          upload_url = required types.str;

          agent_id = shipped types.str;
          scrape_interval_secs = shipped types.ints.unsigned;
          upload_interval_secs = shipped types.ints.unsigned;
          sntp_servers = shipped (types.listOf types.str);
          sntp_timeout_secs = shipped types.ints.unsigned;
          spool_path = shipped types.str;
          spool_max_bytes = shipped types.ints.unsigned;
          spool_busy_timeout_secs = shipped types.ints.unsigned;
          scrape_timeout_secs = shipped types.ints.unsigned;
          scrape_max_body_bytes = shipped types.ints.unsigned;
          upload_timeout_secs = shipped types.ints.unsigned;
          upload_jitter_max_secs = shipped types.ints.unsigned;
          upload_backoff_max_secs = shipped types.ints.unsigned;
          upload_batch_max_bytes = shipped types.ints.unsigned;
          compression_level = shipped types.int;

          log = mkOption {
            description = ''
              Trace-line collection. Setting it opens the unit up by what
              reading a journal takes, which is the privilege ADR 0010 is
              about and which nix/unit.nix spells out; leaving it null starts
              no journalctl and grants nothing.
            '';
            default = null;
            type = types.nullOr (
              types.submodule {
                options = {
                  # The unit this module renders runs the agent on its own, with
                  # no node upstream of it, so the pipe has nothing to read.
                  # Naming it here is refused rather than rendered, because a
                  # rendered one gets /dev/null, reads EOF at once, and
                  # Restart=always turns that into a loop collecting nothing.
                  source = mkOption {
                    type = types.enum [ "journald" ];
                    default = "journald";
                  };
                  journal_unit = required types.str;
                  # Not `shipped`: which journalctl exists is this module's to
                  # know, and the hardened unit's PATH is not to be relied on.
                  journalctl_path = mkOption {
                    type = types.str;
                    default = "${config.systemd.package}/bin/journalctl";
                  };
                  namespace_roots = shipped (types.listOf types.str);
                  namespaces = shipped (types.listOf types.str);
                  log_max_bytes = shipped types.ints.unsigned;
                  respawn_backoff_secs = shipped types.ints.unsigned;
                  start_grace_secs = shipped types.ints.positive;
                };
              }
            );
          };
        };
      };
    };
  };

  config = lib.mkIf cfg.enable {
    systemd.services.metsuke = {
      description = "metsuke telemetry agent";
      wantedBy = [ "multi-user.target" ];
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];

      serviceConfig = {
        ExecStart = lib.concatStringsSep " " [
          "${cfg.package}/bin/metsuke"
          "--config"
          configFile
          "--signing-key"
          "\${CREDENTIALS_DIRECTORY}/${credential}"
        ];
        LoadCredential = "${credential}:${cfg.signingKeyFile}";
        Restart = "always";
        RestartSec = cfg.restartSecs;
      }
      // unit.hardening {
        inherit stateDirectory;
        inherit (unit) addressFamilies;
        # The whole privilege delta of ADR 0010, and only for an operator who
        # asked for trace lines.
        readsTheJournal = cfg.settings.log != null;
      };
    };

    assertions = [
      {
        assertion = cfg.settings.spool_path == null || lib.hasPrefix "${writable}/" cfg.settings.spool_path;
        message = "services.metsuke.settings.spool_path has to be under ${writable}: the unit may write nothing else.";
      }
      {
        assertion = lib.hasPrefix "/" cfg.signingKeyFile;
        message = "services.metsuke.signingKeyFile has to be an absolute path: LoadCredential reads a relative one as the name of a credential inherited from the manager, which is not a file on this host.";
      }
    ];
  };
}
