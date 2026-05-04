# Firmament RFQ Maker Runtime

Firmament is a hackathon Solana RFQ maker runtime for managed liquidity demos.

The project is focused on operator-side liquidity work, not a web swap frontend.
It keeps local inventory state for USDC, SOL, and cbBTC, exposes RFQ endpoints,
records events and ledger movement in SQLite, and has integration work for
Solana HTLCs, Jupiter Swap API V2, and Circle Gateway.

This is hackathon code. Treat live paths as demo flows to verify with small
amounts in your own environment, not as production infrastructure.

## What Is In This Repo

- Rust single crate.
- Axum HTTP API.
- Maker runtime binary for HTTP/API/workers.
- Web app and HTTP API operator surfaces.
- Static hackathon landing page in `web/landing`.
- SQLite persistence through `rusqlite`.
- Domain modules for assets, inventory, quote math, risk, settlement, events,
  ledger, and P&L projection.
- Adapters for Solana wallets/HTLCs, Jupiter, and Circle Gateway.
- Unit and integration tests that run without live credentials.
- Env-gated live smoke tests for mainnet-only tiny-amount checks.

## Running Locally

```bash
cp .env.example .env
cp config.example.toml config.toml
cargo run --release
```

This starts the maker runtime only. The default HTTP bind in
`config.example.toml` is `127.0.0.1:8080`.

For the full demo flow, start the maker runtime and use the web app or HTTP API:

```bash
scripts/run_demo.sh
```

The script builds release binaries, starts the maker runtime in the background,
waits 5 seconds for warmup, and leaves the web app and API ready for demo
operations.
Set `FIRMAMENT_WARMUP_SECONDS=10` if your RPC or machine needs a longer warmup.

The landing page can be opened directly from:

```text
web/landing/dist/index.html
```

`config.example.toml` currently sets `runtime.enable_protocol_workers = false`.
With that default, the maker runtime boots the local projection/API without
attaching live protocol workers, and the web app can render projection-only state.
Enable protocol workers only after configuring wallets, RPC, API keys, caps,
and a verified cbBTC Solana mint.

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

## HTTP API

- `POST /v1/rfq`
- `POST /v1/quotes/{quote_id}/accept`
- `GET /v1/trades/{trade_id}`
- `GET /v1/runtime/state`
- `GET /v1/runtime/events`

When the live orchestrator is not attached, mutating RFQ routes return an
unavailable response instead of pretending to settle.

## Web App And API Operator Surface

The web app is the taker/demo surface:

```bash
scripts/run_demo.sh
```

It starts the maker runtime, then the web app or HTTP API can read the runtime
projection and show balances, RFQs, liquidity state, risk decisions, ledger/P&L
data, rebalance activity, and Gateway refill state. Taker settlement uses a
connected Solana browser wallet.

## Tests

Safe default test run:

```bash
cargo test
```

Smoke helpers:

```bash
scripts/smoke.sh unit
scripts/smoke.sh fake-e2e
scripts/smoke.sh api
scripts/smoke.sh all-non-mutating
```

Live tests skip cleanly unless their `RUN_LIVE_*` variables are set. Mutating
live tests also check amount caps in `scripts/smoke.sh` before they run.

Read `scripts/smoke.sh` before running a mutating group.

## Web App Notes

Firmament runs as a backend API/runtime plus two web builds: `web/landing` for `/` and `web/app` for `/app` and `/app/admin`.

Public takers connect a Solana browser wallet and use the wallet-settlement endpoints. The backend no longer requires a configured taker keypair for the web app path.

Admin login uses Argon2id PHC password hashes from `FIRMAMENT_ADMIN_PASSWORD_HASHES` and an HttpOnly signed session cookie using `FIRMAMENT_ADMIN_SESSION_SECRET`.
