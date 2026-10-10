"""MarketForge Python SDK (strategy.v1).

Talks only to the HTTP API. Does not access the database or internal actors.
"""

from __future__ import annotations

import json
import urllib.error
import urllib.parse
import urllib.request
from typing import Any, Optional

PROTOCOL_VERSION = "strategy.v1"


class MarketForgeError(RuntimeError):
    def __init__(self, status: int, body: str) -> None:
        super().__init__(f"{status}: {body}")
        self.status = status
        self.body = body


class Client:
    def __init__(
        self,
        base_url: str,
        bearer: Optional[str] = None,
        user_id: Optional[str] = None,
        trusted_owner_urls: Optional[list[str]] = None,
        cursor: Optional[int] = None,
        timeout: float = 30,
    ) -> None:
        self.base_url = base_url.rstrip("/")
        self.bearer = bearer
        self.user_id = user_id
        self.trusted_owner_urls = [url.rstrip("/") for url in (trusted_owner_urls or [])]
        self.cursor = cursor
        self.timeout = timeout
        if self.trusted_owner_urls and self.base_url not in self.trusted_owner_urls:
            raise MarketForgeError(400, f"untrusted owner url: {self.base_url}")

    def _headers(self, idempotency_key: Optional[str] = None) -> dict[str, str]:
        headers = {"content-type": "application/json"}
        if self.bearer:
            headers["authorization"] = f"Bearer {self.bearer}"
        if self.user_id:
            headers["x-user-id"] = self.user_id
        if idempotency_key:
            headers["idempotency-key"] = idempotency_key
        return headers

    def _request(
        self,
        method: str,
        path: str,
        body: Optional[dict[str, Any]] = None,
        idempotency_key: Optional[str] = None,
        query: Optional[dict[str, Any]] = None,
    ) -> Any:
        if query:
            filtered = {}
            for key, value in query.items():
                if value is None:
                    continue
                if isinstance(value, bool):
                    filtered[key] = "true" if value else "false"
                else:
                    filtered[key] = value
            path = path + "?" + urllib.parse.urlencode(filtered)
        data = None if body is None else json.dumps(body).encode("utf-8")
        req = urllib.request.Request(
            self.base_url + path,
            data=data,
            headers=self._headers(idempotency_key),
            method=method,
        )
        try:
            with urllib.request.urlopen(req, timeout=self.timeout) as resp:
                raw = resp.read().decode("utf-8")
                return json.loads(raw) if raw else None
        except urllib.error.HTTPError as exc:
            payload = exc.read().decode("utf-8")
            raise MarketForgeError(exc.code, payload) from exc

    def observe(
        self,
        room_id: str,
        account_id: int,
        instrument_id: Optional[str] = None,
    ) -> Any:
        result = self._request(
            "GET",
            f"/rooms/{room_id}/observe",
            query={"account_id": account_id, "instrument_id": instrument_id},
        )
        observation = result.get("observation", result) if isinstance(result, dict) else {}
        if isinstance(observation, dict) and "step" in observation:
            self.cursor = observation["step"]
        return result

    def place(
        self,
        room_id: str,
        account_id: int,
        side: str,
        price_tick: int,
        qty: int,
        idempotency_key: Optional[str] = None,
    ) -> Any:
        action = {
            "PlaceLimit": {
                "side": "Buy" if side.lower() == "buy" else "Sell",
                "price_tick": price_tick,
                "qty": qty,
            }
        }
        return self._request(
            "POST",
            f"/rooms/{room_id}/orders",
            {
                "participant_id": "python-sdk",
                "account_id": account_id,
                "action": action,
            },
            idempotency_key=idempotency_key,
        )

    def cancel(
        self,
        room_id: str,
        account_id: int,
        order_id: int,
        idempotency_key: Optional[str] = None,
    ) -> Any:
        return self._request(
            "POST",
            f"/rooms/{room_id}/orders",
            {
                "participant_id": "python-sdk",
                "account_id": account_id,
                "action": {"Cancel": {"order_id": order_id}},
            },
            idempotency_key=idempotency_key,
        )

    def start_training(self, request: dict[str, Any]) -> Any:
        return self._request("POST", "/training/runs", request)

    def training_status(self, run_id: str) -> Any:
        return self._request("GET", f"/training/runs/{run_id}")

    def abort_training(self, run_id: str) -> Any:
        return self._request("POST", f"/training/runs/{run_id}/abort", {})

    def training_result(self, run_id: str) -> Any:
        return self._request("GET", f"/training/runs/{run_id}/result")

    def training_report(self, run_id: str) -> Any:
        return self._request("GET", f"/training/runs/{run_id}/report")

    def list_bots(self) -> Any:
        return self._request("GET", "/bots")

    def room_bots(self, room_id: str) -> Any:
        return self._request("GET", f"/rooms/{room_id}/bots")

    def start_agents(self, room_id: str, agents: list[dict[str, Any]], interval_ms: int = 1000) -> Any:
        return self._request("POST", f"/rooms/{room_id}/agents", {"agents": agents, "interval_ms": interval_ms})

    def stop_agents(self, room_id: str) -> Any:
        return self._request("POST", f"/rooms/{room_id}/agents/stop", {})

    def pause_room(self, room_id: str, idempotency_key: Optional[str] = None) -> Any:
        return self._request("POST", f"/rooms/{room_id}/pause", {}, idempotency_key=idempotency_key)

    def step_bots(self, room_id: str, idempotency_key: Optional[str] = None) -> Any:
        return self._request("POST", f"/rooms/{room_id}/clock/step", {}, idempotency_key=idempotency_key)

    def clock(self, room_id: str) -> Any:
        return self._request("GET", f"/rooms/{room_id}/clock")

    def advance_clock(
        self,
        room_id: str,
        steps: int,
        idempotency_key: Optional[str] = None,
    ) -> Any:
        return self._request(
            "POST",
            f"/rooms/{room_id}/clock/advance",
            {"steps": steps},
            idempotency_key=idempotency_key,
        )

    def events(
        self,
        room_id: str,
        limit: Optional[int] = None,
        from_start: bool = True,
    ) -> Any:
        return self._request(
            "GET",
            f"/rooms/{room_id}/events",
            query={"limit": limit, "from_start": from_start},
        )

    def trades(self, room_id: str, limit: Optional[int] = None) -> Any:
        return self._request(
            "GET",
            f"/rooms/{room_id}/trades",
            query={"limit": limit},
        )

    def health_ready(self) -> Any:
        return self._request("GET", "/health/ready")

    def add_member(self, room_id: str, user_id: str, role: str) -> Any:
        return self._request(
            "POST",
            f"/rooms/{room_id}/members",
            {"user_id": user_id, "role": role},
        )

    def remove_member(self, room_id: str, user_id: str) -> Any:
        return self._request("POST", f"/rooms/{room_id}/members/{user_id}", {})

    def assign_account(self, room_id: str, account_id: int, user_id: str) -> Any:
        return self._request(
            "POST",
            f"/rooms/{room_id}/accounts/{account_id}/owners",
            {"user_id": user_id},
        )

    def candles(self, room_id, instrument_id, interval_ms=1000, limit=500, before_open_time_ms=None, after_open_time_ms=None):
        """Bounded source bars; cursors are exclusive and time is simulated."""
        room, market = (urllib.parse.quote(v, safe="") for v in (room_id, instrument_id))
        return self._request("GET", f"/rooms/{room}/instruments/{market}/candles", query={"interval_ms":interval_ms,"limit":limit,"before_open_time_ms":before_open_time_ms,"after_open_time_ms":after_open_time_ms})

    def risk_events(self, room_id, instrument_id, account_id, after_command_seq=None, from_start=False, limit=100):
        room, market = (urllib.parse.quote(v, safe="") for v in (room_id, instrument_id))
        return self._request("GET", f"/rooms/{room}/instruments/{market}/risk-events", query={"account_id":account_id,"after_command_seq":after_command_seq,"from_start":from_start,"limit":limit})

    def protect_position(self, room_id, instrument_id, account_id, take_profit_tick=None, stop_loss_tick=None, position_side="Both", trigger="Mark", idempotency_key=None, **protection_fields):
        """Replace the full perpetual position protection; omit TP/SL to remove."""
        spec = {"take_profit_tick":take_profit_tick,"stop_loss_tick":stop_loss_tick,"trigger":trigger,**protection_fields} if take_profit_tick is not None or stop_loss_tick is not None or protection_fields else None
        room, market = (urllib.parse.quote(v, safe="") for v in (room_id, instrument_id))
        return self._request("POST", f"/rooms/{room}/instruments/{market}/orders", {"participant_id":"python-sdk","account_id":account_id,"action":{"SetPositionProtection":{"position_side":position_side,"protection":spec}}}, idempotency_key=idempotency_key)

    def place_bracket(self, room_id, instrument_id, account_id, side, qty, take_profit_tick=None, stop_loss_tick=None, price_tick=None, position_side="Both", trigger="Mark", idempotency_key=None, **protection_fields):
        """Atomic perpetual limit entry (price) or unbounded market entry (no price) with exits."""
        room, market = (urllib.parse.quote(v, safe="") for v in (room_id, instrument_id))
        action={"PlaceBracket":{"side":side,"qty":qty,"price_tick":price_tick,"position_side":position_side,"protection":{"take_profit_tick":take_profit_tick,"stop_loss_tick":stop_loss_tick,"trigger":trigger,**protection_fields}}}
        return self._request("POST", f"/rooms/{room}/instruments/{market}/orders", {"participant_id":"python-sdk","account_id":account_id,"action":action}, idempotency_key=idempotency_key)

    def market_indicators(self, room_id, instrument_id, interval_ms=1000, limit=500, period=14, **cursors):
        from .agents.market_data import indicators
        data = self.candles(room_id, instrument_id, interval_ms, limit, **cursors)
        return {**data, "indicators": indicators(data["candles"], period)}

    def chart_export(self, room_id, instrument_id, interval_ms=1000, limit=500, period=14, width=1000, height=600, indicator=None, analysis_url="http://127.0.0.1:18086/api/v1", **cursors):
        """PNG/base64 metadata; install marketforge[agents] for rendering."""
        from .agents.market_data import indicators, render_chart
        data = self.candles(room_id, instrument_id, interval_ms, limit, **cursors)
        from .agents.advanced_tools import AdvancedTools
        overlay=AdvancedTools().compute_indicator(data,{"instrument":instrument_id,**indicator},analysis_url.rstrip("/"))["indicator"] if indicator else None
        return render_chart(data, indicators(data["candles"], period), width, height, overlay=overlay)

    def submit_action(self, room_id, instrument_id, account_id, action, idempotency_key):
        """Submit any native OrderAction with a stable exchange intent key."""
        room, market = (urllib.parse.quote(v, safe="") for v in (room_id, instrument_id))
        return self._request("POST", f"/rooms/{room}/instruments/{market}/orders", {"participant_id":"python-sdk","account_id":account_id,"action":action}, idempotency_key=idempotency_key)

    def account_history(self, room_id, instrument_id, account_id, **query):
        room, market = (urllib.parse.quote(v, safe="") for v in (room_id, instrument_id))
        return self._request("GET", f"/rooms/{room}/instruments/{market}/account-history",query={"account_id":account_id,**query})

    def iter_account_history(self, room_id, instrument_id, account_id, **query):
        """Yield complete journal pages, including empty pages that advance the cursor."""
        query={"from_start":True, "limit":500, **query}
        while True:
            page=self.account_history(room_id,instrument_id,account_id,**query)
            yield page
            if not page["has_more"]: break
            query.pop("from_start",None)
            query["after_command_seq"]=page["next_after_command_seq"]

    def order_activity(self, room_id, instrument_id, account_id, order_id):
        """Exact old/native order lookup by decimal ID across complete durable history."""
        return [activity for page in self.iter_account_history(room_id,instrument_id,account_id,order_id=str(order_id)) for activity in page["activities"]]

    def market_rules(self, room_id, instrument_id):
        room, market = (urllib.parse.quote(v,safe="") for v in (room_id,instrument_id))
        return self._request("GET",f"/rooms/{room}/instruments/{market}/rules")

    def portfolio(self, room_id, account_id):
        room=urllib.parse.quote(room_id,safe="")
        return self._request("GET",f"/rooms/{room}/accounts/{account_id}/portfolio")

    def conditional_orders(self, room_id, instrument_id, account_id):
        room, market = (urllib.parse.quote(v,safe="") for v in (room_id,instrument_id))
        return self._request("GET",f"/rooms/{room}/instruments/{market}/conditionals",query={"account_id":account_id})

    def set_conditional(self, room_id, instrument_id, account_id, key, spec, idempotency_key):
        return self.submit_action(room_id,instrument_id,account_id,{"SetConditional":{"key":key,"spec":spec}},idempotency_key)

    def indicator_compute(self, room_id, instrument_id, analysis_url="http://127.0.0.1:18086/api/v1", interval_ms=1000, limit=500, **indicator):
        """Use the same provided-bar engine as the human CandleScope workbench."""
        from .agents.advanced_tools import AdvancedTools
        data=self.candles(room_id,instrument_id,interval_ms,limit)
        return AdvancedTools().compute_indicator(data,{"instrument":instrument_id,**indicator},analysis_url.rstrip("/"))
