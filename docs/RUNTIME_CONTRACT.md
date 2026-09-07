# MarketForge runtime contract

This document is the P0 binding contract for later backend stages. It records
the semantics implemented at baseline commit `5047275` and names the rules later
phases must preserve. Where current code diverges from the target contract, the
gap is called out explicitly; later stages close those gaps instead of silently
changing meaning.

Integer types in this document (`u64`, `i64`, `i128`) are the in-process types.
JSON encodings that can lose those values are a P2 concern; recovery and
matching use the native integers.

## 1. Identifiers and sequences

### 1.1 Room-global `command_seq`

Each matching command that the room actor accepts or rejects consumes exactly
one room-global `command_seq` (`ActorSeq`, `u64`). The next value lives on the
`SimulationRoom` / `ExchangeActor` (`next_command_seq`) and is assigned when
the actor applies a command, not when the HTTP handler receives the request.

- Seed scenario orders consume `command_seq` starting at `0`.
- User, bot, and system commands continue that same sequence.
- Automatic liquidations and peer-cancels that the actor emits as follow-on
  executions also consume subsequent `command_seq` values in the same apply.
- Pause, resume, close, deposits, withdrawals, venue-to-venue transfers, and
  clock advances are **mutations**, not commands. They do not consume
  `command_seq`.
- Rejected commands still consume `command_seq` and are journaled. A rejected
  command that never reached the actor (authz, lease, fingerprint conflict)
  does not.

`command_seq` is the durable cursor for `/rooms/{room_id}/events` and SSE
`id`. Pages after `after_command_seq` return executions with
`command_seq > after_command_seq`. A cursor that is not in the room timeline
returns HTTP 409 rather than skipping command zero.

### 1.2 In-engine event sequence

Inside `exchange-core`, `EventLog` assigns a separate `LogSeq` to each
matching `Event` (`OrderAccepted`, `TradePrinted`, …). That per-book event
sequence is used by the in-process replay helper (`ReplayEngine`) and JSONL
logs. The HTTP journal does **not** persist that engine `LogSeq`. Durable
event identity on the server is `(room_id, command_seq, event index in the
execution summary)`.

### 1.3 Mutation sequence and command cursor

Each durable non-command transition is a `JournalMutation`:

| Field | Meaning |
| --- | --- |
| `mutation_seq` | Strictly increasing per journal (in-memory) or per room (PostgreSQL `RETURNING`). Never reused. |
| `command_cursor` | The **next** room-global `command_seq` after this mutation’s replay point. A mutation at cursor `0` happens before command zero. A mutation at cursor `N` happens after command `N-1` and before command `N`. |
| `schema_version` | Currently `ROOM_MUTATION_SCHEMA_VERSION = 1`. Unknown versions fail recovery closed. |

Kinds: `state_checkpoint`, `clock_advanced`, `deposit_submitted`,
`withdrawal_submitted`, `venue_to_venue_transfer_submitted`, `status_changed`.

Recovery interleaves mutations and commands by `command_cursor`:

1. Restore from the latest compatible `StateCheckpoint` mutation (or scenario
   bootstrap / legacy snapshot).
2. Replay mutations whose `mutation_seq` is after that checkpoint.
3. When a mutation’s `command_cursor` is `<=` the next command to apply, apply
   the mutation first.
4. Apply the command whose `command_seq` equals the current cursor, then
   increment.
5. Remaining mutations after the last command are applied in order.

A later mutation with a smaller `command_cursor` than an earlier one is a
recovery error. Replay that diverges from stored execution summaries, clearing
amounts, transfer results, or room status fails startup.

### 1.4 `market_time_ms`

`SimulationClock` stores `step`, `market_time_ms`, and `step_duration_ms`
(default 1000). `advance_step` adds `step_duration_ms` to `market_time_ms`.

Every new journaled command execution stores `market_time_ms` as the
authoritative simulation time at which the engine evaluated that command. The
same value is copied into order, trade, and market-tick projections (migration
0009). Legacy rows may have `null`; wall-clock `created_at` is operational
metadata and must not be used as simulated market time.

Clock advances are mutations. They change `step` / `market_time_ms` and may
complete due transfers. They do not by themselves emit a matching command
unless the actor also produced journalable system executions (for example
liquidation) during that advance; those executions are stored in the same
mutation transaction.

### 1.5 Order IDs

API and participant new-orders take the next value from the process-wide
`AppState.next_order_id` via `OrderGateway::take_order_id`. Recovery restores
that cursor from submitted **non-seed** `NewOrder` commands:

- Start at `1`.
- For each recovered `NewOrder` with `order_id < 9_000_000_000_000_000_000`,
  set `next_order_id = max(next_order_id, order_id + 1)`.
