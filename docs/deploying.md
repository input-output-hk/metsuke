# Deploying a server and an agent

What we run, as opposed to what an operator runs. The operator's side is the
onboarding page the server renders at `/`, and this file does not repeat it.

`nix/e2e-test.nix` stands the whole thing up in one VM and is the reference
every value here was read against. When this file and that test disagree, the
test is right.

## The constraint that shapes the deployment

The agent refuses a plaintext `upload_url` unless it points at a loopback
address (`crates/metsuke/src/endpoint.rs`). The server speaks plain HTTP and has
no TLS of its own. So an agent on a different host than the server can only
reach it through something that terminates TLS, and standing that up is part of
deploying the server, not an afterthought.

The e2e test avoids this by putting both on one host and uploading to
`http://127.0.0.1:8080`. A real deployment does not get that.

The agent also refuses a `metrics_url` that is not loopback (ADR 0007). The
agent therefore lives on the node host. That is not negotiable by configuration.

## Before anything is deployed

**The bucket and its IAM user.** Not in this repository. The server needs
`s3:PutObject`, `s3:GetObject` and `s3:ListBucket` on one bucket and nothing
else, plus KMS decrypt and generate scoped to the key alias if the bucket is
SSE-KMS. The server never creates a bucket and never deletes an object. Beads
issue `metsuke-4zo.15` holds the full list of protections the bucket wants.

**The credentials.** The server reads `AWS_ACCESS_KEY_ID` and
`AWS_SECRET_ACCESS_KEY` from its process environment, never from its config
file, which is what keeps the config Nix-managed and readable in the open. Put
them in a sops-managed file and hand it to the module as `environmentFile`. The
module asserts it is set whenever the archive is S3.

**The developer password.** One shared account, not one per person. A file the
module loads through systemd `LoadCredential`, so the service user never reads
the original path.

**The allowlist.** Generated offline, never hand-written:

```
metsuke-allowlist query \
  --socket-dir <db-sync socket dir> --dbname <db> --role <role> \
  --metadata-label 674 \
  --metadata-key musashinet_incentives_application_code \
  --statement-timeout 30sec > registrations.csv

metsuke-allowlist generate applications.csv registrations.csv \
  --code-column application_code \
  --pool-columns pool_id_1,pool_id_2,pool_id_3
```

The label and key are `METADATA_LABEL` and `METADATA_KEY` in
`crates/metsuke-server/src/applications.rs`, and the onboarding page's step 2
shows an operator the same pair. `--statement-timeout` is a duration, not
milliseconds.

`query` reads the chain half off a db-sync. `generate` is offline and pure, so
its output is a file you can keep and diff, and it only keeps a pool whose
application code matches the code in its current on-chain registration. It
prints every pool it excluded, and its verdict, to stderr. Read that rather
than only the table. A run where nobody passes the gate is an error rather than
an empty table, because an empty allowlist stops every submission and otherwise
looks exactly like a program nobody joined. It emits the `[ingest.allowlist]`
table the server module's `settings.ingest.allowlist` expects.

## The server host

Import `nixosModules.metsuke-server`. The module defaults `package`, so the
three values that are ours to supply are these:

```nix
services.metsuke-server = {
  enable = true;
  developerPasswordFile = <sops path>;
  environmentFile = <sops path holding the two AWS variables>;
  settings = { ... };
};
```

`settings` mirrors `contrib/server.example.toml` field for field. That file is
the one to copy from, and it is the only place either the values or the reason
for each is written down, because a test reads it.

Three of its fields are decisions rather than defaults. Bind `listen` to
loopback and put a TLS terminator in front of it. `archive.s3` names the bucket
and region the step above created. `ingest.allowlist` is the generated table,
never a hand-written one.

The service runs under `DynamicUser` with `ProtectSystem=strict` and a
`StateDirectory` of `/var/lib/metsuke-server`. A filesystem archive, if you use
one instead of S3, has to have its root under that path, and the module asserts
it.

`settings.developer.password_file` defaults to the path `LoadCredential` puts
the password at. Leave it alone.

