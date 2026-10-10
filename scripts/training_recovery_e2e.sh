#!/usr/bin/env bash
# F4 training recovery e2e: Bearer HTTP + PostgreSQL, four paths.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "${ROOT}"

DATABASE_URL="${MARKETFORGE_DATABASE_URL:-${MARKETFORGE_TEST_DATABASE_URL:?set MARKETFORGE_TEST_DATABASE_URL}}"
AUTH_JSON='{"admin-token":"admin","trainee-token":"trainee"}'
BIND_A="${MARKETFORGE_E2E_BIND_A:-127.0.0.1:57421}"
BIND_B="${MARKETFORGE_E2E_BIND_B:-127.0.0.1:57422}"
BASE_A="http://${BIND_A}"
BASE_B="http://${BIND_B}"
LOG_DIR="${ROOT}/target/training-recovery-e2e"
mkdir -p "${LOG_DIR}"
A_PID=""
B_PID=""
PASS=0
SUFFIX="${MARKETFORGE_E2E_SUFFIX:-$(date +%s)}"

cleanup() {
  if [[ -n "${B_PID}" ]] && kill -0 "${B_PID}" 2>/dev/null; then
    kill "${B_PID}" 2>/dev/null || true
    wait "${B_PID}" 2>/dev/null || true
  fi
  if [[ -n "${A_PID}" ]] && kill -0 "${A_PID}" 2>/dev/null; then
    kill "${A_PID}" 2>/dev/null || true
    wait "${A_PID}" 2>/dev/null || true
  fi
}
trap cleanup EXIT

reap_server() {
  local pid="$1"
  local sig="${2:-TERM}"
  if [[ -n "${pid}" ]] && kill -0 "${pid}" 2>/dev/null; then
    kill -"${sig}" "${pid}" 2>/dev/null || true
    wait "${pid}" 2>/dev/null || true
  fi
  # Session advisory locks drop with the backend; wait so the next instance
  # does not see a stale exclusive runtime lock.
  sleep 0.5
}

wait_ready() {
  local url="$1" log="$2"
  for _ in $(seq 1 80); do
    if curl -fsS -H 'Authorization: Bearer admin-token' "${url}/health/ready" >/dev/null 2>&1; then
      return 0
    fi
    sleep 0.25
  done
  echo "server not ready at ${url}: $(cat "${log}")" >&2
  return 1
}

start_server() {
  local bind="$1" log="$2" instance="$3" mode="$4" advertise="$5"
  local lock_wait="0"
  if [[ "${mode}" == "single-active" ]]; then
    lock_wait="${MARKETFORGE_E2E_LOCK_WAIT_MS:-3000}"
  fi
  MARKETFORGE_DATABASE_URL="${DATABASE_URL}" \
    MARKETFORGE_AUTH_TOKENS_JSON="${AUTH_JSON}" \
    MARKETFORGE_BIND_ADDR="${bind}" \
    MARKETFORGE_INSTANCE_ID="${instance}" \
    MARKETFORGE_ADVERTISE_URL="${advertise}" \
    MARKETFORGE_RUNTIME_MODE="${mode}" \
    MARKETFORGE_RUNTIME_LOCK_WAIT_MS="${lock_wait}" \
    MARKETFORGE_ROOM_LEASE_DURATION_MS="${MARKETFORGE_ROOM_LEASE_DURATION_MS:-2000}" \
    MARKETFORGE_ROOM_LEASE_RENEW_INTERVAL_MS="${MARKETFORGE_ROOM_LEASE_RENEW_INTERVAL_MS:-500}" \
    MARKETFORGE_JOURNAL_READ_WORKERS="${MARKETFORGE_JOURNAL_READ_WORKERS:-2}" \
    target/debug/exchange-server >"${log}" 2>&1 &
  echo $!
}

req() {
  local token="$1" url="$2"; shift 2
  local tmp
  tmp="$(mktemp)"
  local code
  code="$(curl -sS -o "${tmp}" -w '%{http_code}' -H "Authorization: Bearer ${token}" -H 'content-type: application/json' "$@" "${url}")"
  if [[ "${code}" != 2* ]]; then
    echo "HTTP ${code} ${url}: $(cat "${tmp}")" >&2
    rm -f "${tmp}"
    return 1
  fi
  cat "${tmp}"
  rm -f "${tmp}"
}

admin() {
  local url="$1"; shift
  req admin-token "${url}" "$@"
}

trainee() {
  local url="$1"; shift
  req trainee-token "${url}" "$@"
}

