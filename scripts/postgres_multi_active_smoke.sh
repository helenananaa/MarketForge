#!/usr/bin/env bash
set -euo pipefail

DATABASE_URL="${MARKETFORGE_DATABASE_URL:-postgres://marketforge:marketforge@127.0.0.1:55432/marketforge}"
READ_WORKERS="${MARKETFORGE_JOURNAL_READ_WORKERS:-2}"
LEASE_DURATION_MS="${MARKETFORGE_ROOM_LEASE_DURATION_MS:-2000}"
LEASE_RENEW_INTERVAL_MS="${MARKETFORGE_ROOM_LEASE_RENEW_INTERVAL_MS:-500}"
A_BIND_ADDR="${MARKETFORGE_MULTI_A_BIND_ADDR:-127.0.0.1:57307}"
B_BIND_ADDR="${MARKETFORGE_MULTI_B_BIND_ADDR:-127.0.0.1:57308}"
A_BASE_URL="http://${A_BIND_ADDR}"
B_BASE_URL="http://${B_BIND_ADDR}"
ROOM_A="pg-multi-a-$(date +%s)-$$"
ROOM_B="pg-multi-b-$(date +%s)-$$"
A_LOG="/tmp/marketforge-multi-a-$$.log"
B_LOG="/tmp/marketforge-multi-b-$$.log"
CONFLICT_BODY="/tmp/marketforge-multi-conflict-$$.json"
MIXED_MODE_LOG="/tmp/marketforge-multi-mixed-mode-$$.log"
A_PID=""
B_PID=""

cleanup() {
  if [[ -n "${B_PID}" ]] && kill -0 "${B_PID}" 2>/dev/null; then
    kill "${B_PID}" 2>/dev/null || true
    wait "${B_PID}" 2>/dev/null || true
  fi
  if [[ -n "${A_PID}" ]] && kill -0 "${A_PID}" 2>/dev/null; then
    kill "${A_PID}" 2>/dev/null || true
    wait "${A_PID}" 2>/dev/null || true
  fi
  psql "${DATABASE_URL}" -v ON_ERROR_STOP=1 -c \
    "DELETE FROM marketforge_rooms WHERE room_id IN ('${ROOM_A}', '${ROOM_B}')" \
    >/dev/null 2>&1 || true
}
trap cleanup EXIT

wait_for_server() {
  local base_url="$1"
  local log_file="$2"
  for _ in $(seq 1 80); do
    if curl -fsS "${base_url}/health/ready" >/dev/null 2>&1; then
      return 0
    fi
    sleep 0.25
  done
  echo "exchange-server did not become ready at ${base_url}: $(cat "${log_file}")" >&2
  return 1
}

start_a() {
  MARKETFORGE_DATABASE_URL="${DATABASE_URL}" \
    MARKETFORGE_JOURNAL_READ_WORKERS="${READ_WORKERS}" \
    MARKETFORGE_RUNTIME_MODE="room-leased" \
    MARKETFORGE_INSTANCE_ID="multi-a" \
    MARKETFORGE_ADVERTISE_URL="${A_BASE_URL}" \
    MARKETFORGE_ROOM_LEASE_DURATION_MS="${LEASE_DURATION_MS}" \
    MARKETFORGE_ROOM_LEASE_RENEW_INTERVAL_MS="${LEASE_RENEW_INTERVAL_MS}" \
    MARKETFORGE_RUNTIME_LOCK_WAIT_MS="0" \
    MARKETFORGE_BIND_ADDR="${A_BIND_ADDR}" \
    target/debug/exchange-server >"${A_LOG}" 2>&1 &
  A_PID="$!"
  wait_for_server "${A_BASE_URL}" "${A_LOG}"
}

start_b() {
  MARKETFORGE_DATABASE_URL="${DATABASE_URL}" \
    MARKETFORGE_JOURNAL_READ_WORKERS="${READ_WORKERS}" \
    MARKETFORGE_RUNTIME_MODE="room-leased" \
    MARKETFORGE_INSTANCE_ID="multi-b" \
    MARKETFORGE_ADVERTISE_URL="${B_BASE_URL}" \
    MARKETFORGE_ROOM_LEASE_DURATION_MS="${LEASE_DURATION_MS}" \
    MARKETFORGE_ROOM_LEASE_RENEW_INTERVAL_MS="${LEASE_RENEW_INTERVAL_MS}" \
    MARKETFORGE_RUNTIME_LOCK_WAIT_MS="0" \
    MARKETFORGE_BIND_ADDR="${B_BIND_ADDR}" \
    target/debug/exchange-server >"${B_LOG}" 2>&1 &
  B_PID="$!"
  wait_for_server "${B_BASE_URL}" "${B_LOG}"
}

