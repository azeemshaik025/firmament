# Firmament RFQ Maker Runtime

Firmament is a hackathon Solana RFQ maker runtime for managed liquidity demos.

The project is focused on the backend solver/runtime, not on making a full
consumer DEX product. The web app is a minimal demo surface that proves the
runtime can request firm quotes, drive wallet settlement, and expose read-only
operator stats.

## What Is In This Repo

- Rust single crate for the maker runtime and Axum HTTP API.
- Vite React web app in `web/app`.
- Next.js landing page in `landing`.
- SQLite persistence through `rusqlite`.
- Domain modules for assets, inventory, quote math, risk, settlement, events,
  ledger, and P&L projection.
- Adapters for Solana wallets/HTLCs, Jupiter Swap API V2, and Circle Gateway.
- Unit and integration tests that run without live credentials.
- Env-gated live smoke tests for mainnet-only tiny-amount checks.

## Running Locally

Start the backend API/runtime:

```bash
cp .env.example .env
cp config.example.toml config.toml
cargo run
```

The default backend bind is `127.0.0.1:5050`.

In a second terminal, start the frontend:

```bash
npm run dev
```

The first `npm run dev` installs `web/app` dependencies if needed, then starts
Vite at `http://127.0.0.1:3000/app`. The frontend proxies `/v1` requests to
`http://127.0.0.1:5050`.

Override the frontend port or API target if needed:

```bash
FIRMAMENT_WEB_PORT=3002 npm run dev
FIRMAMENT_API_PROXY_TARGET=http://127.0.0.1:5050 npm run dev
```

Start the landing page separately when needed:

```bash
npm run landing:dev
```

The landing page runs at `http://127.0.0.1:3001` by default so it does not
fight the swap demo for port `3000`.

## Configuration

Secrets belong in `.env`. Do not commit local wallet keys or runtime state.

Common env values:

- `SOLANA_RPC_URL`
- `MAKER_PRIVATE_KEY`, `MAKER_KEYPAIR_PATH`, or `MAKER_KEYPAIR_JSON`
- `JUPITER_API_KEY`
- `CIRCLE_GATEWAY_SOLANA_ADDRESS`

Policy and non-secret settings live in `config.toml`.
Supported assets are cross-quoteable by default. Use
`[[assets.blacklisted_pairs]]` rows only for directional pairs you want to
disable.

Default safety caps in the sample config:

- `$2` max notional per action
- `$15` cumulative automated notional per run
- `$5` non-native, non-stable asset exception cap for route minimums

Before any live cbBTC run, replace the sample placeholder mint only after
verifying the current Solana cbBTC mint and route minimums.

`config.example.toml` currently sets `runtime.enable_protocol_workers = false`.
With that default, the backend boots the local projection/API without attaching
live protocol workers. Enable protocol workers only after configuring wallets,
RPC, API keys, caps, and a verified cbBTC Solana mint.

## HTTP API

- `POST /v1/rfq`
- `POST /v1/quotes/{quote_id}/wallet-settlement`
- `POST /v1/trades/{trade_id}/taker-lock`
- `POST /v1/trades/{trade_id}/taker-redeem`
- `GET /v1/trades/{trade_id}`
- `GET /v1/runtime/state`
- `GET /v1/runtime/events`
- `GET /v1/admin/summary`

When the live orchestrator is not attached, mutating RFQ routes return an
unavailable response instead of pretending to settle.

## Web App

The web app is intentionally minimal:

- `/app` is the swap demo surface.
- `/app/admin` is a password-gated read-only runtime summary.
- Public takers connect a Solana browser wallet and use the wallet-settlement
  endpoints.

The backend does not serve frontend files. Keep `cargo run` and `npm run dev`
running in separate terminals during demos.

## Landing

The landing page lives in `landing/` as part of this monorepo. It is a Next.js
project used for project positioning and submission material. It is not served
by the Rust backend.

Useful commands:

```bash
npm run landing:dev
npm run landing:build
```

Admin login uses Argon2id PHC password hashes from
`FIRMAMENT_ADMIN_PASSWORD_HASHES` and an HttpOnly signed session cookie using
`FIRMAMENT_ADMIN_SESSION_SECRET`.

## Tests

Safe default test run:

```bash
cargo test
```

Live tests skip cleanly unless their `RUN_LIVE_*` variables are set. Mutating
live tests require explicit opt-in variables and tiny mainnet caps.
