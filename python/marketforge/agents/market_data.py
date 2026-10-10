"""Bounded market data and analysis shared by MCP, HTTP and strategy observations."""
import base64
import io
import threading
import urllib.parse

_PLOT_LOCK = threading.Lock()


def indicators(candles, period=14):
    """Window-local SMA, EMA, Wilder RSI/ATR and window VWAP; null during warmup."""
    if type(period) is not int or not 2 <= period <= 200:
        raise ValueError("indicator period must be 2-200")
    rows, prices, ema, gain, loss, atr, total_pv, total_v = [], [], None, None, None, None, 0, 0
    changes, ranges = [], []
    for c in candles:
        close, high, low = c["close_tick"], c["high_tick"], c["low_tick"]
        previous = prices[-1] if prices else close
        prices.append(close)
        ema = close if ema is None else ema + 2 / (period + 1) * (close - ema)
        if len(prices) > 1:
            changes.append(close - previous)
        ranges.append(max(high - low, abs(high - previous), abs(low - previous)))
        rsi = None
        if len(changes) == period:
            gain = sum(max(v, 0) for v in changes) / period
            loss = sum(max(-v, 0) for v in changes) / period
        elif len(changes) > period:
            gain = (gain * (period - 1) + max(changes[-1], 0)) / period
            loss = (loss * (period - 1) + max(-changes[-1], 0)) / period
        if gain is not None:
            rsi = 50.0 if gain == loss == 0 else 100.0 if loss == 0 else 100 - 100 / (1 + gain / loss)
        if len(ranges) == period:
            atr = sum(ranges) / period
        elif len(ranges) > period:
            atr = (atr * (period - 1) + ranges[-1]) / period
        volume = c["volume"]
        total_pv += c.get("quote_volume", close * volume)
        total_v += volume
        rows.append({"open_time_ms": c["open_time_ms"], "sma": sum(prices[-period:]) / period if len(prices) >= period else None,
            "ema": ema if len(prices) >= period else None, "rsi": rsi, "atr": atr,
            "vwap": total_pv / total_v if total_v else None})
    return {"period": period, "scope": "returned candle window; EMA seeded with first close; RSI/ATR use Wilder smoothing; VWAP uses quote volume",
        "rows": rows, "latest": rows[-1] if rows else None}


def render_chart(data, studies, width=1000, height=600, overlay=None):
    """Headless chart of returned source bars; no browser-session state."""
    if type(width) is not int or type(height) is not int or not 400 <= width <= 1600 or not 300 <= height <= 1000:
        raise ValueError("chart dimensions must be width 400-1600, height 300-1000")
    bars = data["candles"]
    if not bars:
        raise ValueError("no traded candles in the requested window")
    if any(abs(c[k]) > 2**53-1 for c in bars for k in ("open_tick", "high_tick", "low_tick", "close_tick", "volume")):
        raise ValueError("chart values exceed safe floating-point precision; use raw data")
    try:
        from matplotlib.figure import Figure
        from matplotlib.backends.backend_agg import FigureCanvasAgg
        from matplotlib.patches import Rectangle
    except ImportError:
        raise ValueError("chart rendering requires the marketforge[agents] matplotlib dependency") from None
    with _PLOT_LOCK:
        fig = Figure(figsize=(width / 100, height / 100), dpi=100, facecolor="#111827")
        canvas = FigureCanvasAgg(fig)
        ax, volume = fig.subplots(2, 1, sharex=True, gridspec_kw={"height_ratios": [4, 1]})
        for axis in (ax, volume):
            axis.set_facecolor("#111827")
            axis.tick_params(colors="#cbd5e1")
            axis.grid(alpha=0.15)
            for spine in axis.spines.values(): spine.set_color("#475569")
        for i, c in enumerate(bars):
            color = "#22c55e" if c["close_tick"] >= c["open_tick"] else "#ef4444"
            ax.vlines(i, c["low_tick"], c["high_tick"], color=color)
            bottom = min(c["open_tick"], c["close_tick"])
            body = abs(c["close_tick"] - c["open_tick"])
            if body: ax.add_patch(Rectangle((i - .3, bottom), .6, body, facecolor=color))
            else: ax.hlines(bottom, i - .3, i + .3, color=color)
            volume.bar(i, c["volume"], color=color, width=.6)
        for key, color in (("sma", "#fbbf24"), ("ema", "#60a5fa")):
            values=[r[key] for r in studies['rows']]
            if any(value is not None for value in values):
                ax.plot(values, label=f"{key.upper()} {studies['period']}", color=color, linewidth=1)
        if overlay:
            if overlay.get('ok') is not True: raise ValueError('indicator failed; inspect indicator_compute result')
            times={1704067200+c['open_time_ms']/1000:i for i,c in enumerate(bars)}
            for line in overlay.get('lines',[])[:32]:
                points=[p for p in line.get('data',[]) if p.get('time') in times and p.get('value') is not None]
                ax.plot([times[p['time']] for p in points],[p['value'] for p in points],label=str(line.get('name',line.get('title',line.get('outputName','indicator'))))[:80],linewidth=1)
        ax.autoscale_view()
        ax.legend(facecolor="#1f2937", labelcolor="#e2e8f0")
        ax.set_ylabel("Price ticks", color="#cbd5e1")
        volume.set_ylabel("Lots", color="#cbd5e1")
        count = min(6, len(bars))
        ticks = sorted({round(i * (len(bars)-1) / max(1,count-1)) for i in range(count)})
        volume.set_xticks(ticks, [f"{bars[i]['open_time_ms']/1000:g}s" for i in ticks])
        volume.set_xlabel("Simulation elapsed time; gaps omitted", color="#cbd5e1")
        ax.set_title(f"{data['instrument_id']} | {data['interval_ms']}ms | market t={data['market_time_ms']}ms", color="#f1f5f9")
        fig.tight_layout()
        out = io.BytesIO(); canvas.print_png(out)
        raw = out.getvalue()
        if len(raw) > 2_000_000: raise ValueError("chart exceeds 2 MiB; narrow the window")
    return {"mime_type": "image/png", "image_base64": base64.b64encode(raw).decode(), "width": width, "height": height,
        "room_id": data["room_id"], "instrument_id": data["instrument_id"], "interval_ms": data["interval_ms"],
        "market_time_ms": data["market_time_ms"], "bar_count": len(bars), "source": "MarketForge traded candles; independent headless render",
        "indicator_engine":"CandleScope" if overlay else "builtin-window-indicators","indicator_lines":len(overlay.get('lines',[])) if overlay else 0}


