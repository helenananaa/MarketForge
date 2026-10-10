# Simulation market

`simulation.html` is a frontend for an independent MarketForge service. The service owns matching,
accounting, risk, agents, room control and persistence; this feature never enters the Host Paper broker.
It reuses the copied chart surface, drawing engine, export flow, theme settings and workspace navigation.
This source is maintained inside MarketForge's `vendor/candlescope`, not in the upstream checkout.

`simulationProtocol` validates wire identities, integer precision and candle finality.
`simulationClient` owns authenticated HTTP with idempotent writes.
`simulationSession` owns cancellation, serialized polling, mutation exclusion and stale-state signaling.
The UI renders the participant observation, not an admin account projection.

Start MarketForge, build this frontend and run `npm run preview`. Open `/simulation.html`.
The backend must include `GET /scenarios/background-market` for the create action.
Connection settings use the standard service URL and Bearer token or local `x-user-id`.
Tokens are never persisted. The default backend is `http://127.0.0.1:57306`.

The primary transport is `simulation.ws.v1`, authenticated in its first message. Whole participant/candle
snapshots replace prior state; a per-connection sequence detects gaps. Idle sockets have a 12 s watchdog,
reconnect with exponential backoff and reset from a new full snapshot. Tokens never enter socket URLs.
Authorization failures stop WS retry. HTTP polls every 750 ms as fallback and every 30 s as reconciliation
while WS is live. Older HTTP responses cannot overwrite newer socket frames. Storage status comes from `/runtime`.

The chart receives the most recent 500 simulation intervals and can load older pages into a 10,000-bar window.
It reuses CandleScope's indicator service for built-ins and safe Pyne/Pine scripts. Recent trades and
the tape/profile use the last 500 executions; they do not claim a full trade archive.
The X axis displays elapsed simulation time. A fixed chart-coordinate epoch never advances the backend clock.
Cross-source `request.security`, multi-chart linking and desktop/plugin packaging remain separate work.
