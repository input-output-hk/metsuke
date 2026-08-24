-- Every label-867 row scoping this pool, as the bytes the chain carries. The
-- scope is a filter and not a claim: what makes a row the pool's is the witness
-- inside it, checked in `cip151`.
--
-- `:k` blocks deep, counted against the chain's own tip (ADR 0008). Genesis
-- rows have no block number and can never be that deep.
SELECT encode(tm.bytes, 'hex') AS registration
FROM tx_metadata tm
JOIN tx ON tx.id = tm.tx_id
JOIN block b ON b.id = tx.block_id
WHERE tm.key = 867
  AND tm.json -> '1' -> '1' ->> 1 = :'scope'
  AND b.block_no IS NOT NULL
  AND b.block_no + :k <= (SELECT MAX(block_no) FROM block);