The shipped rate limits are a runaway-agent backstop, not abuse control, and
`contrib/server.example.toml` states what headroom they assume. Beads
`metsuke-4zo.108` is where the measured numbers will land once the scrape
cadence is settled. Until then they are guesses.

## The node host

cardano-node exposes nothing to scrape until its `TraceOptions` root entry names
a `PrometheusSimple` backend on loopback. The onboarding page's step 4 has the
snippet and the rule for merging it in, and `instructions.rs` has the reason
behind that rule. Follow the page here rather than improvising, the failure it
warns about is silent.

Then import `nixosModules.metsuke`:

```nix
services.metsuke = {
  enable = true;
  signingKeyFile = <the pool's cold.skey>;
  settings = {
    pool_id = "pool1...";
    agent_id = "relay-1";
    metrics_url = "http://127.0.0.1:<the PrometheusSimple port>/metrics";
    upload_url = "https://<the server's public name>/v1/submit";
  };
};
```

Everything else has a shipped default and the module omits an unset field from
the TOML so the crate's own default applies. `contrib/config.example.toml` is
what those defaults are.

Leave `settings.log` out unless you want the node's trace stream. Setting it
adds `SupplementaryGroups=systemd-journal` and turns `ProcSubset=pid` into
`ProcSubset=all`, and that group reads every unit's journal on the host. That is
the entire privilege difference and it is the reason ADR 0010 made the feature
opt-in.

The agent's spool has to be under `/var/lib/metsuke`, which the module asserts.

### What the agent host has to be allowed to reach

Two things leave the node host. Outbound HTTPS to the server, and outbound UDP
123 to whatever `sntp_servers` names, which defaults to
`time.cloudflare.com:123`. The SNTP query is the only source of
`clock_offset_ms`. Block it and every scrape reports a null offset instead of
failing, so this is a firewall rule that goes wrong quietly. Set
`sntp_servers = []` deliberately if the host has no such egress, rather than
leaving the default to time out on every tick.

## Confirming it works

On the node host, what to run and what the lines mean is the onboarding page's
step 9:

```
systemctl status metsuke
journalctl -u metsuke -f
```

On the server host, the archive is the answer:

```
metsuke-server verify-archive --config /etc/metsuke-server/config.toml
```

It fetches every stored object and re-verifies its signature.

From a developer machine, pull it back:

```
metsuke-fetch list --server https://<server> --user metsuke-dev \
  --password-file <path> --timeout-ms 30000

metsuke-fetch sync --server https://<server> --user metsuke-dev \
  --password-file <path> --timeout-ms 30000 \
  --state ./metsuke.state --into ./downloads
```

`sync` prints the duckdb read that matches what it downloaded.
`docs/reading-the-archive.md` says why that read is not the obvious one.

## On a cardano-parts host

cardano-playground and anything else built from cardano-parts brings its own
assumptions, and two of them meet ours.

**Credentials.** There is no static IAM user anywhere in cardano-playground.
AWS access on its EC2 nodes is SSO and instance roles. The server calls
`Credentials::from_env`, and the S3 client does no instance-metadata lookup, so
an instance profile is unusable and the deploy needs a static key pair against
that convention. Beads `metsuke-4zo.125` is the fix, `metsuke-4zo.15` is the
key pair in the meantime.

**Tracing.** cardano-parts sets `useLegacyTracing = mkDefault false`, so the
node forwards its traces to `cardano-tracer` over a socket rather than writing
them itself. ADR 0010 assumes the node's own journal carries the trace lines.
Whether it still does there is unverified, and beads `metsuke-4zo.126` is the
check. Until someone runs it, deploy with `[log]` left out and take metrics
only. The metrics side already agrees: cardano-parts defaults
`cardanoNodePrometheusExporterPort` to 12798, which is the port the shipped
example config scrapes.

## What is not covered here

There is no monitoring of the agents themselves. Nothing currently notices an
agent that stopped reporting, and the server publishes no ingest counters. Those
are beads `metsuke-4zo.110`, `metsuke-4zo.109` and `metsuke-4zo.111`, and until
they are done a silent agent is found by someone looking.

The developer routes carry no rate limit, so a leaked developer password bills
the bucket at whatever rate the credential holder can manage. That is beads
`metsuke-4zo.74`. Treat the password accordingly.
