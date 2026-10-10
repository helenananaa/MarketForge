#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "${ROOT}"
BASE_URL="${MARKETFORGE_BASE_URL:-http://127.0.0.1:57311}"
BIND_ADDR="${MARKETFORGE_BIND_ADDR:-127.0.0.1:57311}"
ROOM_ID="cli-smoke-$(date +%s)-$$"
SERVER_PID=""
LOG="${ROOT}/target/backend-training-smoke.log"

cleanup() {
  if [[ -n "${SERVER_PID}" ]] && kill -0 "${SERVER_PID}" 2>/dev/null; then
    kill "${SERVER_PID}" 2>/dev/null || true
    wait "${SERVER_PID}" 2>/dev/null || true
  fi
}
trap cleanup EXIT

wait_for_server() {
  for _ in $(seq 1 80); do
    if curl -fsS "${BASE_URL}/health/ready" >/dev/null 2>&1; then
      return 0
    fi
    sleep 0.25
  done
  echo "exchange-server did not become ready: $(cat "${LOG}")" >&2
  return 1
}

cargo build -p exchange-server -p marketforge-cli >/dev/null
MARKETFORGE_BIND_ADDR="${BIND_ADDR}" target/debug/exchange-server >"${LOG}" 2>&1 &
SERVER_PID="$!"
wait_for_server

CLI=(target/debug/marketforge --base-url "${BASE_URL}")

SCENARIO="$(mktemp)"
cat >"${SCENARIO}" <<EOF
{
  "scenario": {
    "room_id": "${ROOM_ID}",
    "market": {
      "Spot": {
        "instrument": {
          "instrument_id": "V-BTC-SPOT",
          "venue_id": "default-venue",
          "symbol": "V-BTC-SPOT",
          "base_asset": "V",
          "quote_asset": "BTC",
          "tick_size": 1,
          "lot_size": 1
        },
        "clearing": {"maker_fee_ppm": 0, "taker_fee_ppm": 0},
        "risk": {
          "price_tick_size": null,
          "lot_size": null,
          "max_order_qty": null,
          "max_order_notional": null,
          "allow_short": true
        }
      }
    },
    "accounts": [
      {"Spot": {"account_id": 10, "cash_balance": 10000, "position_qty": 50}},
      {"Basic": {"account_id": 20, "cash_balance": 10000}}
    ],
    "seed_orders": [
      {"NewOrder": {"order_id": 1000, "account_id": 10, "side": "Sell", "kind": {"Limit": {"price_tick": 100}}, "qty": 5, "reduce_only": false}}
    ]
  },
  "agents": [],
  "autostart_agents": false
}
EOF

create_out="$("${CLI[@]}" room create "${SCENARIO}")"
echo "${create_out}" | grep -q "${ROOM_ID}"
rm -f "${SCENARIO}"

order_out="$("${CLI[@]}" --idempotency-key smoke-1 order submit "${ROOM_ID}" 20 buy 100 2)"
echo "${order_out}" | grep -q '"accepted": true'
retry_out="$("${CLI[@]}" --idempotency-key smoke-1 order submit "${ROOM_ID}" 20 buy 100 2)"
echo "${retry_out}" | grep -q '"accepted": true'

resting="$("${CLI[@]}" order submit "${ROOM_ID}" 20 buy 90 1)"
echo "${resting}"
order_id="$(python3 - <<PY
import json,re,sys
text='''${resting}'''
data=json.loads(text)
ids=[]
def walk(v):
    if isinstance(v, dict):
        if "order_id" in v: ids.append(v["order_id"])
        for x in v.values(): walk(x)
    elif isinstance(v, list):
        for x in v: walk(x)
walk(data)
print(ids[-1])
PY
)"
cancel_out="$("${CLI[@]}" order cancel "${ROOM_ID}" 20 "${order_id}")"
echo "${cancel_out}" | grep -qE "OrderCanceled|accepted"

ticker="$("${CLI[@]}" ticker "${ROOM_ID}")"
echo "${ticker}" | grep -q last_trade_tick
clock_before="$("${CLI[@]}" clock get "${ROOM_ID}")"
candles="$("${CLI[@]}" candles "${ROOM_ID}" 1000)"
clock_after="$("${CLI[@]}" clock get "${ROOM_ID}")"
echo "${clock_before}" > /tmp/mf-clock-before.json
echo "${clock_after}" > /tmp/mf-clock-after.json
python3 - <<'PY'
import json
before=json.load(open("/tmp/mf-clock-before.json"))
after=json.load(open("/tmp/mf-clock-after.json"))
assert before["clock"]==after["clock"], (before, after)
PY

pause="$("${CLI[@]}" room pause "${ROOM_ID}")"
echo "${pause}" | grep -q Paused
close="$("${CLI[@]}" room close "${ROOM_ID}")"
echo "${close}" | grep -q Closed
echo "backend training smoke passed for ${ROOM_ID}"
