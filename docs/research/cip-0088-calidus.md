# CIP-0088 / Calidus key research

Answers how a stake pool registers a Calidus ("hot") key on-chain and how a
server verifies that an uploaded payload was signed by the current Calidus
key of a specific pool. Written for musashi-ping-sak.2.

## 1. CIP-0088 base framework

[CIP-0088](https://cips.cardano.org/cip/CIP-0088) ("Token Policy
Registration") defines a generic on-chain registration envelope under
**metadata label 867**, called the Token Registration Payload Object (TRPO).
It was designed to be extensible by other CIPs rather than pool/Calidus
specific.

Top-level structure (label `867`):
- index `0`: version (uint)
- index `1`: Registration Payload (map)
- index `2`: Registration Witness (array)

Payload map keys: `1` scope, `2` feature set, `3` validation method, `4`
nonce, `5` oracle URI (optional), `6` CIP-specific info (optional).

Witness array entries: `[publicKeyBytes, signatureBytes]` for the plain
Ed25519 witness type, signing the hex-encoded CBOR of the payload object.

CIP-0088 itself does not mention Calidus keys, pool cold keys, or key
rotation — that is [CIP-0151](https://cips.cardano.org/cip/CIP-0151) ("On-Chain
Registration - Stake Pools"), which reuses/extends the label-867 TRPO
framework and bumps it to **version 2**.

## 2. CIP-0151: Calidus key registration

Source: https://cips.cardano.org/cip/CIP-0151

- **Calidus key**: an Ed25519 public key "authorized to be used for signing
  authentication or update transactions in the future on behalf of the stake
  pool" — a hot key, distinct from the pool cold key and VRF key.
- **Scope for pool registration**: `[1, h'poolID']`, where `poolID` is the
  blake2b-224 hash of the pool cold verification key (standard pool ID).
- **Cold key anchoring**: the Witness Array *must* include a signature from
  the pool cold key over the payload. This is what proves the pool operator
  authorized this Calidus key — only the cold key holder can add a valid
  witness.
- **Signing payload (v2 change)**: v2 signs `blake2b-256(hex-CBOR(payload
  object))`, not the raw CBOR — chosen for hardware-wallet compatibility.
  Fields must be in numeric index order for deterministic hashing.
- **Witness formats (v2)**:
  - Simple witness: `[witnessType, pubKeyBytes, sigBytes]`.
  - COSE witness (hardware wallets): CIP-0008/CIP-30 `COSE_Key` +
    `COSE_Sign1` pair, used because HW wallets sign via the CIP-8 message
    format. Witness type `2` = CIP-0008 signing.
- **Rotation / re-registration**: nonce is a monotonically increasing uint
  (recommended: current slot height). A new registration with a higher nonce
  supersedes the previous Calidus key for that pool. The **highest valid
  nonce wins** — indexers/servers must track this, not just "latest tx".
  Revocation without replacement: register with `calidusPublicKey =
  h'0000...0000'` (all-zero, 32 bytes).

Concrete CBOR payload observed in `gitmachtl/scripts/cardano/mainnet/15_calidusPoolKey.sh`
(local clone at `/home/manveru/ghq/github.com/gitmachtl/scripts`), lines
332-347:
```
map(5)
  1 -> [1, poolIdHex]        # scope: pool registration
  2 -> []                    # feature set (empty)
  3 -> [2]                   # validation method: CIP-0008 witness
  4 -> nonce                 # uint, defaults to current chain tip slot
  7 -> calidusPublicKeyHex   # 32-byte Ed25519 pubkey
```
matching JSON emitted on-chain (script line 390):
```json
{"867": {"0": 2, "1": {"1":[1,"0x<poolIdHex>"],"2":[],"3":[2],"4":<nonce>,"7":"0x<calidusPubKeyHex>"},
 "2": [ {"1": <COSE_Key>, "2": <COSE_Sign1>} ] } }
```

## 3. Calidus key specifics and tooling

Source: https://github.com/gitmachtl/cardano-signer (README) and the local
`15_calidusPoolKey.sh` script.

- **Derivation path**: `1852H/1815H/0H/0/0` (CIP-1852-style, alias `--path
  calidus` in `cardano-signer keygen`). Produces an extended Ed25519-BIP32
  keypair, restorable from a 24-word mnemonic in light wallets.
- **Calidus ID**: `bech32("calidus", 0xa1 || blake2b_224(calidusPubKeyBytes))`
  — a 58-hex-char / `calidus1...` identifier distinct from the raw pubkey.
- **Registering** (cardano-signer generates the full metadata blob,
  script `genmeta`):
  ```
  cardano-signer sign --cip88 \
    --calidus-public-key <calidus.vkey> \
    --secret-key <pool-cold.skey> [--nonce N] --json-extended
  ```
  This produces the label-867 JSON above, submitted on-chain as tx metadata
  (any transaction, e.g. a self-send).
- **Authenticating with the Calidus key after registration** (script `sign`
  command, line 672) uses **plain Ed25519 signing**, not CIP-8/COSE:
  ```
  cardano-signer sign --data-text "<payload>" --secret-key <calidus.skey> --json-extended
  ```
  Output: `{"signature": "<hex>", "publicKey": "<hex>"}` — a raw 64-byte
  Ed25519 signature over the exact bytes given, no CBOR/COSE wrapping.
- Verification is the exact inverse:
  ```
  cardano-signer verify --data-text "<payload>" \
    --public-key <calidus-pubkey-hex> --signature <hex> --json
  ```
  Result JSON: `{"workMode":"verify","result":"true","verifyDataHex":"...",
  "signature":"...","publicKey":"..."}`.

## 4. Two possible off-chain envelopes

`cardano-signer` supports two distinct signing modes and they matter for
what our server must parse:

| Mode | Trigger | Envelope | Typical signer |
|---|---|---|---|
| Plain Ed25519 | no `--cip8` flag | raw `signature` + `publicKey` hex, no CBOR | CLI-held Calidus `.skey` (server operators, bots) |
| CIP-8/CIP-30 `COSE_Sign1` | `--cip8`/`--cip30` flag | CBOR `COSE_Sign1` (protected header w/ alg + address, payload, signature) + separate `COSE_Key` | browser/light wallets (Typhon, Eternl) and hardware wallets signing via CIP-30 `signData` |

CIP-0151's on-chain **registration witness** from a hardware wallet is
always CIP-8/COSE (because HW wallets can only sign via that message
format) — see script lines 359-390, which calls `cardano-hw-cli message
sign` then `cardano-signer verify --cip8 --cose-key ... --cose-sign1 ...`.
The **off-chain authentication payload** signed by the Calidus key
afterwards can be either, depending on what client library the pool operator
uses to hold/use the Calidus key.

## 5. Server-side pool -> current Calidus key resolution

Koios exposes a dedicated endpoint, confirmed live on mainnet:
```
GET https://api.koios.rest/api/v1/pool_calidus_keys?pool_status=eq.registered&order=block_time.asc&pool_id_bech32=eq.<poolId>
```
(rollout history: Sancho-Net beta -> Preview -> Mainnet beta -> Mainnet v1,
per the Koios gRest changelog:
https://cardano-community.github.io/guild-operators/Build/grest-changelog/)

Response fields per entry (confirmed via schema description and script
lines 613-619):
`pool_id_bech32`, `calidus_id_bech32`, `calidus_pub_key` (hex),
`calidus_nonce`, `pool_status`, `registered` (bool), `bytes` (raw CBOR of
the registration cert), `tx_hash`, `epoch_no`, `block_height`, `block_time`.

A pool can have multiple historical entries; **the correct current key is
the one with `pool_status=registered` and the highest `calidus_nonce`** for
that `pool_id_bech32` — the script's own query only filters
`pool_status=eq.registered` and relies on nonce ordering client-side (see
line 634's "unique key count" stat, which implies duplicates are expected
and must be de-duplicated by nonce).

No mention of a dedicated cardano-db-sync table for Calidus keys was found;
Koios' `pool_calidus_keys` view is itself built by indexing label-867
metadata off db-sync, so db-sync + a metadata-label-867 CBOR parser is the
fallback if Koios availability/rate limits are a concern.

**Rollback/freshness**: Calidus registrations are ordinary on-chain metadata
transactions, so they are subject to normal chain rollback during
short-lived forks. Koios reflects db-sync's canonical chain view, so
querying Koios (rather than caching an unconfirmed mempool tx) is safe
against rollback as long as the server does not act on Calidus keys before
enough confirmations. No CIP or Koios doc gives a specific confirmation-depth
recommendation for Calidus keys specifically; treat it like any other
on-chain metadata (standard confirmation depth, e.g. the ~5-15 block depth
convention used elsewhere in Cardano tooling — not sourced to a Calidus-
specific doc).

## Recommendation

1. **Envelope**: accept both raw Ed25519 (`{signature, publicKey}` hex pair
   over the exact payload bytes) and CIP-8 `COSE_Sign1`+`COSE_Key` — don't
   assume the client is CLI-only. Verify with `cardano-signer verify
   [--cip8] --data-hex <payload> --signature <hex> --public-key <hex>` (shell
   out) or an equivalent Ed25519/COSE library; `cardano-signer` is the
   simplest correctness-verified reference implementation and avoids
   re-deriving the COSE_Sign1 structure by hand.
2. **Lookup**: resolve `pool_id -> calidus_pub_key` via Koios
   `GET /pool_calidus_keys?pool_status=eq.registered&pool_id_bech32=eq.<id>`,
   then pick the row with the max `calidus_nonce`. Compare the verified
   signer's public key (bytes, not bech32 string) against that value.
3. **Caching**: cache pool -> Calidus key per pool ID with a short TTL (e.g.
   minutes, not blocks) since rotation is rare but must be picked up without
   a redeploy; invalidate/refetch on verify failure before rejecting, in
   case of a very recent rotation the cache missed.
4. **Rollback safety**: don't trust a Calidus key from a Koios row with
   fewer than a small confirmation buffer (a handful of blocks) if the
   verification consequence is security-sensitive (e.g. granting write
   access) — Koios itself only reflects confirmed chain state via db-sync,
   so this is a belt-and-suspenders margin, not a Koios data-quality gap.

## Sources
- https://cips.cardano.org/cip/CIP-0088
- https://cips.cardano.org/cip/CIP-0151
- https://github.com/gitmachtl/cardano-signer (README)
- `/home/manveru/ghq/github.com/gitmachtl/scripts/cardano/mainnet/15_calidusPoolKey.sh` (local clone)
- https://cardano-community.github.io/guild-operators/Build/grest-changelog/
- https://api.koios.rest/api/v1/pool_calidus_keys (live endpoint, schema observed)