scenario_json() {
  local room="$1"
  cat <<EOF
{
  "room_id": "${room}",
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
    {"Spot": {"account_id": 20, "cash_balance": 10000, "position_qty": 0}}
  ],
  "seed_orders": [
    {"NewOrder": {"order_id": 1, "account_id": 10, "side": "Buy", "kind": {"Limit": {"price_tick": 99}}, "qty": 5, "reduce_only": false}},
    {"NewOrder": {"order_id": 2, "account_id": 10, "side": "Sell", "kind": {"Limit": {"price_tick": 101}}, "qty": 5, "reduce_only": false}}
  ]
}
EOF
}

start_training_payload() {
  local run="$1" room="$2" horizon="$3"
  # Every path advances the clock explicitly. Keep the short horizon from
  # expiring on the realtime worker before HTTP orders and restart assertions.
  python3 - <<PY
import json
scenario = json.loads('''$(scenario_json "${room}")''')
print(json.dumps({
  "run_id": "${run}",
  "scenario": scenario,
  "agents": [{
    "GridTrader": {
      "participant": {
        "participant_id": "grid",
        "kind": "RuleAgent",
        "room_id": "${room}",
        "account_id": 10,
        "instrument_id": "V-BTC-SPOT"
      },
      "center_price_tick": 100,
      "grid_spacing_ticks": 2,
      "levels": 1,
      "qty_per_level": 1
    }
  }],
  "trainee_account_id": 20,
  "target_qty": 2,
  "horizon_steps": ${horizon},
  "manual_agents": True,
}))
PY
}

assert_terminal() {
  local url="$1" run="$2" room="$3"
  python3 - <<PY
import json, sys, urllib.error, urllib.request
url = "${url}"
run = "${run}"
room = "${room}"
headers = {"Authorization": "Bearer admin-token"}

def get(path):
    req = urllib.request.Request(url + path, headers=headers)
    with urllib.request.urlopen(req) as resp:
        return json.load(resp)

result = get(f"/training/runs/{run}/result")
report = get(f"/training/runs/{run}/report")
orders = get(f"/rooms/{room}/orders")
events = get(f"/rooms/{room}/events?from_start=true&limit=200")
trades = get(f"/rooms/{room}/trades")
status = result["run"]["status"]
if status not in ("Completed", "Aborted"):
    raise SystemExit(f"expected terminal training status, got {status}")
open_trainee = [
    o for o in orders.get("orders", [])
    if o.get("account_id") == 20 and str(o.get("status", "")).lower() in ("open", "resting", "accepted")
]
if open_trainee:
    raise SystemExit(f"residual fillable trainee orders: {open_trainee}")
fills = result["run"].get("fills") or []
for fill in fills:
    if fill.get("book_before") is None:
        raise SystemExit(f"fill missing book_before: {fill}")
    if fill.get("order_id") is None:
        raise SystemExit(f"fill missing order_id: {fill}")
score = result.get("score") or {}
report_json = report.get("json") or {}
report_score = report_json.get("metrics") or report_json.get("score") or report.get("score") or {}
if report_score.get("q") != score.get("q"):
    raise SystemExit(f"report score q {report_score.get('q')} != result score q {score.get('q')}")
if report_score.get("fees_paid") != score.get("fees_paid"):
    raise SystemExit(f"report fees {report_score.get('fees_paid')} != result fees {score.get('fees_paid')}")

def journal_trainee_fills():
    q = 0
    notional = 0
    rows = list(trades.get("trades") or [])
    if not rows:
        for execution in events.get("executions") or []:
            for event in execution.get("events") or []:
                if not isinstance(event, dict):
                    continue
                kind = event.get("type") or next(iter(event.keys()), None)
                body = event if event.get("type") else event.get("TradePrinted")
                if kind not in ("TradePrinted", None) and "TradePrinted" not in event:
                    continue
                if not isinstance(body, dict):
                    continue
                maker = body.get("maker_account_id")
                taker = body.get("taker_account_id")
                if 20 not in (maker, taker):
                    continue
                q += int(body.get("qty") or 0)
                notional += int(body.get("price_tick") or 0) * int(body.get("qty") or 0)
        return q, notional
    for trade in rows:
        maker = trade.get("maker_account_id")
        taker = trade.get("taker_account_id")
        if 20 not in (maker, taker):
            continue
        qty = int(trade.get("qty") or 0)
        px = int(trade.get("price_tick") or 0)
        q += qty
        notional += px * qty
    return q, notional

journal_q, journal_notional = journal_trainee_fills()
if journal_q != int(score.get("q") or 0):
    raise SystemExit(f"journal trainee q {journal_q} != score q {score.get('q')}")
if journal_q:
    ref = int(result["run"]["spec"]["reference_price_tick"])
    vwap_num = journal_notional
    vwap_den = journal_q
    slip = 10000 * (vwap_num - ref * vwap_den) // (ref * vwap_den)
    if score.get("vwap_tick_num") not in (vwap_num, str(vwap_num)):
        raise SystemExit(f"journal vwap num {vwap_num} != score {score.get('vwap_tick_num')}")
    if score.get("buy_slippage_bp") not in (slip, str(slip)):
        raise SystemExit(f"journal slippage {slip} != score {score.get('buy_slippage_bp')}")

frozen_q = score.get("q")
req = urllib.request.Request(
    url + f"/rooms/{room}/orders",
    data=json.dumps({
        "participant_id": "trainee-late",
        "account_id": 20,
        "action": {"PlaceLimit": {"side": "Buy", "price_tick": 101, "qty": 1}},
    }).encode(),
    headers={**headers, "content-type": "application/json"},
    method="POST",
)
try:
    with urllib.request.urlopen(req) as resp:
        late = json.load(resp)
    late_status = str(late).lower()
    accepted = "orderaccepted" in late_status.replace("_", "") or '"accepted": true' in late_status
    rejected = "reject" in late_status
    if accepted and not rejected:
        raise SystemExit(f"post-end trainee order was accepted: {late}")
except urllib.error.HTTPError as exc:
    if exc.code not in (400, 403, 409, 429):
        raise SystemExit(f"post-end order unexpected HTTP {exc.code}: {exc.read().decode()}")
after = get(f"/training/runs/{run}/result")
if (after.get("score") or {}).get("q") != frozen_q:
    raise SystemExit(f"score changed after terminal order: {frozen_q} -> {(after.get('score') or {}).get('q')}")

print(json.dumps({
    "run_id": run,
    "room_id": room,
    "status": status,
    "filled_qty": result["run"].get("filled_qty"),
    "fees_paid": result["run"].get("fees_paid"),
    "score_q": score.get("q"),
    "report_q": report_score.get("q"),
    "journal_q": journal_q,
    "fills": len(fills),
    "open_trainee_orders": 0,
    "book_before_bound": True,
    "score_frozen": True,
    "report_matches_score": True,
}, indent=2))
PY
}

