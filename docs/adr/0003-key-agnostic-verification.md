# 3. Key-agnostic verification: cold key or Calidus, server decides

Status: accepted (2026-08-19)

Some operators will not put a pool cold key on a telemetry machine, and others
will not maintain a second key; mandating either excludes one camp, and which
key an operator signs with is their policy rather than our protocol. Every
submission therefore carries the pool it claims and the key it was signed with,
and the server decides per submission whether that key speaks for that pool —
the cold key because a pool id is its hash, a Calidus key because the pool
registered it on chain (ADR 8).
