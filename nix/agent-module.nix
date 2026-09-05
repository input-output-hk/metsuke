# The NixOS agent. `services.metsuke.settings` is the agent's config file, one
# option per field of `crates/metsuke/src/config.rs`, and every default below
# is read out of `contrib/config.example.toml` rather than restated here.
#
# To read what an option is for and what it defaults to, read that example. It
# is annotated, it is plain text, and it is the same file an operator editing
# by hand is given. A deployment also serves it at `/files/config.example.toml`.
#
# To read it as NixOS options instead, on a host that imports this module:
#
#   nixos-option services.metsuke.settings.scrape_interval_secs
#
# or every option and what it defaults to, from a flake that has one:
#
#   nix eval --raw \
#     .#nixosConfigurations.<host>.options.services.metsuke.settings.type.getSubOptions \
#     --apply 'o: builtins.concatStringsSep "\n" (builtins.attrValues (builtins.mapAttrs
#        (k: v: k + " = " + (v.defaultText.text or
#          (if v ? default then builtins.toJSON v.default else "(required)")))
#        (builtins.removeAttrs (o []) ["_module"])))'
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

  # The annotated example, read rather than restated. Every default and every
  # description below comes out of it, so the prose and the digits live in the
  # file an operator editing by hand already reads, and a key it stops
  # documenting throws here on evaluation. That is the whole guard: this file
  # cannot go quietly stale against it.
  exampleLines = lib.splitString "\n" (builtins.readFile ../contrib/config.example.toml);

  # A key takes the comment block above it. Blocks cover groups, so keys in a
  # group share one, and a blank line or a `[table]` header ends a block.
  annotated =
    (builtins.foldl'
      (
        acc: line:
        let
          key = builtins.match "#? ?([a-z_]+) = .*" line;
          table = lib.hasPrefix "# [" line || lib.hasPrefix "[" line;
        in
        if line == "" || table then
          acc
          // {
            prose = [ ];
            sawKey = false;
          }
        else if key != null then
          acc
          // {
            out = acc.out // {
              ${builtins.head key} = {
                inherit (acc) prose;
                value = builtins.elemAt (builtins.match "#? ?[a-z_]+ = (.*)" line) 0;
              };
            };
            sawKey = true;
          }
        # A comment after a key starts a fresh block rather than extending the
        # one that key took, or a per-key line would inherit its neighbour's.
        else if builtins.match "#.*" line != null then
          (
            if acc.sawKey then
              acc
              // {
                prose = [ line ];
                sawKey = false;
              }
            else
              acc // { prose = acc.prose ++ [ line ]; }
          )
        else
          acc
      )
      {
        prose = [ ];
        out = { };
        sawKey = false;
      }
      exampleLines
    ).out;

  # The first paragraph of a key's block. The later ones in the long blocks are
  # about the group, and would repeat for every key in it. A leading separator
  # is skipped so a block that opens with one still counts.
  described =
    key:
    let
      taken =
        builtins.foldl'
          (
            acc: line:
            if line == "#" && acc.out == [ ] then
              acc
            else if line == "#" then
              acc // { done = true; }
            else if acc.done then
              acc
            else
              acc // { out = acc.out ++ [ line ]; }
          )
          {
            done = false;
            out = [ ];
          }
          annotated.${key}.prose;
    in
    lib.concatStringsSep " " (map (line: lib.removePrefix "# " line) taken.out);

  # TOML string literals arrive quoted; a nix reader wants what is inside them.
  unquoted =
    value:
    let
      quoted = builtins.match "\"(.*)\"" value;
    in
    if quoted == null then value else builtins.head quoted;

  # The three the example marks Required. No default, so evaluation fails
  # naming the option rather than the agent failing to start.
  requiredOptions = builtins.mapAttrs (
    key: type:
    mkOption {
      inherit type;
      description = described key;
    }
  );

  # Defaulted fields of `crates/metsuke/src/config.rs`, keyed by the name each
  # has there. Null leaves one out of the rendered TOML, so the agent applies
  # its own default and the digits stay in the crate that ships them.
  shippedOptions = builtins.mapAttrs (
    key: type:
    mkOption {
      type = types.nullOr type;
      default = null;
      defaultText = lib.literalMD (unquoted annotated.${key}.value);
      description = described key;
    }
  );

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
        The pool's cold or Leios signing key, in cardano-cli TextEnvelope
        form. A cold key has to hash to the configured pool id or the agent
        refuses to start; a Leios key hashes to nothing, so the server's
        roster settles which pool it speaks for.
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

        `contrib/config.example.toml` is the annotated reference and says what
        each of these is for and what it defaults to, in the same words an
        operator editing the file by hand reads. A deployment serves it at
        `/files/config.example.toml`, so a checkout is not needed to read it.

        An option left `null` is left out of the rendered file entirely, so the
        agent applies its own default rather than receiving one from here. The
        options below carrying a description are the ones where this module
        behaves differently from that reference.
      '';
      type = types.submodule {
        options =
          requiredOptions {
            pool_id = types.str;
            metrics_url = types.str;
            upload_url = types.str;
          }
          // {
            # The one field whose default is computed rather than a value the
            # reference can show, so `null` here reads as "unset" when it means
            # something specific and machine-dependent.
            agent_id = mkOption {
              type = types.nullOr types.str;
              default = null;
              defaultText = lib.literalMD "this host's name, folded to lowercase";
              description = ''
                What to call this agent on every line it ships, so a pool
                reporting from more than one can tell them apart. Unset, it is
                this host's hostname folded to lowercase `a-z0-9` in
                dash-separated runs, and a value set here is folded the same way.
                Set it where the hostname is not the name you want, and in a
                container, where the hostname is the runtime's rather than yours.
              '';
            };
            # Bounded here in a way the reference is not: the unit this module
            # renders may write one directory and nowhere else.
            spool_path = mkOption {
              type = types.nullOr types.str;
              default = null;
              defaultText = lib.literalMD (unquoted annotated.spool_path.value);
              description = ''
                Where the SQLite spool goes. It has to be under `${writable}`,
                which is the StateDirectory this module's unit creates and the
                only path `ProtectSystem=strict` leaves it able to write. An
                assertion refuses anything else rather than rendering a unit that
                dies on its first open.
              '';
            };
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
                      description = ''
                        Which stream the trace lines come from. Only `journald`
                        here, unlike the reference, which also offers `pipe`: the
                        unit this module renders runs the agent on its own with
                        no node upstream, so a pipe would get `/dev/null`, read
                        EOF at once, and `Restart=always` would loop it forever
                        collecting nothing. Take the shipped drop-in instead if
                        you want that source.
                      '';
                    };
                  }
                  // requiredOptions { journal_unit = types.str; }
                  // {
                    # Not `shipped`: which journalctl exists is this module's to
                    # know, and the hardened unit's PATH is not to be relied on.
                    journalctl_path = mkOption {
                      type = types.str;
                      default = "${config.systemd.package}/bin/journalctl";
                      defaultText = lib.literalMD "`journalctl` from `config.systemd.package`";
                      description = ''
                        Which journalctl to run. Defaulted here where the
                        reference has no default for it, because this module
                        knows which systemd the host is running and the hardened
                        unit's `PATH` is not one to resolve a program on.
                      '';
                    };
                  }
                  // shippedOptions {
                    namespace_roots = types.listOf types.str;
                    namespaces = types.listOf types.str;
                    log_max_bytes = types.ints.unsigned;
                    respawn_backoff_secs = types.ints.unsigned;
                    start_grace_secs = types.ints.positive;
                  };
                }
              );
            };
          }
          // shippedOptions {
            scrape_interval_secs = types.ints.positive;
            upload_interval_secs = types.ints.positive;
            upload_max_submissions = types.ints.positive;
            sntp_servers = types.listOf types.str;
            sntp_timeout_secs = types.ints.unsigned;
            spool_max_bytes = types.ints.unsigned;
            spool_busy_timeout_secs = types.ints.unsigned;
            scrape_timeout_secs = types.ints.unsigned;
            scrape_max_body_bytes = types.ints.unsigned;
            upload_timeout_secs = types.ints.unsigned;
            upload_jitter_max_secs = types.ints.unsigned;
            upload_backoff_max_secs = types.ints.unsigned;
            upload_batch_max_bytes = types.ints.unsigned;
            compression_level = types.int;
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
          "%d/${credential}"
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
