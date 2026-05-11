# Firmament RFQ Maker Runtime — Production-Readiness Design

> Archived implementation planning note. This file records the May 7 design
> path and should not be read as the current runtime status. For current setup,
> API, and operations guidance, use `README.md` and `docs/maker-setup.mdx`.

**Date:** 2026-05-07
**Branch:** v0.1.0
**Scope:** Solana-only. Port ledger, RFQ lifecycle, reconciliation, and public-API ideas from the Munger reference repo into Firmament without changing schema or adding dependencies.

---

## 1. Architecture & sequencing

### Branching

Stay on `v0.1.0`. Phase 1 commits land directly on the branch. Phase 2 work happens in three parallel git worktrees rooted at the Phase-1 HEAD; each on its own feature branch (`recon-worker`, `read-endpoints`, `gateway-lifecycle`). Phase 3 merges them back.

### Phase 1 — foundation (main thread, sequential)

1. Add four new `LedgerAccountType` variants: `Reserved`, `GatewayReserved`, `PendingDexSpend`, `Receivable`. Add `LedgerAccountId` constructors. Update string parsing/serialization. The `account_type` column is already `TEXT` — no DDL impact.
2. Rewrite `LedgerEventConsumer` event mapping to emit the lifecycle defined in §2. The consumer becomes the single canonical place where lifecycle events translate into ledger transactions. Idempotency keys derive deterministically from `(trade_id, transition)`.
3. Update existing integration tests in lockstep. Tests that assert `working_custody → htlc_escrow` directly become `working_custody → reserved → pending_escrow → htlc_escrow` with intermediate balances asserted.
4. Move RFQ quoteability from live wallet snapshots to ledger reads.

### Phase 2 — parallel worktree subagents

- **Worktree A — Reconciliation worker**: new `src/application/runtime/reconciliation/` module. Always-on tokio task spawned at server start. Default 10 s tick. Per-asset dust thresholds + 3-consecutive-observations rule. Posts `external ↔ working_custody` and `external ↔ gateway` adjustments under deterministic idempotency keys. Emits `RuntimeEvent::Reconciliation(ReconciliationTick)` even on skip.
- **Worktree B — Public read endpoints**: `GET /health`, `GET /v1/runtime/ledger`, `GET /v1/runtime/trades`. Public-read posture matching existing runtime endpoints.
- **Worktree C — Execution path + gateway-backed lifecycle**: `ExecutionPath` enum (`InventoryToInventory`, `GatewayToDex`) on the in-memory quote/trade map. Path-selection function honours quoteability rules. Lifecycle wiring for the Gateway-USDC and Gateway-Jupiter variants.

### Phase 3 — integration

Merge worktrees back. Run `cargo test`, `cargo fmt --check`, `cargo clippy` (informational where not in CI). Frontend smoke against the new endpoints. Verification gate before declaring done.

### Stop-and-ask points

Any new dependency, any DDL change, any deletion, anything outside the locked design.

---

## 2. Ledger model & lifecycle wiring

### New account types

```rust
enum LedgerAccountType {
    // existing
    WorkingCustody, HtlcEscrow, PendingEscrow, Gateway,
    PendingGatewayDeposit, Rebalance, Fees, Trading, External,
    // new
    Reserved,         // earmarked for a specific trade, still in working wallet
    GatewayReserved,  // earmarked Gateway USDC for a specific trade
    PendingDexSpend,  // submitted Jupiter input, awaiting on-chain confirmation
    Receivable,       // inbound funds claim initiated, awaiting confirmation
}
```

Constructors: `LedgerAccountId::reserved(asset, qualifier)`, `gateway_reserved(...)`, `pending_dex_spend(...)`, `receivable(...)`. Trade-scoped accounts use the trade ID as the qualifier so balances per trade are queryable; non-trade accounts use `None` or `"solana"` per existing convention.

### Idempotency keys

```
ledger:trade:{trade_id}:{transition}
```

`transition` ∈ `{ reserve_inventory, reserve_gateway, gateway_to_trading, trading_to_custody, custody_to_reserved, reserved_to_pending_escrow, pending_escrow_to_htlc_escrow, htlc_escrow_to_trading, dex_spend_submit, dex_spend_complete_input, dex_spend_complete_output, receivable_to_custody }`. Retries post the same key; SQLite UNIQUE constraint enforces once-only.

### Lifecycle — inventory-to-inventory

