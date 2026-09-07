#!/usr/bin/env bash
# Declared-load continuous run. Default duration is 24h wall clock
# (MARKETFORGE_SOAK_SECONDS=86400). Operators may set a shorter duration.
# Logs commands, RSS, /metrics snapshots, and exceptions. Does not retry forever.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "${ROOT}"

SOAK_SECONDS="${MARKETFORGE_SOAK_SECONDS:-86400}"
BIND_ADDR="${MARKETFORGE_BIND_ADDR:-127.0.0.1:57318}"
BASE_URL="${MARKETFORGE_BASE_URL:-http://127.0.0.1:57318}"
OUT_DIR="${MARKETFORGE_SOAK_DIR:-${ROOT}/target/p6-soak}"
ROOMS="${MARKETFORGE_SOAK_ROOMS:-2}"
TICK_SECONDS="${MARKETFORGE_SOAK_TICK_SECONDS:-2}"
ROOM_PREFIX="${MARKETFORGE_SOAK_ROOM_PREFIX:-soak-room}"
SERVER_PID=""

mkdir -p "${OUT_DIR}"
COMMANDS="${OUT_DIR}/commands.jsonl"
RESOURCES="${OUT_DIR}/resources.csv"
EXCEPTIONS="${OUT_DIR}/exceptions.log"
METRICS="${OUT_DIR}/metrics.prom"
DECLARED="${OUT_DIR}/declared-load.txt"
STATUS="${OUT_DIR}/status.txt"

cleanup() {
  if [[ -n "${SERVER_PID}" ]] && kill -0 "${SERVER_PID}" 2>/dev/null; then
    kill "${SERVER_PID}" 2>/dev/null || true
    wait "${SERVER_PID}" 2>/dev/null || true
  fi
}
trap cleanup EXIT

{
  echo "declared_load_version=1"
  echo "duration_s=${SOAK_SECONDS}"
  echo "rooms=${ROOMS}"
  echo "room_prefix=${ROOM_PREFIX}"
  echo "agents_per_room=0"
  echo "tick_s=${TICK_SECONDS}"
  echo "orders_per_tick=1"
  echo "journal=memory_unless_MARKETFORGE_DATABASE_URL"
  echo "bind=${BIND_ADDR}"
} > "${DECLARED}"
echo "started $(date -u +%Y-%m-%dT%H:%M:%SZ)" > "${STATUS}"
echo "ts_unix,rss_kb,ready,rooms,queue_depth,errors" > "${RESOURCES}"
: > "${COMMANDS}"
: > "${EXCEPTIONS}"

cargo build -p exchange-server >/dev/null
MARKETFORGE_BIND_ADDR="${BIND_ADDR}" target/debug/exchange-server >"${OUT_DIR}/server.log" 2>&1 &
SERVER_PID="$!"
echo "${SERVER_PID}" > "${OUT_DIR}/server.pid"

for _ in $(seq 1 80); do
  if curl -fsS "${BASE_URL}/health/ready" >/dev/null 2>&1; then
    break
  fi
  if ! kill -0 "${SERVER_PID}" 2>/dev/null; then
    echo "server exited before ready" | tee -a "${EXCEPTIONS}"
    cat "${OUT_DIR}/server.log" >> "${EXCEPTIONS}"
    echo "failed $(date -u +%Y-%m-%dT%H:%M:%SZ)" >> "${STATUS}"
    exit 1
  fi
  sleep 0.25
done
curl -fsS "${BASE_URL}/health/ready" >/dev/null

