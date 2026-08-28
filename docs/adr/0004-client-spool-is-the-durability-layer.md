# 4. Client spool is the only durability layer; ACK means archived

Status: accepted (2026-08-19)

## Context

Scrapes must survive server downtime, client restarts, and upgrades. Durability
could live client-side, server-side, or both; two spools mean two failure modes
and an ambiguous ACK.

## Decision

The client spools scrapes in local SQLite and deletes rows only on server ACK,
retrying all outstanding rows at startup and on every upload interval. The server
holds no spool. It PUTs the object to S3 synchronously and ACKs only after the PUT
succeeds. On PUT failure it returns 5xx and the client's retry takes over. On 4xx
the client keeps the data spooled, logs the server's reason, and retries with
clamped exponential backoff.

## Consequences

- An ACK is a receipt. The data is in the archive, nowhere else, exactly once
  deleted from the client.
- Adding a server-side queue would silently break this contract. Don't.
- Ingest latency includes an S3 round trip; acceptable at telemetry cadence.
- The client spool has a size cap, so an unreachable server degrades by dropping
  oldest data locally rather than filling the disk.
