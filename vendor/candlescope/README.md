# MarketForge-owned CandleScope source fork

This directory contains the frontend, backend analysis code and supporting SDK/plugin sources copied from
`helenananaa/CandleScope` at commit `1a2a0189ceb45e188eecab2a4927e07a3f5d20c6` on 2026-10-09.
The MarketForge simulation integration is maintained in **this repository**. Build and startup scripts
use this copy; the upstream checkout is neither required nor modified.

`UPSTREAM.json` records the imported file hashes and the MarketForge integration files present at import.
Its hashes describe the initial import, not a requirement that locally maintained files stay unchanged.
Retain the upstream GPLv3 text in `LICENSE`, upstream notices, and the license metadata in each package.
The imported directory retains its upstream licensing separately from MarketForge's existing license.

Frontend integration lives in `frontend/src/features/simulation`. The backend owns analysis only;
MarketForge still owns rooms, execution, accounts and the simulation clock. Python and Node dependencies
are installed locally rather than copying another checkout's environments, secrets, databases or caches.
Runtime state belongs under MarketForge `.local/candlescope-runtime`, and official script runtime bundles
are digest-verified using the copied backend release lock.

From the MarketForge root:

```powershell
.\scripts\setup-candlescope-workbench.ps1 -PythonExecutable <Python-3.12-executable>
.\scripts\start-candlescope-workbench.ps1
```

See `docs/CANDLESCOPE_WORKBENCH.md` for supported features and validation boundaries.
