# OpenBlockperf Architecture Research

OpenBlockperf is a Python-based metrics collection and reporting system for Cardano relay nodes, deployed as a systemd service. It captures block propagation timing, peer topology, and node context data, then submits them to a centralized HTTP API.

This document summarizes its design, data flow, dependencies, and operational lessons for teams building similar SPO metrics agents.

## 1. Data Collected

OpenBlockperf captures three categories of telemetry:

### Block Propagation Events

Each block's journey is tracked in four stages (source: `docs/blockperf-client.md`):
1. Time until relay hears about the header
2. Time spent requesting the block body from peers
3. Time until body download completes
4. Time until node validates and adopts the block

Each stage is measured in millisecond detail and aggregated into a `BlockSample` submitted per block hash (source: `src/openblockperf/blocksamplegroup.py`).

Events tracked for block timing (source: `src/openblockperf/handler.py` REGISTERED_NAMESPACES):
- `ChainSync.Client.DownloadedHeader` — header arrival time
- `BlockFetch.Client.SendFetchRequest` — fetch request initiation
- `BlockFetch.Client.CompletedBlockFetch` — body download completion
- `ChainDB.AddBlockEvent.AddedToCurrentChain` — block adoption
- `ChainDB.AddBlockEvent.SwitchedToAFork` — fork switch

### Peer State Changes

Peer connections are tracked at the inbound and remote level (source: `src/openblockperf/handler.py` REGISTERED_NAMESPACES):
- `Net.InboundGovernor.Local.PromotedToWarmRemote`, `PromotedToHotRemote`, `DemotedToColdRemote`, `DemotedToWarmRemote`
- `Net.InboundGovernor.Remote.*` — same transitions for remote-initiated connections
- `Net.PeerSelection.Actions.StatusChanged` — peer status changes

Each peer event includes (source: `src/openblockperf/models/events.py`):
- Local address/port
- Remote address/port
- Connection direction
- Peer state transition type
- Timestamp

### Node Context

Sent once at startup (source: `src/openblockperf/app.py` send_clientinfo_task):
- Cardano-node version
- OpenBlockperf client version
- Relay IP and port

### Inbound Governor Counters

Node peer pool state (source: `src/openblockperf/handler.py` REGISTERED_NAMESPACES):
- `Net.InboundGovernor.Local.InboundGovernorCounters`
- `Net.InboundGovernor.Remote.InboundGovernorCounters`

Each counter event reports: idle peers, cold peers, warm peers, hot peers (source: `EVENTS.md`).

## 2. Ingestion

### Log Source

OpenBlockperf reads from `journalctl` (systemd journal), not directly from cardano-node stdout (source: `src/openblockperf/logreader.py` JournalCtlLogReader).

The log reader:
1. Starts a subprocess running `journalctl -f -u <service> -o cat` to stream logs (source: `logreader.py:104-114`)
2. Parses each line as JSON
3. Replays all historical logs from the last service startup before switching to live tailing (source: `logreader.py:247-376` replay_from_startup)

The replay mechanism finds the last `"ns":"Net.Server.Local.Started"` event to determine where to resume, then replays all events until "now" (source: `logreader.py:263, 281`).

### Required Node Configuration

The node must output traces in **stdout MachineFormat** (JSON) and enable specific trace options (source: `docs/blockperf-traceoptions.md`):

Essential traces:
- `BlockFetch.Client`: DownloadedHeader, SendFetchRequest, CompletedBlockFetch
- `ChainDB.AddBlockEvent`: AddedToCurrentChain, SwitchedToAFork
- `Net.ConnectionManager.Remote`
- `Net.InboundGovernor.Remote` and `Net.InboundGovernor.Local`
- `Net.PeerSelection`

Optional metrics backend (source: `docs/blockperf-traceoptions.md:26`):
- `PrometheusSimple 127.0.0.1 12798` for node metrics scraping

### EKG Metrics Scraping

OpenBlockperf polls a local Prometheus exposition endpoint (source: `src/openblockperf/ekg.py` EkgClient) to fetch node version and sync state:
- URL default: `http://localhost:12798/metrics`
- Metrics requested: `cardano_node_metrics_cardano_version_*` (source: `ekg.py:180-189`)

Used for determining node readiness and reporting context (source: `app.py:178`).

## 3. Transport and Authentication

### HTTP API

All data is submitted via **HTTPS POST** requests to a backend API (source: `src/openblockperf/apiclient/base.py`):

Base URL (network-dependent, source: `src/openblockperf/config.py:65-82`):
- Mainnet: `https://api.openblockperf.cardano.org:443/api/v0/`
- Preprod: `https://preprod.api.openblockperf.cardano.org:443/api/v0/`
- Preview: `https://preview.api.openblockperf.cardano.org:443/api/v0/`

Endpoints (source: `apiclient/client.py`):
- `/submit/blocksample` — block timing data
- `/submit/peerevent` — peer state changes
- `/submit/clientinfo` — node context
- `/registration/calidus/challenge` — Calidus registration (challenge/response)
- `/registration/ip` — IP-based registration

### Authentication

Two registration flows (source: `README.md:276-319`):

**Calidus Key Registration** (SPO-level, multi-relay):
- Requires the stake pool's Calidus signing key
- Command: `blockperf register-calidus --pool-id <bech32> --calidus-skey <path>`
- Backend verifies signature against pool credentials
- API key assigned to the pool's stake(s)

