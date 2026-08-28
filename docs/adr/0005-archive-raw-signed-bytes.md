# 5. Archive stores the raw signed bytes

Status: accepted (2026-08-19), amended (2026-08-20), the key and the metadata
amended by metsuke-jfb.4, the index removed by metsuke-jfb.5

## Context

Received submissions could be stored decompressed, normalized, or split into
analytics-friendly rows. Every transform is lossy against the one property the
signature gives us. The stored bytes are provably what the pool sent.

## Decision

Each accepted submission becomes one S3 object holding the zstd bytes exactly as
received. What the key is made of is `archive::ObjectName`'s to say; that it is
derived from the receipt and from what the signed bytes carry, never from what a
request claimed, is this decision's. The signature and the key that made it ride
along as object metadata headers, so an object carries everything its own
verification needs; nothing else does, because everything else about a submission
is already inside the bytes. The bucket alone is the source of truth. The server
keeps no index of it, and the endpoint that lists objects passes a bucket
listing through.

## Consequences

- The bucket is a self-verifying corpus. Object bytes + metadata headers reproduce
  the original verification without any database.
- The server is stateless. Restarting it loses nothing, and a listing cannot
  disagree with what the bucket holds because there is no second copy.
- Consumers decompress locally; there is no server-side decompressed copy.
  Dashboard-friendly formats are a downstream concern, derived from the archive.
