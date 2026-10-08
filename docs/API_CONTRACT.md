# HTTP API contract (http.v1)

Protocol version: `http.v1`. Additive fields with defaults are compatible.
Breaking changes require a new version string on ticker/candles/user streams.

## CandleScope WebSocket (simulation.ws.v1)

`GET /runtime` is authenticated and reports `{storage:{kind,durable},websocket:{version,snapshot_interval_ms}}`.
It never returns a database URL or credentials. `/health/ready` remains the health check.

`GET /rooms/{id}/ws?account_id=20&interval_ms=1000&instrument_id=...` upgrades to a read-only socket.
Origin must match the configured CORS origins when supplied. The client must send one
`{"kind":"authenticate","token":"..."}` or `{"kind":"authenticate","user_id":"..."}` message
within 5 s. Credentials do not enter the URL. Local identity only works under the existing local auth policy.
Unsupported accounts/intervals, unknown hello fields, failed authentication and revoked permissions fail closed.

The server sends `{"api_version":"simulation.ws.v1","kind":"snapshot","sequence":1,"data":{
"observation":<strategy.v1 response>,"candles":<http.v1 response>}}`.
Snapshots include only the requested account and its orders; book/recent trades are public room data.
Candles cover the latest 500 intervals. Both components have the same simulation time; a concurrent
commit crossing the database read causes the sample to be discarded. Database I/O does not hold the writer lock.

The server samples every 250 ms, sends changed full snapshots, and sends idle `kind:"heartbeat"` frames
every 5 s. Sequence begins at 1 for each connection; it is not a durable resume cursor.
Reconnect always replaces state from a fresh full snapshot. Clients reject gaps and mixed identities/clocks.
Authorization is rechecked even for idle rooms. Errors use `kind:"error",status,error` and terminate the connection.
Inbound messages/frames are limited to 16 KiB; sends time out after 5 s. Slow readers disconnect rather than
accumulating an unbounded queue. Server shutdown closes sockets.

