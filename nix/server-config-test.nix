# The server module renders a config the server accepts, for both archive
# kinds. A derivation and not a VM node: what this asserts is that the binary
# reads the rendered file and binds, which no init system is part of, and a
# check rather than a hydraJob then runs it where runNixOSTest cannot for want
# of /dev/kvm (flake.nix says why that matters).
#
# The credential half is not here — the file the module points `password_file`
# at is written by systemd, so the unit test's `hub` node is what covers it.
{
  pkgs,
  serverModule,
  server,
}:
let
  poolId = "pool13vscgf9dwn0jt56u965wp99ychz6avktk3pyrye326f3xctz4nm";
  password = pkgs.writeText "developer-password" "not-a-real-secret";

  # Every limit the server refuses to start without, so the archive is the only
  # thing that differs between the two runs below. Port 0: the server reports
  # the address it bound, so neither run has to pick one the other is not on.
  settingsFor = archive: {
    inherit archive;
    listen = "127.0.0.1:0";
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
      # Where systemd would have put it, named directly because nothing here
      # loads a credential.
      password_file = "${password}";
    };
  };

  # What systemd would run, read off the unit the module rendered rather than
  # rebuilt here: that the module's own rendering loads is the whole contract.
  execStartFor =
    archive:
    (pkgs.nixos {
      imports = [ serverModule ];
      services.metsuke-server = {
        enable = true;
        package = server;
        developerPasswordFile = "${password}";
        environmentFile = "${pkgs.writeText "aws-environment" ''
          AWS_ACCESS_KEY_ID=not-a-real-key
          AWS_SECRET_ACCESS_KEY=not-a-real-secret
        ''}";
        settings = settingsFor archive;
      };
    }).config.systemd.services.metsuke-server.serviceConfig.ExecStart;

  s3 = execStartFor {
    s3 = {
      bucket = "cardano-playground-metsuke";
      region = "eu-central-1";
      # Nothing is reached: startup constructs the archive and touches no
      # bucket, which is what makes this a config check and not a live one.
      endpoint = "http://127.0.0.1:1";
      request_timeout_ms = 30000;
      signature_validity_secs = 300;
      put_retries = 1;
      put_retry_backoff_ms = 500;
      list_max_pages = 1000;
    };
  };

  filesystem = execStartFor { filesystem.root = "/var/lib/metsuke-server/archive"; };
in
pkgs.runCommand "metsuke-server-config" { } ''
  # The archive root the filesystem config names: the module requires it under
  # the state directory the unit may write, which is not a path this build has.
  export HOME=$PWD
  mkdir -p var/lib/metsuke-server/archive

  # AWS_* are the environmentFile's job under systemd, and an S3 archive reads
  # them from the environment at startup either way.
  export AWS_ACCESS_KEY_ID=not-a-real-key
  export AWS_SECRET_ACCESS_KEY=not-a-real-secret

  # The startup line is written after the config is read, the credential file is
  # read and the listener is bound, so its arrival is the assertion. Polled
  # rather than waited out: the server does not exit on its own.
  started() {
    local label=$1
    shift
    "$@" 2>"$label.stderr" &
    local pid=$!
    local waited=0
    while [ $waited -lt 100 ]; do
      if grep -q "accepting 1 pools" "$label.stderr"; then
        kill $pid 2>/dev/null || true
        wait $pid 2>/dev/null || true
        echo "$label: $(cat "$label.stderr")"
        return 0
      fi
      if ! kill -0 $pid 2>/dev/null; then
        echo "$label: the server exited instead of accepting" >&2
        cat "$label.stderr" >&2
        return 1
      fi
      sleep 0.1
      waited=$((waited + 1))
    done
    kill $pid 2>/dev/null || true
    echo "$label: no startup line in 10s" >&2
    cat "$label.stderr" >&2
    return 1
  }

  started s3 ${s3}
  started filesystem ${filesystem}
  touch $out
''
