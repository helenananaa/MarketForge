# PostgreSQL query projections

Orders continue to compute candidate room state, commit its canonical journal
transaction, and only then install/publish state and return success. PostgreSQL
durability settings are unchanged. Execution batches use set-based journal
inserts; existing bot-wave ordering and actor execution boundaries are unchanged.

The same transaction commits request idempotency, external-action quota, room
writer fencing, snapshots/checkpoints, order/trade identity constraints, and a
durable reference to each execution that needs projection. Transfer events also
enqueue durable references. No correctness dependency uses an in-memory enqueue.

An independent connection/thread projects up to 256 executions and 256 transfer
events per transaction. It folds touched order state in canonical command order
and performs set-based updates of orders, order events, trades, market ticks,
account ledger, position snapshots and current transfers. Query rows, projection
progress and removal of their pending references commit together. Failure rolls
everything back; restart retries the durable pending rows.

Transaction-scoped advisory locking serializes projectors across server instances.
There is no global `MAX(sequence)` skip cursor: sequence allocation and transaction
commit can occur in different orders. Pending rows from a late commit remain
discoverable even after later-allocated rows have been processed.

The actor and replay authority remain the canonical execution/mutation/transfer
journals and snapshots. Checkpoint recovery derives next order IDs from the
synchronous identity registry and latest market ticks from canonical executions,
so it does not require query projections to catch up first. Scheduler deltas,
control idempotency and lease checks keep their existing synchronous semantics.

Query methods capture the room's committed pending references at entry and wait
for that finite set, preserving read-after-write for the existing API. Later
writes do not keep extending the barrier. The wait budget is 30 seconds; failure
returns an error instead of a stale/missing result. Authorization/account filters
remain in the original SQL. Canonical execution/idempotency queries do not wait.

Each pending queue is bounded to 65,536 rows per room on the commit path. At the
limit a new candidate fails before commit/publication and can be retried after
catch-up. Worker memory is bounded by batch size, not by total pending history.
The canonical journals retain their existing history policy.

Prometheus metrics expose sampled database backlog, committed projection batches
and records, and retry errors under `marketforge_query_projection_*`. Counters are
per process; the backlog gauge is a sample, not an atomic end-to-end guarantee.

Migration 18 seeds synchronous identity keys/progress from existing fully committed
query projections; it does not replay or rewrite historical business data. Plan
upgrades with compatible writers and the existing runtime ownership guards; mixed
old/new journal-writing binaries are not a supported rolling-upgrade mode.

## Synchronous-write latency

Connection-local prepared statements are reused with a 128-statement bound.
Eviction and connection replacement discard prepared handles, not durable data.
Transactions, fencing predicates and synchronous commit remain unchanged.

Large PostgreSQL room-mutation JSONB parameters are encoded with the same
checked recovery integer formatter and sent directly as checked JSON bytes,
avoiding an intermediate complete JSON Value tree. New snapshots store that
complete checked UTF-8 JSON in `actor_bytes` (BYTEA), so synchronous snapshot
writes do not build a PostgreSQL JSONB tree. The database enforces exactly one
of `actor_json` and `actor_bytes`. Old JSONB rows remain readable; migration does
not rewrite them. A normal snapshot upsert can replace the encoding at the same
checkpoint key. Recovery decodes either representation into the original Value/actor
path, preserving legacy-actor checks, complete history and recovery heads.
Existing early snapshot/mutation validation stays on the commit path.

Migration 19 adds the snapshot representation and selects LZ4 TOAST compression
for future snapshot/mutation rows when the PostgreSQL build advertises it.
Builds without LZ4 keep their original compression method. Snapshot cadence,
transaction boundaries and synchronous commit are unchanged. Compression can
change stored/WAL bytes; include that tradeoff in performance measurements.

Set `MARKETFORGE_WRITE_PROFILE=1` for diagnostic console records of room waiting,
order preparation, journal queue/operation, canonical execution and snapshot SQL
stages. `MARKETFORGE_WRITE_PROFILE_MIN_MS` defaults to 10; these records cover only
operations above that threshold and include warmup/startup unless filtered by the
measurement harness. Profiling is disabled by default, does not alter committed
state, and must be disabled for the final latency comparison.

State-only mutations without executions, transfers, snapshots, control-idempotency
or status changes use one PostgreSQL statement and its implicit synchronous
transaction. Success is returned after the driver consumes ReadyForQuery, which
follows commit. Fenced writes lock the matching live lease row in the same
statement before inserting; a changed owner/token or expired lease produces no
mutation. Writes with multiple effects retain their explicit atomic transaction.
This removes two round trips without batching acknowledgements or relaxing
durability. Execution writes also borrow coordinator-owned inputs and build
projection arrays directly, avoiding redundant copies.

The in-memory journal stores the same complete serialized mutation envelopes in
tight boxed slices. Records of at least 1 KiB use lossless LZ4 blocks only when
compression saves space; small or incompressible records remain raw. Recovery
checks the retained original length before decoding the original envelope.
History, mutation sequence and replay order are unchanged; no records are trimmed
or coalesced. This reduces retained allocation but does not bound an indefinitely
growing history. PostgreSQL rows and snapshot formats are unaffected by this
in-memory representation. With profiling enabled, memory_retention records expose
logical JSON bytes and actual stored bytes for diagnosis.

Large new PostgreSQL actor_bytes snapshots compress the same checked JSON in the
application before SQL transport. A zero-prefixed, versioned 16-byte envelope
identifies a lossless LZ4 block and its original length; only a smaller encoding
is selected. Small/incompressible snapshots remain raw JSON. Recovery accepts
compressed envelopes, legacy raw JSON bytes, and legacy JSONB, then uses the same
Value/actor and recovery-head validation. Unknown versions, truncation, invalid
lengths and decompression failures return recovery errors. The decoded-length
bound preserves PostgreSQL's original 1 GiB field limit. This changes neither
complete history nor checkpoint cadence and remains inside the original
synchronous transaction. No schema migration or rewrite of existing rows is
required. Earlier binaries that only decode raw actor_bytes cannot read these
compressed snapshots; stop old writers before upgrading and do not downgrade
after producing them. Diagnostic pg_snapshot records include JSON/stored lengths
and separate compression timing.