Order/cancel/control writes continue through the existing HTTP idempotency and durable commit paths.
The socket accepts no trading commands. Existing SSE APIs remain compatible.
Implementation uses the [Axum WebSocket API](https://docs.rs/axum/latest/axum/extract/ws/index.html).

## Spot/perpetual linkage

An optional perpetual `price_link: {spot_instrument_id, max_age_ms}` configures a
same-venue, same-base/quote spot index. Instrument view, ticker and observation
responses expose optional `perp_price`; execution/order summaries and public
streams expose `price_updates`. Updates are atomic with the originating spot
command and verified on journal replay. Linked marks cannot be manually set;
unavailable/stale indexes reject new non-reduce-only orders and amendments.
See [linkage semantics and configuration](SPOT_PERP_LINK.md).

## Perpetual funding

Optional perpetual `funding: {interval_ms, base_rate_ppm, max_rate_ppm, min_coverage_ppm}` requires
`price_link`. Public `perp_price.funding` exposes the estimated period rate,
simulation-time settlement cursor, coverage and last receipt. Perpetual accounts
expose signed cumulative `funding_pnl`; execution summaries expose optional
`funding_settlement`. Private `PerpFundingSettled` clearing events and account
ledger rows (`account_side: "funding"`) carry actual cash deltas. Public streams
include only funding totals; private funding events are filtered by account access.

Clock mutations atomically journal the derived funding executions. Idempotent
clock retries do not pay twice; replay compares settlement and account receipts.
`Command::SettleFunding` is reserved for clock-generated internal executions and
cannot be submitted by a trader. See [funding semantics and configuration](FUNDING.md).

## Identity

- Loopback: optional `x-user-id` (default `local-user`).
- Bearer: `Authorization: Bearer <token>`. `x-user-id` is ignored.
- Writes that allocate an order id accept `Idempotency-Key` (1–128 visible ASCII), scoped to `(user, room)` in the **order** key space.
- Control writes (`POST /rooms/{id}/pause|resume|close`, `/clock/step`, `/clock/advance`) accept the same header in a **separate control** key space, scoped to `(authenticated subject, room, key)`. Order keys and control keys never collide.

## Integer encoding

Matching uses integer ticks and quantities. JSON numbers in JavaScript are IEEE-754.

- Price ticks, quantities, and ordinary order ids in the training range fit in `2^53-1` and are JSON numbers.
- Values that can exceed that (system liquidation order ids, `i128` money) MUST be treated as decimal strings by new clients. The server currently serializes `i128` via serde_json numbers; clients that cannot parse them losslessly should use the Rust `HttpTradingClient` or a big-integer JSON parser. Silent truncation is a client bug.

## Accounts vs positions

- `GET /rooms/{id}/accounts` is the live account/margin snapshot.
- `GET /rooms/{id}/positions` is a historical clearing-leg projection and can lag cross-margin peers.

## Lifecycle

```
POST /rooms
POST /rooms/{id}/orders                 Idempotency-Key optional
POST /rooms/{id}/orders                 action Cancel / Amend
GET  /rooms/{id}/accounts
GET  /rooms/{id}/clock
POST /rooms/{id}/clock/advance          Idempotency-Key optional (control space)
POST /rooms/{id}/clock/step             paused admin manual step; Idempotency-Key optional
POST /rooms/{id}/pause|resume|close     Idempotency-Key optional (control space)
GET  /rooms/{id}/ticker
GET  /rooms/{id}/candles?interval_ms=
GET  /rooms/{id}/stream/public
GET  /rooms/{id}/stream/private
GET  /rooms/{id}/events/stream          admin audit (contiguous command_seq)
POST /rooms/{id}/members                owner/admin; roles owner|admin|instructor|trader|spectator
POST /rooms/{id}/members/{user_id}      remove member and account assignments
POST /rooms/{id}/accounts/{account_id}/owners   assign instructor/trader; frozen after training start
GET  /rooms/{id}/observe?account_id=    strategy.v1 ParticipantObservation
```

## Control idempotency (`control.v1`)

Fingerprint is canonical JSON `{ "operation", "params", "protocol": "control.v1" }`, not the raw URL or request body text.

| Operation | Params |
| --- | --- |
| `pause` / `resume` / `close` / `clock/step` | `{}` |
| `clock/advance` | `{ "steps": N }` |

- Same key + same fingerprint returns the original success body **after a live permission check**.
- Same key + different operation or params returns HTTP 409 and does not mutate.
- Success is saved in the same journal transaction as the mutation (`marketforge_control_idempotency`, migration 0013). Business rejects (no mutation) and infrastructure failures are **not** stored, so they cannot be replayed as success. Keys do not auto-expire.
- HTTP clients: `HttpTradingClient` pause/resume/close/advance do not send a key unless the caller uses the generic idempotent POST. CLI sends `Idempotency-Key` only when `--idempotency-key` is set. Python SDK currently exposes the header on order APIs.

## Error codes

See `docs/RUNTIME_CONTRACT.md` §7. Bodies are `{ "error": "...", "code"?: "...", "room_owner"?: ... }`.

## Streams

- Admin `/events/stream`: contiguous `command_seq`.
- `/stream/public` and `/stream/private`: connection-local `stream_seq` is **not** a cross-connection resume key. Resume with `after_command_seq` (durable room position). Holes after private filtering are expected; do not treat them as loss.
- Snapshot payload includes `cursor: { room_id, scope, version: stream.v1, command_seq }`. Apply later `execution` events only when `command_seq` is greater than that boundary. Mixing a later snapshot with older deltas is invalid.
- Public stream: book, trades, and allowed room status. Private stream: the viewer's current orders/accounts plus rest-only post, partial fill remainder, cancel, amend, and reject for accounts they can access. Access is by membership/assignment, not “this execution ever touched the account”.
- If the in-memory 1024-event cache does not contain `after_command_seq + 1`, the server fills from the durable journal. Live lag still emits `resync_required`. `scope` query that disagrees with the path returns 400. Private streams close after membership revocation; private snapshots omit other accounts.

## Ticker

`last_trade_tick` is the last `TradePrinted` price, or `null`. `bid_tick` / `ask_tick` / `mid_tick` are independent; mid exists only with both sides.

## Candles

Integer OHLCV from trades and `market_time_ms`. Interval `[floor(t/interval)*interval, next)`. Empty intervals omitted. Query does not advance the clock.

## Members and roles

Write enforcement, not hidden fields:

| Role | Public data | Private stream | Trade assigned account | Trade any account | Member admin |
| --- | --- | --- | --- | --- | --- |
| owner / admin | yes | yes | yes | yes | yes |
| instructor | yes | own assigned | yes, after assign | no | no |
| trader | yes | own assigned | yes | no | no |
| spectator | yes | no | no | no | no |

Removing a member deletes that user's `account_owners` rows. Subsequent writes, history queries, and live private subscriptions fail closed.

Account assignment after a training run has started returns HTTP 409.

## Bot plugins (`bot.v1`)

`GET /scenarios/background-market` returns an authenticated `CreateRoomRequest` recipe owned by
the backend, including the default background participants. Reading it has no side effects.
Clients may choose a room name (and update the participants' matching room names) and submit
it through the ordinary `POST /rooms` path. See [CandleScope workbench](CANDLESCOPE_WORKBENCH.md).

`GET /bots` lists authenticated users' available builtin and installed process bot descriptors and parameter definitions. `GET /rooms/{id}/bots` returns the saved scheduler configuration/state (or null), with room-admin authorization. Existing `/rooms/{id}/agents` start/status and `/agents/stop` manage the instances.

Agent requests additionally accept `{ "Plugin": { "participant": ParticipantConfig, "plugin_id": "example.buy-remaining", "plugin_version": "1.0.0", "state_version": 1, "config_version": 1, "seed": 7, "config": { "target_qty": 4 } } }`. All three numeric version/seed fields default to 1. Participant IDs must be unique and include explicit instrument routing. Unknown plugin IDs, versions and parameters return 400 before creation. Commands come only from locally installed manifests loaded with `MARKETFORGE_BOT_PLUGIN_DIR`, never from API bodies.

Builtin template JSON remains supported; builtin plugin IDs are the original template names and version `"1"`. Registration persists initial configuration before stepping. Reapplying an identical instance preserves its state; changing its configuration resets that instance. Adding/removing other instances preserves unchanged instances. Changing the list during an unfinished step returns 409. An empty list durably clears the configuration. Stop preserves state; recovered automatic workers require explicit start.

Automatic market ticks proceed independently of bot decisions. Each bot has at most one outstanding decision and submits against the current market through the ordinary trading gateway. Slow/failed bots do not block other bots or human orders. `/agents/stop` disables bots while preserving the automatic clock; `/pause` pauses the market. An empty start list removes bots but keeps automatic time. Rooms without automatic scheduling retain explicit clock control. Status adds `market_running` and per-instance `bot_errors`; `last_error` remains available. Reapplying the bot list retries failed instances. Pausing/resuming, stopping or replacing bots invalidates pending decisions. Manual `/clock/step` retains deterministic sequential execution.

Process plugins receive one versioned JSON decision request and return actions/state. Automatic submissions journal the bot state, actions and training update atomically; manual steps preserve unfinished-action recovery. Both apply the existing training buy-only/capacity rules and record each execution's book/fill evidence. See [bot installation and protocol](BOT_PLUGINS.md) for manifests, limits and examples.

## External strategy protocol (`strategy.v1`)

`GET /rooms/{id}/observe` returns `{ "api_version": "strategy.v1", "observation": ParticipantObservation }`. Observation is public book/trades/sim time plus the caller's own orders and account. Place/cancel go through `POST /rooms/{id}/orders` with `Idempotency-Key`. External strategies do not access the database or internal actors.

Non-admin actors are capped at 8 actions per simulation step (`429` when exceeded). Illegal `price_tick`/`qty` (`<= 0`) return `400`. Quota is per `(room, user, step)` and does not block other rooms.

The Python SDK (`python/marketforge`) talks only to HTTP: `observe` / `place` / `cancel`, training start/status/abort/result/report, `clock` / `advance_clock`, and `events`. Batch evaluation is `scripts/batch_runner.py` (library `marketforge.batch`).

## Training (`training.v1`)

```
POST /training/runs
GET  /training/runs/{run_id}
POST /training/runs/{run_id}/abort
GET  /training/runs/{run_id}/result
GET  /training/runs/{run_id}/report
```

`POST /training/runs` creates the room, persists `TrainingProgress`, and returns status `Running`. That response is not a finished score. The same `run_id` is looked up and returned; a different run that reuses an existing room is HTTP 409. Agents listed on the request are started on the internal worker (not the Bearer HTTP callback). Optional `manual_agents: true` instead persists an initial manual scheduler without starting a worker; pause the room and use `/clock/step` for complete decision/action/state steps. Clock `advance` / scheduler steps call `TrainingRun::on_step` and `settle_training_residuals` on the finish line.

Agent templates include `NoiseTrader`, `DcaTrader`, `GridTrader`, `ContinuousMarketMaker`, and `CancelAtStep`. `ContinuousMarketMaker` and `NoiseTrader` carry a `seed`. Batch evaluation derives child seeds with the same wrapping-u64 function as `exchange_core::training_scenarios::child_seed(parent, agent_name)` and will not treat a renamed room as a different experiment.

Versioned scenarios `basic_execution`, `liquidity_withdrawal`, and `inventory_stress` live in `exchange-core` (`training_scenarios.rs`). Liquidity withdrawal cancels via the named liquidity account at a sim step; it does not rewrite prices.

## Batch evaluation

`scripts/batch_runner.py BASE SPEC [seeds…]` isolates one room per seed, injects child seeds, drives strategy plus clock, and records a row only after `Completed` / `Failed` / `Aborted` (or an explicit HTTP failure). Local `--state` is temp-file + `os.replace` with a single writer. Crash resume reconciles `GET /training/runs/{id}`; the JSON file is not the score. Failed rows stay in the comparison set. `q=0` is not a low-cost win (`incomplete_penalty_ppm` / `zero_fills`). `--fail-seeds` aborts after start and does not mutate `target_qty`.

## Operations

```
GET /health/live
GET /health/ready
GET /metrics
```

Ready returns 503 when the journal is down or shutdown has started. Metrics are low-cardinality (no room/user labels). Faults fail closed: no infinite auto-retry. See `docs/BACKEND_STORAGE.md`.
