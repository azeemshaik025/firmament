# Firmament RFQ Maker Runtime

Firmament is a Solana RFQ maker runtime for managed liquidity demos.

The project is focused on the backend solver/runtime, not on making a full
consumer DEX product. The web app is a minimal demo surface that proves the
runtime can request firm quotes, drive wallet settlement, and expose read-only
operator stats.

Default local mode is safe: it starts the API and runtime projection with live
protocol workers disabled. Live maker mode requires real credentials, a funded
maker wallet, verified asset mints, and explicit caps.

## What Is In This Repo

- Rust single crate for the maker runtime and Axum HTTP API.
- Vite React web app in `web/app`.
- Next.js landing page in `landing`.
- SQLite persistence through `rusqlite` for ledger, P&L, and trade/settlement
  recovery state.
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

Set exactly one maker keypair source before enabling live workers.

Policy and non-secret settings live in `config.toml`.
Supported assets are cross-quoteable by default. Use
`[[assets.blacklisted_pairs]]` rows only for directional pairs you want to
disable.

Default safety caps in the sample config:

- `$2` max notional per action
- `$15` cumulative automated notional per run
- `$5` non-native, non-stable asset exception cap for route minimums
- user-entered RFQ ranges: `1-2 USDC`, `0.01-0.02 SOL`, and `0.00001-0.00002 cbBTC`

Before any live cbBTC run, replace the sample placeholder mint with the Solana
mint listed by Coinbase:

```text
cbbtcf3aa214zXHbiAZQwf4122FBYbraNdFqgw4iMij
```

Check current Jupiter route minimums before raising caps.

`config.example.toml` currently sets `runtime.enable_protocol_workers = false`.
With that default, the backend boots the local projection/API without attaching
live protocol workers. Enable protocol workers only after configuring wallets,
RPC, API keys, caps, and a verified cbBTC Solana mint.

Full maker setup lives in [docs/maker-setup.mdx](/Users/azeemshaik/work/hackathons/firmament/docs/maker-setup.mdx).

## HTTP API

- `GET /health`
- `GET /v1/assets`
- `GET /v1/pairs`
- `POST /v1/rfq`
- `POST /v1/quotes/{quote_id}/wallet-settlement`
- `POST /v1/trades/{trade_id}/resume`
- `POST /v1/trades/{trade_id}/abandon`
- `POST /v1/trades/{trade_id}/taker-lock`
- `POST /v1/trades/{trade_id}/taker-redeem`
- `POST /v1/trades/{trade_id}/taker-refund`
- `GET /v1/trades/{trade_id}`
- `GET /v1/runtime/state`
- `GET /v1/runtime/events`
- `GET /v1/runtime/ledger`
- `GET /v1/runtime/trades`

When the live orchestrator is not attached, mutating RFQ routes return an
unavailable response instead of pretending to settle.

## Web App

The web app is intentionally minimal:

- `/app` is the swap demo surface.
- `/app/runtime` is the public read-only Live Runtime summary with global recent
  trades.
- Public takers connect a Solana browser wallet and use the wallet-settlement
  endpoints.
- The swap view keeps browser-local recovery state for the connected wallet,
  including a wallet-specific trade history drawer. Preimages stay in the
  browser and are never persisted by the server.
- Wallet-submitted lock/redeem/refund signatures are accepted only after the
  backend confirms the expected Solana HTLC account effect.

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
npm run web:polish
```

`npm run web:polish` catches deploy-facing regressions such as late font imports,
missing favicon metadata, or missing submission-surface checks across the
landing page, app, and docs.

## Tests

Safe default test run:

```bash
cargo test
```

Live tests skip cleanly unless their `RUN_LIVE_*` variables are set. Mutating
live tests require explicit opt-in variables and tiny mainnet caps.

Useful smoke groups:

```bash
scripts/smoke.sh unit
scripts/smoke.sh api
scripts/smoke.sh all-non-mutating
```

Mutating smoke groups are real mainnet actions. Read `scripts/smoke.sh` and set
the required opt-in env vars before running them.

## External References

- [Jupiter Swap API V2 Order & Execute](https://developers.jup.ag/docs/swap/v2/order-and-execute)
- [Jupiter Swap API V2 Build](https://developers.jup.ag/docs/swap/v2/build)
- [Circle Gateway Solana Quickstart](https://developers.circle.com/gateway/quickstarts/unified-balance-solana)
- [Circle Gateway Technical Guide](https://developers.circle.com/gateway/references/technical-guide)
- [Coinbase cbBTC network addresses](https://www.coinbase.com/cbbtc)
