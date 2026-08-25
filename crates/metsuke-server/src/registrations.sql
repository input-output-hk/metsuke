-- Every label-867 row scoping this pool, as the bytes the chain carries. The
-- scope is a filter and not a claim: what makes a row the pool's is the witness
-- inside it, checked in `cip151`.
--
-- $1 is the pool's scope, as db-sync renders a metadata byte string. $2 is k
-- blocks, counted against the chain's own tip (ADR 0008). Genesis rows have no
-- block number and can never be that deep. The depth is `int` and the row bound
-- `bigint` so the two cannot be bound in each other's place unnoticed.
--
-- $3 bounds the rows returned; what a full answer means is the caller's. No
-- ORDER BY: nothing here chooses between rows (ADR 0008).
SELECT tm.bytes AS registration
FROM tx_metadata tm
JOIN tx ON tx.id = tm.tx_id
JOIN block b ON b.id = tx.block_id
WHERE tm.key = 867
  AND tm.json -> '1' -> '1' ->> 1 = $1::text
  AND b.block_no IS NOT NULL
  AND b.block_no + $2::int <= (SELECT MAX(block_no) FROM block)
LIMIT $3::bigint;