| Event | Ledger movement |
|---|---|
| Taker lock confirmed + repricing pass | `working_custody → reserved` |
| Maker HTLC submitted | `reserved → pending_escrow` |
| Maker HTLC confirmed | `pending_escrow → htlc_escrow` |
| Taker redeems maker HTLC | `htlc_escrow → trading` |
| Maker redeems taker HTLC | `receivable → working_custody` |
| Maker leg refund | `htlc_escrow → working_custody` (or `pending_escrow → working_custody`) |

### Lifecycle — gateway-backed USDC output

| Event | Ledger |
|---|---|
| Taker lock confirmed + repricing | `gateway → gateway_reserved` |
| Gateway burn submitted | `gateway_reserved → trading` |
| Gateway mint confirmed | `trading → working_custody` |
| Then inventory leg | `working_custody → reserved`, `→ pending_escrow`, `→ htlc_escrow`, `→ trading` |
| Maker redeems taker HTLC | `receivable → working_custody` |

### Lifecycle — gateway-backed SOL/cbBTC output

| Event | Ledger |
|---|---|
| Taker lock confirmed + repricing | `gateway → gateway_reserved` |
| Gateway burn submitted | `gateway_reserved → trading` |
| Gateway mint confirmed | `trading → working_custody` (USDC) |
| Jupiter swap submitted | `working_custody → pending_dex_spend` (USDC input) |
| Jupiter swap confirmed | `pending_dex_spend → trading` (USDC) AND `trading → working_custody` (target asset) |
| Then inventory leg on target asset | `working_custody → reserved`, `→ pending_escrow`, `→ htlc_escrow`, `→ trading` |
| Maker redeems taker HTLC | `receivable → working_custody` |

### Quoteability — moving off live snapshots

- Inventory-backed quote: read ledger `working_custody:{output_asset}`.
- Gateway-backed quote: read ledger `gateway:USDC` minus `gateway_reserved:USDC`. If output ≠ USDC, also confirm Jupiter has a route quote covering the size.
- If neither covers the requested size → reject. No partial-inventory residual path in v1.

### Repricing pass (v1)

`repricing_passes(quote, taker_locked_amount) → bool`:
- quote not expired (`now < quote.expires_at`)
- taker_locked_amount equals the amount specified at quote
- returns `true` → reservation proceeds. Otherwise emit `Settlement::Cancelled(reason="repricing_failed")` and skip ledger reservation.

Real price re-fetch is a v0.2 follow-up.

### Receivable accounting

When taker locks the source asset, post `external → receivable` for the taker's locked amount in the input asset. When the maker redeems the taker HTLC, `receivable → working_custody` clears it.

### Refund / failure handling

| Failure | Movement |
|---|---|
| Repricing fails before reservation | none (no ledger movement, matches Rejected RFQ rule) |
| Maker leg refund | reverse most recent live position back to `working_custody` |
| Gateway operation fails before maker reserve | `gateway_reserved → gateway` |
| Jupiter swap fails post-submit | `pending_dex_spend → working_custody` |

### Files touched in Phase 1

- `src/adapters/persistence/ledger.rs`
- `src/adapters/persistence/db.rs` (no change, TEXT column)
- `src/adapters/persistence/ledger_consumer.rs` (or wherever `LedgerEventConsumer` lives)
- `src/application/rfq.rs`
- `src/domain/events.rs` (possibly add `Settlement::TakerLockConfirmed` / `MakerLockConfirmed` distinctions)
- `tests/runtime_integration.rs`
- Embedded unit tests inside `ledger.rs`

---

## 3. Reconciliation worker

### Module layout

```
src/application/runtime/reconciliation/
├── mod.rs              // public entry: spawn_worker(), ReconciliationConfig
├── wallet_monitor.rs
├── gateway_monitor.rs
└── drift.rs            // dust threshold, consecutive-observation tracker
```

### Worker shape

Single tokio task started in `bootstrap.rs` after the orchestrator boots, **always-on**. Default 10 s tick. Per tick:

1. Snapshot ledger balances.
2. Fetch on-chain wallet balances via the existing `BalanceReader` port (fakes in tests).
3. Fetch Gateway balances via the existing `CircleGatewayClient` port.
4. Run `wallet_monitor::observe(...)` and `gateway_monitor::observe(...)` per asset.
5. Always emit `RuntimeEvent::Reconciliation(ReconciliationTick)` for freshness visibility.

### Expected-cash formulae

```
expected_wallet_cash = working_custody + reserved + pending_dex_spend
expected_gateway     = gateway + gateway_reserved
drift                = on_chain - expected
```

Excluded from wallet cash: `pending_escrow`, `htlc_escrow`, `receivable`.

### Drift handling

| Drift sign | Wallet | Gateway |
|---|---|---|
| Positive (chain > expected) | `external → working_custody` | `external → gateway` |
| Negative (chain < expected) | `working_custody → external` | `gateway → external` (guarded) |