stop_a() {
  if [[ -n "${A_PID}" ]] && kill -0 "${A_PID}" 2>/dev/null; then
    kill "${A_PID}"
    wait "${A_PID}"
  fi
  A_PID=""
}

stop_b() {
  if [[ -n "${B_PID}" ]] && kill -0 "${B_PID}" 2>/dev/null; then
    kill "${B_PID}"
    wait "${B_PID}"
  fi
  B_PID=""
}

create_room() {
  local base_url="$1"
  local room_id="$2"
  curl -fsS -X POST "${base_url}/rooms" \
    -H 'content-type: application/json' \
    --data "{
      \"scenario\": {
        \"room_id\": \"${room_id}\",
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
  local base_url="$1"
  local room_id="$2"
  local request_id="$3"
  local qty="$4"
  curl -fsS -X POST "${base_url}/rooms/${room_id}/orders" \
    -H 'content-type: application/json' \
    -H "idempotency-key: ${request_id}" \
    --data "{\"participant_id\":\"multi-smoke\",\"account_id\":20,\"action\":{\"PlaceLimit\":{\"side\":\"Buy\",\"price_tick\":104,\"qty\":${qty}}}}" >/dev/null
}

assert_non_owner_conflict() {
  local base_url="$1"
  local room_id="$2"
  local expected_owner="$3"
  local expected_owner_url="$4"
  local status
  status="$(curl -sS -o "${CONFLICT_BODY}" -w '%{http_code}' "${base_url}/rooms/${room_id}/view")"
  if [[ "${status}" != "409" ]] \
    || ! grep -q '"code":"room_owned_by_other_instance"' "${CONFLICT_BODY}" \
    || ! grep -q "\"owner_id\":\"${expected_owner}\"" "${CONFLICT_BODY}" \
    || ! grep -q "\"owner_url\":\"${expected_owner_url}\"" "${CONFLICT_BODY}"; then
    echo "expected room ${room_id} to return owner ${expected_owner} at ${expected_owner_url}, got ${status}: $(cat "${CONFLICT_BODY}")" >&2
    return 1
  fi
}

assert_owner_discovery() {
  local base_url="$1"
  local room_id="$2"
  local expected_owner="$3"
  local expected_owner_url="$4"
  local expected_token="$5"
  local owner
  owner="$(curl -fsS "${base_url}/rooms/${room_id}/owner")"
  if ! grep -q "\"owner_id\":\"${expected_owner}\"" <<<"${owner}" \
    || ! grep -q "\"owner_url\":\"${expected_owner_url}\"" <<<"${owner}" \
    || ! grep -q "\"fencing_token\":${expected_token}" <<<"${owner}"; then
    echo "unexpected owner discovery for ${room_id}: ${owner}" >&2
    return 1
  fi
}

assert_cluster_route() {
  local base_url="$1"
  local room_id="$2"
  local expected_owner="$3"
  local expected_owner_url="$4"
  local directory
  directory="$(curl -fsS "${base_url}/cluster/rooms?limit=500")"
  if ! grep -Fq "\"room_id\":\"${room_id}\",\"owner\":{\"room_id\":\"${room_id}\",\"owner_id\":\"${expected_owner}\",\"owner_url\":\"${expected_owner_url}\"" <<<"${directory}"; then
    echo "cluster directory at ${base_url} did not route ${room_id} to ${expected_owner} at ${expected_owner_url}: ${directory}" >&2
    return 1
  fi
}

assert_single_active_mode_is_rejected() {
  local status
  if timeout --kill-after=1s 5s env \
    MARKETFORGE_DATABASE_URL="${DATABASE_URL}" \
    MARKETFORGE_JOURNAL_READ_WORKERS="${READ_WORKERS}" \
    MARKETFORGE_RUNTIME_MODE="single-active" \
    MARKETFORGE_RUNTIME_LOCK_WAIT_MS="0" \
    MARKETFORGE_BIND_ADDR="127.0.0.1:57309" \
    target/debug/exchange-server >"${MIXED_MODE_LOG}" 2>&1; then
    echo "expected single-active mode to conflict with room-leased processes" >&2
    return 1
  else
    status="$?"
  fi
  if [[ "${status}" == "124" || "${status}" == "137" ]]; then
    echo "single-active process stayed alive alongside room-leased processes" >&2
    return 1
  fi
  if ! grep -q 'already owns the PostgreSQL runtime lock' "${MIXED_MODE_LOG}"; then
    echo "mixed runtime mode failed for an unexpected reason: $(cat "${MIXED_MODE_LOG}")" >&2
    return 1
  fi
}

