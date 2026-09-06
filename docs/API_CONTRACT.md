# HTTP API contract (http.v1)

Protocol version: `http.v1`. Additive fields with defaults are compatible.
Breaking changes require a new version string on ticker/candles/user streams.

## Identity

- Loopback: optional `x-user-id` (default `local-user`).
- Bearer: `Authorization: Bearer <token>`. `x-user-id` is ignored.
- Writes that allocate an order id accept `Idempotency-Key` (1–128 visible ASCII), scoped to `(user, room)`.

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
POST /rooms/{id}/clock/advance
POST /rooms/{id}/clock/step             paused admin manual step
POST /rooms/{id}/pause|resume|close
GET  /rooms/{id}/ticker
GET  /rooms/{id}/candles?interval_ms=
GET  /rooms/{id}/stream/public
GET  /rooms/{id}/stream/private
GET  /rooms/{id}/events/stream          admin audit (contiguous command_seq)
```

## Error codes

See `docs/RUNTIME_CONTRACT.md` §7. Bodies are `{ "error": "...", "code"?: "...", "room_owner"?: ... }`.

## Streams

- Admin `/events/stream`: contiguous `command_seq`.
- `/stream/public` and `/stream/private`: independent `stream_seq`. `command_seq` on payloads is **not** contiguous after filtering. Reconnect with `after_command_seq` as a lower bound, then apply by `stream_seq`. Overrun emits `resync_required`. Private streams stop after membership revocation.

## Ticker

`last_trade_tick` is the last `TradePrinted` price, or `null`. `bid_tick` / `ask_tick` / `mid_tick` are independent; mid exists only with both sides.

## Candles

Integer OHLCV from trades and `market_time_ms`. Interval `[floor(t/interval)*interval, next)`. Empty intervals omitted. Query does not advance the clock.