- Seed orders in the saved scenario are skipped so high seed ids do not jump
  user ids.
- Ids at or above `SYSTEM_LIQUIDATION_ORDER_ID_BASE` (`9_000_000_000_000_000_000`)
  are reserved for reduce-only system liquidations. User/API allocation that
  would enter that range returns a conflict. Recovery of a non-system order in
  that range fails closed.

Cancel and amend reuse the existing order id and do not allocate.

Duplicate order ids are rejected by the matching engine even after fill.

## 2. Wall clock versus simulation time

| Clock | Used for | Must not be used for |
| --- | --- | --- |
| Wall clock | HTTP timeouts, SSE keep-alives, journal worker scheduling, room-lease duration/renewal/expiry (PostgreSQL clock), agent-worker sleep interval, readiness, metrics uptime, `created_at` | Strategy decisions, training deadlines, candle close, transfer delay completion, venue session windows |
| Simulation clock (`step`, `market_time_ms`) | Matching evaluation time stored on executions, venue session/price-limit/circuit rules, T+N settlement, transfer delays, CandleScope kline aggregation, future training deadlines and scenario actions | Lease expiry, process liveness |

Current gap (closed in P1): `AgentRuntime` owns a **separate** `SimulationClock`
that advances on `run_step`. The HTTP agent worker does **not** call
`/clock/advance`; it sleeps `interval_ms` on the wall clock and submits orders
against whatever room time currently is. Reproducible bot continuation therefore
requires a single authoritative room clock (P1.3).

## 3. Three consistency classes

These are different products. Passing one does not prove the others.

### 3.1 Command replay (durable market)

Same scenario + same interleaved command and mutation stream ⇒ same books,
accounts, transfers, `command_seq` assignments, execution summaries, and
clearing amounts.

Proof today: journal recovery compares replayed summaries with stored records
and fails closed on divergence. In-process `ReplayEngine` replays command logs
onto an order book. Periodic `marketforge_room_snapshots` rows are **not** the
recovery fast path; recovery prefers `StateCheckpoint` mutations, else
scenario + full log. Snapshot rows that are not mirrored as checkpoints do not
shorten replay for newly persisted rooms.

### 3.2 Bot continuation (decision resume)

After crash or takeover, the next bot actions must be the actions the
uninterrupted run would have produced: same commands, fills, accounts, and
later agent orders.

This is **not** implied by command replay. Current `NoiseTrader` /
`DcaTrader` / `GridTrader` keep RNG, observed-step counters, and
`has_seeded_grid` only in process memory. The worker HTTP-callbacks the server
and is disabled in Bearer mode. Missing agent state on an old room must be
marked non-continuous, never silently reset (P1.4).

### 3.3 User-visible subscribe (stream apply)

A subscriber that starts from a snapshot plus a cursor must be able to apply
deltas without a gap or an undetectable duplicate. If the bounded live buffer
is overrun, or durable replay cannot reach the captured boundary, the stream
emits `resync_required` and closes. The client must reconnect from the last
durable cursor.

Today this exists only for the **admin** execution SSE
(`/rooms/{room_id}/events/stream`), whose `command_seq` is contiguous for the
whole room. Filtered public/private streams (P2) must not pretend that a
filtered subsequence is still a contiguous global `command_seq`. They either
use an independent stream cursor or document that the cursor is
non-contiguous.

## 4. Intra-step ordering

A **step** is one increment of the room `SimulationClock`. The contract for a
step, once P1.3 is implemented, is:

1. Advance time and run due venue settlement / transfer completions for that
   new step. Every venue in the room advances to the same global step **before**
   any cross-venue transfer links are created. Linked deposits therefore cannot
   complete in the same step that created them when delay is non-zero.
2. Execute due scenario actions as ordinary trading/config commands (never by
   writing last price).
3. Observe and decide in **stable participant-id order** (lexicographic
   `participant_id`). Later participants in that order may observe fills from
   earlier submissions in the same step. Simultaneous-observe is a new
   versioned scheduler if ever introduced.
4. Submit decided actions in that same participant order, then in the order
   the participant returned them. Each action is a normal gateway command:
   identity, account authz, lease fence, risk, match, journal, install.
5. Update training state from the resulting executions.

Human HTTP orders that arrive during a step are **not** merged by wall-clock
arrival into the middle of (3)–(4). They are durable commands ordered by the
single journal writer lane. A reproducibility claim must include that command
order, not only the scenario seed.

Automatic liquidations and peer-cancels triggered by a command are appended in
the same durable transaction, after the triggering command, in the order the
actor produced them. A client disconnect cannot cancel the interval between
journal commit and in-memory install.

