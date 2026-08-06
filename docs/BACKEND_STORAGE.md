# Backend Storage

MarketForge keeps the matching engine in memory and persists the durable journal
around it.

The engine boundary stays pure:

```text
command in -> candidate room state -> execution/events out
```

The server commits that candidate state only after the journal write succeeds.
If the journal append fails, the in-memory room remains unchanged. Durable
transitions run in detached server tasks, so an HTTP client disconnect cannot
cancel the interval between the database commit and installation of the
candidate in-memory state.

## PostgreSQL

Set `MARKETFORGE_DATABASE_URL` before starting `exchange-server`:

```sh
export MARKETFORGE_DATABASE_URL='postgres://marketforge:marketforge@127.0.0.1:5432/marketforge'
cargo run -p exchange-server
```

Without `MARKETFORGE_DATABASE_URL`, the same API runs with an in-memory journal
for local development, but its rooms disappear when the process exits. A
standalone deployment that must survive restarts should configure PostgreSQL.

For local development, copy `.env.example` or use the bundled compose file:

```sh
docker compose -f docker-compose.postgres.yml up -d
export MARKETFORGE_DATABASE_URL='postgres://marketforge:marketforge@127.0.0.1:55432/marketforge'
cargo run -p exchange-server
```

When the variable is set, the server connects to PostgreSQL and runs versioned
schema migrations from `exchange-server/migrations`. Applied migrations are
tracked in `marketforge_schema_migrations`. A bounded journal coordinator owns
one persistent writer connection plus four persistent reader connections by
default. Each connection belongs to a dedicated worker thread. This keeps
synchronous database work off Tokio runtime threads, provides backpressure, and
avoids opening a new connection for every request.

The writer remains the single ordered lane for migrations, recovery,
idempotency lookup, and every durable mutation. Authorization, projection
queries, and event replay are distributed round-robin across the reader
workers, so a slow query does not block unrelated room writes or other readers.
These database reads happen without holding the in-memory `AppState` lock;
in-memory snapshots reacquire that lock only after authorization completes.

Tune the reader count per process with an integer from 0 through 32:

```sh
export MARKETFORGE_JOURNAL_READ_WORKERS=8
```

Zero disables the separate read workers and routes reads through the writer.
Each server process uses `1 + MARKETFORGE_JOURNAL_READ_WORKERS` PostgreSQL
connections, so size this value together with PostgreSQL's connection limit and
the number of server replicas. In-memory mode intentionally uses one worker;
configuring a nonzero read-worker count without `MARKETFORGE_DATABASE_URL`
fails startup instead of silently ignoring the setting.

In the default `single-active` mode, the writer connection acquires an exclusive
PostgreSQL session advisory lock dedicated to the MarketForge runtime. A second
server pointing at the same database fails startup while the first server is
alive, preventing two independently recovered in-memory engines from accepting
writes against the same journal. The lock is released automatically when the
writer connection or process exits, so an active/passive replacement can start
after the active process stops. If that connection is lost at runtime,
readiness fails and the writer is not silently reconnected without its lock;
restart the process to recover safely.

The default lock wait is zero, so conflicting startup fails immediately. A
bounded warm standby can instead wait from 1 millisecond through 1 hour:

```sh
export MARKETFORGE_RUNTIME_LOCK_WAIT_MS=30000
cargo run -p exchange-server
```

The standby binds its listener but does not serve HTTP until it owns the lock,
runs migrations, and recovers the journal. If the active process releases the
lock before the deadline, the standby finishes recovery and becomes ready. If
the deadline expires, startup fails. Configure startup probes accordingly; this
setting requires PostgreSQL and is rejected in in-memory mode.

Schema migration uses a separate advisory lock, so concurrently starting
`room-leased` processes cannot race the migration table. Runtime mode locks are
compatible only within a mode: room-leased processes hold the runtime lock in
shared mode, while a single-active process requires it exclusively. This makes
a mixed-mode deployment fail closed instead of running with incompatible
ownership assumptions.

### Room writer leases and fencing

