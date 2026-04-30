# TBD RFQ Maker Runtime

Hackathon Solana RFQ maker runtime for managed liquidity demos.

The project is focused on operator-side liquidity work, not a web swap frontend.
It keeps local inventory state for USDC, SOL, and cbBTC, exposes RFQ endpoints,
records events and ledger movement in SQLite, and has integration work for
Solana HTLCs, Jupiter Swap API V2, and Circle Gateway.

This is hackathon code. Treat live paths as demo flows to verify with small
amounts in your own environment, not as production infrastructure.

## What Is In This Repo

- Rust single crate.
- Axum HTTP API.
- Ratatui/Crossterm TUI when stdout is a terminal.
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

The default HTTP bind in `config.example.toml` is `127.0.0.1:8080`.

`config.example.toml` currently sets `runtime.enable_protocol_workers = false`.
With that default, the runtime boots the local projection/API/TUI without
attaching live protocol workers. Enable protocol workers only after configuring
wallets, RPC, API keys, caps, and a verified cbBTC Solana mint.

## Configuration

Secrets belong in `.env`. Do not commit local wallet keys or runtime state.

Common env values:

- `SOLANA_RPC_URL`
- `MAKER_PRIVATE_KEY`, `MAKER_KEYPAIR_PATH`, or `MAKER_KEYPAIR_JSON`
- `TAKER_PRIVATE_KEY`, `TAKER_KEYPAIR_PATH`, or `TAKER_KEYPAIR_JSON`
- `JUPITER_API_KEY`
- `CIRCLE_GATEWAY_SOLANA_ADDRESS`

Policy and non-secret settings live in `config.toml`.

Default safety caps in the sample config:

- `$2` max notional per action
- `$15` cumulative automated notional per run
- `$5` cbBTC exception cap for route minimums

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

## TUI

The TUI is the demo surface for operators. It reads the runtime projection and
shows balances, RFQs, liquidity state, risk decisions, ledger/P&L data, and
rebalance or hedge activity when those paths are present in the runtime state.

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
