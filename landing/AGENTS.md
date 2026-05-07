# Firmament Landing Page Agent Brief

## Product Summary

Firmament is a Solana-only RFQ maker runtime. It is backend liquidity
infrastructure for apps and treasuries that need managed USDC, SOL, and cbBTC
working inventory, firm quotes, risk-aware rejection, browser-wallet Solana HTLC
settlement, Jupiter-based rebalancing, Circle Gateway USDC refill, and local
SQLite ledger/P&L projection.

The landing page is a submission narrative surface. The runtime and demo swap
flow remain the proof.

## Colosseum Submission Positioning

This project targets the **Solana Frontier Hackathon** by Colosseum, running
April 6-May 11, 2026. Submissions are judged as startup products, not bounty
entries: there are no Frontier tracks or sponsor bounties to optimize for.

Optimize all landing and submission copy for Colosseum's judging criteria:

- **Functionality:** a working Solana runtime with a crisp demo path.
- **Potential impact:** embedded liquidity infrastructure for Solana apps and
  treasuries.
- **Novelty:** managed inventory, firm RFQs, risk-aware rejection, rebalancing,
  Gateway refill, ledger/P&L, and operator supervision.
- **UX:** a simple taker flow plus a public read-only Runtime surface that
  proves the runtime.
- **Open-source/composability:** clear HTTP API and reusable runtime modules.
- **Business plan:** sell to apps, treasuries, protocols, and payment/commerce
  teams that need controlled liquidity without building maker ops in-house.

The core submission claim:

> Firmament turns Solana liquidity from ad hoc swap routing into managed
> inventory infrastructure for apps and treasuries.

For pitch/video language, spend less time on "we integrated Jupiter/Circle/HTLC"
and more time on why the liquidity-operations layer is valuable, repeatable, and
business-shaped.

## Monorepo Context

- Root repo: `/Users/azeemshaik/work/hackathons/firmament`.
- Backend: Rust Axum API/runtime, started with `cargo run`.
- Swap demo: Vite React app in `web/app`, started from root with `npm run dev`.
- Landing: this Next.js app in `landing`, started from root with
  `npm run landing:dev`.
- The Rust backend does not serve frontend files.

## Messaging Principles

- Lead with "Solana RFQ Maker Runtime" and "managed liquidity operations."
- Explain inventory-first fills: quote from working inventory, then repair the
  book with rebalancing and Gateway refill.
- Use terms like "firm quotes," "risk-aware quoting," "Solana HTLC settlement,"
  "Jupiter Swap API V2," "Circle Gateway USDC refill," "SQLite ledger/P&L,"
  "public Runtime page," and "HTTP API."
- The swap page should be described as a small proof surface, not as the whole
  project.
- Keep claims concrete and demo-scoped.

## Design Principles

- The current direction is compact, high-contrast, block-game inspired:
  bold display type, hard borders, grid background, and sharp color accents.
- Keep layout responsive and legible on mobile.
- Avoid generic crypto gradients, token hype, and claims that imply production
  custody or audited infrastructure.

## What Must Not Be Misrepresented

- Do not imply broad production deployment, audited custody, audited smart
  contracts, or institutional-grade risk systems.
- Do not claim cbBTC route minimums or live liquidity are permanently solved
  beyond what the backend has verified.
- Do not claim automated flows run when `runtime.enable_protocol_workers` is
  disabled.
- Do not imply Circle Gateway or Jupiter integrations are owned by Firmament.