### Example A — clock then agents then a human cancel

Room at `step=4`, `market_time_ms=4000`, `next_command_seq=10`.
Participants `dca-1` then `noise-1`. Auto-step fires.

1. Clock mutation `command_cursor=10`, `steps=1` → `step=5`, `market_time_ms=5000`.
   A delayed deposit due at step 5 completes inside that mutation.
2. `dca-1` observes the post-advance book (deposit already visible), decides a
   buy, submits command 10.
3. `noise-1` observes the book **after** command 10, may trade with `dca-1`’s
   resting order, submits command 11.
4. A human cancel that HTTP-arrived while (2) was in flight is journaled as
   command 12 **after** both agent actions, because the writer lane serializes
   commits. Replaying with a different wall-clock speed but the same commit
   order yields the same books.

Current HTTP worker does not perform (1). P1 must.

### Example B — fill-triggered liquidation in one transaction

A taker order is command 20. The actor also emits a reduce-only liquidation as
command 21. Both rows, their projections, and an optional snapshot are
committed together. Recovery that sees 20 without 21, or that replays 21 as a
user order, fails closed. Installing only 20 in memory if the journal write
fails is forbidden: candidate state is discarded.

### Example C — mutation between commands

Commands 0 and 1 exist. An admin pause mutation is stored with
`command_cursor=2`. Recovery applies 0, 1, then the pause, then later command
2. A clock mutation at `command_cursor=0` in a seedless room replays **before**
command 0. That is why cursor is “next command”, not “last command”.

## 5. Durable write boundary

```text
authorize → clone candidate RoomManager → apply → journal transaction
  (fenced lease row + executions and/or mutation + projections + optional snapshot)
  → install candidate into AppState → publish
```

If the journal append fails, the live `RoomManager` is unchanged. Durable
transitions run in detached tasks so an HTTP client cancel cannot abort the
commit/install interval. Shutdown stops accepting new durable writes, waits for
in-flight ones, then joins agent workers.

Idempotent order POST: `Idempotency-Key` (1–128 visible ASCII), scoped to
`(user_id, room_id)` in the **order** key space. Same key + same fingerprint
returns the original execution without a new order id. Same key + different
payload or instrument is 冲突 / HTTP 409.

Control writes (`pause` / `resume` / `close` / `clock/step` / `clock/advance`)
use the same header in a **separate control** key space
`(user_id, room_id, key)`. Fingerprint is `control.v1` + operation name +
normalized params (advance includes `steps`). Lookup, lease/auth, candidate
computation, result save, and mutation share one journal transaction. Unique
`(user_id, room_id, idempotency_key)` on `marketforge_control_idempotency` is
the last guard: a conflict rolls back the mutation and returns the stored
success body after a live permission check. Success is persisted; business
rejects and infrastructure failures are not. Keys do not auto-expire. The
in-process memory journal matches this protocol in one process and does **not**
claim durability across process restart. PostgreSQL is the cross-process
source of truth; `AppState` does not keep a control-idempotency map.

A manual step that also writes `TrainingProgress` still records the control
result on the `SchedulerProgress` mutation (the first durable substep), not
after later training side effects.

## 6. Identity, lease, and fencing

- Loopback local-dev: `x-user-id` or default `local-user`. Non-loopback bind
  without bearer tokens is refused at startup.
- Bearer mode: `Authorization: Bearer …` maps to a configured user id.
  `x-user-id` is ignored.
- Room membership roles are `owner` / `admin` / `instructor` / `trader` /
  `spectator`. Owner and admin administer the room and may trade any account.
  Instructor is **not** trade-any-account; instructor and trader may trade only
  assigned accounts. Spectators see public data only and cannot open private
  streams or submit orders. `account_owners` is the assignment table.
  Cross-account cancel/amend is forbidden in the gateway even if the caller is
  otherwise a member.
- Current HTTP agent worker uses `HttpTradingClient::with_user_id` and is
  **disabled in Bearer mode**. P1 replaces the callback with an internal
  application path bound to server-assigned participant/account identities.

Room writer leases (migrations 0011–0012): one owner id, PostgreSQL-clock
expiry, strictly increasing fencing token, optional advertised owner URL
(metadata only). Journal appends lock and validate the lease row in the same
transaction. Stale token → `RoomLeaseLost`, no journal or projection change.
Non-owner request → HTTP 409 `code=room_owned_by_other_instance` with
`room_owner`. Takeover reloads the room from the journal, restores
`next_order_id`, then serves. Only the current fencing-token holder may step
or drive agents (P1.5; today the HTTP worker does not check a token on each
tick beyond the request-time lease).

