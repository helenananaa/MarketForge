#!/usr/bin/env python3
"""Headless batch runner: isolated rooms per seed, terminal scores, resume-by-run."""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "python"))

from marketforge.batch import main  # noqa: E402

if __name__ == "__main__":
    raise SystemExit(main())