class MarketDataTools:
    def candle_data(self, config, args):
        from .runtime import integer
        instrument = args["instrument"]; self.check_instrument(config, instrument)
        room, market = (urllib.parse.quote(v, safe="") for v in (config["room"], instrument))
        query = {"interval_ms": integer(args.get("interval_ms", 1000), 1, 2_678_400_000), "limit": integer(args.get("limit", 500), 1, 2000)}
        for field in ("before_open_time_ms", "after_open_time_ms"):
            if field in args: query[field] = integer(args[field], 0, 2**53-1)
        data = self.client(config)._request("GET", f"/rooms/{room}/instruments/{market}/candles", query=query)
        if "candles" not in data: raise ValueError("exchange did not return candle data")
        return data

    def read_risk_events(self, config, args):
        from .runtime import integer
        instrument = args["instrument"]; self.check_instrument(config, instrument)
        room, market = (urllib.parse.quote(v, safe="") for v in (config["room"], instrument))
        query = {"account_id": config["account_id"], "limit": integer(args.get("limit", 100), 1, 500)}
        if "after_command_seq" in args: query["after_command_seq"] = integer(args["after_command_seq"], 0, 2**53-1)
        if "from_start" in args:
            if type(args["from_start"]) is not bool: raise ValueError("from_start must be boolean")
            query["from_start"] = args["from_start"]
        return self.client(config)._request("GET", f"/rooms/{room}/instruments/{market}/risk-events", query=query)

    def strategy_observations(self, config, strategy):
        observed = self.observations(config)
        settings = strategy.get("market_data")
        if settings:
            for instrument, value in observed.items():
                data = self.candle_data(config, {"instrument": instrument, **settings})
                value["analysis_data"] = {"candles": data, "indicators": indicators(data["candles"])}
        return observed

    def notify_risk(self, trader, name, evidence):
        import time
        import uuid
        with self.lock(trader):
            if self.config(trader)["status"] != "running": return
            state = self.alerts.state(trader)
            trigger = {"id": uuid.uuid4().hex, "name": name, "reason": "Account risk event: reassess with fresh context", "evidence": evidence,
                "conditions": [], "wall_time": time.time(), "pause_strategies": False, "priority": "urgent"}
            state["pending"] = [x for x in state["pending"] if x["name"] != name] + [trigger]
            state["generation"] += 1
            state["interrupted_plan"] = self.store.get("decision", trader)
            self.store.put("alerts_state", trader, state)
            self.invalidate_decision(trader)
            # Reuse the alert interrupt path to fence an already-running strategy tick.
            signal = self.alerts.signals.setdefault(trader, threading.Event()); signal.set()
            self.alerts.signals[trader] = threading.Event()
            self.alerts.suspend(trader, [trigger])
            self.store.event(trader, "alert_triggered", {**trigger, "generation": state["generation"]})
            self.wake_events[trader].set()

    def poll_risk_events(self, trader):
        config = self.config(trader)
        for instrument in config["instruments"]:
            key = f"{trader}:{instrument}"
            cursor = self.store.get("risk_cursor", key)
            args = {"instrument": instrument}
            if cursor is not None: args["after_command_seq"] = cursor
            page = self.read_risk_events(config, args)
            if "events" not in page: continue
            # On first attachment deliver the tail rather than silently discarding a just-occurred liquidation.
            for row in page["events"]:
                event = row["event"]
                if event["type"] == "PerpLiquidationSettled" or event["type"] == "PerpMarginStatusChanged" and event.get("new_status") in ("margin_call", "liquidatable"):
                    self.notify_risk(trader, f"risk-{instrument}", row)
            next_cursor = page.get("next_after_command_seq")
            if next_cursor is not None: self.store.put("risk_cursor", key, next_cursor)
