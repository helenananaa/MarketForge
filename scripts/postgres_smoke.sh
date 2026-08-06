#!/usr/bin/env bash
set -euo pipefail

DATABASE_URL="${MARKETFORGE_DATABASE_URL:-postgres://marketforge:marketforge@127.0.0.1:55432/marketforge}"
BASE_URL="${MARKETFORGE_BASE_URL:-http://127.0.0.1:57305}"
READ_WORKERS="${MARKETFORGE_JOURNAL_READ_WORKERS:-4}"
LEASE_DURATION_MS="${MARKETFORGE_ROOM_LEASE_DURATION_MS:-2000}"
LEASE_RENEW_INTERVAL_MS="${MARKETFORGE_ROOM_LEASE_RENEW_INTERVAL_MS:-500}"
STANDBY_BASE_URL="${MARKETFORGE_STANDBY_BASE_URL:-http://127.0.0.1:57306}"
STANDBY_BIND_ADDR="${MARKETFORGE_STANDBY_BIND_ADDR:-127.0.0.1:57306}"
ROOM_ID="pg-smoke-$(date +%s)-$$"
SERVER_PID=""
STANDBY_PID=""

cleanup() {
  if [[ -n "${STANDBY_PID}" ]] && kill -0 "${STANDBY_PID}" 2>/dev/null; then
    kill "${STANDBY_PID}" 2>/dev/null || true
    wait "${STANDBY_PID}" 2>/dev/null || true
  fi
  if [[ -n "${SERVER_PID}" ]] && kill -0 "${SERVER_PID}" 2>/dev/null; then
    kill "${SERVER_PID}" 2>/dev/null || true
    wait "${SERVER_PID}" 2>/dev/null || true
  fi
}
trap cleanup EXIT

wait_for_server() {
  local base_url="${1:-${BASE_URL}}"
  for _ in $(seq 1 80); do
    if curl -fsS "${base_url}/health/ready" >/dev/null 2>&1; then
      return 0
    fi
    sleep 0.25
  done
  echo "exchange-server did not become healthy at ${base_url}" >&2
  return 1
}

assert_operational_endpoints() {
  local base_url="${1:-${BASE_URL}}"
  local metrics
  curl -fsS "${base_url}/health/live" >/dev/null
  metrics="$(curl -fsS "${base_url}/metrics")"
  if ! grep -q '^marketforge_process_up 1$' <<<"${metrics}"; then
    echo "expected process liveness metric, got: ${metrics}" >&2
    return 1
  fi
  if ! grep -q '^marketforge_journal_channel_open 1$' <<<"${metrics}"; then
    echo "expected open journal channel metric, got: ${metrics}" >&2
    return 1
  fi
  if ! grep -q '^marketforge_journal_write_workers 1$' <<<"${metrics}"; then
    echo "expected one journal write worker, got: ${metrics}" >&2
    return 1
  fi
  if ! grep -q "^marketforge_journal_read_workers ${READ_WORKERS}$" <<<"${metrics}"; then
    echo "expected ${READ_WORKERS} journal read workers, got: ${metrics}" >&2
    return 1
  fi
}

assert_second_server_is_rejected() {
  local status
  if timeout --kill-after=1s 5s env \
    MARKETFORGE_DATABASE_URL="${DATABASE_URL}" \
    MARKETFORGE_JOURNAL_READ_WORKERS="${READ_WORKERS}" \
    MARKETFORGE_INSTANCE_ID="smoke-conflict" \
    MARKETFORGE_ROOM_LEASE_DURATION_MS="${LEASE_DURATION_MS}" \
    MARKETFORGE_ROOM_LEASE_RENEW_INTERVAL_MS="${LEASE_RENEW_INTERVAL_MS}" \
    MARKETFORGE_RUNTIME_LOCK_WAIT_MS="0" \
    MARKETFORGE_BIND_ADDR="127.0.0.1:57306" \
    target/debug/exchange-server >/tmp/marketforge-postgres-second-server.log 2>&1; then
    echo "expected a second server for the same database to fail startup" >&2
    return 1
  else
    status="$?"
  fi
  if [[ "${status}" == "124" || "${status}" == "137" ]]; then
    echo "second server stayed alive instead of rejecting the shared database" >&2
    return 1
  fi
  if ! grep -q 'already owns the PostgreSQL runtime lock' /tmp/marketforge-postgres-second-server.log; then
    echo "second server failed for an unexpected reason: $(cat /tmp/marketforge-postgres-second-server.log)" >&2
    return 1
  fi
}

start_server() {
  MARKETFORGE_DATABASE_URL="${DATABASE_URL}" \
    MARKETFORGE_JOURNAL_READ_WORKERS="${READ_WORKERS}" \
    MARKETFORGE_INSTANCE_ID="smoke-primary" \
    MARKETFORGE_ROOM_LEASE_DURATION_MS="${LEASE_DURATION_MS}" \
    MARKETFORGE_ROOM_LEASE_RENEW_INTERVAL_MS="${LEASE_RENEW_INTERVAL_MS}" \
    MARKETFORGE_RUNTIME_LOCK_WAIT_MS="0" \
    cargo run -p exchange-server >/tmp/marketforge-postgres-smoke.log 2>&1 &
  SERVER_PID="$!"
  wait_for_server
}

