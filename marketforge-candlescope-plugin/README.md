# MarketForge CandleScope adapter

This crate is the MarketForge-owned integration boundary for CandleScope. It
does not copy MarketForge into CandleScope and does not require changes to the
CandleScope source tree.

The adapter builds one native JSONL plugin process around one authoritative
MarketForge simulation session:

```text
CandleScope frontend
  -> candlescope.plugin/2 over jsonl/1
  -> MarketForge adapter
       -> RoomManager / matching engine
       -> trade and order-book projection
       -> symbol, K-line, and full-depth provider responses
```

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
- A `command/1` contribution that exposes the adapter-side session boundary
  without inventing a private CandleScope contribution kind.

MarketForge remains the source of truth. The adapter never writes a price,
fabricates a trade, or advances the clock in response to a provider poll.

## Control operations

Mutating operations require `requestContext.userAction: true`.

| Operation | Required input |
| --- | --- |
| `session.load` | `scenario`, optional `epochMs` |
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

## Local verification

From the MarketForge repository root:

```bash
cargo test -p marketforge-candlescope-plugin --all-targets
cargo clippy -p marketforge-candlescope-plugin --all-targets -- -D warnings
```

The executable accepts only the optional `--jsonl` switch. Logs and startup
errors go to stderr; stdout is reserved for one JSON response per line.

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
