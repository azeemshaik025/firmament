# Firmament Landing Page Agent Brief

## Product Summary

Firmament is a firm-quote FX maker for stablecoin payments on Solana. A payment
app asks for an exact recipient amount in EURC, gets the USDC cost held for 30
seconds, and settles both legs atomically over Solana HTLCs, to the sender or
to a named recipient. Underneath: policy-gated RFQs, EURC/USDC/SOL working
inventory, Jupiter rebalancing, Circle Gateway USDC refill, and a double-entry
SQLite ledger.

The landing page is a submission narrative surface. The Pay page and the maker
page remain the proof.

## Colosseum Submission Positioning

This project is entered in the **Crypto World's Fair** hackathon by Colosseum
(Sep 14–Oct 12, 2026), category Payments & Remittance. Everything built before
Sep 14, 2026 was built for the Solana Frontier Hackathon and is disclosed as
prior work; the World's Fair entry adds EURC, exact-output quotes, third-party
recipients, the Pay page, and the redeploy.

Optimize all landing and submission copy for Colosseum's judging criteria:

- **Functionality:** a live mainnet payment with exact recipient amounts.
- **Potential impact:** the FX leg under every multi-currency stablecoin
  payment app on Solana.
- **Novelty:** exact-output firm quotes with atomic settlement; nobody else is
  the maker under the payment.
- **UX:** the number shown before signing is the number delivered.
- **Open-source/composability:** MIT/Apache runtime any app can self-host; a
  two-call HTTP API.
- **Business plan:** spread on conversions; payment, payroll, and remittance
  apps as customers; makers can run their own desk.

The core submission claim:

> The rate you promise is the rate they get.

The insight to repeat: USDC has ~$465M of Solana DEX liquidity and EURC ~$0.7M
(Jupiter, Oct 2026), so any app promising a euro amount through an AMM is
guessing.

For pitch/video language, spend less time on "we integrated Jupiter/Circle/HTLC"
and more time on the promise a payment app can finally make.

## Monorepo Context

- Root repo: `/Users/azeemshaik/work/hackathons/firmament`.
- Backend: Rust Axum API/runtime, started with `cargo run`.
- Swap demo: Vite React app in `web/app`, started from root with `npm run dev`.
- Landing: this Next.js app in `landing`, started from root with
  `npm run landing:dev`.
- The Rust backend does not serve frontend files.

## Messaging Principles

- Lead with "firm FX for stablecoin payments" and the exact-amount promise.
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