stop_server() {
  if [[ -n "${SERVER_PID}" ]] && kill -0 "${SERVER_PID}" 2>/dev/null; then
    kill "${SERVER_PID}" 2>/dev/null || true
    wait "${SERVER_PID}" 2>/dev/null || true
  fi
  SERVER_PID=""
}

start_standby() {
  MARKETFORGE_DATABASE_URL="${DATABASE_URL}" \
    MARKETFORGE_JOURNAL_READ_WORKERS="${READ_WORKERS}" \
    MARKETFORGE_INSTANCE_ID="smoke-standby" \
    MARKETFORGE_ROOM_LEASE_DURATION_MS="${LEASE_DURATION_MS}" \
    MARKETFORGE_ROOM_LEASE_RENEW_INTERVAL_MS="${LEASE_RENEW_INTERVAL_MS}" \
    MARKETFORGE_RUNTIME_LOCK_WAIT_MS="10000" \
    MARKETFORGE_BIND_ADDR="${STANDBY_BIND_ADDR}" \
    target/debug/exchange-server >/tmp/marketforge-postgres-standby.log 2>&1 &
  STANDBY_PID="$!"
  sleep 0.25
  if ! kill -0 "${STANDBY_PID}" 2>/dev/null; then
    echo "standby exited before the active server released its lock: $(cat /tmp/marketforge-postgres-standby.log)" >&2
    return 1
  fi
  if curl --max-time 0.2 -fsS "${STANDBY_BASE_URL}/health/ready" >/dev/null 2>&1; then
    echo "standby became ready while the active server still owned the database" >&2
    return 1
  fi
}

stop_standby() {
  if [[ -n "${STANDBY_PID}" ]] && kill -0 "${STANDBY_PID}" 2>/dev/null; then
    kill "${STANDBY_PID}" 2>/dev/null || true
    wait "${STANDBY_PID}" 2>/dev/null || true
  fi
  STANDBY_PID=""
}

create_room() {
  curl -fsS -X POST "${BASE_URL}/rooms" \
    -H 'content-type: application/json' \
    --data "{
      \"scenario\": {
        \"room_id\": \"${ROOM_ID}\",
        \"market\": {
          \"Spot\": {
            \"instrument\": {
              \"instrument_id\": \"V-BTC-SPOT\",
              \"venue_id\": \"default-venue\",
              \"symbol\": \"V-BTC-SPOT\",
              \"base_asset\": \"V\",
              \"quote_asset\": \"BTC\",
              \"tick_size\": 1,
              \"lot_size\": 1
            },
            \"clearing\": {\"maker_fee_ppm\": 0, \"taker_fee_ppm\": 0},
            \"risk\": {
              \"price_tick_size\": null,
              \"lot_size\": null,
              \"max_order_qty\": null,
              \"max_order_notional\": null,
              \"allow_short\": false
            }
          }
        },
        \"accounts\": [
          {\"Spot\": {\"account_id\": 10, \"cash_balance\": 10000, \"position_qty\": 120}},
          {\"Basic\": {\"account_id\": 20, \"cash_balance\": 10000}}
        ],
        \"seed_orders\": [
          {\"NewOrder\": {\"order_id\": 10000, \"account_id\": 10, \"side\": \"Sell\", \"kind\": {\"Limit\": {\"price_tick\": 104}}, \"qty\": 8, \"reduce_only\": false}}
        ]
      },
      \"agents\": [],
      \"autostart_agents\": false
    }" >/dev/null
}

submit_order() {
  curl -fsS -X POST "${BASE_URL}/rooms/${ROOM_ID}/orders" \
    -H 'content-type: application/json' \
    -H 'idempotency-key: smoke-order-1' \
    --data '{"participant_id":"human-smoke","account_id":20,"action":{"PlaceLimit":{"side":"Buy","price_tick":104,"qty":2}}}' >/dev/null
}

