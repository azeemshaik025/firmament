# Firmament RFQ Maker Runtime Implementation Plan

> Archived implementation planning note. This file records the May 7 build plan
> and should not be read as the current runtime status. For current setup, API,
> and operations guidance, use `README.md` and `docs/maker-setup.mdx`.

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Bring Firmament's Solana-only RFQ Maker Runtime to production-ready state by porting Munger's ledger/RFQ/reconciliation patterns: 4 new ledger account types, full lifecycle wiring per the design, always-on reconciliation worker, 3 public read endpoints, and frontend alignment.

**Architecture:** Phase 1 sequential foundation (ledger types + consumer rewire + quoteability move) on `v0.1.0`. Phase 2 three parallel git worktrees (reconciliation worker, public endpoints, gateway-backed lifecycle) rooted at Phase-1 HEAD. Phase 3 merge + frontend alignment + verification gate. Zero schema changes; in-memory trade state preserved.

**Tech Stack:** Rust (rusqlite, tokio, axum, time, uuid, serde, thiserror), SQLite, React + Vite + TypeScript frontend, Solana SDK, Jupiter Swap V2, Circle Gateway.

**Companion design doc:** `docs/plans/2026-05-07-firmament-rfq-runtime-design.md` — read first for context on locked decisions.

**Working directory:** `/Users/azeemshaik/work/hackathons/firmament`. Branch: `v0.1.0`.

