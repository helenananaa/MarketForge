"""Reconstruct frozen formula oracles without downloaded or ignored Pine files."""
from __future__ import annotations

import hashlib
import json
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))
from research_strategy_compat import source_for

# Diagnostic titles differ from Pine variable names for these references.
EXPRESSIONS = {
    "macd": ("m", "s", "h"),
    "roc": ("r",),
    "cci": ("c",),
    "williams": ("w",),
    "supertrend": ("line", "direction"),
    "ut_atr": ("loss", "trail"),
    "ssl": ("hi", "lo", "side"),
    "wavetrend": ("wt", "sig"),
    "squeeze": ("mid", "dev", "krange", "mom", "squeezed ? 1 : 0"),
}


def reference_sources():
    """Render and verify both historical hashes before exposing any source."""
    catalog = json.loads((ROOT / "docs/native-pine-popular-catalog.json").read_text(encoding="utf-8"))
    sources = {}
    for port in catalog["ports"]:
        filename = Path(port["pine_source"]).name
        name = Path(filename).stem.removeprefix("native_")
        prefix = source_for(name, port["formula"], port["entry"], port["exit"])
        if hashlib.sha256(prefix.encode()).hexdigest() != port["reference_sha256"]:
            raise ValueError(f"frozen formula hash differs: {filename}")
        source = prefix + "// Numeric diagnostics; trading rules above are frozen.\n"
        expressions = EXPRESSIONS.get(name, port["diagnostics"])
        if len(expressions) != len(port["diagnostics"]):
            raise ValueError(f"diagnostic count differs: {filename}")
        source += "".join(f'plot({expression}, "{title}")\n'
                          for expression, title in zip(expressions, port["diagnostics"]))
        if "family_source" in port:
            source += f'// Family reference: {port["family_author"]} - {port["family_source"]}\n'
            source += "// MarketForge formula adaptation, not the full published script.\n"
        encoded = source.encode("utf-8")
        if hashlib.sha256(encoded).hexdigest() != port["installed_sha256"]:
            raise ValueError(f"frozen diagnostic source hash differs: {filename}")
        sources[filename] = encoded
    return sources
