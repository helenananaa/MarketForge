# Backend Storage

MarketForge keeps the matching engine in memory and persists the durable journal
around it.

The engine boundary stays pure:

```text
command in -> candidate room state -> execution/events out
```

The server commits that candidate state only after the journal write succeeds.
If the journal append fails, the in-memory room remains unchanged.

## PostgreSQL

Set `MARKETFORGE_DATABASE_URL` before starting `exchange-server`:

```sh
export MARKETFORGE_DATABASE_URL='postgres://marketforge:marketforge@127.0.0.1:5432/marketforge'
cargo run -p exchange-server
```

For local development, copy `.env.example` or use the bundled compose file:

```sh
docker compose -f docker-compose.postgres.yml up -d
export MARKETFORGE_DATABASE_URL='postgres://marketforge:marketforge@127.0.0.1:55432/marketforge'
cargo run -p exchange-server
```

When the variable is set, the server connects to PostgreSQL and runs versioned
schema migrations from `exchange-server/migrations`. Applied migrations are
tracked in `marketforge_schema_migrations`.

The initial migration creates:

- `marketforge_schema_migrations`
- `marketforge_rooms`
- `marketforge_executions`
- `marketforge_room_snapshots`
- `marketforge_orders`
- `marketforge_order_events`
- `marketforge_trades`
- `marketforge_market_ticks`
- `marketforge_account_ledger`
- `marketforge_position_snapshots`
- `marketforge_users`
- `marketforge_room_members`
- `marketforge_account_owners`

`marketforge_executions` remains the canonical command journal. The order,
event, trade, and tick tables are query projections written in the same
transaction as the journal row. They make the common exchange queries direct:
order lifecycle by account, event audit by order, printed trades, and the trade
tape / last-price stream.

Clearing projections are written in the same transaction too:

- `marketforge_account_ledger` stores one row per affected account per trade,
  including cash delta, position delta, fee, realized PnL, and post-settlement
  balances.
- `marketforge_position_snapshots` stores the post-settlement account position
  state after each clearing leg. For perp accounts it also stores average entry,
  realized/unrealized PnL, equity, and initial margin.

## Query API

Projection tables are exposed through read-only room endpoints:

```text
GET /rooms/{room_id}/orders?account_id=20&limit=100
GET /rooms/{room_id}/trades?account_id=20&limit=100
GET /rooms/{room_id}/ticks?limit=100
GET /rooms/{room_id}/ledger?account_id=20&limit=100
GET /rooms/{room_id}/positions?account_id=20&limit=100
```

`account_id` is optional where it applies. `limit` defaults to 100 and is capped
at 500.

## Access Control

The server uses a lightweight user boundary based on the `x-user-id` request
header. If the header is omitted, the local development user is `local-user`.

Room creation records:

- a user row in `marketforge_users`
- an owner membership row in `marketforge_room_members`
- ownership rows for the scenario accounts in `marketforge_account_owners`

Room-level endpoints check membership before returning data. Order submission
checks that the current user can access the submitted account. Projection
queries are filtered by user: room owners/admins can see the full room, while
account owners only see rows tied to their owned accounts.

When the variable is not set, the server uses the in-memory journal. This keeps
local tests and frontend development lightweight.

## Startup Recovery

On startup, the server loads persisted rooms, executions, and the latest room
snapshot from the journal. If a room has a snapshot, recovery restores the
serialized `MarketActor` and replays only executions after that checkpoint. If a
room has no snapshot, recovery rebuilds it from the scenario and replays the
journal.

Seed orders are already part of the saved scenario, so recovery skips their
duplicated journal rows while preserving them in the room timeline. The next API
order id is restored from submitted non-seed orders, so high seed ids do not
force user order ids to jump.

## Snapshots

Snapshots are stored in `marketforge_room_snapshots` as serialized actor state:

- `room_id`
- `command_seq`
- `actor_json`
- `created_at`

Room creation stores an initial snapshot when seed executions exist. Submitted
commands store a new snapshot every 100 command sequences. The execution journal
remains the durable audit trail; snapshots are checkpoints used to reduce
startup replay work.

Timeline responses are served from recovered execution summaries, not from the
replayed actor history. This keeps `/rooms/{room_id}/events` complete even when
the in-memory actor was restored from a snapshot.

## Current Scope

The storage slice persists room scenarios, seed executions, submitted
executions, room status updates, and room snapshots. Recovery uses the latest
snapshot when available and replays only the journal tail after that snapshot.

The PostgreSQL path also persists normalized order actions, order event rows,
trade prints, market ticks, account ledger rows, and per-account position
snapshots derived from matching and clearing events. Room snapshots remain the
fast recovery checkpoint for full engine state.

## Verification

Unit tests do not require PostgreSQL. To run the optional PostgreSQL integration
test, set `MARKETFORGE_TEST_DATABASE_URL`:

```sh
export MARKETFORGE_TEST_DATABASE_URL='postgres://marketforge:marketforge@127.0.0.1:55432/marketforge'
cargo test -p exchange-server postgres_journal_persists_and_recovers_room_when_configured -- --nocapture
```

The smoke script exercises the same persistence path through HTTP and restarts
the server to confirm recovery:

```sh
MARKETFORGE_DATABASE_URL='postgres://marketforge:marketforge@127.0.0.1:55432/marketforge' \
  ./scripts/postgres_smoke.sh
```