Migration 11 adds `marketforge_room_writer_leases` as the durable coordination
primitive for the multi-active runtime. A lease is scoped to one room
and carries an owner id, an expiry determined by PostgreSQL's clock, and a
strictly increasing fencing token. Lease durations must be between 1
millisecond and 5 minutes.

Migration 12 adds the optional advertised owner URL used for discovery and
routing hints. It is metadata only: ownership and write safety continue to be
decided exclusively by the unexpired owner id and fencing token.

Acquisition is a single atomic PostgreSQL statement. A live lease cannot be
stolen; after release or expiry, the next owner increments the existing token.
Release expires the row instead of deleting it so a token is never reset to
one while the room exists. Renewal succeeds only for the current, unexpired
owner and token.

The journal exposes fenced execution and room-mutation transactions. They lock
and validate the current lease row inside the same transaction as the journal
append. A delayed writer with an expired or superseded token receives a
`RoomLeaseLost` error and commits no journal or projection changes. Holding the
lease row lock also orders an in-flight commit before a competing takeover.

The managed standalone server can enable multi-active, room-scoped ownership
with a unique instance id:

```sh
export MARKETFORGE_RUNTIME_MODE=room-leased
export MARKETFORGE_INSTANCE_ID=marketforge-a
export MARKETFORGE_ADVERTISE_URL=https://marketforge-a.example.com
export MARKETFORGE_ROOM_LEASE_DURATION_MS=15000
export MARKETFORGE_ROOM_LEASE_RENEW_INTERVAL_MS=5000
export MARKETFORGE_RUNTIME_LOCK_WAIT_MS=0
```

The duration defaults to 15 seconds and the renewal interval to 5 seconds. The
renewal interval must be at least 10 milliseconds and shorter than the lease
duration. This mode requires PostgreSQL, a non-empty instance id unique among
live processes, and a zero runtime-lock wait.

`MARKETFORGE_ADVERTISE_URL` is the externally routable HTTP(S) base URL stored
with every lease. It defaults to the listener URL when binding a concrete
address. An explicit value is required for wildcard binds such as `0.0.0.0`
or `::`, and should point at the instance-specific proxy/backend address rather
than a load-balanced cluster URL. Credentials, query strings, and fragments are
rejected.

At startup each process attempts every durable room once and restores only the
rooms whose leases it acquired. New rooms and their first lease are committed
atomically. Orders, clock changes, transfers, price/status changes, and their
snapshots all use fenced journal transactions. A background task renews leases.
If renewal loses ownership, room-leased mode stops that room's agent worker,
unloads its in-memory state and event channel, and remains ready to serve other
owned rooms.

An authorized request for a room owned by another live process returns
`409 Conflict`; the error identifies the owner instance, fencing token, and
database-clock expiry. It also returns a stable
`code: "room_owned_by_other_instance"` and a structured `room_owner` object
containing the advertised owner URL. After release or expiry, the receiving process acquires
the next fencing token, reloads and validates that room from the journal,
renews the lease, and only then serves it. This request-driven takeover also
advances the process's order-id cursor from the recovered room. Graceful
shutdown drains active writes, stops renewal, and expires owned leases.

`GET /rooms` lists rooms currently loaded by that process, not every room in the
database. The authenticated cluster directory provides the durable, user-scoped
view instead:

```text
GET /cluster/rooms?after_room_id={room_id}&limit=100
```

It reads room membership and active writer leases in one bounded query, orders
by room id, and returns `next_after_room_id` plus `has_more` for cursor paging.
Rooms remain visible when no live lease exists; their `owner` is `null` until an
instance takes them over. A populated owner includes the same URL, fencing
token, and database-clock expiry as the single-room discovery response. This is
a routing hint and can expire immediately after the response; lease validation
remains authoritative.