create_room() {
  local id="$1"
  python3 - "${BASE_URL}" "${id}" <<'PY' >>"${COMMANDS}"
import json, sys, urllib.request, time
base, room = sys.argv[1], sys.argv[2]
body = {
  "room_id": room,
  "market": {"Spot": {"instrument": {"instrument_id": "V-BTC-SPOT", "venue_id": "default-venue", "symbol": "V-BTC-SPOT", "base_asset": "V", "quote_asset": "BTC", "tick_size": 1, "lot_size": 1}, "clearing": {"maker_fee_ppm": 0, "taker_fee_ppm": 0}, "risk": {"price_tick_size": None, "lot_size": None, "max_order_qty": None, "max_order_notional": None, "allow_short": True}}},
  "accounts": [{"Spot": {"account_id": 10, "cash_balance": 1000000, "position_qty": 100000}}, {"Basic": {"account_id": 20, "cash_balance": 1000000}}],
  "seed_orders": [{"NewOrder": {"order_id": 1, "account_id": 10, "side": "Sell", "kind": {"Limit": {"price_tick": 101}}, "qty": 100000, "reduce_only": False}}]
}
req = urllib.request.Request(base + "/rooms", data=json.dumps(body).encode(), method="POST", headers={"content-type": "application/json"})
with urllib.request.urlopen(req, timeout=10) as resp:
    print(json.dumps({"ts": time.time(), "op": "create", "room": room, "status": resp.status}))
PY
}

for i in $(seq 1 "${ROOMS}"); do
  create_room "${ROOM_PREFIX}-${i}" || { echo "create ${ROOM_PREFIX}-${i} failed" | tee -a "${EXCEPTIONS}"; echo "failed $(date -u +%Y-%m-%dT%H:%M:%SZ)" >> "${STATUS}"; exit 1; }
done

deadline=$(( $(date +%s) + SOAK_SECONDS ))
ticks=0
errors=0
while [[ "$(date +%s)" -lt "${deadline}" ]]; do
  ticks=$((ticks + 1))
  for i in $(seq 1 "${ROOMS}"); do
    if ! python3 - "${BASE_URL}" "${ROOM_PREFIX}-${i}" "${ticks}" >>"${COMMANDS}" 2>>"${EXCEPTIONS}" <<'PY'
import json, sys, urllib.request, time
base, room, tick = sys.argv[1], sys.argv[2], sys.argv[3]
body = {"participant_id": "soak", "account_id": 20, "action": {"PlaceLimit": {"side": "Buy", "price_tick": 101, "qty": 1}}}
req = urllib.request.Request(base + f"/rooms/{room}/orders", data=json.dumps(body).encode(), method="POST", headers={"content-type": "application/json"})
with urllib.request.urlopen(req, timeout=10) as resp:
    payload = json.loads(resp.read().decode())
    print(json.dumps({"ts": time.time(), "op": "order", "room": room, "tick": int(tick), "accepted": payload.get("accepted"), "command_seq": payload.get("command_seq")}))
PY
    then
      errors=$((errors + 1))
      echo "order failed room=${i} tick=${ticks}" >> "${EXCEPTIONS}"
      if [[ "${errors}" -ge 20 ]]; then
        echo "too many errors, stopping (no infinite retry)" | tee -a "${EXCEPTIONS}"
        echo "failed $(date -u +%Y-%m-%dT%H:%M:%SZ) errors=${errors}" >> "${STATUS}"
        exit 1
      fi
    fi
  done
  rss="NA"
  if [[ -r "/proc/${SERVER_PID}/status" ]]; then
    rss="$(awk '/VmRSS:/{print $2}' "/proc/${SERVER_PID}/status")"
  fi
  ready=0
  if curl -fsS "${BASE_URL}/health/ready" >/dev/null 2>&1; then
    ready=1
  else
    echo "readiness failed at tick ${ticks}" >> "${EXCEPTIONS}"
    echo "failed $(date -u +%Y-%m-%dT%H:%M:%SZ) ready=0" >> "${STATUS}"
    exit 1
  fi
  curl -fsS "${BASE_URL}/metrics" > "${METRICS}" || true
  queue="$(awk '/^marketforge_journal_queue_depth /{print $2}' "${METRICS}" 2>/dev/null || echo 0)"
  echo "$(date +%s),${rss},${ready},${ROOMS},${queue:-0},${errors}" >> "${RESOURCES}"
  sleep "${TICK_SECONDS}"
done

curl -fsS -X POST "${BASE_URL}/rooms/${ROOM_PREFIX}-1/pause" >/dev/null || true
curl -fsS -X POST "${BASE_URL}/rooms/${ROOM_PREFIX}-1/close" >/dev/null || true
echo "completed $(date -u +%Y-%m-%dT%H:%M:%SZ) ticks=${ticks} errors=${errors}" >> "${STATUS}"
echo "soak completed ticks=${ticks} errors=${errors} dir=${OUT_DIR}"
