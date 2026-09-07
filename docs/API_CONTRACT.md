# HTTP API contract (http.v1)

Protocol version: `http.v1`. Additive fields with defaults are compatible.
Breaking changes require a new version string on ticker/candles/user streams.

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
- `/stream/public` and `/stream/private`: independent `stream_seq`. `command_seq` on payloads is **not** contiguous after filtering. Reconnect with `after_command_seq` as a lower bound, then apply by `stream_seq`. Overrun emits `resync_required`. Private streams stop after membership revocation.

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

## External strategy protocol (`strategy.v1`)

`GET /rooms/{id}/observe` returns `{ "api_version": "strategy.v1", "observation": ParticipantObservation }`. Observation is public book/trades/sim time plus the caller's own orders and account. Place/cancel go through `POST /rooms/{id}/orders` with `Idempotency-Key`. External strategies do not access the database or internal actors.

Non-admin actors are capped at 8 actions per simulation step (`429` when exceeded). Illegal `price_tick`/`qty` (`<= 0`) return `400`. Quota is per `(room, user, step)` and does not block other rooms.

## Operations

```
GET /health/live
GET /health/ready
GET /metrics
```

Ready returns 503 when the journal is down or shutdown has started. Metrics are low-cardinality (no room/user labels). Faults fail closed: no infinite auto-retry. See `docs/BACKEND_STORAGE.md`.