wait_for_takeover() {
  local base_url="$1"
  local room_id="$2"
  local output_file="/tmp/marketforge-multi-takeover-$$.json"
  local status
  for _ in $(seq 1 80); do
    status="$(curl -sS -o "${output_file}" -w '%{http_code}' "${base_url}/rooms/${room_id}/view")"
    if [[ "${status}" == "200" ]]; then
      if ! grep -q '"price_tick":104,"qty":6' "${output_file}"; then
        echo "takeover recovered unexpected room state: $(cat "${output_file}")" >&2
        return 1
      fi
      return 0
    fi
    sleep 0.1
  done
  echo "room ${room_id} was not taken over at ${base_url}: $(cat "${output_file}")" >&2
  return 1
}

assert_owner_and_token() {
  local room_id="$1"
  local expected="$2"
  local actual
  actual="$(psql "${DATABASE_URL}" -Atc "
    SELECT owner_id || '|' || owner_url || '|' || fencing_token
    FROM marketforge_room_writer_leases
    WHERE room_id = '${room_id}'
      AND lease_expires_at > clock_timestamp();
  ")"
  if [[ "${actual}" != "${expected}" ]]; then
    echo "expected active lease ${expected} for ${room_id}, got ${actual}" >&2
    return 1
  fi
}

cargo build -p exchange-server
start_a
start_b
assert_single_active_mode_is_rejected

create_room "${A_BASE_URL}" "${ROOM_A}"
submit_order "${A_BASE_URL}" "${ROOM_A}" "multi-a-order-1" 2
assert_non_owner_conflict "${B_BASE_URL}" "${ROOM_A}" "multi-a" "${A_BASE_URL}"
assert_owner_discovery "${B_BASE_URL}" "${ROOM_A}" "multi-a" "${A_BASE_URL}" 1
assert_cluster_route "${B_BASE_URL}" "${ROOM_A}" "multi-a" "${A_BASE_URL}"
assert_owner_and_token "${ROOM_A}" "multi-a|${A_BASE_URL}|1"

stop_a
wait_for_takeover "${B_BASE_URL}" "${ROOM_A}"
assert_owner_discovery "${B_BASE_URL}" "${ROOM_A}" "multi-b" "${B_BASE_URL}" 2
assert_owner_and_token "${ROOM_A}" "multi-b|${B_BASE_URL}|2"
submit_order "${B_BASE_URL}" "${ROOM_A}" "multi-b-order-2" 1

start_a
create_room "${A_BASE_URL}" "${ROOM_B}"
assert_non_owner_conflict "${A_BASE_URL}" "${ROOM_A}" "multi-b" "${B_BASE_URL}"
assert_non_owner_conflict "${B_BASE_URL}" "${ROOM_B}" "multi-a" "${A_BASE_URL}"
assert_owner_discovery "${A_BASE_URL}" "${ROOM_A}" "multi-b" "${B_BASE_URL}" 2
assert_owner_discovery "${B_BASE_URL}" "${ROOM_B}" "multi-a" "${A_BASE_URL}" 1
assert_cluster_route "${A_BASE_URL}" "${ROOM_A}" "multi-b" "${B_BASE_URL}"
assert_cluster_route "${A_BASE_URL}" "${ROOM_B}" "multi-a" "${A_BASE_URL}"
assert_cluster_route "${B_BASE_URL}" "${ROOM_A}" "multi-b" "${B_BASE_URL}"
assert_cluster_route "${B_BASE_URL}" "${ROOM_B}" "multi-a" "${A_BASE_URL}"
curl -fsS "${A_BASE_URL}/health/ready" >/dev/null
curl -fsS "${B_BASE_URL}/health/ready" >/dev/null

stop_a
stop_b
active_leases="$(psql "${DATABASE_URL}" -Atc "
  SELECT count(*)
  FROM marketforge_room_writer_leases
  WHERE room_id IN ('${ROOM_A}', '${ROOM_B}')
    AND lease_expires_at > clock_timestamp();
")"
if [[ "${active_leases}" != "0" ]]; then
  echo "expected graceful shutdown to release both smoke leases, got ${active_leases}" >&2
  exit 1
fi

echo "PostgreSQL multi-active smoke passed for ${ROOM_A} and ${ROOM_B}"