drive_to_terminal() {
  local url="$1" run="$2" room="$3"
  trainee "${url}/rooms/${room}/members" --data '{"user_id":"admin","role":"admin"}' >/dev/null
  trainee "${url}/rooms/${room}/orders" --data '{
    "participant_id": "trainee",
    "account_id": 20,
    "action": {"PlaceLimit": {"side": "Buy", "price_tick": 101, "qty": 1}}
  }' >/dev/null
  trainee "${url}/rooms/${room}/orders" --data '{
    "participant_id": "trainee",
    "account_id": 20,
    "action": {"PlaceLimit": {"side": "Buy", "price_tick": 90, "qty": 1}}
  }' >/dev/null
  admin "${url}/rooms/${room}/clock/advance" --data '{"steps":3}' >/dev/null
  assert_terminal "${url}" "${run}" "${room}"
}

cargo build -p exchange-server >/dev/null

echo "== path 1 continuous =="
A_PID="$(start_server "${BIND_A}" "${LOG_DIR}/continuous.log" "e2e-a" "single-active" "${BASE_A}")"
wait_ready "${BASE_A}" "${LOG_DIR}/continuous.log"
trainee "${BASE_A}/training/runs" --data "$(start_training_payload "e2e-cont-${SUFFIX}" "e2e-cont-${SUFFIX}" 3)" >/dev/null
drive_to_terminal "${BASE_A}" "e2e-cont-${SUFFIX}" "e2e-cont-${SUFFIX}"
reap_server "${A_PID}"; A_PID=""
PASS=$((PASS + 1))

echo "== path 2 restart =="
A_PID="$(start_server "${BIND_A}" "${LOG_DIR}/restart-1.log" "e2e-restart" "single-active" "${BASE_A}")"
wait_ready "${BASE_A}" "${LOG_DIR}/restart-1.log"
trainee "${BASE_A}/training/runs" --data "$(start_training_payload "e2e-restart-${SUFFIX}" "e2e-restart-${SUFFIX}" 3)" >/dev/null
trainee "${BASE_A}/rooms/e2e-restart-${SUFFIX}/members" --data '{"user_id":"admin","role":"admin"}' >/dev/null
trainee "${BASE_A}/rooms/e2e-restart-${SUFFIX}/orders" --data '{
  "participant_id": "trainee",
  "account_id": 20,
  "action": {"PlaceLimit": {"side": "Buy", "price_tick": 101, "qty": 1}}
}' >/dev/null
trainee "${BASE_A}/rooms/e2e-restart-${SUFFIX}/orders" --data '{
  "participant_id": "trainee",
  "account_id": 20,
  "action": {"PlaceLimit": {"side": "Buy", "price_tick": 90, "qty": 1}}
}' >/dev/null
reap_server "${A_PID}" TERM; A_PID=""
A_PID="$(start_server "${BIND_A}" "${LOG_DIR}/restart-2.log" "e2e-restart" "single-active" "${BASE_A}")"
wait_ready "${BASE_A}" "${LOG_DIR}/restart-2.log"
admin "${BASE_A}/rooms/e2e-restart-${SUFFIX}/clock/advance" --data '{"steps":3}' >/dev/null
assert_terminal "${BASE_A}" "e2e-restart-${SUFFIX}" "e2e-restart-${SUFFIX}"
reap_server "${A_PID}"; A_PID=""
PASS=$((PASS + 1))

