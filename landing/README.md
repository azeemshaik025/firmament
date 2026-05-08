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
npm ci
npm run dev
```

## Scripts

```bash
npm run dev
npm run build
npm run start
npm run lint
```

The landing page uses `next/font` from `src/app/layout.tsx` for the display,
body, and mono faces. Keep remote CSS font imports out of `globals.css`; the
root `npm run web:polish` check enforces this so the hero wordmark does not swap
late after first paint.

The landing favicon is owned by `src/app/favicon.ico` and `src/app/icon.svg`.
Keep both aligned with the Firmament mark so deployed previews never fall back
to the platform default icon.

After `npm run build`, `npm run start` stages static assets beside the generated
standalone Next.js server and serves it. Use `PORT=3001 npm run start` when
previewing it locally.

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
