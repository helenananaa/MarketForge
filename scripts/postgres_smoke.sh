#!/usr/bin/env bash
set -euo pipefail

DATABASE_URL="${MARKETFORGE_DATABASE_URL:-postgres://marketforge:marketforge@127.0.0.1:55432/marketforge}"
BASE_URL="${MARKETFORGE_BASE_URL:-http://127.0.0.1:57305}"
ROOM_ID="pg-smoke-$(date +%s)-$$"
SERVER_PID=""

cleanup() {
  if [[ -n "${SERVER_PID}" ]] && kill -0 "${SERVER_PID}" 2>/dev/null; then
    kill "${SERVER_PID}" 2>/dev/null || true
    wait "${SERVER_PID}" 2>/dev/null || true
  fi
}
trap cleanup EXIT

wait_for_server() {
  for _ in $(seq 1 80); do
    if curl -fsS "${BASE_URL}/health" >/dev/null 2>&1; then
      return 0
    fi
    sleep 0.25
  done
  echo "exchange-server did not become healthy at ${BASE_URL}" >&2
  return 1
}

start_server() {
  MARKETFORGE_DATABASE_URL="${DATABASE_URL}" cargo run -p exchange-server >/tmp/marketforge-postgres-smoke.log 2>&1 &
  SERVER_PID="$!"
  wait_for_server
}

stop_server() {
  cleanup
  SERVER_PID=""
}

create_room() {
  curl -fsS -X POST "${BASE_URL}/rooms" \
    -H 'content-type: application/json' \
    --data "{
      \"scenario\": {
        \"room_id\": \"${ROOM_ID}\",
        \"market\": {
          \"Spot\": {
            \"instrument\": {\"symbol\": \"V-BTC-SPOT\", \"tick_size\": 1, \"lot_size\": 1},
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
          {\"NewOrder\": {\"order_id\": 10000, \"account_id\": 10, \"side\": \"Sell\", \"kind\": {\"Limit\": {\"price_tick\": 104}}, \"qty\": 8}}
        ]
      },
      \"agents\": [],
      \"autostart_agents\": false
    }" >/dev/null
}

submit_order() {
  curl -fsS -X POST "${BASE_URL}/rooms/${ROOM_ID}/orders" \
    -H 'content-type: application/json' \
    --data '{"participant_id":"human-smoke","account_id":20,"action":{"PlaceLimit":{"side":"Buy","price_tick":104,"qty":2}}}' >/dev/null
}

assert_projected_tables() {
  local counts
  counts="$(psql "${DATABASE_URL}" -Atc "
    SELECT
      (SELECT name FROM marketforge_schema_migrations WHERE version = 1),
      (SELECT name FROM marketforge_schema_migrations WHERE version = 2),
      (SELECT count(*) FROM marketforge_orders WHERE room_id = '${ROOM_ID}'),
      (SELECT count(*) FROM marketforge_trades WHERE room_id = '${ROOM_ID}'),
      (SELECT count(*) FROM marketforge_market_ticks WHERE room_id = '${ROOM_ID}'),
      (SELECT count(*) FROM marketforge_account_ledger WHERE room_id = '${ROOM_ID}'),
      (SELECT count(*) FROM marketforge_position_snapshots WHERE room_id = '${ROOM_ID}');
  ")"
  if [[ "${counts}" != "initial_schema|access_control|2|1|1|2|2" ]]; then
    echo "expected migration/order/trade/tick/ledger/position counts initial_schema|access_control|2|1|1|2|2, got: ${counts}" >&2
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
  local view
  view="$(curl -fsS "${BASE_URL}/rooms/${ROOM_ID}/view")"
  if ! grep -q '"price_tick":104,"qty":6' <<<"${view}"; then
    echo "expected recovered ask level qty 6, got: ${view}" >&2
    return 1
  fi
}

start_server
create_room
submit_order
assert_projected_tables
assert_query_api
assert_user_isolation
stop_server
start_server
assert_recovered_view

echo "PostgreSQL journal smoke passed for ${ROOM_ID}"
