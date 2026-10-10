"""The project-owned service exposes provided-bar analysis without a market host."""
from fastapi.testclient import TestClient

from app.marketforge_analysis import app


def test_provided_bar_engine_is_available_without_market_or_account_routes(monkeypatch):
    monkeypatch.setenv("CANDLESCOPE_OFFICIAL_PLUGIN_BOOTSTRAP", "0")
    monkeypatch.setenv("CANDLESCOPE_PLUGIN_HOST_ENABLED", "0")
    with TestClient(app) as client:
        health = client.get("/health").json()
        assert health["kind"] == "marketforge-analysis"
        assert health["data_manager"] is None
        assert client.get("/api/v1/indicators/presets").status_code == 200
        assert client.get("/api/v1/klines").status_code == 404
        assert client.get("/rooms").status_code == 404
        bars = [dict(time=1704067200 + index, open=value, high=value, low=value,
                     close=value, volume=1) for index, value in enumerate((10, 20, 30, 40))]
        result = client.post("/api/v1/indicators/compute", json={
            "mode": "builtin", "name": "MA", "params": {"period": 3}, "ohlcv": bars,
            "exchange": "marketforge", "market_type": "simulation", "symbol": "TEST", "interval": "1s",
        }).json()
        assert result["ok"] is True, result
        assert [point["value"] for point in result["lines"][0]["data"]] == [20, 30]
