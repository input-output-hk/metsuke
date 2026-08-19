# 7. Agent reads only the loopback Prometheus endpoint

Status: accepted (2026-08-19)

## Context

The agent may eventually run on mainnet block producers, so its privilege set is
the attack surface. cardano-node offers three data sources: the PrometheusSimple
metrics endpoint (loopback HTTP), the node socket via LocalStateQuery (filesystem
group access, version-coupled decoding), and journald traces (systemd-journal
group, no documented schema).

## Decision

The agent scrapes only the node's PrometheusSimple endpoint on loopback. No node
socket, no journal access, no supplementary groups. Node version and revision come
from the build-info metric on the same endpoint. Clock offset comes from the
agent's own SNTP query — one code path, comparable across pools. All sampled
fields are nullable: a failed scrape is itself signal and still uploads.

## Consequences

- The example systemd unit runs with DynamicUser and no group grants; compromise
  of the agent yields read access to a metrics page and nothing else.
- SPOs must enable the PrometheusSimple backend in node config — the one setup
  step this choice imposes; the instructions page carries it.
- Log-based telemetry (v2) will pressure this boundary. Widening privileges then
  is a new decision that supersedes this one, not an implementation detail.