MarketForge does not proxy a non-owner request to its owner. The Rust
`HttpTradingClient` exposes `cluster_rooms`/`cluster_rooms_after` for directory
pages and can opt into a one-hop request retry by registering each permitted
instance URL with `trust_owner_url` or `with_trusted_owner_url`. Advertised URLs
must match that normalized allowlist exactly before user or bearer credentials
are forwarded; routing is disabled by default and a second 409 is returned
without another retry. Clients should still use idempotency keys for writes
because an ambiguous network failure can occur after the request reaches the
owner. Other clients and load balancers can use the directory and 409 owner
response, retry after failover, or maintain room affinity. Takeover recovery
queries rooms, executions, mutations, and the latest snapshot directly by room
id, so unrelated journals are neither loaded nor parsed on that path.

The client also exposes the room lifecycle calls used by trusted integrations:
`room_clock`, `advance_room_clock`, `pause_room`, `resume_room`, and
`close_room`, plus instrument-scoped `set_mark_price_for`. The corresponding
close mutation is `POST /rooms/{room_id}/close`; like pause and resume, it is
durably journaled as a room status change before the response is returned.

An authenticated client can discover ownership without triggering takeover:

```text
GET /rooms/{room_id}/owner
```

The response contains `owner_id`, `owner_url`, `fencing_token`, and
`expires_at_unix_ms`. Authorization uses durable room membership and the query
does not require the target room to be loaded by the receiving process.

The migration set creates:

- `marketforge_schema_migrations`
- `marketforge_rooms`
- `marketforge_executions`
- `marketforge_room_mutations`
- `marketforge_room_writer_leases`
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

`marketforge_executions` is the canonical command journal, while
`marketforge_room_mutations` records durable state transitions that are not
matching commands: clock advancement, deposits, withdrawals, venue-to-venue
transfers, and room status changes. A mutation uses a command cursor (the next
room-global command sequence), which unambiguously orders mutations before
command sequence zero and between later commands.

Every new command execution also stores `market_time_ms`, the authoritative
simulation-clock time at which the engine evaluated the command. The same value
is copied into order, trade, and market-tick projections. Rows written by an
older server remain readable and expose this field as `null`; wall-clock
`created_at` is operational metadata and must not be used as simulated market
time.

The order, event, trade, and tick tables are query projections written in the
same transaction as their journal records. Clock-triggered liquidation
executions, transfer rows, the clock mutation, and an optional snapshot are
also committed atomically.

Clearing projections are written in the same transaction too:

- `marketforge_account_ledger` stores one row per affected account per trade,
  including cash delta, position delta, fee, realized PnL, and post-settlement
  balances.
- `marketforge_position_snapshots` stores the post-settlement account position
  state after each clearing leg. For perp accounts it also stores average entry,
  realized/unrealized PnL, equity, instrument and portfolio initial/maintenance
  margin, and margin status.

## Query API

Projection tables are exposed through read-only room endpoints:

```text
GET /rooms/{room_id}/orders?account_id=20&limit=100
GET /rooms/{room_id}/trades?account_id=20&limit=100
GET /rooms/{room_id}/ticks?limit=100
GET /rooms/{room_id}/ledger?account_id=20&limit=100
GET /rooms/{room_id}/positions?account_id=20&limit=100
GET /rooms/{room_id}/events?from_start=true&limit=100
GET /rooms/{room_id}/events?after_command_seq=120&limit=100
GET /rooms/{room_id}/events/stream
```

`account_id` is optional where it applies. `limit` defaults to 100 and is capped
at 500.

The event timeline uses the room-global `command_seq` as a durable cursor.
Start a complete traversal with `from_start=true`; subsequent requests should
supply the returned `next_after_command_seq` as `after_command_seq`, which
returns executions strictly after that cursor in ascending order. The response
also includes `latest_command_seq` and `has_more`; keep requesting while
`has_more` is true. Without either option, the endpoint preserves its original
behavior and returns the latest page. A cursor that does not exist in the room
timeline returns `409 Conflict` instead of silently skipping command zero.