### Hard guards

1. **`gateway_reserved > 0` blocks negative Gateway adjustment.** Skip and emit `Skipped { reason: "gateway_reserved_active" }`.
2. **In-flight states pause adjustments.** If a trade is in `TakerLocked` / `MakerLocked` / `MakerRedeeming` and the asset's ledger has non-zero `pending_escrow + htlc_escrow + receivable + pending_dex_spend`, skip wallet adjustment and emit `Skipped { reason: "trade_in_flight" }`.
3. **`receivable` is never wallet cash.**
4. **Per-asset dust threshold.** Drifts ≤ dust ignored. Defaults: `USDC: 10000`, `SOL: 100000`, `cbBTC: 100`.
5. **Consecutive-observation rule.** A drift exceeding dust must be observed in 3 consecutive ticks with the same sign and within 50% of magnitude before posting an adjustment.

### Drift tracker

In-memory `HashMap<(AccountKind, AssetId), DriftWindow>` where `DriftWindow` holds the last 3 observations. Adjustment idempotency key:

```
recon:{wallet|gateway}:{asset_id}:{utc_date}:{sequence}
```

Sequence is a per-day counter. On startup the worker reseeds the in-memory dedup set from the latest day's reconciliation rows.

### Config

```toml
[reconciliation]
interval_seconds = 10
consecutive_ticks_for_adjustment = 3
emit_event_on_skip = true

[reconciliation.dust]
USDC = 10000
SOL = 100000
cbBTC = 100
```

All keys have defaults; an absent section behaves identically.

### `RuntimeEvent::Reconciliation` variant

```rust
ReconciliationTick {
    asset: AssetId,
    scope: ReconciliationScope,   // Wallet | Gateway
    on_chain: BigUint,
    expected: BigUint,
    drift: i128,
    outcome: ReconciliationOutcome, // WithinDust | Building | Adjusted | Skipped(reason)
    timestamp: DateTime<Utc>,
}
```

`/v1/runtime/events` already exposes this — no new endpoint needed for visibility.

### Files touched in Phase 2A

- `src/application/runtime/reconciliation/` (new module, 4 files)
- `src/application/runtime/bootstrap.rs`
- `src/application/runtime/orchestrator.rs` (accessors)
- `src/domain/events.rs`
- `src/config.rs` (or current config struct location)
- `config.example.toml`
- `tests/reconciliation.rs` (new)

---

## 4. Public read endpoints

All three are unauthenticated GETs, matching the posture of existing `/v1/runtime/state`. Handlers in a new `src/interfaces/http/runtime_read.rs`.

### `GET /health`

Liveness only — server up = `ok`. No DB or RPC dependency.

```json
{ "status": "ok", "service": "firmament", "version": "0.1.0" }
```

### `GET /v1/runtime/ledger` and `GET /v1/runtime/ledger?account_type=<type>`

Source: `SqliteLedgerRepository::all_balances()` + `entry_count()` + `integrity_report()`.

```json
{
  "healthy": true,
  "entry_count": 142,
  "balances": [
    {
      "account_type": "working_custody",
      "asset": "USDC",
      "qualifier": null,
      "balance_raw": "1500000000",
      "decimals": 6,
      "display_amount": "1500.000000"
    }
  ]
}
```

`balance_raw` is a stringified `i128` to avoid JSON precision loss. `decimals`/`display_amount` come from `AssetCatalog`. `qualifier` echoes the entry qualifier — `null` for chain-scoped, trade ID for trade-scoped accounts. Never includes wallet addresses.

`healthy = true` iff the integrity report shows zero unbalanced transactions and zero negative balances on accounts that should never be negative (`working_custody`, `gateway`, `reserved`, `gateway_reserved`, `pending_escrow`, `htlc_escrow`, `pending_dex_spend`, `receivable`, `pending_gateway_deposit`).

`entry_count` is total `ledger_entries` rows; both `entry_count` and `healthy` describe the whole ledger even when filtered.

Accepted `account_type` values (validated, 400 on unknown):

```
working_custody, reserved, pending_dex_spend, receivable,
htlc_escrow, pending_escrow, gateway, gateway_reserved,
pending_gateway_deposit, rebalance, fees, trading, external
```

### `GET /v1/runtime/trades?limit=<n>`

Source: in-memory trade map on `RuntimeOrchestrator`. Trades are not dropped from memory on `Complete`. Default `limit = 10`, hard cap 100, out-of-range → 400.

