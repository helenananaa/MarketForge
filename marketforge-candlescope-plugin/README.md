# MarketForge CandleScope adapter

This crate is the MarketForge-owned integration boundary for CandleScope. It
does not copy MarketForge into CandleScope and does not require changes to the
CandleScope source tree.

The adapter builds one native JSONL plugin process around one authoritative
MarketForge simulation session. It supports two explicit session modes:

```text
CandleScope frontend
  -> candlescope.plugin/2 over jsonl/1
  -> MarketForge adapter
       -> embedded: RoomManager / matching engine in the plugin process
       -> remote: exchange-server room over authenticated HTTP
       -> trade and order-book projection
       -> symbol, K-line, and full-depth provider responses
```

`session.load` selects embedded mode and creates the room in-process.
`session.attach` selects remote mode and attaches to an already-created room.
Both modes share the same CandleScope symbol, history, stream, and projection
contracts.

## Implemented contracts

- Full synchronous lifecycle: `handshake`, `describe`, `activate`, `invoke`,
  `eventBatch`, `healthCheck`, `cancel`, `prepareUpgrade`, `deactivate`, and
  `shutdown`.
- Strict JSON objects, duplicate-key rejection, message/container/depth bounds,
  generation ownership, and stdout protocol isolation.
- `symbol-provider/1` with spot/perp symbol discovery and paging.
- `market-data-provider/1` with `history.read`, `stream.open`, `stream.poll`,
  and `stream.close` over `candlescope.stream/1`.
- Trade-derived `1s`, `1m`, `5m`, `15m`, and `1h` K-lines. A K-line becomes
  final only after the MarketForge simulation clock crosses its close time.
- Full-depth snapshot plus linked range-sequenced deltas, capped at 100 levels
  per side.
- Incremental `RoomManager::execution_history` synchronization, including
  automatic liquidations and cross-instrument cancellations appended by the
  engine after the caller's primary execution.
- Remote room discovery through the durable `/cluster/rooms` directory,
  owner-aware HTTP retries, gap-checked command cursors, authoritative
  `market_time_ms`, and bounded polling that honors `stream.poll.waitMs`.
- A `command/1` contribution that exposes the adapter-side session boundary
  without inventing a private CandleScope contribution kind.

MarketForge remains the source of truth. The adapter never writes a price,
fabricates a trade, or advances the clock in response to a provider poll.

## Control operations

Mutating operations require `requestContext.userAction: true`.

| Operation | Required input |
| --- | --- |
| `session.load` | `scenario`, optional `epochMs` |
| `session.attach` | `scenario`, optional `epochMs` |
| `session.unload` | none |
| `session.describe` | none |
| `command.apply` | `roomId`, `instrumentId`, `command` |
| `clock.advance` | `roomId`, `steps` (1 to 10,000 per call) |
| `room.pause` | `roomId` |
| `room.resume` | `roomId` |
| `room.close` | `roomId` |

`scenario` and `command` use the existing `exchange-core` Serde shapes. One
plugin process owns one active room. Loading or unloading a room closes all
provider streams so the Host must open fresh streams against the new session.

For `session.attach`, the scenario is metadata: its `room_id`, instruments,
symbols, market types, venues, and tick sizes must describe the existing remote
room. The plugin does not create or seed that room. It replays the durable event
journal from command sequence zero, rejects gaps or legacy executions without
authoritative market time, then follows incremental pages on provider polls.

Remote mode supports clock advance, pause, resume, close, and
`Command::SetMarkPrice`. Raw `NewOrder`, `CancelOrder`, and `AmendOrder` values
are deliberately rejected in remote `command.apply`, because the standalone
backend's trading API owns participant identity, account authorization,
idempotency, and server-assigned order IDs. Submit those actions through the
backend order endpoints.

## Remote backend configuration

Remote credentials are process configuration and never travel through the
CandleScope command JSON:

| Environment variable | Meaning |
| --- | --- |
| `MARKETFORGE_PLUGIN_BACKEND_URL` | Base URL for `exchange-server`; enables `session.attach` |
| `MARKETFORGE_PLUGIN_BACKEND_TOKEN` | Optional bearer token |
| `MARKETFORGE_PLUGIN_BACKEND_USER_ID` | Optional loopback-development `x-user-id`; mutually exclusive with the token |
| `MARKETFORGE_PLUGIN_TRUSTED_OWNER_URLS` | Optional comma-separated exact owner base URLs for multi-active routing |

Example for a local standalone backend:

```bash
export MARKETFORGE_PLUGIN_BACKEND_URL=http://127.0.0.1:57305
cargo run -p exchange-server
# In a second shell with the same plugin environment:
cargo run -p marketforge-candlescope-plugin -- --jsonl
```

The checked-in CandleScope manifest remains zero-network and therefore runs the
embedded mode in sandboxed installations. Direct remote HTTP is intended for a
`trusted-local` or developer-local installation where the native process may
open sockets. CandleScope's marketplace `network.connect` capability is an
HTTPS host-call gateway with a manifest-pinned domain, not ambient socket
permission; a marketplace-safe remote package will need a pinned deployment
manifest plus a Host HTTP bridge before remote mode can be enabled there.

The standalone backend also exposes owner-aware SSE through
`HttpTradingClient::room_event_stream_*`. The current CandleScope provider
contract is synchronous polling, so the plugin uses bounded durable REST pages
instead of keeping an uncancellable background SSE task alive across plugin
lifecycle transitions.

## Local verification

From the MarketForge repository root:

```bash
cargo test -p marketforge-candlescope-plugin --all-targets
cargo clippy -p marketforge-candlescope-plugin --all-targets -- -D warnings
```

The executable accepts only the optional `--jsonl` switch. Invalid remote
configuration fails startup. Logs and startup errors go to stderr; stdout is
reserved for one JSON response per line.

```bash
cargo run -p marketforge-candlescope-plugin -- --jsonl
```

## Windows plugin package

Build the executable on Windows with the MSVC Rust toolchain, then assemble a
local plugin directory without changing CandleScope:

```powershell
cargo build --release -p marketforge-candlescope-plugin
New-Item -ItemType Directory -Force .\dist\marketforge-candlescope\runtime
Copy-Item .\marketforge-candlescope-plugin\plugin\manifest.json `
  .\dist\marketforge-candlescope\manifest.json
Copy-Item .\target\release\marketforge-candlescope-plugin.exe `
  .\dist\marketforge-candlescope\runtime\marketforge-candlescope-plugin.exe
```

The checked-in manifest is a local-development schema-v3 manifest. It declares
no filesystem, network, account, or trading permission. Before marketplace
release, add a canonical `controlTranscript` probe and supply-chain lock for the
exact Windows artifact; those are release evidence, not runtime behavior.

## Deliberate boundary

This adapter exposes CandleScope's current public Provider and Command
contracts only. It does not pretend that `paper-*` contributions can own a
MarketForge ledger or matching engine. A future authoritative simulation UI
should bind to this crate's session methods when CandleScope publishes a
dedicated simulation/room contract.
