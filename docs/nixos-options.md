# NixOS options

`services.metsuke`, as `nixosModules.metsuke` declares it.

Generated: edit `nix/agent-module.nix`, then
`nix build .#nixos-options` and commit what it wrote here.
Every description and default below is read out of
`contrib/config.example.toml`, so those are changed there.

## services\.metsuke\.enable

Whether to enable the metsuke telemetry agent\.



*Type:*
boolean



*Default:*

```nix
false
```



*Example:*

```nix
true
```

*Declared by:*
 - [nix/agent-module\.nix](https://github.com/input-output-hk/metsuke/blob/main/nix/agent-module.nix)



## services\.metsuke\.package



The agent to run\.



*Type:*
package

*Declared by:*
 - [nix/agent-module\.nix](https://github.com/input-output-hk/metsuke/blob/main/nix/agent-module.nix)



## services\.metsuke\.restartSecs



How long systemd waits before restarting a stopped agent\.



*Type:*
unsigned integer, meaning >=0



*Default:*

```nix
30
```

*Declared by:*
 - [nix/agent-module\.nix](https://github.com/input-output-hk/metsuke/blob/main/nix/agent-module.nix)



## services\.metsuke\.settings



The agent’s configuration file, one option per field of
` crates/metsuke/src/config.rs ` bar ` signing_key `, which this module
owns: the key arrives as a systemd credential\. The first three have no
default and are set here or evaluation fails naming them\.

` contrib/config.example.toml ` is the annotated reference and says what
each of these is for and what it defaults to, in the same words an
operator editing the file by hand reads\. A deployment serves it at
` /files/config.example.toml `, so a checkout is not needed to read it\.

An option left ` null ` is left out of the rendered file entirely, so the
agent applies its own default rather than receiving one from here\. The
options below carrying a description are the ones where this module
behaves differently from that reference\.



*Type:*
submodule

*Declared by:*
 - [nix/agent-module\.nix](https://github.com/input-output-hk/metsuke/blob/main/nix/agent-module.nix)



## services\.metsuke\.settings\.agent_id



What to call this agent on every line it ships, so a pool
reporting from more than one can tell them apart\. Unset, it is
this host’s hostname folded to lowercase ` a-z0-9 ` in
dash-separated runs, and a value set here is folded the same way\.
Set it where the hostname is not the name you want, and in a
container, where the hostname is the runtime’s rather than yours\.



*Type:*
null or string



*Default:*
this host’s name, folded to lowercase

*Declared by:*
 - [nix/agent-module\.nix](https://github.com/input-output-hk/metsuke/blob/main/nix/agent-module.nix)



## services\.metsuke\.settings\.compression_level



zstd level for the upload body (0 = zstd’s default)\.



*Type:*
null or signed integer



*Default:*
0

*Declared by:*
 - [nix/agent-module\.nix](https://github.com/input-output-hk/metsuke/blob/main/nix/agent-module.nix)



## services\.metsuke\.settings\.log



Trace-line collection\. Setting it opens the unit up by what
reading a journal takes, which is the privilege ADR 0010 is
about and which nix/unit\.nix spells out; leaving it null starts
no journalctl and grants nothing\.



*Type:*
null or (submodule)



*Default:*

```nix
null
```

*Declared by:*
 - [nix/agent-module\.nix](https://github.com/input-output-hk/metsuke/blob/main/nix/agent-module.nix)



## services\.metsuke\.settings\.log\.journal_unit



Your node’s unit, and journalctl’s absolute path\. Neither has a default\.



*Type:*
string

*Declared by:*
 - [nix/agent-module\.nix](https://github.com/input-output-hk/metsuke/blob/main/nix/agent-module.nix)



## services\.metsuke\.settings\.log\.journalctl_path



Which journalctl to run\. Defaulted here where the
reference has no default for it, because this module
knows which systemd the host is running and the hardened
unit’s ` PATH ` is not one to resolve a program on\.



*Type:*
string



*Default:*
` journalctl ` from ` config.systemd.package `

*Declared by:*
 - [nix/agent-module\.nix](https://github.com/input-output-hk/metsuke/blob/main/nix/agent-module.nix)



## services\.metsuke\.settings\.log\.log_max_bytes



The trace-line spool’s cap, and how long a journalctl that died waits before it is started again\.



*Type:*
null or (unsigned integer, meaning >=0)



*Default:*
268435456

*Declared by:*
 - [nix/agent-module\.nix](https://github.com/input-output-hk/metsuke/blob/main/nix/agent-module.nix)



## services\.metsuke\.settings\.log\.namespace_roots



The ceiling on what may be selected, and the selection itself\. Both are prefixes matched on segment boundaries\.



*Type:*
null or (list of string)



*Default:*
\[“Consensus”, “ChainDB”, “Forge”]

*Declared by:*
 - [nix/agent-module\.nix](https://github.com/input-output-hk/metsuke/blob/main/nix/agent-module.nix)



## services\.metsuke\.settings\.log\.namespaces



The ceiling on what may be selected, and the selection itself\. Both are prefixes matched on segment boundaries\.



*Type:*
null or (list of string)



*Default:*
\[“Consensus\.LeiosKernel”, “Consensus\.LeiosPeer”, “ChainDB\.AddBlockEvent\.AddedToCurrentChain”, “Forge\.Loop\.AdoptedBlock”]

*Declared by:*
 - [nix/agent-module\.nix](https://github.com/input-output-hk/metsuke/blob/main/nix/agent-module.nix)



## services\.metsuke\.settings\.log\.respawn_backoff_secs



The trace-line spool’s cap, and how long a journalctl that died waits before it is started again\.



*Type:*
null or (unsigned integer, meaning >=0)



*Default:*
30

*Declared by:*
 - [nix/agent-module\.nix](https://github.com/input-output-hk/metsuke/blob/main/nix/agent-module.nix)



## services\.metsuke\.settings\.log\.source



Which stream the trace lines come from\. Only ` journald `
here, unlike the reference, which also offers ` pipe `: the
unit this module renders runs the agent on its own with
no node upstream, so a pipe would get ` /dev/null `, read
EOF at once, and ` Restart=always ` would loop it forever
collecting nothing\. Take the shipped drop-in instead if
you want that source\.



*Type:*
value “journald” (singular enum)



*Default:*

```nix
"journald"
```

*Declared by:*
 - [nix/agent-module\.nix](https://github.com/input-output-hk/metsuke/blob/main/nix/agent-module.nix)



## services\.metsuke\.settings\.log\.start_grace_secs



How long a just-started journalctl has to stay running before the agent accepts it as following\.



*Type:*
null or (positive integer, meaning >0)



*Default:*
1

*Declared by:*
 - [nix/agent-module\.nix](https://github.com/input-output-hk/metsuke/blob/main/nix/agent-module.nix)



## services\.metsuke\.settings\.metrics_url



Required: the node’s loopback PrometheusSimple endpoint\.



*Type:*
string

*Declared by:*
 - [nix/agent-module\.nix](https://github.com/input-output-hk/metsuke/blob/main/nix/agent-module.nix)



## services\.metsuke\.settings\.pool_id



Required: your pool id, bech32\.



*Type:*
string

*Declared by:*
 - [nix/agent-module\.nix](https://github.com/input-output-hk/metsuke/blob/main/nix/agent-module.nix)



## services\.metsuke\.settings\.scrape_interval_secs



Cadences: scraping and uploading are independent\.



*Type:*
null or (positive integer, meaning >0)



*Default:*
300

*Declared by:*
 - [nix/agent-module\.nix](https://github.com/input-output-hk/metsuke/blob/main/nix/agent-module.nix)



## services\.metsuke\.settings\.scrape_max_body_bytes



Scrape limits\.



*Type:*
null or (unsigned integer, meaning >=0)



*Default:*
4194304

*Declared by:*
 - [nix/agent-module\.nix](https://github.com/input-output-hk/metsuke/blob/main/nix/agent-module.nix)



## services\.metsuke\.settings\.scrape_timeout_secs



Scrape limits\.



*Type:*
null or (unsigned integer, meaning >=0)



*Default:*
5

*Declared by:*
 - [nix/agent-module\.nix](https://github.com/input-output-hk/metsuke/blob/main/nix/agent-module.nix)



## services\.metsuke\.settings\.sntp_servers



SNTP clock-offset probe: servers tried in order, per-server timeout\.



*Type:*
null or (list of string)



*Default:*
\[“time\.cloudflare\.com:123”]

*Declared by:*
 - [nix/agent-module\.nix](https://github.com/input-output-hk/metsuke/blob/main/nix/agent-module.nix)



## services\.metsuke\.settings\.sntp_timeout_secs



SNTP clock-offset probe: servers tried in order, per-server timeout\.



*Type:*
null or (unsigned integer, meaning >=0)



*Default:*
5

*Declared by:*
 - [nix/agent-module\.nix](https://github.com/input-output-hk/metsuke/blob/main/nix/agent-module.nix)



## services\.metsuke\.settings\.spool_busy_timeout_secs



SQLite spool: scrapes wait here until the server acks them\. Oldest bytes beyond spool_max_bytes are dropped\. spool_busy_timeout_secs is how long one write waits for the other connection, which exists only with \[log] below\.



*Type:*
null or (unsigned integer, meaning >=0)



*Default:*
5

*Declared by:*
 - [nix/agent-module\.nix](https://github.com/input-output-hk/metsuke/blob/main/nix/agent-module.nix)



## services\.metsuke\.settings\.spool_max_bytes



SQLite spool: scrapes wait here until the server acks them\. Oldest bytes beyond spool_max_bytes are dropped\. spool_busy_timeout_secs is how long one write waits for the other connection, which exists only with \[log] below\.



*Type:*
null or (unsigned integer, meaning >=0)



*Default:*
33554432

*Declared by:*
 - [nix/agent-module\.nix](https://github.com/input-output-hk/metsuke/blob/main/nix/agent-module.nix)



## services\.metsuke\.settings\.spool_path



Where the SQLite spool goes\. It has to be under ` /var/lib/metsuke `,
which is the StateDirectory this module’s unit creates and the
only path ` ProtectSystem=strict ` leaves it able to write\. An
assertion refuses anything else rather than rendering a unit that
dies on its first open\.



*Type:*
null or string



*Default:*
/var/lib/metsuke/spool\.sqlite

*Declared by:*
 - [nix/agent-module\.nix](https://github.com/input-output-hk/metsuke/blob/main/nix/agent-module.nix)



## services\.metsuke\.settings\.upload_backoff_max_secs



Upload limits: request deadline, the spread that places this agent within the upload interval, and the clamp on the rejection backoff\. The first accepted submission picks a point in upload_jitter_max_secs and every upload after it keeps that point, so agents installed together do not all upload in the same second while yours still lands on a cadence you can watch for\. It is a ceiling: a spread wider than the interval places nobody better than one exactly as wide, so a shorter upload_interval_secs bounds it instead\.



*Type:*
null or (unsigned integer, meaning >=0)



*Default:*
86400

*Declared by:*
 - [nix/agent-module\.nix](https://github.com/input-output-hk/metsuke/blob/main/nix/agent-module.nix)



## services\.metsuke\.settings\.upload_batch_max_bytes



Upload limits: request deadline, the spread that places this agent within the upload interval, and the clamp on the rejection backoff\. The first accepted submission picks a point in upload_jitter_max_secs and every upload after it keeps that point, so agents installed together do not all upload in the same second while yours still lands on a cadence you can watch for\. It is a ceiling: a spread wider than the interval places nobody better than one exactly as wide, so a shorter upload_interval_secs bounds it instead\.



*Type:*
null or (unsigned integer, meaning >=0)



*Default:*
4194304

*Declared by:*
 - [nix/agent-module\.nix](https://github.com/input-output-hk/metsuke/blob/main/nix/agent-module.nix)



## services\.metsuke\.settings\.upload_interval_secs



Cadences: scraping and uploading are independent\.



*Type:*
null or (positive integer, meaning >0)



*Default:*
3600

*Declared by:*
 - [nix/agent-module\.nix](https://github.com/input-output-hk/metsuke/blob/main/nix/agent-module.nix)



## services\.metsuke\.settings\.upload_jitter_max_secs



Upload limits: request deadline, the spread that places this agent within the upload interval, and the clamp on the rejection backoff\. The first accepted submission picks a point in upload_jitter_max_secs and every upload after it keeps that point, so agents installed together do not all upload in the same second while yours still lands on a cadence you can watch for\. It is a ceiling: a spread wider than the interval places nobody better than one exactly as wide, so a shorter upload_interval_secs bounds it instead\.



*Type:*
null or (unsigned integer, meaning >=0)



*Default:*
300

*Declared by:*
 - [nix/agent-module\.nix](https://github.com/input-output-hk/metsuke/blob/main/nix/agent-module.nix)



## services\.metsuke\.settings\.upload_max_submissions



Upload limits: request deadline, the spread that places this agent within the upload interval, and the clamp on the rejection backoff\. The first accepted submission picks a point in upload_jitter_max_secs and every upload after it keeps that point, so agents installed together do not all upload in the same second while yours still lands on a cadence you can watch for\. It is a ceiling: a spread wider than the interval places nobody better than one exactly as wide, so a shorter upload_interval_secs bounds it instead\.



*Type:*
null or (positive integer, meaning >0)



*Default:*
16

*Declared by:*
 - [nix/agent-module\.nix](https://github.com/input-output-hk/metsuke/blob/main/nix/agent-module.nix)



## services\.metsuke\.settings\.upload_timeout_secs



Upload limits: request deadline, the spread that places this agent within the upload interval, and the clamp on the rejection backoff\. The first accepted submission picks a point in upload_jitter_max_secs and every upload after it keeps that point, so agents installed together do not all upload in the same second while yours still lands on a cadence you can watch for\. It is a ceiling: a spread wider than the interval places nobody better than one exactly as wide, so a shorter upload_interval_secs bounds it instead\.



*Type:*
null or (unsigned integer, meaning >=0)



*Default:*
60

*Declared by:*
 - [nix/agent-module\.nix](https://github.com/input-output-hk/metsuke/blob/main/nix/agent-module.nix)



## services\.metsuke\.settings\.upload_url



Required: the metsuke-server submission endpoint\.



*Type:*
string

*Declared by:*
 - [nix/agent-module\.nix](https://github.com/input-output-hk/metsuke/blob/main/nix/agent-module.nix)



## services\.metsuke\.signingKeyFile



The pool’s cold or Leios signing key, in cardano-cli TextEnvelope
form\. A cold key has to hash to the configured pool id or the agent
refuses to start; a Leios key hashes to nothing, so the server’s
roster settles which pool it speaks for\.
Read by systemd as root and handed to the agent as a credential, so
the file itself stays unreadable to the service user\.



*Type:*
string

*Declared by:*
 - [nix/agent-module\.nix](https://github.com/input-output-hk/metsuke/blob/main/nix/agent-module.nix)