`/events/stream` is a Server-Sent Events endpoint. Each `execution` event uses
`command_seq` as its SSE `id`. Reconnect with `Last-Event-ID`, or pass
`after_command_seq` explicitly. A connection without either cursor starts at
the live edge; `replay_from_start=true` requests the complete durable room
timeline. The server subscribes and captures a replay boundary before paging
the journal, so executions through that boundary come from storage and later
executions come from the live channel without a gap. If a slow consumer
overruns the bounded live channel, or durable replay cannot reach the captured
boundary, it receives `resync_required` and the stream closes; reconnect using
the supplied last cursor. Like the paginated event endpoint, this is currently
a room-admin surface because an execution can contain data for multiple
accounts.

The blocking Rust client exposes `room_event_stream_from_start` and
`room_event_stream_after`. Its iterator validates that the SSE id matches the
payload, the room id is unchanged, and command sequences remain contiguous. On
a transport disconnect it reconnects once from the last accepted command. The
reconnect first uses the configured base URL, follows one trusted 409 owner
hint, and falls back across explicitly trusted instance URLs when the base
address is unreachable. Bearer or user credentials are never sent to an
address outside that allowlist. Duplicate replayed events are ignored; a
sequence gap, malformed stream, repeated close, or `resync_required` event is
returned as a terminal `HttpTradingError`, after which the caller should
establish a new stream from the last durable cursor. Dropping the iterator
closes the HTTP response. The client intentionally omits a cursorless live-edge
helper because such a connection cannot prove that no event was lost before
its first event.

## Idempotent Order Submission

Order endpoints accept an optional `Idempotency-Key` header containing 1 to 128
visible ASCII characters:

```text
POST /rooms/{room_id}/orders
POST /rooms/{room_id}/instruments/{instrument_id}/orders
Idempotency-Key: client-order-2026-08-06-0001
```

The key is scoped to the authenticated user and room. Its request fingerprint
and resulting execution are stored atomically in `marketforge_executions`.
Retrying the same request returns the original execution without allocating a
new order id or appending another command. Reusing the key for a different
payload or instrument returns `409 Conflict`. This closes the usual ambiguous
outcome window where the journal commit succeeded but the HTTP response was
lost.

`/positions` is a historical clearing-leg projection, not the authoritative
current account state. In a shared-collateral venue, activity on one instrument
can change another instrument's portfolio margin context without producing a
clearing leg for that peer instrument, so its projected row can lag the live
cross-margin state. Use `GET /rooms/{room_id}/accounts` for the primary market
or `GET /rooms/{room_id}/instruments/{instrument_id}/accounts` for an explicit
instrument when current account and portfolio-margin values are required.

## Access Control

Loopback development keeps the lightweight `x-user-id` boundary; if the header
is omitted, the development user is `local-user`. The server refuses a
non-loopback bind in this mode.

For authenticated deployments, configure an opaque bearer-token to user-id
mapping:

```sh
export MARKETFORGE_AUTH_TOKENS_JSON='{"replace-with-a-long-token":"alice"}'
export MARKETFORGE_BIND_ADDR='0.0.0.0:57305'
export MARKETFORGE_CORS_ORIGINS='https://marketforge.example.com'
cargo run -p exchange-server
```

`MARKETFORGE_CORS_ORIGINS` is a comma-separated allowlist of complete `http` or
`https` origins without paths. Invalid values fail startup. When omitted, only
`http://127.0.0.1:57304` and `http://localhost:57304` are allowed.

In bearer mode, requests must send `Authorization: Bearer ...` and
`x-user-id` is ignored, so callers cannot select another identity. The current
HTTP-based background agent worker is deliberately disabled in bearer mode;
use loopback development mode for it until the worker has an internal trusted
command path. The Rust `HttpTradingClient::with_bearer_token` constructor is
available for external trusted callers; its debug representation redacts the
token.

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

On startup, the server loads persisted rooms, command executions, versioned
room mutations, and snapshots. Recovery restores a compatible versioned
`StateCheckpoint` mutation when one is available (or rebuilds the scenario),
then interleaves mutations and commands by the room-global command cursor.
Stored execution summaries, commands, clearing events, transfer results, and
final room status are compared with replayed results; divergence fails startup
instead of silently accepting different state.

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
remains the durable audit trail. Startup recovery currently selects versioned
`StateCheckpoint` mutations; snapshot rows remain useful for compatibility,
inspection, and migration input. Periodic snapshot rows are not yet mirrored to
`StateCheckpoint` mutations, so they do not currently reduce replay for newly
persisted rooms.

