# Firmament Landing

Next.js landing page for Firmament inside the main monorepo.

The landing page explains the Solana RFQ maker runtime and submission story. It
is separate from the Rust backend and is not served by `cargo run`.

## Run Locally

From the monorepo root:

```bash
npm run landing:dev
```

Open:

```text
http://127.0.0.1:3001
```

From this folder directly:

```bash
npm install
npm run dev
```

## Scripts

```bash
npm run dev
npm run build
npm run start
npm run lint
```

## Positioning

Firmament is a Solana RFQ maker runtime for managed liquidity operations:

- USDC/SOL/cbBTC working inventory
- firm RFQ quotes
- risk-aware quote rejection
- browser-wallet Solana HTLC settlement demo
- Jupiter Swap API V2 rebalancing
- Circle Gateway USDC refill
- SQLite double-entry ledger and P&L estimates
- minimal swap demo plus public read-only Runtime stats
- Axum HTTP API for runtime inspection

Keep claims demo-scoped unless the backend runtime has been updated to support
stronger production claims.
