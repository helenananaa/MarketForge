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
            filtered = {key: value for key, value in query.items() if value is not None}
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