Timeline responses are served from the canonical journal, not from the replayed
actor history. This keeps `/rooms/{room_id}/events` complete even when the
in-memory actor was restored from a snapshot. Runtime state retains only the
latest 1,024 execution summaries per room; REST traversal and SSE catch-up page
older history directly from the journal, so long-running rooms do not grow this
cache without bound.

## Operational Endpoints

The standalone server exposes three unauthenticated, low-cardinality
operational surfaces on the same listener:

```text
GET /health/live
GET /health/ready
GET /metrics
```

`/health/live` only confirms that the HTTP process can answer. It does not wait
for PostgreSQL and should be used as a liveness probe. `/health/ready` checks
that shutdown has not started and runs the journal health check (`SELECT 1` for
PostgreSQL); use it as a readiness probe. The legacy `/health` path remains an
alias for readiness.

`/metrics` uses the Prometheus text exposition format and sets `Cache-Control:
no-store`. It reports process-local gauges and counters for durable writes, SSE
connections and resyncs, loaded rooms, agent workers, bounded event-cache use,
and aggregate journal queue/worker activity. The
`marketforge_journal_write_workers` and `marketforge_journal_read_workers`
gauges expose the active topology. Metrics contain no room, account, user, or
instrument labels, avoiding unbounded label cardinality. Counters reset when
the process restarts.

When room lease enforcement is enabled,
`marketforge_room_writer_leases_owned` must match `marketforge_rooms` for the
instance to remain ready. `marketforge_room_writer_leases_lost` and
`marketforge_room_writer_lease_renew_failures_total` expose lease loss without
adding room or instance labels.

## Runtime Model

Room mutation remains deliberately single-writer for deterministic ordering.
The global coordinator is asynchronous and does not block Tokio worker threads,
but it is not a multi-room parallel execution engine. PostgreSQL projection and
authorization reads can run in parallel on bounded reader workers while all
writes retain one deterministic lane. Readiness checks the writer and every
configured reader, and returns `503` when any durable connection is unavailable
or graceful shutdown has started.

In the default `single-active` mode, only one server process may own a
PostgreSQL journal at a time. In `room-leased` mode, multiple ready processes
may share the database, but each room still has exactly one fenced writer.

`exchange-server` handles Ctrl-C and SIGTERM as graceful shutdown signals. It
stops accepting new connections and durable writes, closes SSE streams, stops
and joins background agent workers, and waits for detached durable transactions
to leave the commit/install interval before the process exits. This preserves
the guarantee that a committed journal update is installed in memory even when
the originating client disconnects while the server is shutting down.

## Verification

Unit tests do not require PostgreSQL. To run the PostgreSQL integration test,
set `MARKETFORGE_TEST_DATABASE_URL`:

```sh
export MARKETFORGE_TEST_DATABASE_URL='postgres://marketforge:marketforge@127.0.0.1:55432/marketforge'
cargo test -p exchange-server postgres_journal_persists_and_recovers_room_when_configured -- --nocapture
```

CI sets `MARKETFORGE_REQUIRE_POSTGRES_TESTS=1`, which makes a missing database
URL a test failure rather than a silent skip.

The smoke script exercises the same persistence path through HTTP and restarts
the server to confirm recovery:

```sh
MARKETFORGE_DATABASE_URL='postgres://marketforge:marketforge@127.0.0.1:55432/marketforge' \
  ./scripts/postgres_smoke.sh
```

The multi-active smoke starts two ready processes, verifies explicit non-owner
conflicts, transfers a room after graceful release, checks recovered state and
the incremented fencing token, then confirms both instances can own different
rooms concurrently:

```sh
MARKETFORGE_DATABASE_URL='postgres://marketforge:marketforge@127.0.0.1:55432/marketforge' \
  ./scripts/postgres_multi_active_smoke.sh
```
