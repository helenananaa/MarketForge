# Realtime bot scheduling — 2026-10-08

Automatic market ticks and bot decisions now run independently. Bot computation
uses immutable observations outside the shared market lock; completed actions
enter the ordinary trading gateway against current state. One decision per bot
may be in flight. Failures suspend only the affected bot until the list is
restarted. Stop-bots leaves automatic market time running; pause stops the room.
Manual single steps retain their deterministic sequential behavior.

Live clock/order commits include any training update in the same journal
mutation as bot state and executions. The optional `SchedulerProgress.training`
payload and `SchedulerState.bots_enabled` preserve reading older records. Live
execution receipts identify the submitting participant directly, including when
multiple bots share an account. Pending results are fenced across pause/resume,
stop and replacement. Recovered unfinished manual steps must be completed before
switching to automatic trading.

## Validation

- `cargo fmt --all -- --check`: passed.
- `cargo clippy --workspace --all-targets -- -D warnings`: passed.
- `cargo test --workspace`: 337 passed, zero failures. This includes six new
  realtime tests, existing manual scheduler crash/recovery tests, and process
  plugin protocol/timeout tests.
- `python -m unittest discover -s python/tests -v`: 19 tests reported, nine
  skipped; no failures. Database-dependent integration cases were skipped.
- `git diff --check`: passed.

The new tests hold a bot behind a release gate while asserting clock progress,
healthy-bot activity, another room's progress, and successful HTTP orders in both
rooms. They also cover a real Python process timing out, panic/error isolation,
late-decision fencing, submission against a changed book, duplicate stale-state
suppression, correct shared-account participant attribution, recovery with and
without snapshots, and training deadlines advancing only with market time.

Windows test commands prepend `.venv/Scripts` to PATH to select the repository's
working `python3.exe`. An initial run without this setting had three failures
caused by the system Python alias exiting with code 9009; the configured rerun
passed. Local command output is retained in `.local/bot-scheduling-*.log`.

No PostgreSQL test URL was configured. Rust database tests return early under
that condition, so these counts do not establish a fresh PostgreSQL integration
pass. The validation uses local test servers and in-memory journal recovery;
it does not establish deployment or load-test performance. Existing rooms that
have not enabled automatic scheduling retain explicit clock control. Server
restart restores state but still requires explicitly restarting automatic mode.
