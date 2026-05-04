# Firmament RFQ Maker Runtime Implementation Brief

## Summary

Build the hackathon project in this repository:

```text
/Users/azeemshaik/work/hackathons/firmament
```

Firmament is a **Solana-only RFQ Maker Runtime** proving the liquidity-layer thesis: apps and treasuries can maintain managed USDC/SOL/cbBTC inventory, expose firm RFQ liquidity, reject unsafe flow, settle through live Solana HTLCs, rebalance through Jupiter, refill USDC through Circle Gateway, and supervise everything from the web app and HTTP API operator surfaces.

Primary demo proof: **managed liquidity operations**, not a web swap frontend.

Default command:

```bash
cargo run --release
```

This starts the HTTP API, live workers, SQLite persistence, and web operator surface.

## Key Implementation Decisions

- **Stack:** Rust single crate, Axum for HTTP API, SQLite for local durable state, and web app operator surface.
- **Mode:** Mainnet tiny-amount live demo. No simulated fallback in the spec.
- **Assets:** v1 supports USDC, SOL, and cbBTC. All three assets are mandatory for the hackathon demo.
- **Pairs:** support USDC<->SOL, USDC<->cbBTC, and SOL<->cbBTC.
- **Settlement:** Solana-only HTLC model, reusing Munger's existing native/SPL HTLC program IDs and adapting client/encoding code.
- **Wallets:** two demo wallets: maker/operator wallet and taker/app wallet, both configured through `.env`.
- **Fill mode:** inventory-first. The solver fills accepted RFQs from working inventory, then rebalances after.
- **Rebalancing:** live Jupiter-based drift correction, native SOL top-up, and live Circle Gateway USDC refill.
- **Gateway path:** Solana Gateway balance only: deposit USDC into Gateway from Solana, then mint/refill to solver working wallet.
- **Ledger:** simple append-only double-entry ledger in SQLite with accounts like Working, HTLC Escrow, Rebalance, Fees, and P&L.
- **P&L:** token ledger entries plus USDC-estimated realized spread, fees, and rebalance costs using reference prices.

## Runtime Shape

Implement these modules:

- `config`: loads `.env` secrets and `config.toml` policy.
- `assets`: USDC/SOL/cbBTC metadata, decimals, mint addresses, HTLC program mapping.
- `wallets`: maker and taker Solana keypairs, ATA checks/creation, balance reads.
- `htlc`: initiate, validate, redeem, refund, and query for native/SPL HTLCs.
- `jupiter`: Swap API V2 pricing and live swaps for rebalancing.
- `gateway`: Circle Gateway Solana deposit, balance check, transfer/mint refill.
- `inventory`: current balances, target allocation, quoteable thresholds, drift.
- `quote_engine`: reference price, base spread, inventory skew, fees, min profit.
- `risk`: allowlist, max notional, inventory threshold, exposure limits, stale price checks.
- `ledger`: SQLite entries for quote, fill, HTLC escrow, fees, rebalance, and P&L.
- `runtime`: event bus and state projection consumed by API and web app.
- `frontend`: web app operator console.

Use official docs as implementation references:

- [Jupiter Swap API V2 Order & Execute](https://developers.jup.ag/docs/swap/v2/order-and-execute)
- [Jupiter Swap API V2 Build](https://developers.jup.ag/docs/swap/v2/build)
- [Circle Gateway Overview](https://developers.circle.com/gateway)
- [Circle Gateway Solana Quickstart](https://developers.circle.com/gateway/quickstarts/unified-balance-solana)

Important current API note: Jupiter Ultra is deprecated/superseded; use Swap API V2.

## Interfaces

Expose HTTP API for the web app operator surface:

- `POST /v1/rfq`
  - input: `input_mint`, `output_mint`, `input_amount_raw`, `taker_wallet`, optional `expiry_seconds`
  - output accepted: quote id, quoted output amount, spread bps, expiry, HTLC acceptance terms
  - output rejected: rejection reason and risk check details
- `POST /v1/quotes/{quote_id}/accept`
  - starts the live two-wallet demo HTLC settlement flow.
- `GET /v1/trades/{trade_id}`
  - returns settlement status, tx signatures, amounts, and ledger summary.
- `GET /v1/runtime/state`
  - returns inventory, risk, P&L, active RFQs, rebalances, and recent events.
- `GET /v1/runtime/events`
  - returns recent runtime events for external app/demo inspection.

Web app operator views:

- **Overview:** balances, target allocation, drift, Gateway status, live health.
- **RFQs:** incoming requests, accepted/rejected quotes, current settlement.
- **Liquidity:** working inventory, Gateway balance, native SOL buffer, thresholds.
- **Risk:** active limits, failed checks, rejection reasons, exposure.
- **Ledger/P&L:** double-entry movements, realized spread, fees, net USDC estimate.
- **Rebalance:** pending/completed Jupiter swaps and Gateway refills.

Web app operator controls:

- generate normal tiny RFQ
- generate oversized RFQ to show inventory-threshold rejection
- accept quote
- trigger rebalance check
- trigger Gateway refill check
- quit gracefully

## Demo Scenario

1. Start runtime with funded maker/taker wallets and tiny caps: default max `$2` per action, max `$15` cumulative automated spend per demo run, and a documented one-off `$5` cbBTC exception if route minimums require it.
2. Web app opens on Overview/Liquidity Cockpit.
3. Operator triggers a normal USDC->SOL RFQ.
4. Solver fetches Jupiter reference price, applies spread/inventory/risk checks, and returns a firm quote.
5. Taker accepts; live Solana HTLC settlement runs.
6. Ledger records escrow, fill, fees, spread, and balance changes.
7. Inventory drift appears in the web app.
8. Runtime performs Jupiter rebalance if thresholds are crossed.
9. Runtime performs Circle Gateway Solana USDC refill if working USDC falls below threshold.
10. Operator triggers an oversized RFQ; solver rejects it with "inventory below quoteable threshold."
11. Web app shows final liquidity status, P&L, risk decision, and tx signatures.

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
  - one successful quote/fill/rebalance/refill is visible in the web app.
  - one oversized RFQ is rejected with a human-readable reason.
  - P&L and ledger remain internally balanced after the run.

## Assumptions And Defaults

- Project name is `Firmament`.
- The submission frames the project for **Solana apps and treasuries**, not primarily professional market makers.
- Mainnet tiny amounts are acceptable for demo credibility and financial safety.
- Existing Munger Solana HTLC programs are usable on mainnet and should be reused rather than redeployed.
- cbBTC mint/liquidity must be verified through Jupiter token search/config during implementation; if route minimums are problematic, use the documented `$5` cbBTC exception rather than cutting cbBTC.
- `.env` stores secrets: Solana RPC URL, maker keypair, taker keypair, Jupiter API key, Circle/Gateway credentials if required.
- `config.toml` stores non-secret policy: assets, caps, spreads, inventory targets, risk limits, Gateway thresholds.
- The web frontend is the product/demo interface.
