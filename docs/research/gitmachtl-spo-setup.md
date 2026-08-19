# Gitmachtl SPO Setup Research

This document summarizes SPO setup patterns based on the gitmachtl/scripts toolkit, which is the standard reference for stake pool operators on Cardano. The gitmachtl scripts are comprehensive transaction and key management tools that assume a running cardano-node and provide guidance on typical SPO infrastructure patterns.

## 1. How cardano-node is Launched

The gitmachtl/scripts toolkit does not include systemd units or node launch scripts directly. Instead, the README references external tutorials as authoritative sources:
- [Cardano-Node-Compiling-Guide (IOHK)](https://github.com/input-output-hk/cardano-node-wiki/blob/main/docs/getting-started/install.md)
- [Cardano-Node-Installation-Guide (Stakepool247.eu)](https://cardano-node-installation.stakepool247.eu/)
- [Coincashew Haskell SPO Tutorial](https://www.coincashew.com/coins/overview-ada/guide-how-to-build-a-haskell-stakepool-node)
- [Cardano Foundation SPO Course](https://cardano-foundation.gitbook.io/stake-pool-course/stake-pool-guide/getting-started/install-node)

**Implicit SPO patterns from toolkit config:**
- Node runs as a systemd service (typical for production setups referenced in guides)
- Node user account created for service isolation
- Restart policy not specified in scripts (assumed restart=always or on-failure per standard guides)
- Node launched with config JSON and topology JSON (see `/cardano/mainnet/00_common.sh:50-51`)
- File references: `/home/manveru/ghq/github.com/gitmachtl/scripts/cardano/mainnet/00_common.sh`

## 2. Logging Configuration

**Output routing:**
- cardano-node typically outputs to journald via systemd stdout (standard practice, not explicit in scripts)
- No file-based logging configuration in gitmachtl toolkit
- Scripts assume JSON/structured logging from node for consistency

**Trace/logging config:**
- Node accepts `--trace-blockchain`, `--trace-mempool`, etc. CLI flags
- RTView (real-time metrics monitoring tool) commonly paired with node for visualization
- Logging config shipped with node via Haskell RTS flags (-N, -A, -qn)
- Default logging to journald, queried by operators via `journalctl -u cardano-node`
- File references: `/home/manveru/ghq/github.com/gitmachtl/scripts/cardano/mainnet/README.md:107-113`

## 3. Node Configuration, Topology, and Socket Paths

**Config and topology files:**
- Downloaded separately from [Cardano book (official source)](https://book.play.dev.cardano.org/environments.html)
- Configs for MAINNET, PREPROD, PREVIEW, SANCHONET available
- Include both Shelley and Byron genesis files
- File references: `/home/manveru/ghq/github.com/gitmachtl/scripts/cardano/mainnet/README.md:9`, `/cardano/mainnet/00_common.sh:50-51`

**Socket path:**
- Default configurable socket: `db-mainnet/node.socket` (scripts parameter: `socket="db/node.socket"`)
- Can vary: examples show `$HOME/cnode/sockets/node.socket` in comments
- Socket created by cardano-node at startup
- Socket path must be passed to cardano-cli via `CARDANO_NODE_SOCKET_PATH` environment variable
- File reference: `/home/manveru/ghq/github.com/gitmachtl/scripts/cardano/mainnet/00_common.sh:46`

**Socket permissions:**
- Not explicitly documented in scripts, but standard Unix socket permissions apply
- Running agents must have read/write access to socket file
- Group membership may be required (cardano-node runs as specific user, socket accessible by group)
- File reference: `/home/manveru/ghq/github.com/gitmachtl/scripts/cardano/mainnet/00_common.sh:272`

## 4. Key Handling Conventions & Calidus Keys

**Standard key file naming (poolname is reference identifier):**
- `poolname.node.skey` / `poolname.node.vkey` — cold keys (operator responsibility)
- `poolname.kes-XXX.skey` / `poolname.kes-XXX.vkey` — hot keys (rotated every KES period)
- `poolname.node-XXX.opcert` — operational certificate (derives from KES key + counter)
- `poolname.vrf.skey` / `poolname.vrf.vkey` — VRF keys for block lottery
- `poolname.pool.json` — pool metadata, includes relay info and security parameters
- `poolname.kes.counter` — increments with each KES generation
- File references: `/home/manveru/ghq/github.com/gitmachtl/scripts/cardano/mainnet/README.md:837,993-996`

**Key protection:**
- Scripts support CLI (`cli`), encrypted (`enc`), and hardware wallet (`hw`) modes
- Encrypted keys: `.skey` files password-protected; prompted on use via `01_protectKey.sh`
- Hardware wallet support: keys managed off-device (Ledger/Trezor), only public keys stored locally
- File reference: `/home/manveru/ghq/github.com/gitmachtl/scripts/cardano/mainnet/00_common.sh:34-56`

**Calidus Pool Keys:**
- Calidus is a pool performance/validation signing mechanism (separate from node cold keys)
- Script: `15_calidusPoolKey.sh`
- File naming: `poolname.calidus.skey` / `poolname.calidus.vkey`
- Mnemonics stored separately: `poolname.calidus.mnemonics`
- Calidus ID: `poolname.calidus.id` (bech32 format)
- Path derivation: `1852H/1815H/0H/0/0` (standard Cardano BIP-32 path for pool operations)
- Uses `cardano-signer` binary for key generation and signing
- File references: `/home/manveru/ghq/github.com/gitmachtl/scripts/cardano/mainnet/15_calidusPoolKey.sh:66,117-150`

## 5. Implications for a Metrics Agent Supporting Gitmachtl Setups

**Discovery and access patterns:**
1. **Node socket location**: Must be configurable or auto-discovered via common paths (db/node.socket, $HOME/cnode/sockets/node.socket)
2. **Socket group membership**: Agent process must be in `cardano` or same group as cardano-node to read socket
3. **Journald access**: Agent must have permissions to read node logs via `journalctl` (typically `systemd-journal` group membership or root)
4. **Key file discovery**: Keys located in pool operator's working directory; no standard location (operator dependent)

**Configuration discovery:**
1. Scripts read config from:
   - `/cardano/mainnet/00_common.sh` (source script)
   - `common.inc` (calling directory override)
   - `$HOME/.common.inc` (global override)
2. Agent must support reading environment variable `CARDANO_NODE_SOCKET_PATH` or scanning config files
3. Genesis files (Shelley + Byron) required for KES period calculation; typically co-located with node config

**Operational constraints:**
1. **Online vs. Offline mode**: Agent only runs on "online" machines (where cardano-node is running)
2. **Light-mode unsupported**: Agents cannot operate where node uses API-only queries (no local socket)
3. **Key access**: Agent should NOT access `.skey` files; only `.vkey` files and metadata
4. **Calidus integration**: If monitoring pool registrations, agent needs to parse `poolname.calidus.id` from file system

**Permissions and security model:**
- Agent runs as non-root service user (should NOT be cardano user)
- Requires supplementary group membership: `cardano` (for socket) and `systemd-journal` (for logs)
- Key files remain protected; agent reads only metadata and performance data via node API
- Encrypted keys handled by cardano-cli, not exposed to agent

**Reference implementations:**
- Node configuration sourced from: https://book.play.dev.cardano.org/environments.html
- Key generation via `04a_genNodeKeys.sh`, `04c_genKESKeys.sh`, `04d_genNodeOpCert.sh`
- Calidus key setup via `15_calidusPoolKey.sh`
- Socket path typically passed via `CARDANO_NODE_SOCKET_PATH` env var to scripts

