# Pine VM checkpoints

`MARKETFORGE_PINE_VM_CHECKPOINT=1`, together with the `pine-in-process` build
feature and `MARKETFORGE_PINE_IN_PROCESS=1`, selects broker schema 3 for new
eligible Pine instances. Existing schema-2 instances keep their original full
transcript protocol. Do not relabel an old state or erase its history to migrate
an active account.

The schema-3 eligibility boundary remains the six exact installed Pine source hashes,
broker execution, integer input overrides, no automatic hedge and no exit
protection. This change executes their compiled Pine programs; it does not
replace them with separately handwritten strategy implementations. Arbitrary
Pine source is still outside this checkpoint-qualified path. The separate
[ecosystem direct cohort](validation/2026-10-24-pine-continuous.md) admits 201 exact
sources using schema 4 compressed replay logs. Those logs retain full historical
inputs and replay all of them on cold recovery; they are not schema-3 VM snapshots.
Only that ecosystem cohort migrates existing schema-2 state after verifying all
historical intents. The original six sources retain the schema-2 contract described
above when restoring an existing schema-2 state.

V9 verifies source once at each direct admission: it reads the complete current
file and compares it exactly, after newline normalization, with the qualified
installed source. A private borrowed verification token is bound to the same
host and script and passed through private execution helpers within that attempt.
It is not persisted or reused across attempts, and does not authorize another
host or script. Public `decide`, `decide_borrowed` and `decide_value` calls each
perform fresh verification; there is no TTL, mtime or startup-only source cache.

The successful source read defines this attempt's admission point. If the file
is edited afterward, the current call may finish using the already installed
program; the next attempt reads the source again and rejects the change. This
does not reload arbitrary edited code into the qualified path. Schema-3 window
metadata may inspect the immutable installed set without file I/O, but that
metadata check cannot authorize execution.

Once direct execution starts, any error is returned without retrying through
the process adapter. Only an initially unqualified route can consider the
existing process path for compatible legacy state; schemas 3 and 4 reject that path
explicitly. Source-check multiplicity was normally two per direct call in v8
and is normally one per admitted attempt in v9; this is a behavior description,
not a measured performance claim.

The durable state contains a calculation checkpoint plus fewer than 32 external
passes. A checkpoint captures retained series, active series, persistent scalar
variables, rolling windows/extrema, RSI and other supported scalar accumulators,
the absolute bar cursor and external-account position history. It binds source,
configuration and participant identity, the runtime protocol, a SHA-256 digest
of the prepared IR, input overrides, chart context and runtime limits. Numbers
inside the VM and replay passes preserve floating-point bits across JSON storage.
The complete IR is not repeated in each bot's state.

Fixed integer input values are used to re-run the compiler's history analysis.
Only the retention metadata changes; executable statements are preserved.
This matters because the generic VM conservatively retains the full source
series for a dynamic window length. Compact execution requires a statically
proven retention bound; unresolved dynamic history is rejected. The bound comes
from the actual configured lookback, not an arbitrary number of recent candles.

A cold worker restores the checkpoint into a freshly prepared runtime and
replays only the bounded tail. Replayed intents must match the frozen committed
tail. A warm worker reuses a session only when its checkpoint, tail and settings
equal the committed state. A rejected or uncommitted speculative turn therefore
cannot authorize subsequent VM advancement. The checkpoint cursor, replay tail,
evaluated count, candle count and last time must agree.
The VM payload and the complete execution envelope have separate SHA-256 seals.
The latter also covers replay passes and saved provider samples, so a changed
pass cannot evade detection merely because it originally emitted no order.
Host execution receipts remain outside that envelope because the host adds them
after committing a decision.

The transcript is not replaced by a moving-average warm-up window. Calculation
state continues across the complete execution lifetime. A bounded candle window
is accepted only when the trusted exchange host provides a versioned SHA-256
chain over the complete omitted prefix. The adapter continues that chain through
its previously committed cursor and compares the saved digest before execution.
Changes before the visible window are detected as well as changes within it.
Without a proof, only complete, untruncated input is accepted. A new instance
cannot start from an omitted prefix. A bot that has fallen behind the available
window fails closed rather than silently skipping decisions.

For the qualified `basis.pine` source, the request expression is scalar `close`
at the chart interval. The replay pass freezes the current/prior visible request
samples, including a committed prior sample for gaps. Other request expressions
need their own checkpoint and differential qualification before admission.

The protocol is for order intent execution. Historical plots and rendering
objects are not restored; drawing/collection state and unsupported TA/request
state are rejected. Schema 3 requires the compatible in-process runtime on
restart. Process fallback refuses it explicitly instead of attempting an
incorrect transcript reconstruction.

Own-fill accounting and order receipts remain authoritative and use the existing
broker translation. Their history completeness is checked separately from candle
window truncation. This checkpoint work does not remove own-fill retention limits
or implement a compact fill ledger. It also does not prove an end-to-end bot
capacity gain: journal encoding, observation preparation, queue delay and actual
commit rate require separate measurements.

`exchange-server/tests/pine_vm_checkpoint.rs` contains the executable qualification
gates: six compiled sources compared against continuous VM execution through
4097 bars; actual JSON checkpoint restoration after 512 and 2048 bars; host
comparison against schema 2; speculative retries; rolling 64-bar windows beyond
2048 bars; and rejection of historical corrections and malformed recovery state.
The test source describes coverage, not a claim that a particular build passed.