**Hard constraints (from master prompt):**
- No new dependencies without explicit approval.
- No DB schema changes without explicit approval.
- No mutating mainnet actions during implementation.
- Stop and ask before deleting files, editing `.env` or secrets.
- Update existing tests in lockstep with behavior changes (don't `#[ignore]` them).

---

## Phase 1 — Foundation (main thread, sequential)

### Task 1: Add new `LedgerAccountType` variants

**Files:**
- Modify: `src/adapters/persistence/ledger.rs:21-84` (enum + as_str + try_from)

**Step 1: Read the existing enum**

Open `src/adapters/persistence/ledger.rs` and confirm the current variants match the design doc inventory.

**Step 2: Add four new variants**

In the `LedgerAccountType` enum (currently lines 21-40), append:

```rust
    /// Earmarked working-custody funds for an in-flight trade. Still in the wallet, just reserved.
    Reserved,
    /// Earmarked Gateway USDC for an in-flight trade.
    GatewayReserved,
    /// Submitted Jupiter swap input awaiting on-chain confirmation.
    PendingDexSpend,
    /// Inbound funds claim initiated, awaiting confirmation.
    Receivable,
```

In `as_str` (lines 44-56), add the matching string forms:

```rust
            Self::Reserved => "reserved",
            Self::GatewayReserved => "gateway_reserved",
            Self::PendingDexSpend => "pending_dex_spend",
            Self::Receivable => "receivable",
```

In `TryFrom<&str>` (lines 65-83), add the matching parses.

**Step 3: Build to verify**

Run: `cargo build --lib`
Expected: clean compile.

**Step 4: Commit**

```bash
git add src/adapters/persistence/ledger.rs
git commit -m "feat(ledger): add Reserved/GatewayReserved/PendingDexSpend/Receivable account types"
```

---

### Task 2: Add `LedgerAccountId` constructors for new accounts

**Files:**
- Modify: `src/adapters/persistence/ledger.rs:97-180` (LedgerAccountId impl block)

**Step 1: Add four constructors**

After the existing constructors (currently ends around line 180), add:

```rust
    /// Reserved working-custody funds for a trade. Qualifier is the trade ID.
    #[must_use]
    pub fn reserved(asset: AssetId, trade_id: impl Into<String>) -> Self {
        Self::new(LedgerAccountType::Reserved, asset, Some(trade_id.into()))
    }

    /// Reserved Gateway USDC for a trade. Qualifier is the trade ID.
    #[must_use]
    pub fn gateway_reserved(asset: AssetId, trade_id: impl Into<String>) -> Self {
        Self::new(
            LedgerAccountType::GatewayReserved,
            asset,
            Some(trade_id.into()),
        )
    }

    /// Pending Jupiter swap input. Qualifier is the trade ID.
    #[must_use]
    pub fn pending_dex_spend(asset: AssetId, trade_id: impl Into<String>) -> Self {
        Self::new(
            LedgerAccountType::PendingDexSpend,
            asset,
            Some(trade_id.into()),
        )
    }

    /// Inbound claim awaiting confirmation. Qualifier is the trade ID.
    #[must_use]
    pub fn receivable(asset: AssetId, trade_id: impl Into<String>) -> Self {
        Self::new(LedgerAccountType::Receivable, asset, Some(trade_id.into()))
    }
```

Convention: trade-scoped accounts use `trade_id` as qualifier; non-trade balances aggregate by asset.

**Step 2: Build to verify**

Run: `cargo build --lib`
Expected: clean compile.

**Step 3: Commit**

```bash
git add src/adapters/persistence/ledger.rs
git commit -m "feat(ledger): add constructors for trade-scoped account types"
```

---

### Task 3: Add round-trip parsing test for all account variants

**Files:**
- Modify: `src/adapters/persistence/ledger.rs` — add to the existing `#[cfg(test)] mod tests` block at the bottom of the file (locate it; it's already in the file).

**Step 1: Write the failing test**

Append to the test module:

```rust
    #[test]
    fn account_type_round_trips_all_variants_including_new_ones() {
        let variants = [
            LedgerAccountType::WorkingCustody,
            LedgerAccountType::Reserved,
            LedgerAccountType::PendingDexSpend,
            LedgerAccountType::Receivable,
            LedgerAccountType::HtlcEscrow,
            LedgerAccountType::PendingEscrow,
            LedgerAccountType::Gateway,
            LedgerAccountType::GatewayReserved,
            LedgerAccountType::PendingGatewayDeposit,
            LedgerAccountType::Rebalance,
            LedgerAccountType::Fees,
            LedgerAccountType::Trading,
            LedgerAccountType::External,
        ];

        for variant in variants {
            let s = variant.to_string();
            let parsed = LedgerAccountType::try_from(s.as_str()).expect("parse known variant");
            assert_eq!(parsed, variant, "round trip failed for {variant:?}");
        }
    }
```

**Step 2: Run to verify it passes**

Run: `cargo test --lib adapters::persistence::ledger::tests::account_type_round_trips`
Expected: PASS.

**Step 3: Commit**

```bash
git add src/adapters/persistence/ledger.rs
git commit -m "test(ledger): round-trip all account type variants"
```

---

### Task 4: Map the existing `LedgerEventConsumer` in detail

**No code changes** — this is a read-only task to ground the next several tasks.

**Step 1: Read and summarize**

Read `src/adapters/persistence/ledger.rs` from line 748 to end of file. Locate the `LedgerEventConsumer` struct, its `consume` (or equivalent) method, and every match arm that maps a `RuntimeEvent` (Quote/Settlement/Swap/Gateway) to ledger transactions.

Read `src/domain/types.rs` and confirm the shape of `HtlcReceipt` — specifically: does it carry a "leg" (maker vs taker) and a confirmation/status field that lets you distinguish "submitted but not confirmed" from "confirmed"?

**Step 2: Write a short note in this plan**

Append to this file under a new section `### Phase-1 Field Findings` capturing:
- Current `LedgerEventConsumer` location (file:line range).
- Existing `RuntimeEvent` → ledger movement mapping (one line per arm).
- How maker vs taker leg is distinguished today (field on `HtlcReceipt`? Separate event variants?).
- Where in the orchestrator each event is emitted.

This becomes the reference for Tasks 5–7. Commit the note.

```bash
git add docs/plans/2026-05-07-firmament-rfq-runtime-implementation.md
git commit -m "docs(plan): record current ledger consumer mapping"
```

---

### Phase-1 Field Findings

**Captured:** 2026-05-07
**By:** Task 4

#### LedgerEventConsumer location

- Struct + impl: `src/adapters/persistence/ledger.rs:792-824`
- Match dispatcher: `transactions_for_event` at `src/adapters/persistence/ledger.rs:826-950`
- `LedgerConsumeSummary` value object: `src/adapters/persistence/ledger.rs:781-789`

#### Current event → ledger mapping

| Event variant | Distinguishing field/condition | Ledger movement |
|---|---|---|
| `Gateway::RefillRequested { amount }` (`ledger.rs:833`) | always | DR `pending_gateway_deposit(asset)` / CR `gateway(asset)` for `amount.amount_raw` |
| `Gateway::RefillCompleted { receipt }` (`ledger.rs:852`) | always | DR `working(asset)` / CR `pending_gateway_deposit(asset)` for `receipt.amount.amount_raw` |
| `Gateway::BalanceChecked { .. }` (`ledger.rs:896`) | always (snapshot) | none — added to `summary.ignored` |
| `Gateway::Failed { .. }` (`ledger.rs:901`) | always | none — added to `summary.ignored` |
| `Swap::Executed { receipt }` (`ledger.rs:871`) | only when `receipt.output_amount.is_some()` | DR `working(output.asset)` / CR `rebalance(output.asset)` for `output_amount.amount_raw`. If `output_amount` is None → ignored. |
| `Swap::PriceObserved` / `Swap::Quoted` (`ledger.rs:937`) | always | none — ignored |
| `Swap::Failed { .. }` (`ledger.rs:942`) | always | none — ignored |
| `Settlement::Started` (`ledger.rs:908`) | always | **none** — ignored ("settlement started has no asset amount") |
| `Settlement::Initiated { receipt }` (`ledger.rs:909`) | always | **none** — ignored ("settlement initiated receipt has no amount") |
| `Settlement::Redeemed { receipt }` (`ledger.rs:910`) | always | **none** — ignored ("settlement redeemed receipt has no amount") |
| `Settlement::Refunded { receipt }` (`ledger.rs:911`) | always | **none** — ignored ("settlement refunded receipt has no amount") |
| `Settlement::StatusChanged` (`ledger.rs:912`) | always | **none** — ignored |
| `Settlement::Failed` (`ledger.rs:913`) | always | **none** — ignored |
| `Quote(_)` (`ledger.rs:917`) | always | none — ignored |
| `Risk(_)` (`ledger.rs:922`) | always | none — ignored |
| `Inventory(_)` (`ledger.rs:927`) | always | none — ignored |
| `System(_)` (`ledger.rs:932`) | always | none — ignored |

**Observation:** today the consumer writes ledger transactions for **only three** event arms — `Gateway::RefillRequested`, `Gateway::RefillCompleted`, and `Swap::Executed` (when `output_amount` is set). All settlement / HTLC events are explicitly ignored — no escrow movement, no reserved/receivable bookkeeping, no taker-lock recording.

#### HtlcReceipt fields

Source: `src/domain/types.rs:428-437`.

- `trade_id: TradeId` — trade this receipt belongs to
- `status: SettlementStatus` — coarse high-level status (see below)
- `signature: Option<TxSignature>` — Solana tx signature, if submitted

`SettlementStatus` (`src/domain/types.rs:412-426`) variants: `Pending`, `Initiated`, `Redeemed`, `Refunded`, `Failed`. There is **no** `Submitted`-vs-`Confirmed` distinction in the enum — `Initiated` collapses both.

**Maker vs taker leg distinguished by:** **NOT distinguishable from `HtlcReceipt` alone.** The receipt has no `leg`, no `actor`, no `funder`/`redeemer`, and no wallet address. The maker-vs-taker information lives outside the receipt:
- The orchestrator separately calls `record_taker_lock` (`settlement.rs:195`) vs `record_maker_lock` (`settlement.rs:214`) and stores receipts on `taker_input_leg.lock_receipt` vs `maker_output_leg.lock_receipt` (`settlement.rs:159-161`).
- Both transitions emit the **same** event variant `SettlementEvent::Initiated { metadata, receipt }` (`settlement.rs:205-208` and `settlement.rs:222-227`).
- The receipts produced by each transition are **identical in shape**: same `trade_id`, same `status: SettlementStatus::Initiated`, only the `signature` differs (`settlement.rs:316-322`).

**A consumer reading only the `RuntimeEvent::Settlement(Initiated{..})` stream cannot tell the maker leg from the taker leg today.**

**Submitted vs confirmed distinguished by:** **NOT distinguishable today.** `record_*_lock` calls always set `SettlementStatus::Initiated` regardless of whether the adapter has confirmed the on-chain transaction. The orchestrator's wallet flow (`orchestrator.rs:644-671`) calls `record_external_initiate`/`initiate_with_external_redeemer`, immediately publishes `Initiated`, then proceeds to the next step — there is no "confirmed" follow-up event distinct from the initial submission. The non-wallet `run_settlement_locks` (`orchestrator.rs:780-844`) does call `htlc_client.status(trade_id)` after taker lock and emits `SettlementEvent::Failed` if it isn't `Initiated`, but on the success path no separate "confirmed" event fires.

#### Settlement event emission sites in orchestrator

Source: `src/application/runtime/orchestrator.rs`. `publish` helper at `orchestrator.rs:1254`.

**Wallet-mediated flow (browser wallet provides taker funds):**
- `start_wallet_settlement` (`orchestrator.rs:565`)
  - `orchestrator.rs:596` — emits `SettlementEvent::Started` (from `settlement.start`)
- `record_wallet_taker_lock` (`orchestrator.rs:638`)
  - `orchestrator.rs:654` — emits `SettlementEvent::Initiated` from `settlement.record_taker_lock` (taker leg)
  - `orchestrator.rs:670` — emits `SettlementEvent::Initiated` from `settlement.record_maker_lock` (maker leg) — same arm, same fields, no leg discriminator
- `complete_wallet_taker_redeem` (`orchestrator.rs:716`)
  - `orchestrator.rs:733` — emits `SettlementEvent::Redeemed` from `record_taker_redeem` (taker redeem of maker output)
  - `orchestrator.rs:742` — emits `SettlementEvent::Redeemed` from `record_maker_redeem` (maker redeem of taker input) — same arm, same fields
  - `orchestrator.rs:744-749` — emits an extra `SettlementEvent::StatusChanged { status: Redeemed }`

**Inventory-backed flow (server-only HTLC client):**
- `settle_quote` → `run_settlement_locks` (`orchestrator.rs:780`)
  - `orchestrator.rs:787` — emits `Started`
  - `orchestrator.rs:808` — emits `Initiated` (taker)
  - `orchestrator.rs:842` — emits `Initiated` (maker)
- `run_settlement_redeems` (`orchestrator.rs:846`)
  - `orchestrator.rs:873` — emits `Redeemed` (taker redeem of maker output)
  - `orchestrator.rs:890` — emits `Redeemed` (maker redeem of taker input)
- `settle_quote` (`orchestrator.rs:549-554`) emits the trailing `StatusChanged { Redeemed }` after both redeems

**Other settlement emission sites:**
- `record_settlement_failure` → `orchestrator.rs:1224` emits `SettlementEvent::Failed`
- Manual settlement-status transition path emits `StatusChanged` at `orchestrator.rs:549-554` and `744-749`

**Event field summary for settlement payloads:**
- `Started`: `metadata`, `trade_id`, `quote_id`. No leg, no amount.
- `Initiated`/`Redeemed`/`Refunded`: `metadata`, `receipt: HtlcReceipt` (= `trade_id`, `status`, `Option<signature>`). No leg, no amount, no actor/wallet.
- `StatusChanged`: `metadata`, `trade_id`, `status`. No amount.
- `Failed`: `metadata`, `trade_id`, `reason`. No amount.

**Quote events (`orchestrator.rs:512`, `516`, `446`):** emitted but contain no token-amount delta the consumer can post.

**Inventory events (`orchestrator.rs:389`, `924`, `942`):** emitted but explicitly ignored by the consumer (`ledger.rs:927`).

#### Today's lifecycle (inventory-backed RFQ, both legs server-controlled)

Walking the wallet-flow path (`orchestrator.rs:565-684`, then `716-767`) since the inventory-backed wallet flow is the canonical path:

1. **Quote accepted →** `accept_quote_for_trade` publishes `QuoteEvent::Accepted` (`orchestrator.rs:512`). Consumer: ignored.
2. **Wallet settlement started →** `start_wallet_settlement` publishes `SettlementEvent::Started` (`orchestrator.rs:596`). Consumer: ignored.
3. **Taker submits HTLC (browser-signed) →** `record_wallet_taker_lock` calls `record_external_initiate`, then publishes `SettlementEvent::Initiated` for the taker leg (`orchestrator.rs:654`). Consumer: **ignored** (`ledger.rs:909`).
4. **Maker submits HTLC →** same method calls `initiate_with_external_redeemer`, then publishes `SettlementEvent::Initiated` for the maker leg (`orchestrator.rs:670`). Consumer: **ignored** (same arm collapses both legs).
5. **Taker redeems maker leg →** `complete_wallet_taker_redeem` calls `record_external_redeem`, then publishes `SettlementEvent::Redeemed` (`orchestrator.rs:733`). Consumer: **ignored** (`ledger.rs:910`).
6. **Maker redeems taker leg →** same method calls `htlc_client.redeem`, then publishes `SettlementEvent::Redeemed` (`orchestrator.rs:742`). Consumer: **ignored**.
7. **Trailing →** `SettlementEvent::StatusChanged { Redeemed }` (`orchestrator.rs:744`). Consumer: **ignored** (`ledger.rs:912`).
8. **Inventory refresh →** `refresh_inventory_and_automation` (`orchestrator.rs:761`) publishes `InventoryEvent::Snapshot` and may publish a Gateway `RefillRequested`/`RefillCompleted` pair. The Gateway pair is the **only** part of the entire RFQ-fill lifecycle that produces ledger transactions today.

**Net effect today:** an inventory-backed RFQ that completes successfully writes **zero** ledger transactions for the trade itself. Working balances are not debited, no reserved or receivable accounts move, no escrow accounts move. Only Gateway refills (a separate side-effect) and Jupiter-swap rebalances mutate the ledger.

#### Implications for Task 5

1. **HtlcReceipt is leg-agnostic and confirmation-agnostic today.** T5 must either:
   - **Option A (recommended):** add a `leg: SettlementLeg` field to `HtlcReceipt` (`src/domain/types.rs:430`) and populate it in `TwoSidedSettlement::receipt` (`src/domain/settlement.rs:316-322`). All four `record_*` transitions (`settlement.rs:195, 214, 233, 252`) already know the leg implicitly — wire it through. Touchpoints: `domain/types.rs`, `domain/settlement.rs`, plus any deserializers / fixtures (the `tests` block in `settlement.rs:325+` and `ledger.rs:1103+`).
   - **Option B:** introduce new variant pairs (`SettlementEvent::TakerInitiated` / `MakerInitiated`, etc.) instead of a leg field. Larger blast radius — affects `domain/events.rs:107-156`, `interfaces/http/types.rs`, web-app event renderers, and existing tests.
   - **Option C:** rely on call-site context (consumer is wired into the orchestrator and can be told the leg out-of-band). This breaks the event-driven invariant; not recommended.
2. **Submitted-vs-confirmed must be added.** The current single `Initiated` arm fires immediately after adapter submission. T5 needs the new `pending_escrow → htlc_escrow` transition to key off a "confirmed" signal that does not exist yet. Options:
   - Add `confirmed: bool` to `HtlcReceipt`, or
   - Replace `SettlementStatus::Initiated` with two states (`Submitted`, `Confirmed`), or
   - Split `SettlementEvent::Initiated` into `Submitted` + `Confirmed` variants and have the orchestrator emit a follow-up after `htlc_client.status(...)` returns `Initiated` (today, `run_settlement_locks` polls status at `orchestrator.rs:811-815` but does not emit a separate event).
3. **Settlement events carry no `TokenAmount`.** T5's debits/credits need an amount; `HtlcReceipt` does not have one. The amount is recoverable via `SettlementTerms.taker_input` / `SettlementTerms.maker_output` held by the orchestrator's `TwoSidedSettlement`, but **not** by a downstream consumer reading the event stream. T5 must either: (a) attach `amount: TokenAmount` to `HtlcReceipt`, or (b) attach amount to the `Initiated`/`Redeemed` variants directly, or (c) thread settlement-terms lookup into the consumer (couples the consumer to in-memory orchestrator state — not recommended).
4. **Trailing `StatusChanged { Redeemed }` is redundant once leg-aware `Redeemed` events fire** — T5 should decide whether to keep it as a "settlement complete" marker or drop it.
5. **No taker-lock event today produces a `Receivable` movement.** T5 must add a new ledger arm that fires on the (leg=taker, status=submitted) initiated event, debiting `receivable(input_asset)` and crediting working/reserved depending on the new lifecycle.
6. **Quote-accepted event today writes nothing.** T5's `working → reserved` reservation hook needs to fire either on `QuoteEvent::Accepted` (`orchestrator.rs:512`) or on `SettlementEvent::Started` (`orchestrator.rs:596`/`787`). `Started` is preferable because it carries the `trade_id`, while `Accepted` carries `quote_id` + `trade_id` both. Neither variant carries the reserved amount today — same fix as (3) above (need amount on the event or on `SettlementTerms` propagated through).

**Bottom-line scope assessment for T5:** This is **not** a pure consumer rewrite. T5 requires changes to `HtlcReceipt` (leg + confirmation + amount), to `TwoSidedSettlement::receipt` and the four `record_*` transitions, and likely to `SettlementEvent` itself (new variants or new fields) — and the consumer changes follow from those. The plan's T5 description should be revised to reflect this widened blast radius.

---

### Task 5: Add `Receivable` and reservation hooks in the consumer

**Files:**
- Modify: `src/adapters/persistence/ledger.rs` — `LedgerEventConsumer` body (around lines 748+)
- Modify: `tests/runtime_integration.rs` (test asserting new lifecycle)

**Step 1: Write the failing test**

In `tests/runtime_integration.rs`, add a test `inventory_path_emits_full_ledger_sequence` that:
1. Bootstraps the runtime with fake adapters (use the existing fakes — `FakeHtlcClient`, `FakePriceProvider`, etc.).
2. Drives an inventory-backed RFQ: request → accept → simulate taker lock → wait through maker leg → simulate taker redeem → simulate maker redeem.
3. After each event, queries `SqliteLedgerRepository::all_balances()` and asserts the **exact** intermediate balances per the design doc §2 inventory-to-inventory table:
   - After taker-lock-confirmed + repricing pass: `working_custody` ↓, `reserved(trade_id)` ↑, and `external → receivable(trade_id)` for the taker's input asset.
   - After maker HTLC submitted: `reserved` ↓, `pending_escrow` ↑.
   - After maker HTLC confirmed: `pending_escrow` ↓, `htlc_escrow` ↑.
   - After taker redeems: `htlc_escrow` ↓, `trading` ↑.
   - After maker redeems taker HTLC: `receivable` ↓, `working_custody` ↑.

Use `assert_eq!` against `BigUint` balances at each checkpoint.

**Step 2: Run to verify it fails**

Run: `cargo test --test runtime_integration inventory_path_emits_full_ledger_sequence`
Expected: FAIL — current consumer skips the reservation step and never posts to `Receivable`.

**Step 3: Update the consumer**

In `LedgerEventConsumer`, change the mapping for inventory-backed flow:

| Trigger event | Old (delete or replace) | New |
|---|---|---|
| Settlement::Initiated for maker leg, before confirmation | `working_custody → htlc_escrow` (current) | `reserved → pending_escrow` |
| Settlement::Initiated when receipt is confirmed | (same) | `pending_escrow → htlc_escrow` |
| Settlement::Redeemed for maker leg by taker | (varies) | `htlc_escrow → trading` |
| Settlement::Redeemed for taker leg by maker | (varies) | `receivable → working_custody` |
| New: taker-lock-confirmed | (no current handler) | `working_custody → reserved` AND `external → receivable` |

To distinguish maker vs taker leg and submitted vs confirmed:
- Use the `HtlcReceipt` field that identifies the leg. If no such field exists today, add a `leg: HtlcLeg { Maker, Taker }` field to `HtlcReceipt` and populate it in the producing adapters. **If this is a wider change than expected, stop and ask before proceeding** (touches multiple files; could be scoped as its own task).
- Use the receipt's existing confirmation/status field. If a single `Initiated` event covers both submitted-and-confirmed today, you'll need to split it into two emissions in the orchestrator. Same stop-and-ask rule.

Idempotency keys: `format!("ledger:trade:{trade_id}:{transition}")` where `transition` is one of the strings from §2 of the design doc.

**Step 4: Run the test to verify pass**

Run: `cargo test --test runtime_integration inventory_path_emits_full_ledger_sequence`
Expected: PASS.

**Step 5: Run the full test suite**

Run: `cargo test`
Expected: existing tests may now fail because the lifecycle changed. **Do not skip them** — note which fail and fix in Tasks 6 and 7.

**Step 6: Commit (only if step 4 passed; existing-test fixes come next)**

```bash
git add src/adapters/persistence/ledger.rs tests/runtime_integration.rs src/domain/types.rs
git commit -m "feat(ledger): wire reserved + receivable into inventory-backed lifecycle"
```

---

### Task 6: Fix existing integration tests broken by Task 5

**Files:**
- Modify: `tests/runtime_integration.rs`
- Modify: any embedded unit tests that asserted old lifecycle (locate via `cargo test 2>&1 | grep FAILED`).

**Step 1: Identify failing tests**

Run: `cargo test 2>&1 | tee /tmp/test-output.log | grep -E 'FAILED|test result'`
Note each failing test name and the assertion that broke.

**Step 2: For each failing test, update assertions to the new lifecycle**

Walk each failed test. If it asserted `working_custody → htlc_escrow` directly, rewrite to the 3-step `working_custody → reserved → pending_escrow → htlc_escrow` and check intermediate balances.

Do not weaken assertions. Do not `#[ignore]` tests. If a test's intent is no longer valid (e.g. it asserted a behavior the new design removes), delete the test and note why in the commit message.

**Step 3: Run the full suite**

Run: `cargo test`
Expected: all green.

**Step 4: Commit**

```bash
git add tests/runtime_integration.rs <other modified test files>
git commit -m "test: update integration tests to reserved/receivable lifecycle"
```

---

### Task 7: Add `protected_account_types` helper

**Files:**
- Modify: `src/adapters/persistence/ledger.rs`

**Step 1: Write the failing test**

In the ledger.rs test module:

```rust
    #[test]
    fn protected_account_types_excludes_pnl_buckets() {
        use std::collections::HashSet;
        let protected: HashSet<_> = LedgerAccountType::protected_account_types()
            .iter()
            .copied()
            .collect();
        assert!(protected.contains(&LedgerAccountType::WorkingCustody));
        assert!(protected.contains(&LedgerAccountType::Reserved));
        assert!(protected.contains(&LedgerAccountType::Gateway));
        assert!(protected.contains(&LedgerAccountType::GatewayReserved));
        assert!(protected.contains(&LedgerAccountType::HtlcEscrow));
        assert!(protected.contains(&LedgerAccountType::PendingEscrow));
        assert!(protected.contains(&LedgerAccountType::PendingDexSpend));
        assert!(protected.contains(&LedgerAccountType::Receivable));
        assert!(protected.contains(&LedgerAccountType::PendingGatewayDeposit));
        // P&L / boundary accounts are NOT protected from going negative.
        assert!(!protected.contains(&LedgerAccountType::Trading));
        assert!(!protected.contains(&LedgerAccountType::Fees));
        assert!(!protected.contains(&LedgerAccountType::External));
        assert!(!protected.contains(&LedgerAccountType::Rebalance));
    }
```

**Step 2: Run to verify failure**

Run: `cargo test --lib protected_account_types_excludes_pnl_buckets`
Expected: FAIL — method does not exist.

**Step 3: Implement**

Add to `impl LedgerAccountType`:

```rust
    /// Account types that must never hold a negative balance.
    /// Used by the integrity reporter and the ledger health check.
    #[must_use]
    pub const fn protected_account_types() -> &'static [Self] {
        &[
            Self::WorkingCustody,
            Self::Reserved,
            Self::Gateway,
            Self::GatewayReserved,
            Self::HtlcEscrow,
            Self::PendingEscrow,
            Self::PendingDexSpend,
            Self::Receivable,
            Self::PendingGatewayDeposit,
        ]
    }
```

**Step 4: Verify pass**

Run: `cargo test --lib protected_account_types_excludes_pnl_buckets`
Expected: PASS.

**Step 5: Commit**

```bash
git add src/adapters/persistence/ledger.rs
git commit -m "feat(ledger): add protected_account_types helper for integrity checks"
```

---

### Task 8: Move RFQ inventory-quoteability to ledger reads

**Files:**
- Modify: `src/application/rfq.rs`
- Modify: relevant tests in `tests/runtime_integration.rs`

**Step 1: Write the failing test**

In `tests/runtime_integration.rs`, add `rfq_quoteability_uses_ledger_working_custody_not_live_balance`:
1. Bootstrap with a fake `BalanceReader` returning a deliberately wrong on-chain balance (e.g. 0).
2. Seed the ledger with `working_custody:USDC = 1_000_000_000` via direct ledger transactions.
3. Submit an RFQ for an output equivalent to 500 USDC.
4. Assert quote is `Accepted`.
5. Drop the ledger to `working_custody:USDC = 100_000_000` (i.e., 100 USDC) by posting a transfer to External.
6. Submit the same RFQ.
7. Assert quote is `Rejected` with reason `insufficient_inventory`.

**Step 2: Run to verify failure**

Run: `cargo test --test runtime_integration rfq_quoteability_uses_ledger_working_custody`
Expected: FAIL — current path consults the live wallet snapshot.

**Step 3: Update `rfq.rs`**

Find the inventory-check call site. Replace the live-balance fetch with `repository.account_balance(&LedgerAccountId::working(asset))`. Keep the wallet snapshot for the `BalanceSnapshot` that goes into events (for visibility), but make the gate use the ledger value.

**Step 4: Verify pass + full suite**

```
cargo test --test runtime_integration rfq_quoteability_uses_ledger_working_custody
cargo test
```

Expected: targeted test passes; full suite stays green.

**Step 5: Commit**

```bash
git add src/application/rfq.rs tests/runtime_integration.rs
git commit -m "feat(rfq): use ledger working_custody for inventory quoteability"
```

---

### Task 9: Add Gateway-quoteability ledger read

**Files:**
- Modify: `src/application/rfq.rs`
- Modify: `tests/runtime_integration.rs`

**Step 1: Failing test**

`rfq_gateway_quoteability_uses_ledger_minus_reserved`: seed `gateway:USDC = 1000` and `gateway_reserved:USDC = 400`. RFQ for 500 USDC inventory should be rejected (only 600 free), but RFQ for 600 USDC should accept on the gateway-backed path. Use a fake price feed and Jupiter route quote.

**Step 2: Run to verify failure**

Run: `cargo test --test runtime_integration rfq_gateway_quoteability_uses_ledger_minus_reserved`
Expected: FAIL.

**Step 3: Implement**

Add a helper in `rfq.rs`:

```rust
fn gateway_free_balance(repo: &dyn LedgerRepository, asset: &AssetId) -> AppResult<BigUint> {
    let gateway = repo.account_balance(&LedgerAccountId::gateway(asset.clone()))?;
    let reserved = repo.account_balance(&LedgerAccountId::gateway_reserved(asset.clone(), ""))?
        .or_aggregated_by_type(); // OR: sum across all qualifiers — see implementation note below
    Ok(gateway.saturating_sub(reserved))
}
```

**Implementation note:** `gateway_reserved` is qualified per trade. The free balance must aggregate across **all** trade qualifiers — i.e. sum of all `gateway_reserved:USDC:*` rows. If `account_balance` requires an exact qualifier, add a new repository method `aggregate_balance_by_type(account_type, asset)` that sums all qualifiers. Test it.

Use this `gateway_free_balance` in the gateway-quoteability gate.

**Step 4: Verify pass**

Run: `cargo test --test runtime_integration rfq_gateway_quoteability_uses_ledger_minus_reserved`
Expected: PASS.

**Step 5: Commit**

```bash
git add src/application/rfq.rs src/adapters/persistence/ledger.rs tests/runtime_integration.rs
git commit -m "feat(rfq): use ledger gateway minus gateway_reserved for quoteability"
```

---

### Task 10: Phase 1 verification gate

**Step 1: Full suite**

```
cargo build
cargo test
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

If `cargo fmt --check` fails on files unrelated to this PR, report and skip — do not reformat.

**Step 2: Manual sanity**

Start the server: `cargo run` (in another terminal).
Call: `curl http://127.0.0.1:5050/v1/runtime/state` — should still work (no contract change).
Stop the server.

**Step 3: Tag for worktree branch-off**

```bash
git tag phase-1-foundation
```

This tag is the rooting point for the three Phase-2 worktrees.

---

## Phase 2 — Parallel worktrees

Use the `superpowers:using-git-worktrees` skill when creating each worktree. Three worktrees branch off `phase-1-foundation` and run independently.

```
git worktree add ../firmament-recon-worker -b recon-worker phase-1-foundation
git worktree add ../firmament-read-endpoints -b read-endpoints phase-1-foundation
git worktree add ../firmament-gateway-lifecycle -b gateway-lifecycle phase-1-foundation
```

Dispatch subagents per worktree (one subagent owns one worktree end-to-end).

---

### Worktree A — Reconciliation worker

#### Task A1: Add `RuntimeEvent::Reconciliation` variant

**Files:**
- Modify: `src/domain/events.rs`

**Step 1: Read existing `RuntimeEvent` enum** (in `src/domain/events.rs:39-54`).

**Step 2: Add a new top-level variant**

```rust
    /// Reconciliation observation tick (always emitted, even on skip).
    Reconciliation(ReconciliationEvent),
```

And the supporting types in the same file:

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReconciliationEvent {
    Tick {
        metadata: EventMetadata,
        asset: AssetId,
        scope: ReconciliationScope,
        on_chain_raw: String,
        expected_raw: String,
        drift_raw: String,
        outcome: ReconciliationOutcome,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconciliationScope { Wallet, Gateway }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReconciliationOutcome {
    WithinDust,
    Building { observations: u8 },
    Adjusted { idempotency_key: String },
    Skipped { reason: String },
}
```

**Step 3: Build**

Run: `cargo build --lib`
Expected: clean.

**Step 4: Commit**

```bash
git add src/domain/events.rs
git commit -m "feat(events): add ReconciliationEvent for drift observation"
```

---

#### Task A2: Drift window data structure

**Files:**
- Create: `src/application/runtime/reconciliation/mod.rs`
- Create: `src/application/runtime/reconciliation/drift.rs`

**Step 1: Failing test**

Create `src/application/runtime/reconciliation/drift.rs` with a stub and unit tests:

```rust
//! Per-account drift observation window.
use std::collections::VecDeque;

#[derive(Debug, Clone)]
pub struct DriftWindow { /* TODO */ }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_triggers_after_three_same_sign_observations_above_dust() {
        let mut w = DriftWindow::new(/* dust */ 100);
        assert_eq!(w.observe(50), DriftOutcome::WithinDust);
        assert_eq!(w.observe(200), DriftOutcome::Building { observations: 1 });
        assert_eq!(w.observe(210), DriftOutcome::Building { observations: 2 });
        assert_eq!(w.observe(205), DriftOutcome::Trigger { drift: 205 });
    }

    #[test]
    fn window_resets_on_sign_flip() {
        let mut w = DriftWindow::new(100);
        w.observe(200);
        w.observe(210);
        assert_eq!(w.observe(-200), DriftOutcome::Building { observations: 1 });
    }

    #[test]
    fn window_resets_on_magnitude_swing_over_50_percent() {
        let mut w = DriftWindow::new(100);
        w.observe(200);
        w.observe(210);
        assert_eq!(w.observe(500), DriftOutcome::Building { observations: 1 });
    }

    #[test]
    fn window_below_dust_is_within_dust() {
        let mut w = DriftWindow::new(100);
        assert_eq!(w.observe(50), DriftOutcome::WithinDust);
    }
}
```

**Step 2: Run to confirm fail**

Run: `cargo test --lib application::runtime::reconciliation::drift::tests`
Expected: compile failure / test stub failure.

**Step 3: Implement**

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriftOutcome {
    WithinDust,
    Building { observations: u8 },
    Trigger { drift: i128 },
}

#[derive(Debug, Clone)]
pub struct DriftWindow {
    dust: u128,
    consecutive: u8,
    threshold: u8,
    history: VecDeque<i128>,
}

impl DriftWindow {
    pub fn new(dust: u128) -> Self {
        Self {
            dust,
            consecutive: 3,
            threshold: 3,
            history: VecDeque::with_capacity(3),
        }
    }

    pub fn observe(&mut self, drift: i128) -> DriftOutcome {
        if drift.unsigned_abs() <= self.dust {
            self.history.clear();
            return DriftOutcome::WithinDust;
        }

        if let Some(&last) = self.history.back() {
            let same_sign = (last >= 0) == (drift >= 0);
            let last_abs = last.unsigned_abs();
            let drift_abs = drift.unsigned_abs();
            let max = last_abs.max(drift_abs);
            let min = last_abs.min(drift_abs);
            let within_50_pct = max == 0 || (max - min) * 2 <= max;
            if !same_sign || !within_50_pct {
                self.history.clear();
            }
        }

        if self.history.len() == 3 { self.history.pop_front(); }
        self.history.push_back(drift);

        if self.history.len() == self.threshold as usize {
            let avg = self.history.iter().sum::<i128>() / self.history.len() as i128;
            self.history.clear();
            DriftOutcome::Trigger { drift: avg }
        } else {
            DriftOutcome::Building { observations: self.history.len() as u8 }
        }
    }
}
```

In `src/application/runtime/reconciliation/mod.rs`:

```rust
//! Always-on Solana reconciliation worker.
pub mod drift;
pub mod wallet_monitor;
pub mod gateway_monitor;
pub use drift::{DriftOutcome, DriftWindow};
```

Wire the new module into `src/application/runtime/mod.rs` (add `pub mod reconciliation;`).

**Step 4: Verify pass**

Run: `cargo test --lib application::runtime::reconciliation::drift::tests`
Expected: all 4 tests pass.

**Step 5: Commit**

```bash
git add src/application/runtime/reconciliation/ src/application/runtime/mod.rs
git commit -m "feat(reconciliation): add drift window with consecutive-observation rule"
```

---

#### Task A3: Wallet monitor — expected cash + drift detection

**Files:**
- Create: `src/application/runtime/reconciliation/wallet_monitor.rs`
- Create: `tests/reconciliation.rs`

**Step 1: Failing test in `tests/reconciliation.rs`**

```rust
//! Tests for the always-on reconciliation worker.
// (Add assertions per design §3 hard guards.)
#[tokio::test]
async fn recon_wallet_positive_drift_adjusts_after_three_consecutive_ticks() {
    // Bootstrap fakes: ledger seeded with working_custody:USDC = 1_000_000.
    // Fake BalanceReader returns 1_001_000 (drift = +1000, above dust=10_000? No.
    // Use a drift that's ABOVE dust — set fake to return 1_020_000 (drift = +20_000).
    // Tick 3 times via worker.tick().await.
    // After 3 ticks, assert ledger has external → working_custody adjustment of 20_000.
    // Assert RuntimeEvent::Reconciliation::Adjusted was emitted with idempotency_key.
}
```

Write the full test using the existing `bootstrap_with_fakes` helper. Drive ticks deterministically — the worker should expose a `tick()` method for tests in addition to its background loop.

**Step 2: Run to verify failure**

Run: `cargo test --test reconciliation recon_wallet_positive_drift_adjusts`
Expected: FAIL (module doesn't exist).

**Step 3: Implement `wallet_monitor.rs`**

```rust
//! Wallet-side reconciliation: working_custody + reserved + pending_dex_spend vs on-chain.
use std::collections::HashMap;
use crate::adapters::persistence::ledger::{LedgerAccountId, LedgerAccountType, SqliteLedgerRepository};
use crate::domain::types::AssetId;
use crate::ports::balance_reader::BalanceReader;
use super::DriftWindow;

pub struct WalletMonitor<'a> {
    repo: &'a SqliteLedgerRepository,
    balance_reader: &'a dyn BalanceReader,
    windows: HashMap<AssetId, DriftWindow>,
    dust_per_asset: HashMap<AssetId, u128>,
}

impl<'a> WalletMonitor<'a> {
    pub async fn observe_asset(&mut self, asset: &AssetId) -> WalletObservation { /* ... */ }
}

pub struct WalletObservation {
    pub asset: AssetId,
    pub on_chain: BigUint,
    pub expected: BigUint,
    pub drift: i128,
    pub outcome: ObservationOutcome,
}
```

Implement `expected = working_custody + sum(reserved:*) + sum(pending_dex_spend:*)`. Drift = on-chain − expected. Run through the per-asset drift window. On `Trigger`, post a balanced ledger transaction:
- positive drift: debit `working_custody`, credit `external:reconciliation`
- negative drift: debit `external:reconciliation`, credit `working_custody`

Idempotency key: `recon:wallet:{asset}:{utc_date_iso}:{seq}`. Seq seeds from a `recon_state` in-memory map keyed by date; on startup, query the ledger for today's reconciliation idempotency keys and seed the seq.

In-flight guard: before applying any adjustment, check if any trade is in `TakerLocked` / `MakerLocked` state with non-zero `pending_escrow + htlc_escrow + receivable + pending_dex_spend` for this asset. If so, emit `Skipped { reason: "trade_in_flight" }` and reset the drift window.

**Step 4: Verify pass**

```
cargo test --test reconciliation recon_wallet_positive_drift_adjusts
cargo test --test reconciliation
```

Expected: targeted test passes; new file's other tests remain to be added.

**Step 5: Commit**

```bash
git add src/application/runtime/reconciliation/wallet_monitor.rs tests/reconciliation.rs
git commit -m "feat(reconciliation): wallet monitor with expected-cash formula and adjustment"
```

---

#### Task A4: Wallet monitor — remaining test cases

For each in §5 of the design under reconciliation tests, add a focused test:

| Test | Scenario |
|---|---|
| `recon_wallet_negative_drift_adjusts` | drift = −20_000, asserts `working_custody → external` |
| `recon_dust_threshold_skips` | drift = 5_000 (under dust 10_000), no adjustment, emits `WithinDust` |
| `recon_sign_flip_resets_window` | +20_000, +20_000, −20_000 → no adjustment, emits `Building { observations: 1 }` after the flip |
| `recon_in_flight_trade_pauses` | seed reserved:USDC for an in-flight trade; drift triggers but adjustment skipped; emit `Skipped { reason: "trade_in_flight" }` |
| `recon_emits_event_on_skip` | for any skip path, `RuntimeEvent::Reconciliation::Tick` must be emitted with `outcome = Skipped` |

One test → one commit.

```bash
git add tests/reconciliation.rs
git commit -m "test(reconciliation): wallet monitor coverage"
```

---

#### Task A5: Gateway monitor

**Files:**
- Create: `src/application/runtime/reconciliation/gateway_monitor.rs`
- Modify: `tests/reconciliation.rs`

**Step 1: Failing tests**

```rust
#[tokio::test]
async fn recon_gateway_positive_drift_adjusts_external_to_gateway() { /* ... */ }

#[tokio::test]
async fn recon_gateway_skip_when_gateway_reserved_active() {
    // Seed gateway_reserved:USDC = 100. Set fake gateway balance below expected.
    // Tick 3 times. Assert NO ledger adjustment posted; emit Skipped { reason: "gateway_reserved_active" }.
}
```

**Step 2: Run to verify failure**

Run: `cargo test --test reconciliation recon_gateway`
Expected: FAIL.

**Step 3: Implement `gateway_monitor.rs`**

Same shape as `wallet_monitor.rs` but:
- expected = `gateway:USDC:circle:solana` + sum(`gateway_reserved:USDC:*`)
- on-chain = call `CircleGatewayClient::balance()` for the configured Solana address
- For NEGATIVE drift: if `sum(gateway_reserved) > 0`, emit `Skipped { reason: "gateway_reserved_active" }` and reset window. Never post the adjustment in this case.
- For POSITIVE drift: post `external:reconciliation → gateway` regardless (positive drift is always safe to credit).
- Idempotency key prefix: `recon:gateway:USDC:{utc_date}:{seq}`.

**Step 4: Verify pass**

Run: `cargo test --test reconciliation recon_gateway`
Expected: PASS.

**Step 5: Commit**

```bash
git add src/application/runtime/reconciliation/gateway_monitor.rs tests/reconciliation.rs
git commit -m "feat(reconciliation): gateway monitor with reserved-active guard"
```

---

#### Task A6: Reconciliation config

**Files:**
- Modify: `src/config.rs` (locate the `AppConfig` struct)
- Modify: `config.example.toml`

**Step 1: Add config struct**

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct ReconciliationConfig {
    #[serde(default = "default_recon_interval")]
    pub interval_seconds: u64,
    #[serde(default = "default_recon_threshold")]
    pub consecutive_ticks_for_adjustment: u8,
    #[serde(default = "default_recon_emit_on_skip")]
    pub emit_event_on_skip: bool,
    #[serde(default)]
    pub dust: HashMap<String, u128>,
}

fn default_recon_interval() -> u64 { 10 }
fn default_recon_threshold() -> u8 { 3 }
fn default_recon_emit_on_skip() -> bool { true }

impl Default for ReconciliationConfig {
    fn default() -> Self {
        let mut dust = HashMap::new();
        dust.insert("USDC".to_string(), 10_000);
        dust.insert("SOL".to_string(), 100_000);
        dust.insert("cbBTC".to_string(), 100);
        Self {
            interval_seconds: 10,
            consecutive_ticks_for_adjustment: 3,
            emit_event_on_skip: true,
            dust,
        }
    }
}
```

Add `pub reconciliation: ReconciliationConfig` (with `#[serde(default)]`) to `AppConfig`.

**Step 2: Add to `config.example.toml`**

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

**Step 3: Build + test config load**

Run: `cargo build && cargo test --lib config`
Expected: clean. If a config-load test exists, add an assertion that `AppConfig::load()` returns the expected default reconciliation config when the section is missing.

**Step 4: Commit**

```bash
git add src/config.rs config.example.toml
git commit -m "feat(config): reconciliation defaults and dust thresholds"
```

---

#### Task A7: Wire the worker into bootstrap

**Files:**
- Modify: `src/application/runtime/bootstrap.rs`

**Step 1: Spawn the worker**

In the bootstrap function (after the orchestrator is constructed and event publisher exists), spawn:

```rust
let recon_handle = tokio::spawn(reconciliation::run_loop(
    orchestrator.clone(),
    config.reconciliation.clone(),
    shutdown_signal.clone(),
));
```

Implement `run_loop` in `reconciliation/mod.rs` — it ticks at `interval_seconds`, calls `WalletMonitor::observe_asset` for each enabled asset, then `GatewayMonitor::observe_asset(USDC)`. Honour shutdown signal.

**Step 2: Idempotency reseed at startup**

In `run_loop` initialization, query ledger for today's reconciliation transactions (where `idempotency_key LIKE 'recon:%'`) and seed the in-memory seq counters per scope+asset+date.

**Step 3: Add an integration test**

In `tests/reconciliation.rs`:

```rust
#[tokio::test]
async fn recon_idempotent_across_restart() {
    // Tick once, posting an adjustment with idempotency key K.
    // Restart the worker (drop and recreate).
    // Tick again with same drift.
    // Assert no duplicate adjustment was posted (count of recon:* idempotency keys for today == 1).
}
```

**Step 4: Run**

```
cargo test --test reconciliation recon_idempotent_across_restart
cargo test
```

Expected: green.

**Step 5: Commit**

```bash
git add src/application/runtime/bootstrap.rs src/application/runtime/reconciliation/mod.rs tests/reconciliation.rs
git commit -m "feat(reconciliation): always-on worker spawned at bootstrap"
```

---

#### Task A8: Worktree A verification

```
cargo build
cargo test
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

All green → push branch → ready for merge in Phase 3.

```bash
git push -u origin recon-worker
```

---

### Worktree B — Public read endpoints

#### Task B1: `GET /health` endpoint

**Files:**
- Create: `src/interfaces/http/runtime_read.rs`
- Modify: `src/interfaces/http/mod.rs`
- Modify: `tests/api.rs`

**Step 1: Failing test**

In `tests/api.rs`:

```rust
#[tokio::test]
async fn health_endpoint_returns_ok() {
    let app = build_test_app().await;
    let response = app
        .oneshot(Request::builder().uri("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap()
    ).unwrap();
    assert_eq!(body["status"], "ok");
    assert_eq!(body["service"], "firmament");
}
```

(Use the existing test harness pattern in `tests/api.rs` — locate `build_test_app` or its equivalent.)

**Step 2: Run to verify failure**

Run: `cargo test --test api health_endpoint_returns_ok`
Expected: FAIL — 404.

**Step 3: Implement**

In `runtime_read.rs`:

```rust
use axum::Json;
use serde_json::{json, Value};

pub async fn get_health() -> Json<Value> {
    Json(json!({
        "status": "ok",
        "service": "firmament",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}
```

In `src/interfaces/http/mod.rs:113-141` (the `Router::new()` chain), add:

```rust
        .route("/health", get(runtime_read::get_health))
```

Add `pub mod runtime_read;` near the top.

**Step 4: Verify pass**

Run: `cargo test --test api health_endpoint_returns_ok`
Expected: PASS.

**Step 5: Commit**

```bash
git add src/interfaces/http/runtime_read.rs src/interfaces/http/mod.rs tests/api.rs
git commit -m "feat(http): add /health liveness endpoint"
```

---

#### Task B2: `GET /v1/runtime/ledger` endpoint

**Files:**
- Modify: `src/interfaces/http/runtime_read.rs`
- Modify: `src/interfaces/http/types.rs`
- Modify: `src/interfaces/http/mod.rs`
- Modify: `tests/api.rs`

**Step 1: Failing test**

```rust
#[tokio::test]
async fn ledger_endpoint_returns_balances() {
    let app = build_test_app().await;
    seed_ledger_with_balances(&app).await;
    let response = app
        .oneshot(Request::builder().uri("/v1/runtime/ledger").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value = serde_json::from_slice(/* ... */).unwrap();
    assert_eq!(body["healthy"], true);
    assert!(body["entry_count"].as_u64().unwrap() > 0);
    assert!(body["balances"].as_array().unwrap().iter().any(
        |b| b["account_type"] == "working_custody" && b["asset"] == "USDC"
    ));
}
```

**Step 2: Run to verify failure**

Run: `cargo test --test api ledger_endpoint_returns_balances`
Expected: FAIL.

**Step 3: Implement**

In `types.rs`:

```rust
#[derive(Debug, Serialize)]
pub struct LedgerSnapshotResponse {
    pub healthy: bool,
    pub entry_count: u64,
    pub balances: Vec<LedgerBalanceEntry>,
}

#[derive(Debug, Serialize)]
pub struct LedgerBalanceEntry {
    pub account_type: String,
    pub asset: String,
    pub qualifier: Option<String>,
    pub balance_raw: String,
    pub decimals: u8,
    pub display_amount: String,
}
```

In `runtime_read.rs`:

```rust
#[derive(Debug, Deserialize)]
pub struct LedgerQuery {
    pub account_type: Option<String>,
}

pub async fn get_ledger(
    State(ctx): State<Arc<ApiContext>>,
    Query(q): Query<LedgerQuery>,
) -> Result<Json<LedgerSnapshotResponse>, ApiError> {
    let repo = ctx.runtime.ledger_repository();
    let asset_catalog = ctx.runtime.asset_catalog();

    let filter = match q.account_type.as_deref() {
        Some(s) => Some(LedgerAccountType::try_from(s).map_err(|_| ApiError::BadRequest {
            code: "invalid_account_type",
            message: format!("unknown account_type '{}'", s),
        })?),
        None => None,
    };

    let all_balances = repo.all_balances()?;
    let balances: Vec<LedgerBalanceEntry> = all_balances.into_iter()
        .filter(|b| filter.map_or(true, |f| b.account_type == f))
        .map(|b| {
            let asset = asset_catalog.get(&b.asset);
            let decimals = asset.map_or(0, |a| a.decimals);
            LedgerBalanceEntry {
                account_type: b.account_type.to_string(),
                asset: b.asset.to_string(),
                qualifier: b.qualifier,
                balance_raw: b.amount_raw.to_string(),
                decimals,
                display_amount: format_decimal(&b.amount_raw, decimals),
            }
        })
        .collect();

    let healthy = repo.integrity_report()?.is_healthy();
    let entry_count = repo.entry_count()?;

    Ok(Json(LedgerSnapshotResponse { healthy, entry_count, balances }))
}
```

In `mod.rs`, register the route. Make sure `RuntimeHandle` exposes `ledger_repository()` and `asset_catalog()` — if not, add accessors.

**Step 4: Verify pass + add filter test + invalid-type test**

```rust
#[tokio::test]
async fn ledger_endpoint_filters_by_account_type() { /* assert only working_custody returned */ }

#[tokio::test]
async fn ledger_endpoint_rejects_unknown_account_type() {
    // ?account_type=foo → 400 with code "invalid_account_type"
}
```

Run: `cargo test --test api ledger_endpoint`
Expected: PASS.

**Step 5: Commit**

```bash
git add src/interfaces/http/{runtime_read.rs,types.rs,mod.rs} tests/api.rs
git commit -m "feat(http): add GET /v1/runtime/ledger with account_type filter"
```

---

#### Task B3: `GET /v1/runtime/trades` endpoint

**Files:**
- Modify: `src/interfaces/http/runtime_read.rs`
- Modify: `src/interfaces/http/types.rs`
- Modify: `src/application/runtime/orchestrator.rs` (add `recent_trades(limit)` accessor + `tx_signatures: Vec<TradeSignature>` on `RuntimeTrade`)
- Modify: `tests/api.rs`

**Step 1: Failing tests**

```rust
#[tokio::test] async fn trades_endpoint_returns_recent_trades() { /* drive 3 fake trades, assert order/count */ }
#[tokio::test] async fn trades_endpoint_respects_limit_param() { /* ?limit=2 → 2 entries */ }
#[tokio::test] async fn trades_endpoint_caps_at_100() { /* ?limit=500 → 400 */ }
#[tokio::test] async fn trades_endpoint_includes_tx_signatures_with_kinds() { /* asserts each kind */ }
#[tokio::test] async fn trades_endpoint_excludes_rebalance_signatures() { /* fake rebalance swap, signature absent on user trade */ }
```

**Step 2: Run to verify failure**

```
cargo test --test api trades_endpoint
```

Expected: FAIL.

**Step 3: Implement**

Add to `RuntimeTrade` (in `src/application/runtime/orchestrator.rs`):

```rust
pub struct TradeSignature {
    pub kind: TradeSignatureKind,
    pub signature: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TradeSignatureKind {
    TakerLock, TakerRedeem, TakerRefund,
    MakerLock, MakerRedeem, MakerRefund,
    GatewayBurn, GatewayMint,
    JupiterSwap,
}
```

Append `tx_signatures` at the orchestrator hooks where settlement events update state (settlement_initiated → maker_lock; settlement_redeemed → maker_redeem or taker_redeem depending on leg; gateway events → gateway_burn/mint; swap events → jupiter_swap **but only when the swap is causally tied to a trade — set a `trade_id: Option<TradeId>` on the swap event and skip if `None`**).

Add accessor:

```rust
impl RuntimeOrchestrator {
    pub fn recent_trades(&self, limit: usize) -> Vec<RuntimeTrade> {
        let mut all: Vec<_> = self.trades.read().values().cloned().collect();
        all.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        all.truncate(limit);
        all
    }

    pub fn trade_counts(&self) -> (usize, usize) {
        let trades = self.trades.read();
        let total = trades.len();
        let successful = trades.values()
            .filter(|t| t.settlement_status == SettlementStatus::Redeemed)
            .count();
        (total, successful)
    }
}
```

In `runtime_read.rs`:

```rust
#[derive(Debug, Deserialize)]
pub struct TradesQuery {
    #[serde(default = "default_limit")]
    pub limit: usize,
}

fn default_limit() -> usize { 10 }

pub async fn get_trades(
    State(ctx): State<Arc<ApiContext>>,
    Query(q): Query<TradesQuery>,
) -> Result<Json<TradesResponse>, ApiError> {
    if q.limit == 0 || q.limit > 100 {
        return Err(ApiError::BadRequest {
            code: "invalid_limit",
            message: format!("limit must be 1..=100, got {}", q.limit),
        });
    }
    let orch = ctx.service.orchestrator(); // assumes accessor; see step below
    let (total_count, successful_count) = orch.trade_counts();
    let trades = orch.recent_trades(q.limit).into_iter()
        .map(TradeSummary::from_trade)
        .collect();
    Ok(Json(TradesResponse { total_count, successful_count, trades }))
}
```

**Step 4: Verify pass**

```
cargo test --test api trades_endpoint
```

Expected: PASS.

**Step 5: Commit**

```bash
git add src/interfaces/http/{runtime_read.rs,types.rs,mod.rs} src/application/runtime/orchestrator.rs tests/api.rs
git commit -m "feat(http): add GET /v1/runtime/trades with tx signatures"
```

---

#### Task B4: Privacy + healthy-flag tests

**Step 1: Add tests**

```rust
#[tokio::test]
async fn ledger_endpoint_omits_wallet_addresses() {
    // Drive a trade end-to-end. Fetch /v1/runtime/ledger.
    // Walk the JSON and assert no field contains a Solana-address-shaped string
    // (44-char base58 of pubkey). Trade IDs and signatures are allowed.
}

#[tokio::test]
async fn ledger_endpoint_marks_unhealthy_on_negative_protected_account() {
    // Force a ledger transaction that would (incorrectly) make working_custody negative
    // by hand-crafting an entry. Fetch /v1/runtime/ledger.
    // Assert healthy=false.
    // Cleanup so other tests aren't poisoned.
}
```

**Step 2: Run + commit**

```
cargo test --test api
git add tests/api.rs
git commit -m "test(http): privacy and healthy-flag invariants"
```

---

#### Task B5: Worktree B verification

```
cargo build
cargo test
cargo fmt --check
cargo clippy --all-targets -- -D warnings
git push -u origin read-endpoints
```

---

### Worktree C — Execution path + Gateway-backed lifecycle

#### Task C1: Add `ExecutionPath` enum

**Files:**
- Modify: `src/domain/types.rs`

**Step 1: Add enum**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionPath {
    InventoryToInventory,
    GatewayToDex,
}
```

**Step 2: Failing test**

In `tests/runtime_integration.rs`:

```rust
#[tokio::test]
async fn quote_records_inventory_to_inventory_path_when_custody_covers() {
    // seed working_custody:SOL to cover the requested output → quote produced with path = InventoryToInventory.
}

#[tokio::test]
async fn quote_records_gateway_to_dex_path_when_only_gateway_covers() {
    // seed working_custody:SOL = 0; gateway:USDC = enough → path = GatewayToDex.
}
```

**Step 3: Implement path selection**

In `src/application/rfq.rs`, add `select_execution_path` that:
1. If `ledger.working_custody:{output_asset}` covers requested output → `InventoryToInventory`.
2. Else if `gateway:USDC - gateway_reserved:USDC` (in USDC equivalent of output) covers → `GatewayToDex`.
3. Else reject with `insufficient_liquidity`.

Attach `execution_path` to the `Quote` (and `RuntimeTrade`) struct in memory. No DB persistence.

**Step 4: Verify + commit**

```
cargo test --test runtime_integration quote_records_
git add src/domain/types.rs src/application/rfq.rs src/application/runtime/orchestrator.rs tests/runtime_integration.rs
git commit -m "feat(rfq): record execution_path on quote and trade"
```

---

#### Task C2: Gateway-backed USDC lifecycle wiring

**Files:**
- Modify: `src/adapters/persistence/ledger.rs` (LedgerEventConsumer)
- Modify: `src/application/runtime/orchestrator.rs` (event emission for gateway events)
- Modify: `tests/runtime_integration.rs`

**Step 1: Failing test**

```rust
#[tokio::test]
async fn settlement_gateway_usdc_path_emits_full_ledger_sequence() {
    // Bootstrap with execution_path = GatewayToDex, output asset = USDC.
    // Drive: taker lock confirmed → repricing → reservation → gateway burn → gateway mint
    //        → maker leg (working_custody → reserved → pending_escrow → htlc_escrow)
    //        → taker redeem → maker redeem.
    // After each event, assert ledger movements per design §2 Gateway-backed USDC table.
    // Specifically check gateway_reserved goes to zero after gateway mint (released into trading then custody).
}
```

**Step 2: Run to verify failure**

Run: `cargo test --test runtime_integration settlement_gateway_usdc`
Expected: FAIL.

**Step 3: Implement consumer mapping for gateway events**

In `LedgerEventConsumer`, on the gateway event arms:
- `Gateway::BurnIntentSubmitted { trade_id, amount, ... }` (or equivalent) → `gateway → gateway_reserved` if not already reserved at taker-lock-confirmed; otherwise `gateway_reserved → trading`.
- `Gateway::MintConfirmed { trade_id, amount, ... }` → `trading → working_custody` (USDC).

Adjust orchestrator to emit a Gateway event when the burn intent is submitted **with a trade_id correlator**, and another when mint is confirmed.

The reservation step (`gateway → gateway_reserved`) happens at the same point as `working_custody → reserved` for inventory paths: taker-lock-confirmed + repricing-pass.

**Step 4: Verify pass**

Run: `cargo test --test runtime_integration settlement_gateway_usdc`
Expected: PASS.

**Step 5: Commit**

```bash
git add src/adapters/persistence/ledger.rs src/application/runtime/orchestrator.rs src/domain/events.rs tests/runtime_integration.rs
git commit -m "feat(lifecycle): gateway-backed USDC ledger sequence"
```

---

#### Task C3: Gateway-backed SOL/cbBTC (Jupiter) lifecycle wiring

**Files:** same as C2 plus swap events.

**Step 1: Failing test**

```rust
#[tokio::test]
async fn settlement_gateway_sol_path_emits_jupiter_spend_sequence() {
    // execution_path = GatewayToDex, output asset = SOL.
    // Drive: taker lock → reserve gateway → gateway burn → gateway mint
    //        → jupiter swap submitted → jupiter swap confirmed
    //        → maker leg using SOL → taker redeem → maker redeem.
    // Assert ledger:
    //   - working_custody:USDC → pending_dex_spend:USDC (on swap submit, qualifier = trade_id)
    //   - pending_dex_spend:USDC → trading:USDC AND trading:SOL → working_custody:SOL (on swap confirm)
    //   - working_custody:SOL → reserved:SOL → pending_escrow:SOL → htlc_escrow:SOL
}
```

**Step 2: Run to verify failure**

Run: `cargo test --test runtime_integration settlement_gateway_sol`
Expected: FAIL.

**Step 3: Implement consumer mapping for swap events**

On `SwapEvent::Submitted { trade_id: Some(tid), input_amount, output_amount, ... }`:
- `working_custody → pending_dex_spend` (input asset, qualifier=trade_id)

On `SwapEvent::Confirmed { trade_id: Some(tid), input_amount_used, output_amount_received }`:
- `pending_dex_spend → trading` (input asset)
- `trading → working_custody` (output asset)

Critically: only post these movements when the swap event has a non-`None` `trade_id`. Background rebalances stay on the existing `working_custody → rebalance → working_custody` path (which is unrelated to trade lifecycle).

**Step 4: Verify pass**

```
cargo test --test runtime_integration settlement_gateway_sol_path
```

Expected: PASS.

**Step 5: Commit**

```bash
git add src/adapters/persistence/ledger.rs src/application/runtime/orchestrator.rs src/domain/events.rs tests/runtime_integration.rs
git commit -m "feat(lifecycle): gateway+jupiter ledger sequence for non-USDC output"
```

---

#### Task C4: Failure-path tests

**Step 1: Add tests**

```rust
#[tokio::test]
async fn settlement_gateway_failure_releases_reservation() {
    // Trigger a gateway burn that fails post-reserve. Assert gateway_reserved → gateway (released).
}

#[tokio::test]
async fn settlement_jupiter_failure_unwinds_pending_dex_spend() {
    // Trigger a swap submit then a swap-failed event. Assert pending_dex_spend → working_custody.
}

#[tokio::test]
async fn settlement_repricing_failure_skips_reservation() {
    // Force quote to be expired before taker-lock-confirmed. Assert no working_custody → reserved movement.
    // RuntimeTrade reaches a Cancelled state with reason "repricing_failed".
}
```

**Step 2: Implement minimal failure-event handling in the consumer if missing.**

**Step 3: Run + commit**

```
cargo test --test runtime_integration
git add src/adapters/persistence/ledger.rs src/application/runtime/orchestrator.rs tests/runtime_integration.rs
git commit -m "test(lifecycle): failure-path ledger movements"
```

---

#### Task C5: Worktree C verification

```
cargo build
cargo test
cargo fmt --check
cargo clippy --all-targets -- -D warnings
git push -u origin gateway-lifecycle
```

---

## Phase 3 — Integration

### Task M1: Merge worktrees

Order: A → B → C (most domain coupling last).

```bash
git checkout v0.1.0
git merge --no-ff recon-worker
# resolve conflicts if any
cargo test
git merge --no-ff read-endpoints
cargo test
git merge --no-ff gateway-lifecycle
cargo test
```

**Stop and ask** before resolving any non-trivial merge conflict — flag the conflicting files and discuss.

After all three merges:

```
cargo build
cargo test
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

All green required.

---

### Task M2: Frontend alignment

**Files:**
- Modify: `web/app/src/api.ts`
- Modify: `web/app/src/pages/RuntimePage.tsx`

**Step 1: Add API clients**

In `web/app/src/api.ts`:

```typescript
export type LedgerBalance = {
  account_type: string;
  asset: string;
  qualifier: string | null;
  balance_raw: string;
  decimals: number;
  display_amount: string;
};

export type LedgerSnapshot = {
  healthy: boolean;
  entry_count: number;
  balances: LedgerBalance[];
};

export type TxSignature = { kind: string; signature: string };

export type TradeAmount = { asset: string; amount_raw: string; decimals: number; display_amount: string };

export type TradeSummary = {
  trade_id: string;
  quote_id: string;
  settlement_status: string;
  input: TradeAmount;
  output: TradeAmount;
  tx_signatures: TxSignature[];
};

export type TradesSnapshot = { total_count: number; successful_count: number; trades: TradeSummary[] };

export async function getHealth() { /* fetch /health */ }
export async function getLedger(accountType?: string): Promise<LedgerSnapshot> { /* ... */ }
export async function getTrades(limit = 10): Promise<TradesSnapshot> { /* ... */ }
```

**Step 2: Wire into RuntimePage**

In `RuntimePage.tsx`, replace any old runtime-state-only calls with the new endpoints. Render:
- A balance grid grouped by `account_type`.
- A trade list showing input/output amounts, status badge, and signature links (to a Solana explorer URL).

Refresh on a 5-second interval. Match existing styling — no new components beyond what's needed.

**Step 3: Smoke**

In one terminal: `cargo run`.
In another: `cd web/app && npm run dev`.
Open `http://127.0.0.1:3000/app/`. Verify the runtime block renders without console errors.

**Step 4: Commit**

```bash
git add web/app/src/api.ts web/app/src/pages/RuntimePage.tsx
git commit -m "feat(web): align runtime page with new ledger and trades endpoints"
```

---

### Task M3: Live-mainnet test additions (no execution)

**Files:**
- Modify: `tests/live_mainnet.rs`

**Step 1: Add new live-test fns**

Following the existing pattern (env-gated, early-return skip, opt-in), add:

- `live_inventory_path_full_lifecycle` — gated `RUN_LIVE_INVENTORY_PATH=1`. Small inventory-backed RFQ on the maker wallet. Reads keys via `LoadedWallet::from_maker_env()` / `from_taker_env()`. Asserts the new ledger sequence on success.
- `live_reconciliation_observation` — gated `RUN_LIVE_RECONCILIATION=1`. Boots the runtime with reconciliation in observe-only mode (add a config knob `dry_run`); ticks once; asserts a `RuntimeEvent::Reconciliation::Tick` was emitted with a real `on_chain_raw` value.

**Do not write tests for `RUN_LIVE_GATEWAY_USDC_PATH` or `RUN_LIVE_GATEWAY_SOL_PATH` yet — those are mutating mainnet and need explicit per-action approval before scaffolding.**

**Step 2: Verify they compile but skip when env not set**

Run: `cargo test --test live_mainnet`
Expected: tests run, all skip with the standard "env not set" message.

**Step 3: Commit**

```bash
git add tests/live_mainnet.rs
git commit -m "test(live): inventory-path and reconciliation-observation live tests"
```

---

### Task M4: Final verification gate

**Step 1: Full default suite**

```
cargo build
cargo test
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

If `cargo fmt --check` flags pre-existing unrelated drift, **report it**, do not reformat.

**Step 2: Manual API smoke**

In one terminal: `cargo run`.

```
curl http://127.0.0.1:5050/health
curl http://127.0.0.1:5050/v1/runtime/ledger
curl 'http://127.0.0.1:5050/v1/runtime/ledger?account_type=working_custody'
curl 'http://127.0.0.1:5050/v1/runtime/trades?limit=10'
```

Verify shapes match design §4. Capture any deviation as a follow-up task.

**Step 3: Frontend smoke**

`cd web/app && npm run dev`. Open `http://127.0.0.1:3000/app/`. Walk the runtime block, confirm rendering.

**Step 4: Live-mainnet inventory test (only after explicit user approval)**

```
RUN_LIVE_SOLANA_TESTS=1 RUN_LIVE_INVENTORY_PATH=1 cargo test --test live_mainnet -- --ignored
```

This requires `.env` to be loaded with the maker/taker keys. **Do not run unless user explicitly says yes for this run.**

**Step 5: Final report**

Produce a status report listing:
- Final design decisions implemented (point-by-point against the master prompt).
- All files changed.
- All tests run + pass/fail/skip counts.
- Any deviations from plan with rationale.
- Remaining risks/follow-ups (from design §Risks & follow-ups).

---

## Skill references

- `superpowers:test-driven-development` — every task uses red→green→refactor.
- `superpowers:using-git-worktrees` — Phase 2 worktree creation.
- `superpowers:subagent-driven-development` — dispatch one subagent per task or per worktree.
- `superpowers:verification-before-completion` — gates at end of Phase 1, end of each worktree, and end of Phase 3.
- `superpowers:requesting-code-review` — before final merge to `v0.1.0`.

## Stop conditions (recap)

Pause and ask before:
- Adding any dependency.
- Any DDL change.
- Deleting files.
- Editing `.env`, secrets, keypairs.
- Running mutating mainnet (Jupiter swap, Gateway refill, real HTLC submit).
- Refactors beyond what these tasks require.
- Frontend changes beyond API compatibility.
- Anything contradicting the design doc.
