"""Auditable statistics from public aggregate trades; no synthetic fills or quotes."""
from dataclasses import dataclass
from datetime import date, datetime, time, timezone
from decimal import Decimal, InvalidOperation
import csv
import hashlib
import io
import math
from pathlib import Path
import re
import statistics
import urllib.request
import zipfile

SOURCE_DOC = "https://github.com/binance/binance-public-data"
BASE_URL = "https://data.binance.vision/data/"


def quantiles(values):
    values = sorted(values)
    if not values:
        return {"count": 0, "p05": None, "p50": None, "p95": None, "p99": None}
    def at(p):
        position = (len(values) - 1) * p
        left = int(position)
        fraction = position - left
        return values[left] * (1 - fraction) + values[min(left + 1, len(values) - 1)] * fraction
    return {"count": len(values), "p05": at(.05), "p50": at(.5), "p95": at(.95), "p99": at(.99)}


def correlation(left, right):
    if len(left) < 3:
        return None
    a, b = statistics.mean(left), statistics.mean(right)
    variance = sum((x - a)**2 for x in left) * sum((y - b)**2 for y in right)
    return sum((x - a) * (y - b) for x, y in zip(left, right)) / math.sqrt(variance) if variance else None


def day_start(day):
    return int(datetime.combine(date.fromisoformat(day), time(), timezone.utc).timestamp())


def acquire(symbol, day, cache):
    """Download fixed daily files and a funding month, verifying every SHA256."""
    if not re.fullmatch(r"[A-Z0-9]{3,32}", symbol):
        raise ValueError("symbol must contain uppercase letters and digits")
    date.fromisoformat(day)
    cache = Path(cache)
    cache.mkdir(parents=True, exist_ok=True)
    month = day[:7]
    paths = {"spot": f"spot/daily/aggTrades/{symbol}/{symbol}-aggTrades-{day}.zip",
             "perp": f"futures/um/daily/aggTrades/{symbol}/{symbol}-aggTrades-{day}.zip",
             "funding": f"futures/um/monthly/fundingRate/{symbol}/{symbol}-fundingRate-{month}.zip"}
    receipts = []
    for leg, relative in paths.items():
        url = BASE_URL + relative
        with urllib.request.urlopen(url + ".CHECKSUM", timeout=30) as response:
            expected = response.read(4096).decode("ascii").split()[0]
        if not re.fullmatch(r"[a-fA-F0-9]{64}", expected):
            raise ValueError("invalid checksum response")
        destination = cache / f"{leg}-{month if leg == 'funding' else day}.zip"
        if not destination.exists():
            temporary = destination.with_suffix(".part")
            with urllib.request.urlopen(url, timeout=30) as response, temporary.open("wb") as output:
                size = 0
                while chunk := response.read(1024**2):
                    size += len(chunk)
                    if size > 200 * 1024**2:
                        raise ValueError("archive exceeds 200MiB download bound")
                    output.write(chunk)
            temporary.replace(destination)
        digest = hashlib.sha256(destination.read_bytes()).hexdigest()
        if digest != expected.lower():
            raise ValueError(f"checksum mismatch: {destination}")
        receipts.append({"leg": leg, "url": url, "checksum_url": url + ".CHECKSUM", "sha256": digest,
                         "path": str(destination.resolve()), "bytes": destination.stat().st_size})
    return receipts


def archive_rows(path):
    with zipfile.ZipFile(path) as archive:
        entries = archive.infolist()
        if len(entries) != 1 or not entries[0].filename.endswith(".csv") or entries[0].file_size > 1024**3:
            raise ValueError("expected one bounded CSV member")
        with archive.open(entries[0]) as binary, io.TextIOWrapper(binary, encoding="utf-8", newline="") as text:
            yield from csv.reader(text)


@dataclass(frozen=True)
class ReferenceTrade:
    time_us: int
    price: Decimal
    qty: Decimal
    buy: bool
    raw_count: int


