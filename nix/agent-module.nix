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

  configFile = toml.generate "metsuke-config.toml" (
    lib.filterAttrs (_: value: value != null) cfg.settings
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
      type = lib.types.path;
      description = ''
        The cold or Calidus signing key, in cardano-cli TextEnvelope form.
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

          sample_interval_secs = shipped types.ints.unsigned;
          upload_interval_secs = shipped types.ints.unsigned;
          sntp_servers = shipped (types.listOf types.str);
          sntp_timeout_secs = shipped types.ints.unsigned;
          spool_path = shipped types.str;
          spool_max_samples = shipped types.ints.unsigned;
          scrape_timeout_secs = shipped types.ints.unsigned;
          scrape_max_body_bytes = shipped types.ints.unsigned;
          upload_timeout_secs = shipped types.ints.unsigned;
          upload_jitter_max_secs = shipped types.ints.unsigned;
          upload_backoff_max_secs = shipped types.ints.unsigned;
          compression_level = shipped types.int;
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
        addressFamilies = unit.agentAddressFamilies;
      };
    };

    assertions = [
      {
        assertion = cfg.settings.spool_path == null || lib.hasPrefix "${writable}/" cfg.settings.spool_path;
        message = "services.metsuke.settings.spool_path has to be under ${writable}: the unit may write nothing else.";
      }
    ];
  };
}
