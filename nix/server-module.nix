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

  inherit (lib) mkOption types;

  # These carry no description: the option's name is the Rust field's name, and
  # `settings` below points at the file that owns what it means.
  positive = mkOption { type = types.ints.positive; };
  required = type: mkOption { inherit type; };

  # `ArchiveConfig` is one tagged enum in Rust, so it is one here: an attrTag
  # cannot hold a bucket and a filesystem root at once, where a flat submodule
  # of nullable fields could and would only be refused at startup.
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
          request_timeout_secs = positive;
          signature_validity_secs = positive;
          put_retries = mkOption { type = types.ints.unsigned; };
          put_retry_backoff_ms = positive;
          list_max_pages = positive;
        };
      };
    };
  };

  # Back to the `kind = "…"` shape `ArchiveConfig`'s serde tag deserializes.
  taggedArchive =
    archive:
    let
      kind = lib.head (lib.attrNames archive);
    in
    { inherit kind; } // archive.${kind};

  configFile = toml.generate "metsuke-server-config.toml" (
    cfg.settings // { archive = taggedArchive cfg.settings.archive; }
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
      type = lib.types.path;
      description = ''
        The developer account's password, alone in a file. Read by systemd as
        root, so the deployed secret stays unreadable to the service user.
      '';
    };

    environmentFile = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
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
                max_body_bytes = positive;
                max_header_bytes = positive;
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
    systemd.services.${unitName} = {
      description = "metsuke ingest server";
      wantedBy = [ "multi-user.target" ];
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];

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
      };
    };

    assertions = [
      {
        assertion = !(cfg.settings.archive ? s3) || cfg.environmentFile != null;
        message = "services.metsuke-server.environmentFile is required by an S3 archive: that is where the server reads AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY from.";
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
