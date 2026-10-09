# MarketForge

AI 交易员入口支持 `agent.v1` 插件：主动查行情、联网研究、直接交易、编写多文件 Python 策略、安装第三方及系统依赖，并在隔离环境中测试和运行。启动与账户配置见 [AI 交易员指南](docs/AI_TRADERS.md)。

MarketForge is a Rust market simulation engine with a standalone HTTP backend,
a React web interface, and a CandleScope plugin adapter.

It models spot and perpetual markets, order matching, accounts and portfolios,
margin and liquidation, venue rules, transfers, and replayable room events.
The backend supports PostgreSQL journal persistence and room writer leases.

The web's “创建仿真市场” action starts 20 background participants with finite capital:
dynamic makers, value and trend traders, adaptive noise, and TWAP execution.
See [Background market behavior and experiments](docs/BACKGROUND_MARKET.md).

The web now offers linked spot/perpetual rooms by default, with a spot-derived
index and mark price plus three finite-capital perpetual makers. See
[Spot/perpetual linkage](docs/SPOT_PERP_LINK.md) for configuration and boundaries.

Linked perpetuals also support [periodic funding](docs/FUNDING.md), including
account cashflows, collateral risk, deterministic recovery and public/private receipts.

## Components

- `exchange-core`: matching, risk, simulation, training, and room state.
- `exchange-server`: HTTP API, authentication, and durable journals.
- `marketforge-cli`: JSON CLI for rooms, orders, clock, agents, training, replay, and members.
- `python/marketforge`: HTTP SDK (`strategy.v1`) for observe/place/cancel/training.
- `marketforge-candlescope-plugin`: embedded and remote CandleScope integration.
- `marketforge-web`: React/TypeScript interface.
- `vendor/candlescope`: project-owned frontend and analysis source copy; see [simulation workbench](docs/CANDLESCOPE_WORKBENCH.md). Integration changes stay in MarketForge.

## Local development

Install Rust with edition 2024 support and a Node.js version supported by Vite 7.

Start the backend:

```sh
cargo run -p exchange-server
```

The default API address is `http://127.0.0.1:57305`. Without a configured
PostgreSQL database, room state is held in memory and is lost on restart.

In a separate terminal, start the web interface:

```sh
cd marketforge-web
npm ci
npm run dev
```

For PostgreSQL configuration and deployment behavior, see
[Backend storage](docs/BACKEND_STORAGE.md). Example settings are in
[.env.example](.env.example); export the settings required by your deployment.

## CLI

```sh
cargo run -p marketforge-cli -- --base-url http://127.0.0.1:57305 room list
```

Commands: `room`, `clock`, `ticker`, `candles`, `account`, `member`, `observe`,
`agent`, `order`, `training`, `replay`. Bearer token, `x-user-id`, idempotency
keys, and trusted owner URLs are flags. Credentials are not logged.

## Python SDK and batch runner

```sh
PYTHONPATH=python python3 python/examples/buy_remaining.py http://127.0.0.1:57305 ROOM_ID 20 1
python3 scripts/batch_runner.py http://127.0.0.1:57305 scripts/fixtures/p5_batch_spec.json 1 2 --state /tmp/batch-state.json
```

The SDK talks only to HTTP. It does not open the database or internal actors.

## Validation

Development and test builds disable source-level debug information (`debug = 0`)
while retaining incremental compilation (`incremental = true`). Windows MSVC may
still emit smaller PDBs with function symbols and standard-library information.
Existing symbols and caches are not automatically removed.
For source-level debugging, temporarily enable symbols for the relevant profile:
`cargo --config profile.dev.debug=2 build` or
`cargo --config profile.test.debug=2 test`.

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
export MARKETFORGE_TEST_DATABASE_URL='postgres://marketforge:marketforge@127.0.0.1:55432/marketforge'
export MARKETFORGE_REQUIRE_POSTGRES_TESTS=1
cargo test --workspace
MARKETFORGE_DATABASE_URL="$MARKETFORGE_TEST_DATABASE_URL" ./scripts/postgres_smoke.sh
MARKETFORGE_DATABASE_URL="$MARKETFORGE_TEST_DATABASE_URL" ./scripts/postgres_multi_active_smoke.sh
./scripts/backend_training_smoke.sh
python3 -m compileall -q python
```

Declared-load continuous run (override duration; default is 86400 seconds):

```sh
MARKETFORGE_SOAK_SECONDS=60 ./scripts/backend_soak.sh
```

The GitHub backend workflow runs PostgreSQL recovery tests with
`MARKETFORGE_TEST_DATABASE_URL` configured and
`MARKETFORGE_REQUIRE_POSTGRES_TESTS=1`, plus Python SDK compile and the
training smoke.

Frontend `npm` build is optional for backend work.

## Documentation

- [Design](docs/DESIGN.md)
- [Backend execution plan (Chinese)](docs/BACKEND_EXECUTION_PLAN.md)
- [Backend follow-up execution plan (Chinese)](docs/BACKEND_FOLLOWUP_EXECUTION_PLAN.md)
- [Runtime contract](docs/RUNTIME_CONTRACT.md)
- [HTTP API contract](docs/API_CONTRACT.md)
- [双向持仓配置与开平仓](docs/HEDGE_MODE.md)
- [Backend storage and runtime configuration](docs/BACKEND_STORAGE.md)
- [Bot plugins and installation](docs/BOT_PLUGINS.md)
- [Pine background strategies and mixed market](docs/PINE_BOTS.md)
- [Historical storage, archives, and capacity validation](docs/HISTORICAL_STORAGE.md)
- [CandleScope adapter](marketforge-candlescope-plugin/README.md)
- [CandleScope durable workbench](docs/CANDLESCOPE_WORKBENCH.md)

## License

Copyright 2026 MarketForge contributors.

MarketForge is licensed under the [Apache License, Version 2.0](LICENSE).
Third-party dependencies remain under their respective licenses.