**IP-based Registration** (single relay):
- Command: `blockperf register-ip`
- API key bound to the relay's public IP (IPv4/IPv6)
- Supports renewal (`--force-renewal`) and IP rebinding (`--update-ip`)

### Request Headers

All API requests include (source: `apiclient/base.py:101-102`):
- `X-Api-Key: <api_key>` — authentication
- `X-Hostname: <node_name>` — relay identifier

### Transport Security

Uses **httpx.AsyncClient** with standard HTTPS (source: `apiclient/base.py:72-75`). TLS is handled by the system's certificate store; no custom cert pinning observed.

## 4. Operational Shape

### Installation

Provided via an interactive bash installer script (source: `blockperf-install.sh`):

Steps:
1. Check/install OS prerequisites (Python 3.x, jq, curl, systemd, coreutils)
2. Resolve service user/group, node name, and cardano-node unit
3. Resolve network and API-key strategy
4. Install Python venv, install package via pip, write env file, systemd unit, CLI wrapper
5. Optionally start service

Installer modes:
- Interactive (default) — guided step-by-step
- Non-interactive (`--yes`) — env vars or CLI flags define all settings

Install artifacts:
- Systemd unit: `/etc/systemd/system/openblockperf.service`
- Environment file: `/etc/default/openblockperf`
- CLI wrapper: `/usr/local/bin/blockperf`
- App + venv: `/opt/cardano/openblockperf`
- Logs: `journalctl -fu openblockperf.service`

### Runtime Requirements

- **Python 3.12+** (source: `pyproject.toml:10`)
- **Dependencies** (source: `pyproject.toml:11-23`):
  - `httpx` — async HTTP client
  - `pydantic` — data validation
  - `pydantic-settings` — configuration management
  - `loguru` — structured logging
  - `click`, `typer` — CLI framework
  - `textual` — TUI (if used)
  - `psutil`, `cbor2`, `pycardano` — system and crypto utilities

### Concurrency Model

Uses **asyncio TaskGroup** for concurrent task execution (source: `src/openblockperf/app.py:95-127`):

Tasks running in parallel:
1. `send_clientinfo_task` — submit node version once
2. `process_events_task` — log replay + live event stream
3. `send_block_samples_task` — periodically check and submit block samples
4. `print_peer_statistics_task` — debug output
5. `monitor_sync_state_task` — poll node sync status

All tasks fail-fast: any task exception crashes the app (source: `app.py:160-161`).

### Failure Handling

**Log replay robustness** (source: `logreader.py:247-376`):
- Gracefully handles missing startup marker with `StartupMarkerNotFoundError`
- Skips malformed JSON lines with debug logging
- Cleans up subprocess with timeout-based termination

**API retry logic** (source: `app.py:167-187`):
- Exponential backoff on API errors: starts at 10s, increments by 10s, caps at 60s
- `send_clientinfo_task` retries indefinitely until success

**Sync gate** (source: `app.py:171-172`):
- Configurable sync check threshold (default 99.9%)
- Blocks event processing until node is synced (unless disabled)

### Service Lifecycle

Runs indefinitely as systemd service (source: `README.md:174`). Configuration reload requires service restart.

## 5. Lessons: Reuse and Avoid

### Conceptually Sound Patterns

1. **Journal as source of truth** — Reading from journalctl ensures one authoritative log stream without competing with node stdout. Good choice for SPO machines with multiple node processes.

2. **Startup replay pattern** — Replaying from the last startup marker before switching to live tailing eliminates event gaps during restart cycles. Worth replicating.

3. **Pydantic event models** — Strongly-typed event parsing (source: `src/openblockperf/models/events.py`) with singledispatch handlers (`src/openblockperf/handler.py:119-122`) is clear and maintainable.

4. **Async-first architecture** — asyncio TaskGroup with fail-fast semantics makes error propagation transparent; no silent failures.

5. **Configuration layering** — Environment variables > CLI flags > .env file > config file (source: `config.py:85-122`). Balances operational flexibility with auditability.

### Dependencies to Reconsider

1. **Cardano-tracer is an external dependency** — OpenBlockperf assumes traces come from a specific external trace daemon. This couples metrics collection to the tracer's output format and availability. A lighter agent might read cardano-node logs directly.

2. **Heavyweight Python stack** — Pydantic, Textual, and Typer add features (TUI, rich validation, CLI polish) that a minimal SPO agent may not need. Core logic could run in a simpler language (Rust, Go, Nushell).

3. **systemd + journalctl coupling** — Assumes systemd/journald availability (common on Linux but not universal). A portable agent should support multiple log sources.

4. **Blocking event processing** — All tasks are sync-or-block; there's no graceful degradation if one task stalls (e.g., API timeouts block the event loop). Consider task isolation with timeout channels.

### Operational Observations

1. **Installation automation is essential** — The bash installer handles a complex multi-step setup (user, venv, systemd, env file) in one call. Worth emulating for any SPO tool.

2. **Explicit API key registration** — Requiring manual registration (Calidus or IP-based) before data can flow adds friction but is a deliberate choice for accountability. A lighter agent might skip this for internal-only deployments.

3. **Metrics scope is broad** — Tracking four stages of block propagation plus peer topology plus node context is comprehensive. A minimal agent might track only one stage or peer counts only.

4. **No local persistence** — All data is submitted immediately; logs are only replayed to catch up after restarts. No local buffer/queue. A more resilient agent might queue data during API downtime.