echo "== path 3 takeover =="
A_PID="$(start_server "${BIND_A}" "${LOG_DIR}/takeover-a.log" "e2e-take-a" "room-leased" "${BASE_A}")"
wait_ready "${BASE_A}" "${LOG_DIR}/takeover-a.log"
trainee "${BASE_A}/training/runs" --data "$(start_training_payload "e2e-take-${SUFFIX}" "e2e-take-${SUFFIX}" 3)" >/dev/null
trainee "${BASE_A}/rooms/e2e-take-${SUFFIX}/members" --data '{"user_id":"admin","role":"admin"}' >/dev/null
trainee "${BASE_A}/rooms/e2e-take-${SUFFIX}/orders" --data '{
  "participant_id": "trainee",
  "account_id": 20,
  "action": {"PlaceLimit": {"side": "Buy", "price_tick": 101, "qty": 1}}
}' >/dev/null
reap_server "${A_PID}" TERM; A_PID=""
B_PID="$(start_server "${BIND_B}" "${LOG_DIR}/takeover-b.log" "e2e-take-b" "room-leased" "${BASE_B}")"
wait_ready "${BASE_B}" "${LOG_DIR}/takeover-b.log"
admin "${BASE_B}/rooms/e2e-take-${SUFFIX}/clock/advance" --data '{"steps":3}' >/dev/null
assert_terminal "${BASE_B}" "e2e-take-${SUFFIX}" "e2e-take-${SUFFIX}"
reap_server "${B_PID}"; B_PID=""
PASS=$((PASS + 1))

echo "== path 4 speed =="
A_PID="$(start_server "${BIND_A}" "${LOG_DIR}/speed.log" "e2e-speed" "single-active" "${BASE_A}")"
wait_ready "${BASE_A}" "${LOG_DIR}/speed.log"
trainee "${BASE_A}/training/runs" --data "$(start_training_payload "e2e-fast-${SUFFIX}" "e2e-fast-${SUFFIX}" 3)" >/dev/null
drive_to_terminal "${BASE_A}" "e2e-fast-${SUFFIX}" "e2e-fast-${SUFFIX}" >"${LOG_DIR}/e2e-fast.json"
trainee "${BASE_A}/training/runs" --data "$(start_training_payload "e2e-slow-${SUFFIX}" "e2e-slow-${SUFFIX}" 3)" >/dev/null
trainee "${BASE_A}/rooms/e2e-slow-${SUFFIX}/members" --data '{"user_id":"admin","role":"admin"}' >/dev/null
trainee "${BASE_A}/rooms/e2e-slow-${SUFFIX}/orders" --data '{
  "participant_id": "trainee",
  "account_id": 20,
  "action": {"PlaceLimit": {"side": "Buy", "price_tick": 101, "qty": 1}}
}' >/dev/null
trainee "${BASE_A}/rooms/e2e-slow-${SUFFIX}/orders" --data '{
  "participant_id": "trainee",
  "account_id": 20,
  "action": {"PlaceLimit": {"side": "Buy", "price_tick": 90, "qty": 1}}
}' >/dev/null
admin "${BASE_A}/rooms/e2e-slow-${SUFFIX}/clock/advance" --data '{"steps":1}' >/dev/null
admin "${BASE_A}/rooms/e2e-slow-${SUFFIX}/clock/advance" --data '{"steps":1}' >/dev/null
admin "${BASE_A}/rooms/e2e-slow-${SUFFIX}/clock/advance" --data '{"steps":1}' >/dev/null
assert_terminal "${BASE_A}" "e2e-slow-${SUFFIX}" "e2e-slow-${SUFFIX}" >"${LOG_DIR}/e2e-slow.json"
python3 - <<PY
import json
fast = json.load(open("${LOG_DIR}/e2e-fast.json"))
slow = json.load(open("${LOG_DIR}/e2e-slow.json"))
for key in ("status", "filled_qty", "fees_paid", "score_q"):
    if fast[key] != slow[key]:
        raise SystemExit(f"speed mismatch {key}: {fast[key]} vs {slow[key]}")
print(json.dumps({"speed_match": True, "status": fast["status"], "filled_qty": fast["filled_qty"]}))
PY
reap_server "${A_PID}"; A_PID=""
PASS=$((PASS + 1))

echo "training recovery e2e passed ${PASS}/4 paths"