def parse_trades(rows, leg, day):
    if leg not in ("spot", "perp"):
        raise ValueError("unknown market leg")
    start_us = day_start(day) * 1_000_000
    multiplier = 1 if leg == "spot" and day >= "2025-01-01" else 1000
    previous_id = previous_time = None
    for number, row in enumerate(rows, 1):
        if number == 1 and row and row[0] in ("agg_trade_id", "aggregate_trade_id"):
            continue
        try:
            if len(row) != (8 if leg == "spot" else 7):
                raise ValueError("column count")
            identity, first, last, timestamp = int(row[0]), int(row[3]), int(row[4]), int(row[5]) * multiplier
            price, qty = Decimal(row[1]), Decimal(row[2])
            if not price.is_finite() or not qty.is_finite() or price <= 0 or qty <= 0:
                raise ValueError("nonpositive/nonfinite price or quantity")
            if identity < 0 or first < 0 or last < first:
                raise ValueError("trade IDs")
            if row[6].lower() not in ("true", "false"):
                raise ValueError("buyer maker flag")
            if not start_us <= timestamp < start_us + 86_400_000_000:
                raise ValueError("timestamp outside the requested UTC day; check ms/us")
            if previous_id is not None and (identity <= previous_id or timestamp < previous_time):
                raise ValueError("duplicate/unsorted aggregate IDs or timestamps")
            previous_id, previous_time = identity, timestamp
            yield ReferenceTrade(timestamp, price, qty, row[6].lower() == "false", last - first + 1)
        except (ValueError, InvalidOperation, IndexError) as error:
            raise ValueError(f"{leg} row {number}: {error}") from error


