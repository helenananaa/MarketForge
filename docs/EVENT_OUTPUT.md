# Public market and private account events

The scoped SSE endpoints retain `http.v1` / `stream.v1` and add structured
event fields. The administrator audit endpoint remains available separately.
Simulation time (`market_time_ms`) is authoritative; subscribing never advances
the clock.

## Public stream

`GET /rooms/{room_id}/stream/public`

The initial `snapshot` includes `markets[]` with each instrument's book, ticker,
and perpetual price snapshot. The legacy primary `book` and `ticker` fields
remain. Subsequent `execution` payloads contain:

| Field | Meaning |
| --- | --- |
| `instrument_id`, `command_seq`, `market_time_ms` | Instrument and durable execution boundary |
| `trades[]` | Every actual fill: `trade_id`, engine `seq`, price, quantity, taker side, simulation time |
| `book_delta` | Changed bid/ask levels, in the same shape as a book snapshot |
| `book_delta_available` | Whether a depth receipt was stored for this execution |
| `liquidations[]` | Each actual forced liquidation fill, including partial attempts |
| `price_updates`, `funding_settlement` | Public linked price and funding settlement information |
| `events[]`, `status`, `accepted` | Existing anonymous event names and room/execution status |

Depth quantities are **absolute replacements**, not signed increments. Replace
each changed price level; quantity zero deletes it. Unmentioned levels remain.
An empty delta means no visible depth change. Apply executions in ascending
`command_seq`, independently by instrument. A trade is identified by the room,
instrument, and `trade_id`; IDs are not globally unique across instruments.

`LiquidationTrade` includes `liquidation_attempt_seq`, `trade_id`, instrument,
time, order side, position side, execution price, and executed quantity. A
liquidation waiting for liquidity has no fill to publish. A partial attempt
publishes its actual fills immediately; a later attempt publishes its own fills.
These fields expose no account IDs, order IDs, wallet balances, or private loss
allocations. This is MarketForge's contract, not an exchange-specific wire format.

## Private stream

`GET /rooms/{room_id}/stream/private`

Membership and account assignment are checked for the current viewer. Owner and
administrator roles can see all accounts; other permitted roles see assigned
accounts. Spectators cannot subscribe. Permission is checked again before a
queued payload is emitted; revocation terminates the stream with
`resync_required`.

Private snapshots include all visible instrument `account_states`, instrument
scoped `open_orders`, venue and portfolio `wallet_balances`, and legacy primary
`orders` / visible `accounts` fields. Private `execution` payloads include:

| Field | Meaning |
| --- | --- |
| `events[]` | Owned order lifecycle events |
| `trades[]` | Owned fill legs, own order/account, maker flag, side, position side, price, quantity, time |
| `clearing_events[]` | Accounting projected separately for each accessible account |
| `wallet_changes[]` | Venue asset balances after actual total/reservation changes |

Accounting types include `SpotTradeSettled`, `PerpTradeSettled`,
`PerpFundingSettled`, `PerpMarginStatusChanged`, `PerpMarginUpdated`,
`PerpLiquidationSettled`, `PerpAutoDeleveraged`, and
`PerpSocializedLossApplied`. Fees, realized PnL, funding cash deltas, margin,
equity, and the account snapshot accompany their respective receipts.
`PerpMarginUpdated` also covers numerical changes while status remains healthy,
including changes in peer markets under cross margin; its `instrument_id`
identifies the updated account projection.

Counterparty identifiers and account snapshots are removed from trade accounting.
Liquidation owners do not receive other accounts' ADL/loss allocation lists.
ADL and socialized-loss recipients receive their own allocation and account
snapshot even if they had no order fill in the initiating command.

`WalletChanged` includes `account_id`, `venue_id`, `balances[]` and `reasons[]`.
Each asset has `total`, `reserved`, and `available`, encoded as decimal strings.
Resting orders, cancellation, expiry, trading fees/PnL, funding, liquidation,
ADL and loss sharing can change these balances. Unchanged balances do not cause
a wallet notification.

## Transfer wallet events and reconnect

Transfers are durable room mutations, not matching commands. They use a separate
cursor and private SSE `kind: "wallet"` with `command_seq: null`:

```
GET /rooms/{room_id}/wallet-events?wallet_cursor=0&limit=100
GET /rooms/{room_id}/stream/private?after_command_seq=42&wallet_cursor=7
```

`wallet-events` returns `events`, `next_wallet_cursor`, `latest_wallet_cursor`,
and `has_more`. `wallet_cursor` is the index of the next receipt; zero starts at
the beginning. Each receipt has `wallet_seq`, transfer ID, account, venue, asset,
kind, `Pending` / `Completed` / `Rejected` status, amount, reject reason, and
venue/portfolio balance snapshots. Perpetual account `margin_updates` accompany
transfers so collateral changes are observable too. Amounts and wallet balances
are decimal strings. Delayed completion is a separate receipt. Venue-to-venue
transfers include the source withdrawal and destination deposit.

The server advances the wallet cursor over filtered receipts even when a page
has no visible events. Page size is capped at 500. The private stream polls the
published durable wallet ledger every 250 ms; it does not synthesize transfers
from sampled balance differences. Wallet and execution cursors are independent;
do not infer a single ordering between the two streams.

Without resume parameters, the snapshot's
`cursor: {room_id, scope, version, command_seq, wallet_cursor}` establishes the
current state and both boundaries. Save **both** durable cursors and pass both on
reconnect. An omitted wallet cursor starts at the current wallet boundary.
The SSE `id` / `stream_seq` is local to one connection and is not a resume key.
Execution catch-up traverses all necessary journal pages beyond the 1,024-entry
cache. Live broadcast overrun emits `resync_required`; get a fresh snapshot.
Future execution or wallet cursors return HTTP 409.

Modern receipts survive journal recovery and checkpoints. Legacy execution rows
can lack depth/wallet/margin receipts: `book_delta_available: false` explicitly
marks missing depth, and clients must obtain a fresh book snapshot before applying
later deltas. Transfers before this feature's stored checkpoint may lack wallet
receipts; the current wallet snapshot remains authoritative. Historical missing
fields are not fabricated.

Validation: `cargo test -p exchange-server event_output` exercises fills, depth
reconstruction, private accounting, partial liquidation, ADL, healthy margin
updates, transfer cursors, revocation, pagination and recovery. Set
`MARKETFORGE_TEST_DATABASE_URL` and `MARKETFORGE_REQUIRE_POSTGRES_TESTS=1` to require
the PostgreSQL recovery case. `scripts/validate_event_output.py` runs a separate
real HTTP/SSE smoke test in live and restart/replay phases.