```json
{
  "total_count": 47,
  "successful_count": 39,
  "trades": [
    {
      "trade_id": "trade_abc123",
      "quote_id": "quote_xyz789",
      "settlement_status": "redeemed",
      "input":  { "asset": "USDC", "amount_raw": "1000000000", "decimals": 6, "display_amount": "1000.000000" },
      "output": { "asset": "SOL",  "amount_raw": "5000000000", "decimals": 9, "display_amount": "5.000000000" },
      "tx_signatures": [
        { "kind": "taker_lock",   "signature": "..." },
        { "kind": "maker_lock",   "signature": "..." },
        { "kind": "taker_redeem", "signature": "..." },
        { "kind": "maker_redeem", "signature": "..." },
        { "kind": "gateway_mint", "signature": "..." },
        { "kind": "jupiter_swap", "signature": "..." }
      ]
    }
  ]
}
```

`settlement_status` snake_case from existing `SettlementStatus` enum. Sort newest first. `tx_signatures.kind` ∈ `{ taker_lock, taker_redeem, taker_refund, maker_lock, maker_redeem, maker_refund, gateway_burn, gateway_mint, jupiter_swap }`. Background rebalance signatures are excluded.

`successful_count` = trades where `settlement_status == "redeemed"`.

### Wiring

- Extend the in-memory `RuntimeTrade` struct with `tx_signatures: Vec<TradeSignature>`. Append at orchestrator hooks where settlement events update state. No DB persistence (matches existing in-memory trade-state model).
- Add response types in `src/interfaces/http/types.rs`: `LedgerSnapshotResponse`, `LedgerBalanceEntry`, `TradesResponse`, `TradeSummary`, `TradeAmount`, `TradeSignature`.
- Handlers in `runtime_read.rs` are thin: parse query params → call orchestrator/repo accessors → serialize.

### Files touched in Phase 2B

- `src/interfaces/http/mod.rs` (3 routes)
- `src/interfaces/http/runtime_read.rs` (new)
- `src/interfaces/http/types.rs`
- `src/application/runtime/orchestrator.rs` (`recent_trades(limit)` accessor + `tx_signatures` field)
- `src/adapters/persistence/ledger.rs` (`protected_account_types()` helper if missing)
- `tests/api.rs`

---

## 5. Testing strategy & verification gate

### Layering

| Layer | Where | Coverage | Fakes |
|---|---|---|---|
| Unit | inside `src/...` | account-type parsing, drift window, lifecycle helpers, expected-cash formula, dust/consecutive logic | none (pure functions) |
| Application integration | `tests/runtime_integration.rs`, `tests/reconciliation.rs` | full RFQ → quote → accept → taker lock → reservation → maker leg → redeem; reconciliation drift cycles | `FakePriceProvider`, `FakeHtlcClient`, `FakeSwapExecutor`, `FakeGatewayClient`, `FakeBalanceReader` |
| HTTP | `tests/api.rs` | endpoint shapes, query validation, auth, error codes | same fakes via `bootstrap` |

### Verification target → test

**Ledger model**
- `ledger_account_parsing_round_trips_all_variants`
- `ledger_balance_derivation_per_asset_qualifier`
- `ledger_filter_by_account_type_returns_subset`

**Quoteability**
- `rfq_quoteability_uses_ledger_working_custody_not_live_balance`
- `rfq_gateway_quoteability_uses_ledger_minus_reserved`

**Settlement lifecycle**
- `settlement_inventory_path_emits_full_ledger_sequence`
- `settlement_gateway_usdc_path_emits_full_ledger_sequence`
- `settlement_gateway_sol_path_emits_jupiter_spend_sequence`

**Reconciliation**
- `recon_wallet_positive_drift_adjusts_after_consecutive_ticks`
- `recon_wallet_negative_drift_adjusts`
- `recon_dust_threshold_skips`
- `recon_sign_flip_resets_window`
- `recon_gateway_positive_drift_adjusts`
- `recon_gateway_skip_when_reserved_active`
- `recon_in_flight_trade_pauses`
- `recon_emits_event_on_skip`
- `recon_idempotent_across_restart`

**HTTP**
- `health_endpoint_returns_ok`
- `ledger_endpoint_returns_balances`
- `ledger_endpoint_filters_by_account_type`
- `ledger_endpoint_rejects_unknown_account_type`
- `ledger_endpoint_omits_wallet_addresses`
- `ledger_endpoint_marks_unhealthy_on_negative_protected_account`
- `trades_endpoint_returns_recent_trades`
- `trades_endpoint_respects_limit_param`
- `trades_endpoint_caps_at_100`
- `trades_endpoint_includes_tx_signatures_with_kinds`
- `trades_endpoint_excludes_rebalance_signatures`

