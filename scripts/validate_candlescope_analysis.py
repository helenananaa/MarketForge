"""Read-only analysis integration proof against a paused, task-owned room."""
import argparse
import json
from pathlib import Path
from urllib.request import Request, urlopen


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--room", required=True)
    parser.add_argument("--market", default="http://127.0.0.1:57306")
    parser.add_argument("--candlescope", default="http://127.0.0.1:18086/api/v1")
    parser.add_argument("--user", default="local-user")
    parser.add_argument("--output", required=True)
    args = parser.parse_args()

    def read(url, body=None, market=False):
        headers = {"Content-Type": "application/json"}
        if market:
            headers["x-user-id"] = args.user
        request = Request(url, headers=headers, data=None if body is None else json.dumps(body).encode())
        with urlopen(request, timeout=30) as response:
            return json.load(response)

    base = f"{args.market}/rooms/{args.room}"
    observation = read(f"{base}/observe?account_id=20", market=True)["observation"]
    assert observation["status"] == "Paused", "Pause the test room before obtaining comparable evidence"
    instrument = observation["instrument_id"]
    source = read(f"{base}/candles?interval_ms=1000&instrument_id={instrument}", market=True)
    bars = source["candles"]
    assert len(bars) >= 30, "Need enough real executions for script warmup"
    assert all(a["open_time_ms"] < b["open_time_ms"] for a, b in zip(bars, bars[1:])), "Candle time ordering"
    periods = [1000, 3000, 60000, 180000, 300000, 900000, 3600000, 7200000, 14400000, 86400000, 604800000]
    counts = {}
    for period in periods:
        result = read(f"{base}/candles?interval_ms={period}&instrument_id={instrument}", market=True)
        assert result["market_time_ms"] == source["market_time_ms"]
        expected = {}
        for bar in bars:
            key = bar["open_time_ms"] // period * period
            row = expected.setdefault(key, {"volume": 0, "quote_volume": 0, "trades": 0, "taker_buy_base": 0, "taker_buy_quote": 0})
            for field in row:
                row[field] += bar[field]
        assert len(result["candles"]) == len(expected)
        for bar in result["candles"]:
            assert {field: bar[field] for field in expected[bar["open_time_ms"]]} == expected[bar["open_time_ms"]]
            assert bar["is_final"] == (source["market_time_ms"] >= bar["close_time_ms"])
        counts[str(period)] = len(result["candles"])

    pivot = bars[len(bars) // 2]["open_time_ms"]
    page = read(f"{base}/candles?interval_ms=1000&instrument_id={instrument}&before_open_time_ms={pivot}&limit=7", market=True)["candles"]
    assert page == [bar for bar in bars if bar["open_time_ms"] < pivot][-7:]
    ohlcv = [{"time": 1704067200 + bar["open_time_ms"] / 1000, "open": bar["open_tick"], "high": bar["high_tick"],
              "low": bar["low_tick"], "close": bar["close_tick"], "volume": bar["volume"], "is_closed": bar["is_final"]} for bar in bars]
    scripts = {
        "builtin": {"mode": "builtin", "name": "MA", "params": {"period": 3}},
        "pyne": {"mode": "script", "language": "pyne", "securityMode": "safe", "script": 'indicator("Sim MA", overlay=True)\nplot(ta.sma(close, 3), "Sim MA")'},
        "pine": {"mode": "script", "language": "pine", "securityMode": "safe", "script": '//@version=6\nindicator("Sim MA", overlay=true)\nplot(ta.sma(close, 3), title="Sim MA")'},
    }
    outputs = {}
    for language, request in scripts.items():
        result = read(f"{args.candlescope}/indicators/compute", {**request, "ohlcv": ohlcv, "exchange": "marketforge",
                      "market_type": "simulation", "symbol": instrument, "interval": "1s"})
        assert result.get("ok") is True, (language, result.get("error"), result.get("detail"))
        points = result["lines"][0]["data"]
        assert points, (language, "No rendered points")
        expected = sum(bar["close"] for bar in ohlcv[-3:]) / 3
        assert abs(points[-1]["value"] - expected) < 1e-6, (language, points[-1], expected)
        assert points[-1]["time"] == ohlcv[-1]["time"]
        outputs[language] = {"points": len(points), "last": points[-1]}
    after = read(f"{base}/observe?account_id=20", market=True)["observation"]
    assert after["market_time_ms"] == observation["market_time_ms"], "Analysis advanced the market clock"
    proof = {"room": args.room, "market_time_ms": source["market_time_ms"], "source_bars": len(bars),
             "periods": counts, "history_page": len(page), "indicators": outputs, "analysis_clock_unchanged": True}
    Path(args.output).write_text(json.dumps(proof, ensure_ascii=False, indent=2), encoding="utf-8")
    print(json.dumps(proof, ensure_ascii=False))


if __name__ == "__main__":
    main()