## 7. Named error classes

HTTP continues to use status codes. Bodies SHOULD include a stable `code`
string. Clients classify by `code` first, status second, message last.

| Class | `code` | Typical HTTP | Meaning |
| --- | --- | --- | --- |
| 参数错误 | `invalid_request` | 400 | Malformed JSON, empty ids, illegal ticks/qty, agent template/room mismatch, seed order in the reserved id range, invalid idempotency key syntax |
| 未授权 | `unauthorized` | 401 | Missing/invalid bearer; local-dev invalid `x-user-id` stays 400 |
| 账户不可用 | `account_unavailable` | 403 | Caller cannot access the named account, or the account is not in the room |
| 房间状态冲突 | `room_state_conflict` | 409 | Room exists, room paused/closed for the attempted write, unknown event cursor, order-id range exhausted, agent worker not allowed in this auth mode, mixed runtime-mode lock |
| 非 owner | `room_owned_by_other_instance` | 409 | Live lease held by another instance. Body includes `room_owner`. |
| 幂等冲突 | `idempotency_conflict` | 409 | Key reused with a different fingerprint |
| 需要重新同步 | `resync_required` | SSE event (stream closes) | Gap, overrun, or replay that cannot reach the captured boundary |

Lease lost on a writer the instance thought it owned: `room_lease_lost` /
`room_lease_not_owned` with HTTP 503. Shutdown and journal unavailability are
503 without those codes. Room missing is 404. Internal journal/recovery
failures are 500 and fail closed.

Baseline mapping: many handlers still return only `error` text with no
`code`. P2’s versioned HTTP contract fills the codes; new handlers MUST set
them. Existing `room_owned_by_other_instance`, `room_lease_lost`, and
`room_lease_not_owned` remain unchanged.

## 8. Version and compatibility policy

| Artifact | Current version | Compatibility |
| --- | --- | --- |
| SQL migrations | `0001`–`0012` under `exchange-server/migrations` | Append-only. Applied versions are immutable; repairs are a new version. Tracked in `marketforge_schema_migrations`. |
| Room mutation schema | `1` | Unknown version fails recovery. Additive fields must default. |
| `StateCheckpoint` actor JSON | implicit serde of `SimulationRoom` | Unknown/legacy shapes either normalize (tested) or fail closed. Periodic snapshot table rows are not this schema. |
| Scenario config | serde of `ScenarioConfig` | Stored on the room row. Breaking field changes need a version field before use as a training product (P3). |
| Agent template JSON | `AgentTemplate` serde | Config only; **not** recoverable strategy state. P1 introduces versioned agent state blobs. |
| Strategy protocol | `strategy.v1` | Observation + HTTP place/cancel. Breaking observation fields require a new version string. |
| Scoring | not published | P3 adds a scoring-rule version; reports must record it. |
| HTTP API | unversioned paths | Additive fields with defaults. Breaking response changes require a versioned path or an explicit compat window documented in the validation record. |
| CandleScope plugin | contract tests in `marketforge-candlescope-plugin/tests` | Plugin tests must stay green. Aggregation uses `market_time_ms` and refuses integers above `MAX_SAFE_INTEGER` (`2^53-1`) in JS-facing JSON. |

Fail closed: divergent replay, unsupported mutation version, mixed
single-active / room-leased locks, missing postgres when
`MARKETFORGE_REQUIRE_POSTGRES_TESTS=1`.

Fail open is forbidden for: installing candidate state after a journal error,
resetting agent RNG because state was missing, treating a skipped database
test as persistence evidence, or applying a filtered SSE subsequence as if it
were a contiguous `command_seq`.

## 9. Known baseline gaps (not defects in matching)

These are accepted P0 facts. They do not fail the P0 gate. Later stages must
not assume they are already solved.

1. Agent workers HTTP-callback the same process and are blocked in Bearer mode.
2. `AgentRuntime` clock is not the room clock; workers sleep on wall time.
3. No versioned participant/agent state in the journal.
4. `MarketView.accounts` is the full account set; it is not a participant
   observation protocol.
5. Periodic snapshots are not `StateCheckpoint` mutations, so they do not
   accelerate recovery of newly persisted rooms.
6. User-visible public/private streams do not exist; the execution SSE is an
   admin audit surface.
7. Control writes are durable-idempotent under `control.v1` (see §5). In-memory
   journal replay matches in-process only.
8. Error bodies are mostly uncoded strings.

Matching, journal-then-install, fencing tokens, command/mutation interleaving,
and `market_time_ms` on new executions **are** in place and are the baseline
later stages extend.