### Live mainnet tests

Default `cargo test` is 100 % fake-driven, no env required, no mainnet calls. A live-mainnet test module extends the existing `tests/live_mainnet.rs` pattern:

- Reads keys via `LoadedWallet::from_maker_env()` / `from_taker_env()` and `SOLANA_RPC_URL_ENV` (existing helpers).
- `#[ignore]` by default; gated on `RUN_LIVE_SOLANA_TESTS=1` plus a per-scenario flag (matching existing convention).
- New scenarios:
  - `live_inventory_path_full_lifecycle` (gated `RUN_LIVE_INVENTORY_PATH=1`) — small RFQ on the maker wallet, real HTLC initiate/redeem, asserts ledger sequence matches §2 inventory path.
  - `live_gateway_usdc_path` (gated `RUN_LIVE_GATEWAY_USDC_PATH=1`) — small Gateway-backed USDC delivery RFQ. **Requires explicit per-run approval before invocation** (Gateway refill is mutating-mainnet).
  - `live_gateway_sol_path` (gated `RUN_LIVE_GATEWAY_SOL_PATH=1`) — small Gateway+Jupiter delivery RFQ. **Requires explicit per-run approval before invocation** (Jupiter swap is mutating-mainnet).
  - `live_reconciliation_observation` (gated `RUN_LIVE_RECONCILIATION=1`) — read-only: starts the reconciliation worker against a real Solana RPC and Circle Gateway, observes drift events without posting adjustments (worker can be put in observe-only mode for this test).

Zero secrets in any committed file. All live-test reads go through `std::env::var`.

### Verification gate

1. `cargo build` — clean compile.
2. `cargo test` — full default suite green.
3. `cargo fmt --check` — fix on touched files; do not reformat unrelated pre-existing drift, report instead.
4. `cargo clippy --all-targets -- -D warnings` (informational if not in CI).
5. Manual smoke against new endpoints with `cargo run`.
6. Frontend smoke: `npm run dev` in `web/app`, load `http://127.0.0.1:3000/app/`, exercise the runtime block.
7. (Optional, on user approval) live-mainnet inventory-path test.

If any step fails, stop and report.

### Frontend alignment (small, scoped)

- `web/app/src/api.ts` — typed clients `getHealth()`, `getLedger(accountType?)`, `getTrades(limit?)` mirroring §4 shapes.
- `web/app/src/pages/RuntimePage.tsx` — wire the runtime block to call `getLedger()` and `getTrades()` on mount + interval refresh. Render ledger balance table grouped by `account_type` and trade list with status badges and signature links.

No styling rework. No new components beyond what's needed.

---

## Stop conditions during implementation

Pause and ask before:

- Adding any dependency (Rust crate or npm package)
- Changing the database schema or migrations
- Deleting any file
- Editing `.env`, secrets, keypairs, credentials
- Running mutating mainnet actions (Jupiter swap, Gateway refill, HTLC submit, rebalance)
- Refactors beyond what these changes require
- Frontend changes beyond API compatibility
- Anything contradicting locked decisions

---

## Risks & follow-ups (out of scope)

- **Trade persistence across restart** — trades stay in-memory; restart wipes them. v0.2.
- **Real repricing** — v1 only checks expiry + amount match. v0.2 should re-fetch from the price oracle.
- **Reconciliation idempotency window** — restart-time reseeding from SQLite assumes the worker hasn't been off for >1 day. Document.
- **Health readiness check** — `/health` is liveness-only. `?check=ready` mode pinging DB + Solana RPC is a v0.2 enhancement.
- **Observability** — no metrics emission planned in this scope. Tracing/Prometheus pass eventually.
- **Auth posture review** — public reads are acceptable for current deployment posture. Revisit if Firmament moves to the public internet.

---

## Approved decisions log

- Solana-only.
- 4 new ledger account types added; account_type column stays TEXT (no DDL).
- Quoteability moves to ledger reads.
- Two execution paths: `inventory_to_inventory`, `gateway_to_dex`. No partial-inventory residual.
- Reservation triggers on taker-lock-confirmed + repricing-pass; v1 repricing is expiry+amount match only.
- Reconciliation always-on, 10 s default, per-asset dust + 3 consecutive observations, gateway_reserved guard.
- 3 new public read endpoints, unauthenticated.
- HTTP + Gateway + Jupiter signatures all surfaced on trades, with `kind` discriminator.
- Default `cargo test` is fake-only; live mainnet tests are env-gated, opt-in, and Jupiter/Gateway live actions still require per-run approval.
- Frontend runtime block is realigned to consume new endpoint shapes; no behavior change beyond API compatibility.
