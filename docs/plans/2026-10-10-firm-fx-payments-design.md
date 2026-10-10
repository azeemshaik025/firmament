# Firm FX for stablecoin payments

Design for the Crypto World's Fair submission (deadline 2026-10-12 23:59 PDT). Builds on the Frontier-era runtime; everything before 2026-09-14 is disclosed prior work.

## Pitch

The rate you promise is the rate they get. A payment app asks "pay 89.30 EURC to this address"; Firmament answers with the exact USDC cost, holds it for the quote window, and settles atomically over Solana HTLCs, so the recipient receives exactly that amount or the sender is refunded.

Why it matters: on Solana, USDC has ~$465M of DEX liquidity and EURC ~$0.7M (Jupiter, 2026-10-10). Any app that shows a recipient a euro amount before routing through an AMM is guessing. A maker that holds the non-USD inventory, quotes a locked rate, and refills from USDC through Circle Gateway removes the guess.

## Scope

In:

- EURC as a configured asset; USDC↔EURC in both directions, SOL kept for gas and existing pairs.
- Exact-output quotes ("recipient gets X") alongside the existing exact-input quotes.
- Optional third-party `recipient` on wallet settlement, so the maker leg pays someone other than the sender.
- A Pay page at `/app` (the swap page moves to `/app/swap`).
- Landing page rewritten for the pitch; API and web app redeployed on free platform domains.
- Demo video (Remotion, ≤3 min), pitch video (Loom, 2–3 min), submission.

Out: fiat rails, KYC, chains other than Solana, x402, new on-chain programs, cbBTC in the story (it stays configured).

Stretch, Tuesday morning only if the core demo is already recorded: a thin MCP server (`mcp/`, TypeScript) wrapping the Pay API with a local signing keypair, exposing one tool, `pay` (asset, amount, recipient), that runs quote → lock → redeem. Gives a 20-second demo clip and one pitch line: same API, humans or agents.

## Backend

### EURC asset

`config.example.toml` gains an `[[assets.supported]]` row: id `EURC`, mint `HzwqbKZw8HxMN6bF2yFZNrht3c2iXXzpKcFu7uBEDKtr` (SPL Token program, 6 decimals, verified on mainnet), target weights re-split to sum to one (USDC 0.45, SOL 0.30, EURC 0.20, cbBTC 0.05), `quoteable_threshold_raw` and trade limits at demo scale, plus a `[reconciliation.dust]` row. Three places classify assets by hard-coded id and must treat EURC as a stablecoin: the exposure-limit choice in `application/rfq.rs` (`"USDC" | "SOL"`), `is_non_stable` in `application/rebalance.rs`, and `RfqContext::from_config` which seeds only USDC's USD price (EURC's USD price arrives through the orchestrator's `usd_prices` map like any non-USDC asset). The Jupiter client derives each asset's reference amount from its decimals, so it needs no change. `AssetRegistry::default()` stays as it is; the live registry is config-driven.

### Exact-output quotes

`domain::quote_engine` gains `calculate_quote_exact_output(pair, output_amount_raw, reference_price, input_decimals, output_decimals, inventory, config)`. Math, all rounding in the maker's favour:

1. `reference_input_ui = output_ui / output_per_input`, `reference_input_raw = ceil(reference_input_ui)` in input units.
2. Spread as today: `total_spread_bps = max(base + fee + skew, min_profit)`, rejected at ≥ 10 000.
3. `input_raw = ceil(reference_input_raw × 10000 / (10000 − total_spread_bps))`.

The breakdown keeps its fields; `reference_output_amount_raw` is the requested output. Invariant checked by a test: for any accepted exact-output quote, feeding its `input_raw` back through `calculate_quote` yields an output ≥ the requested output.

`application::rfq::RfqRequest` replaces `input_amount_raw: AmountRaw` with `amount: RequestedAmount`, an enum `Input(AmountRaw) | Output(AmountRaw)`. `request_quote` dispatches on it; the per-asset amount gate runs on the input amount in both cases; events and the firm quote are unchanged (they already carry input and output). The 23 construction sites are updated mechanically.

### Recipient

`POST /v1/quotes/{quote_id}/wallet-settlement` accepts `recipient: Option<WalletAddress>`; absent means the taker. It is validated as a Solana pubkey and stored in the persisted settlement state (serialized into `state_json`, so no schema migration; `#[serde(default)]` keeps old rows readable).

Settlement changes in `application/runtime/orchestrator.rs` and `adapters/solana/htlc/client.rs`:

- The maker lock is initiated with the recipient as redeemer (today `state.taker_wallet` at the maker-lock call and in `restore_wallet_settlement`).
- The taker redeem transaction is built with the recipient's token account as destination and the taker as fee payer (`build_external_redeem` gains a `payer`). When the recipient's associated token account does not exist, an idempotent create-ATA instruction paid by the taker is prepended. Native SOL output needs no account creation.
- Trade responses and `/v1/runtime/trades` rows carry `recipient` (equal to `taker_wallet` when none was given).

The Munger HTLC program already stores a fixed redeemer and lets any signer submit `redeem` with the secret, which is what makes this a plumbing change rather than a protocol change.

### HTTP contract

- `POST /v1/rfq` friendly shape: `amount` (exact input) or `output_amount` (exact output), never both. Legacy mint shape stays exact-input.
- `POST /v1/quotes/{id}/wallet-settlement`: `recipient` optional.
- `GET /v1/trades/{id}`, `GET /v1/runtime/trades`: `recipient` field.
- `docs/openapi.yaml` updated for the three changes.

## Web app

`PayPage.tsx` becomes the `/app` route; `SwapPage.tsx` moves to `/app/swap` unchanged. Pay flow: EUR amount (exact-output EURC) and recipient address → quote card showing the USDC cost, the rate, and a countdown → one wallet confirmation (taker lock) → progress (maker lock, redeem) → receipt with amounts and Solscan links. Refresh resumes through the existing `/resume` route; an expired lock offers the existing refund path. The page keeps its own minimal state machine; helpers it shares with the swap page move to `web/app/src/lib/`. The runtime page is retitled "Maker" and already lists every configured asset.

## Landing

Same Next.js skeleton, new copy around the pitch and the liquidity numbers, screenshots from the live Pay page, links to the app, repo, and docs.

## Deployment

- API: one Railway service built from `docker/solver.Dockerfile`, a volume at `/data`, secrets entered in the Railway dashboard by Azeem (`SOLANA_RPC_URL`, `MAKER_KEYPAIR_JSON`, `JUPITER_API_KEY`, `CIRCLE_GATEWAY_SOLANA_ADDRESS`, admin hashes). The entrypoint gains support for `FIRMAMENT_CONFIG_B64`: when set, it writes `/app/config.toml` from it, since Railway cannot mount files. Fallback if the trial blocks it: Fly.io (card) or Koyeb (no volume).
- Web app and landing: Vercel, free tier; `web/app/vercel.json` rewrites `/v1/*` and `/health` to the Railway URL.
- Maker wallet: new keypair generated locally with `solana-keygen`, funded by Azeem with SOL for gas, USDC, and EURC. `CIRCLE_GATEWAY_SOLANA_ADDRESS` is that wallet's address; Gateway refill is demonstrated only if there is time to fund the Gateway balance.
- RPC: a free Helius key.

## Testing

- Unit, written first: exact-output math (rounding, the round-trip invariant, spread floor, tiny amounts, zero output), `RequestedAmount` request parsing in the HTTP layer (`amount` vs `output_amount`, both, neither).
- Integration (`tests/runtime_integration.rs`, `tests/api.rs`): recipient stored and echoed, maker lock redeemer equals recipient, redeem transaction accounts (recipient ATA, taker payer, create-ATA when missing), default recipient equals taker.
- `cargo test`, `cargo clippy`, `cargo fmt --check`, `npm run web:polish`, `npm run app:build`, `npm run landing:build` green before every push.
- One live smoke at the smallest caps before recording, evidence captured per `docs/demo-evidence-checklist.md` outside git.

## Risks

- Exact-output rounding: covered by the invariant test; the maker never under-collects.
- Recipient ATA creation: adds ~0.002 SOL of rent paid by the sender; shown on the quote card.
- Railway trial limits: fall back to Fly or Koyeb the same hour.
- EURC inventory: ~€20 is enough for a dozen demo payments at €1–2.
- Gateway refill: optional for the demo; the pitch describes it as the refill rail without depending on a live refill in the video.

## Timeline (IST)

- Sat night: spec, plan.
- Sun: backend (EURC, exact-output, recipient) with tests; API deployed; wallet funded.
- Mon: Pay page, web and landing deployed, mainnet run, evidence, landing copy.
- Tue to 10:00: demo video, pitch video, submission form, builder updates; buffer to 12:29.

## Submission notes

Disclose: Firmament was built for Frontier (Apr–May 2026); commits before 2026-09-14 are prior work; this entry adds EURC, exact-output quotes, third-party settlement, the Pay page, and the redeploy. Categories: Payments & Remittance. Public-goods angle: MIT/Apache runtime any app can self-host.