assert_projected_tables() {
  local counts
  counts="$(psql "${DATABASE_URL}" -Atc "
    SELECT
      (SELECT name FROM marketforge_schema_migrations WHERE version = 1),
      (SELECT name FROM marketforge_schema_migrations WHERE version = 2),
      (SELECT name FROM marketforge_schema_migrations WHERE version = 3),
      (SELECT name FROM marketforge_schema_migrations WHERE version = 4),
      (SELECT name FROM marketforge_schema_migrations WHERE version = 5),
      (SELECT name FROM marketforge_schema_migrations WHERE version = 6),
      (SELECT name FROM marketforge_schema_migrations WHERE version = 7),
      (SELECT name FROM marketforge_schema_migrations WHERE version = 8),
      (SELECT name FROM marketforge_schema_migrations WHERE version = 9),
      (SELECT name FROM marketforge_schema_migrations WHERE version = 10),
      (SELECT name FROM marketforge_schema_migrations WHERE version = 11),
      (SELECT name FROM marketforge_schema_migrations WHERE version = 12),
      (SELECT count(*) FROM marketforge_orders WHERE room_id = '${ROOM_ID}'),
      (SELECT count(*) FROM marketforge_trades WHERE room_id = '${ROOM_ID}'),
      (SELECT count(*) FROM marketforge_market_ticks WHERE room_id = '${ROOM_ID}'),
      (SELECT count(*) FROM marketforge_account_ledger WHERE room_id = '${ROOM_ID}'),
      (SELECT count(*) FROM marketforge_position_snapshots WHERE room_id = '${ROOM_ID}'),
      (SELECT count(*) FROM marketforge_executions WHERE room_id = '${ROOM_ID}' AND market_time_ms IS NOT NULL),
      (SELECT count(*) FROM marketforge_executions WHERE room_id = '${ROOM_ID}' AND idempotency_key = 'smoke-order-1'),
      (SELECT owner_id FROM marketforge_room_writer_leases WHERE room_id = '${ROOM_ID}'),
      (SELECT fencing_token FROM marketforge_room_writer_leases WHERE room_id = '${ROOM_ID}');
  ")"
  if [[ "${counts}" != "initial_schema|access_control|instrument_projection_scope|transfer_journal|margin_projection_fields|claim_unowned_legacy_rooms|room_mutation_journal|portfolio_margin_projection_fields|authoritative_market_time|order_request_idempotency|room_writer_leases|room_writer_owner_url|2|1|1|2|2|2|1|smoke-primary|1" ]]; then
    echo "expected current migrations and projection counts, got: ${counts}" >&2
    return 1
  fi
}

assert_room_lease_owned() {
  local base_url="${1:-${BASE_URL}}"
  local metrics room_count lease_count
  metrics="$(curl -fsS "${base_url}/metrics")"
  room_count="$(awk '$1 == "marketforge_rooms" { print $2 }' <<<"${metrics}")"
  lease_count="$(awk '$1 == "marketforge_room_writer_leases_owned" { print $2 }' <<<"${metrics}")"
  if [[ -z "${room_count}" || "${lease_count}" != "${room_count}" ]]; then
    echo "expected every loaded room to have a writer lease, got: ${metrics}" >&2
    return 1
  fi
  if ! grep -q '^marketforge_room_writer_leases_lost 0$' <<<"${metrics}"; then
    echo "expected no lost room writer leases, got: ${metrics}" >&2
    return 1
  fi
}

assert_query_api() {
  local trades ledger positions
  trades="$(curl -fsS "${BASE_URL}/rooms/${ROOM_ID}/trades?account_id=20&limit=10")"
  ledger="$(curl -fsS "${BASE_URL}/rooms/${ROOM_ID}/ledger?account_id=20&limit=10")"
  positions="$(curl -fsS "${BASE_URL}/rooms/${ROOM_ID}/positions?account_id=20&limit=10")"

  if ! grep -q '"taker_account_id":20' <<<"${trades}"; then
    echo "expected trade query API to include taker account 20, got: ${trades}" >&2
    return 1
  fi
  if ! grep -q '"cash_delta":-208' <<<"${ledger}"; then
    echo "expected ledger query API to include buyer cash delta -208, got: ${ledger}" >&2
    return 1
  fi
  if ! grep -q '"position_qty":2' <<<"${positions}"; then
    echo "expected position query API to include buyer position qty 2, got: ${positions}" >&2
    return 1
  fi
}

assert_user_isolation() {
  local status
  status="$(curl -sS -o /tmp/marketforge-forbidden.json -w '%{http_code}' \
    -H 'x-user-id: smoke-intruder' \
    "${BASE_URL}/rooms/${ROOM_ID}/trades?account_id=20&limit=10")"
  if [[ "${status}" != "403" ]]; then
    echo "expected intruder trade query to return 403, got ${status}: $(cat /tmp/marketforge-forbidden.json)" >&2
    return 1
  fi
}

assert_recovered_view() {
  local base_url="${1:-${BASE_URL}}"
  local view
  view="$(curl -fsS "${base_url}/rooms/${ROOM_ID}/view")"
  if ! grep -q '"price_tick":104,"qty":6' <<<"${view}"; then
    echo "expected recovered ask level qty 6, got: ${view}" >&2
    return 1
  fi
}

start_server
assert_operational_endpoints
assert_second_server_is_rejected
create_room
assert_room_lease_owned
submit_order
submit_order
assert_projected_tables
assert_query_api
assert_user_isolation
start_standby
stop_server
wait_for_server "${STANDBY_BASE_URL}"
assert_operational_endpoints "${STANDBY_BASE_URL}"
assert_room_lease_owned "${STANDBY_BASE_URL}"
assert_recovered_view "${STANDBY_BASE_URL}"
stop_standby
start_server
assert_operational_endpoints
assert_room_lease_owned
assert_recovered_view

echo "PostgreSQL journal smoke passed for ${ROOM_ID}"