class Segment:
    def __init__(self, start_us, seconds):
        self.start_us, self.seconds = start_us, seconds
        self.bars = {}
        self.qty = []
        self.interarrival = []
        self.previous = None
        self.transitions = self.same_side = self.agg_count = self.raw_count = self.buy_count = 0
        self.buy_volume = Decimal(0)

    def add(self, trade):
        second = (trade.time_us - self.start_us) // 1_000_000
        if not 0 <= second < self.seconds:
            raise ValueError("trade outside segment")
        bar = self.bars.setdefault(second, {"last": trade.price, "volume": Decimal(0), "count": 0})
        bar["last"] = trade.price
        bar["volume"] += trade.qty
        bar["count"] += 1
        self.qty.append(float(trade.qty))
        self.agg_count += 1
        self.raw_count += trade.raw_count
        self.buy_count += trade.buy
        self.buy_volume += trade.qty if trade.buy else Decimal(0)
        if self.previous:
            self.interarrival.append((trade.time_us - self.previous.time_us) / 1000)
            self.transitions += 1
            self.same_side += trade.buy == self.previous.buy
        self.previous = trade

    def summary(self):
        if not self.agg_count:
            raise ValueError("empty reference segment")
        total = sum((bar["volume"] for bar in self.bars.values()), Decimal(0))
        returns = {second: float((bar["last"] / self.bars[second - 1]["last"] - 1) * 1_000_000)
                   for second, bar in self.bars.items() if second - 1 in self.bars}
        adjacent = [(abs(returns[s - 1]), abs(r)) for s, r in returns.items() if s - 1 in returns]
        minute_volumes = [float(sum((self.bars.get(s, {}).get("volume", Decimal(0))
                                    for s in range(m * 60, min((m + 1) * 60, self.seconds))), Decimal(0)))
                          for m in range((self.seconds + 59) // 60)]
        first, last = self.bars[min(self.bars)]["last"], self.bars[max(self.bars)]["last"]
        minute_returns = [float((self.bars[s]["last"] / self.bars[s - 60]["last"] - 1) * 1_000_000)
                          for s in range(119, self.seconds, 60) if s in self.bars and s - 60 in self.bars]
        return {"seconds": self.seconds, "active_seconds": len(self.bars), "aggregate_trades": self.agg_count,
                "underlying_trades": self.raw_count, "aggregate_trades_per_second": self.agg_count / self.seconds,
                "total_base_quantity": str(total), "aggregate_qty": quantiles(self.qty),
                "minute_base_volume": quantiles(minute_volumes), "interarrival_ms": quantiles(self.interarrival),
                "taker_buy_count_fraction": self.buy_count / self.agg_count,
                "taker_buy_volume_fraction": float(self.buy_volume / total),
                "taker_side_persistence": self.same_side / self.transitions if self.transitions else None,
                "first_price": str(first), "last_price": str(last),
                "return_std_ppm_1s": statistics.pstdev(returns.values()) if len(returns) >= 2 else None,
                "absolute_return_ppm_1s": quantiles([abs(r) for r in returns.values()]),
                "absolute_return_ppm_60s": quantiles([abs(r) for r in minute_returns]),
                "absolute_return_lag1_correlation": correlation([a for a, _ in adjacent], [b for _, b in adjacent]),
                "return_samples": len(returns)}


def paired_summary(spot, perp):
    common = sorted(spot.bars.keys() & perp.bars.keys())
    basis = [float((perp.bars[s]["last"] / spot.bars[s]["last"] - 1) * 1_000_000) for s in common]
    return {"paired_active_seconds": len(common), "last_trade_basis_ppm": quantiles(basis),
            "positive_last_trade_basis_fraction": sum(x > 0 for x in basis) / len(basis) if basis else None,
            "is_executable_basis": False,
            "alignment": "both legs traded within the same closed one-second bucket; no future filling"}


def funding_rows(rows, day):
    previous = None
    selected = []
    for number, row in enumerate(rows, 1):
        if number == 1 and row == ["calc_time", "funding_interval_hours", "last_funding_rate"]:
            continue
        try:
            if len(row) != 3:
                raise ValueError("column count")
            stamp, hours, rate = int(row[0]), int(row[1]), Decimal(row[2])
            if not rate.is_finite() or abs(rate) > 1 or not 1 <= hours <= 24:
                raise ValueError("funding values")
            if previous is not None and stamp <= previous:
                raise ValueError("duplicate/unsorted funding timestamp")
            previous = stamp
            if day_start(day) * 1000 <= stamp < (day_start(day) + 86400) * 1000:
                selected.append({"time_ms": stamp, "interval_hours": hours, "rate": str(rate)})
        except (ValueError, InvalidOperation) as error:
            raise ValueError(f"funding row {number}: {error}") from error
    if not selected:
        raise ValueError("no funding records on selected day")
    return selected


def reference_profile(receipts, day, window_seconds=3600, symbol="BTCUSDT"):
    if window_seconds < 120 or window_seconds > 86400 or window_seconds % 120:
        raise ValueError("window must be a multiple of 120 seconds within one UTC day")
    start = day_start(day) * 1_000_000
    half = window_seconds // 2
    segments = {leg: [Segment(start, half), Segment(start + half * 1_000_000, half)] for leg in ("spot", "perp")}
    scanned = {}
    funding = None
    seen = set()
    for receipt in receipts:
        leg = receipt["leg"]
        if leg not in ("spot", "perp", "funding") or leg in seen:
            raise ValueError("expected one source per market leg and funding")
        seen.add(leg)
        if hashlib.sha256(Path(receipt["path"]).read_bytes()).hexdigest() != receipt["sha256"]:
            raise ValueError("source changed after download")
        if leg == "funding":
            funding = funding_rows(archive_rows(receipt["path"]), day)
            continue
        count = 0
        for trade in parse_trades(archive_rows(receipt["path"]), leg, day):
            count += 1
            relative = (trade.time_us - start) // 1_000_000
            if relative < window_seconds:
                segments[leg][0 if relative < half else 1].add(trade)
        scanned[leg] = count
    if set(scanned) != {"spot", "perp"} or funding is None:
        raise ValueError("both market legs and funding are required")
    profile = {"version": 1, "symbol": symbol, "day_utc": day, "window_seconds": window_seconds,
               "fit_seconds": half, "holdout_seconds": half, "source_documentation": SOURCE_DOC,
               "sources": receipts, "validated_daily_aggregate_rows": scanned, "funding_day": funding,
               "limitations": ["aggregate trades do not expose quotes, depth, cancellations, trader identity or latency",
                   "last-trade basis is not an executable bid/ask opportunity", "one historical window is not broad market qualification"]}
    for index, name in enumerate(("fit", "holdout")):
        profile[name] = {leg: segments[leg][index].summary() for leg in ("spot", "perp")}
        profile[name]["pair"] = paired_summary(segments["spot"][index], segments["perp"][index])
    return profile
