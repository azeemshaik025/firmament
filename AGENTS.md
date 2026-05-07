# Firmament RFQ Maker Runtime Implementation Brief

## Summary

Build the hackathon project in this repository:

```text
/Users/azeemshaik/work/hackathons/firmament
```

Firmament is a **Solana-only RFQ Maker Runtime** proving the liquidity-layer thesis: apps and treasuries can maintain managed USDC/SOL/cbBTC inventory, expose firm RFQ liquidity, reject unsafe flow, settle through live Solana HTLCs, rebalance through Jupiter, refill USDC through Circle Gateway, and supervise everything from the web app and HTTP API operator surfaces.

Primary demo proof: **managed liquidity operations**, not a web swap frontend.

## Colosseum Frontier Hackathon Alignment

Target hackathon: **Solana Frontier Hackathon**, run by Colosseum and presented
by Solana. The contest runs April 6-May 11, 2026, with product submissions due
by 11:59pm PT on May 11, 2026. Treat this as a startup sprint, not a sponsor
bounty sprint: Colosseum explicitly removed tracks and bounties for Frontier.

Important references:

- [Frontier announcement](https://blog.colosseum.com/announcing-the-solana-frontier-hackathon/)
- [Official rules](https://colosseum.com/legal/Solana%20Frontier%20Hackathon%20Rules.pdf)
- [How to Win a Colosseum Hackathon](https://blog.colosseum.com/how-to-win-a-colosseum-hackathon/)

Prize strategy:

- Aim first for one of the 20 standout startup prizes, with Grand Champion
  upside if the demo and business story are unusually crisp.
- Accelerator fit matters: winners are evaluated for Colosseum's accelerator,
  where accepted teams receive pre-seed funding and founder support.
- Public Goods positioning is secondary unless a specific open-source runtime
  standard or reusable Solana liquidity primitive is made central.

Judge-facing rubric to optimize for:

- **Functionality/code quality:** show the runtime actually working, with live
  or credibly recorded Solana proof, clear error handling, and a reliable demo.
- **Potential impact:** frame Firmament as liquidity infrastructure for Solana
  apps and treasuries that need embedded, managed execution rather than another
  swap UI.
- **Novelty:** emphasize inventory-aware firm RFQs, risk rejection, rebalancing,
  Gateway refill, ledger/P&L, and operator supervision as the differentiated
  liquidity-operations layer.
- **UX:** make the taker flow simple, but make the Live Runtime proof clear:
  inventory, risk, settlement state, rebalances, Gateway state, and accounting.
- **Open-source/composability:** keep the HTTP API and runtime modules easy for
  other Solana apps to integrate or fork.
- **Business plan:** explain who pays: Solana apps, treasuries, protocols, and
  payment/commerce teams that need controlled liquidity without building maker
  infrastructure in-house.

Narrative to keep consistent across README, landing, video, and submission:

> Firmament turns Solana liquidity from ad hoc swap routing into managed
> inventory infrastructure for apps and treasuries: firm quotes, policy-aware
> flow rejection, live settlement proof, automated inventory repair, USDC refill,
> and operator-grade accounting.

Default command:

```bash
cargo run
```

This starts the backend HTTP API, live workers when enabled, and SQLite persistence.
Run the frontend separately with `npm run dev`; the Vite app proxies `/v1` to
the backend API.
Run the landing page separately with `npm run landing:dev`.

## Key Implementation Decisions

- **Stack:** Rust single crate, Axum for HTTP API, SQLite for local durable state, and web app operator surface.
- **Mode:** Mainnet tiny-amount live demo. No simulated fallback in the spec.
- **Assets:** v1 supports USDC, SOL, and cbBTC. All three assets are mandatory for the hackathon demo.
- **Pairs:** support USDC<->SOL, USDC<->cbBTC, and SOL<->cbBTC.
- **Settlement:** Solana-only HTLC model, reusing Munger's existing native/SPL HTLC program IDs and adapting client/encoding code.
- **Wallets:** maker/operator wallet is configured through `.env`; takers use a browser Solana wallet in the web demo. Legacy local-signing taker keypairs are optional test-only inputs.
- **Fill mode:** inventory-first. The solver fills accepted RFQs from working inventory, then rebalances after.
- **Rebalancing:** live Jupiter-based drift correction, native SOL top-up, and live Circle Gateway USDC refill.
- **Gateway path:** Solana Gateway balance only: deposit USDC into Gateway from Solana, then mint/refill to solver working wallet.
- **Ledger:** simple append-only double-entry ledger in SQLite with accounts like Working, HTLC Escrow, Rebalance, Fees, and P&L.
- **P&L:** token ledger entries plus USDC-estimated realized spread, fees, and rebalance costs using reference prices.

## Runtime Shape

Implement these modules:

- `config`: loads `.env` secrets and `config.toml` policy.
- `assets`: USDC/SOL/cbBTC metadata, decimals, mint addresses, HTLC program mapping.
- `wallets`: maker Solana keypair loading, optional legacy taker keypairs for tests, ATA checks/creation, balance reads.
- `htlc`: initiate, validate, redeem, refund, and query for native/SPL HTLCs.
- `jupiter`: Swap API V2 pricing and live swaps for rebalancing.
- `gateway`: Circle Gateway Solana deposit, balance check, transfer/mint refill.
- `inventory`: current balances, target allocation, quoteable thresholds, drift.
- `quote_engine`: reference price, base spread, inventory skew, fees, min profit.
- `risk`: allowlist, max notional, inventory threshold, exposure limits, stale price checks.
- `ledger`: SQLite entries for quote, fill, HTLC escrow, fees, rebalance, and P&L.
- `runtime`: event bus and state projection consumed by API and web app.
- `frontend`: minimal Vite swap demo and public read-only Live Runtime surface.
- `landing`: Next.js project positioning page for submission material.

Use official docs as implementation references:

- [Jupiter Swap API V2 Order & Execute](https://developers.jup.ag/docs/swap/v2/order-and-execute)
- [Jupiter Swap API V2 Build](https://developers.jup.ag/docs/swap/v2/build)
- [Circle Gateway Overview](https://developers.circle.com/gateway)
- [Circle Gateway Solana Quickstart](https://developers.circle.com/gateway/quickstarts/unified-balance-solana)

Important current API note: Jupiter Ultra is deprecated/superseded; use Swap API V2.

## Interfaces

Expose HTTP API for the web app:

- `POST /v1/rfq`
  - input: `input_mint`, `output_mint`, `input_amount_raw`, `taker_wallet`, optional `expiry_seconds`
  - output accepted: quote id, quoted output amount, spread bps, expiry, HTLC acceptance terms
  - output rejected: rejection reason and risk check details
- `POST /v1/quotes/{quote_id}/wallet-settlement`
  - starts the browser-wallet settlement flow.
- `POST /v1/trades/{trade_id}/taker-lock`
  - records the taker wallet lock signature.
- `POST /v1/trades/{trade_id}/taker-redeem`
  - prepares or records the taker wallet redeem signature.
- `GET /v1/trades/{trade_id}`
  - returns settlement status, tx signatures, amounts, and ledger summary.
- `GET /v1/runtime/state`
  - returns inventory, risk, P&L, active RFQs, rebalances, and recent events.
- `GET /v1/runtime/events`
  - returns recent runtime events for external app/demo inspection.
- `GET /v1/runtime/ledger`
  - returns public read-only ledger/accounting balances for the runtime UI.
- `GET /v1/runtime/trades`
  - returns public read-only recent trade summaries and aggregate counts.

Web app views:

- **Swap:** minimal DEX-style demo page with browser wallet connect, asset/amount
  validation, firm quote request, wallet settlement steps, notification bar, and
  a small user-friendly runtime proof panel.
- **Live Runtime:** public read-only runtime summary for inventory, risk,
  ledger/P&L, RFQs, rebalances, Gateway state, and recent events.

## Demo Scenario

1. Start backend with funded maker wallet and tiny caps: default max `$2` per action, max `$15` cumulative automated spend per demo run, and a documented one-off `$5` cbBTC exception if route minimums require it.
2. Start frontend with `npm run dev` and open `/app`. Start the landing page
   with `npm run landing:dev` only when reviewing submission positioning.
3. Taker connects a browser Solana wallet and requests the default tiny SOL->USDC RFQ.
4. Solver fetches Jupiter reference price, applies spread/inventory/risk checks, and returns a firm quote.
5. Taker accepts; live Solana HTLC settlement runs.
6. Ledger records escrow, fill, fees, spread, and balance changes.
7. Inventory drift appears in the web app.
8. Runtime performs Jupiter rebalance if thresholds are crossed.
9. Runtime performs Circle Gateway Solana USDC refill if working USDC falls below threshold.
10. Web app shows user-friendly settlement status and network proof; Live Runtime shows runtime inventory, ledger/P&L, rebalances, Gateway state, and recent events.

## Test Plan

- Development process:
  - Follow TDD for implementation work. For each independent component, write the tests first, confirm they fail for the right reason when feasible, then implement the smallest correct code until they pass.
  - Prefer unit tests for pure domain logic and protocol encoding/decoding. These should run without live credentials.
  - Do not mock everything by default. For external adapters, add env-gated live tests that run only when the required environment variables are present.
  - Live tests must be safe by default: if required env vars are missing, skip/return cleanly with a clear message instead of failing.
  - Mutating live tests must require an explicit opt-in env var and must honor tiny mainnet caps.
  - Every agent must run the relevant targeted tests for its component before handoff and report exactly what passed or was skipped.
- Unit tests:
  - quote math, inventory skew, risk rejection, ledger balancing, config parsing.
- Adapter tests:
  - Jupiter response parsing/signing path.
  - Circle Gateway Solana instruction/request construction.
  - HTLC encoding/decoding adapted from Munger.
- Live smoke tests with tiny mainnet caps:
  - balance read and ATA validation.
  - Jupiter quote and tiny swap.
  - Solana HTLC initiate/redeem/refund path.
  - Circle Gateway deposit/balance/refill path.
  - end-to-end RFQ accept flow with ledger entries.
- Manual demo acceptance:
  - one successful tiny SOL->USDC quote/fill is visible in the web app.
  - user-facing failures use simple swap language such as "No liquidity sources found."
  - Live Runtime stats show runtime inventory, events, and accounting after the run.
  - P&L and ledger remain internally balanced after the run.

## Assumptions And Defaults

- Project name is `Firmament`.
- The submission frames the project for **Solana apps and treasuries**, not primarily professional market makers.
- Mainnet tiny amounts are acceptable for demo credibility and financial safety.
- Existing Munger Solana HTLC programs are usable on mainnet and should be reused rather than redeployed.
- cbBTC mint/liquidity must be verified through Jupiter token search/config during implementation; if route minimums are problematic, use the documented `$5` cbBTC exception rather than cutting cbBTC.
- `.env` stores secrets: Solana RPC URL, maker keypair, Jupiter API key, Circle/Gateway credentials if required.
- `config.toml` stores non-secret policy: assets, caps, spreads, inventory targets, risk limits, Gateway thresholds.
- The web frontend is a separate Vite app used as the demo interface.
