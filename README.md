# MarketForge

MarketForge is a Rust market simulation engine with a standalone HTTP backend,
a React web interface, and a CandleScope plugin adapter.

It models spot and perpetual markets, order matching, accounts and portfolios,
margin and liquidation, venue rules, transfers, and replayable room events.
The backend supports PostgreSQL journal persistence and room writer leases.

## Components

- `exchange-core`: matching, risk, simulation, and room state.
- `exchange-server`: HTTP API, authentication, and durable journals.
- `marketforge-candlescope-plugin`: embedded and remote CandleScope integration.
- `marketforge-web`: React/TypeScript interface.

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

## Validation

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cd marketforge-web
npm ci
npm run build
```

The GitHub backend workflow also runs PostgreSQL recovery tests with
`MARKETFORGE_TEST_DATABASE_URL` configured and
`MARKETFORGE_REQUIRE_POSTGRES_TESTS=1`.

## Documentation

- [Design](docs/DESIGN.md)
- [Backend storage and runtime configuration](docs/BACKEND_STORAGE.md)
- [CandleScope adapter](marketforge-candlescope-plugin/README.md)

## License

Copyright 2026 MarketForge contributors.

MarketForge is licensed under the [Apache License, Version 2.0](LICENSE).
Third-party dependencies remain under their respective licenses.
