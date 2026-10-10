# Firm FX Payments Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task (Azeem's rules forbid general-purpose subagents, so subagent-driven execution is out). Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn Firmament into a firm-quote FX maker for stablecoin payments (USDC↔EURC, exact-output quotes, third-party recipients), redeploy it, and submit it to the Crypto World's Fair by 2026-10-12 23:59 PDT.

**Architecture:** The Rust runtime keeps its RFQ → HTLC settlement → ledger shape. Three additive changes: an exact-output branch in the quote engine, an enum for the requested amount side threaded from HTTP to the engine, and a `recipient` carried in the persisted settlement state that becomes the maker lock's redeemer and the redeem transaction's destination. The web app gets a Pay page over the same API; the swap page stays at `/app/swap`.

**Tech Stack:** Rust 1.85+ (axum, rust_decimal, solana-sdk 3, rusqlite), Vite + React + TypeScript (`web/app`), Next.js static export (`landing`), Docker on Railway (API), Vercel (web app, landing), Remotion (demo video).

**Spec:** `docs/plans/2026-10-10-firm-fx-payments-design.md`

## Global Constraints

- Deadline: 2026-10-12 23:59 PDT (2026-10-13 12:29 IST). Submission form, videos and links must be in by then.
- Prior code disclosure: everything before 2026-09-14 is prior work; the submission says so.
- EURC mint `HzwqbKZw8HxMN6bF2yFZNrht3c2iXXzpKcFu7uBEDKtr`, SPL Token program, 6 decimals.
- Exact-output rounding always favours the maker (ceil on input, never under-collect).
- `cargo test`, `cargo clippy`, `cargo fmt --check`, `npm run web:polish`, `npm run app:build`, `npm run landing:build` green before every push.
- No secrets in commands, logs, chat, or git: the maker key lives at `~/.config/solana/firmament-maker.json`; Railway and Vercel secrets are typed into their dashboards by Azeem.
- Commit only when Azeem says so; otherwise stage and stop at the commit step.
- Mainnet demo caps stay tiny (≤ $5 per action, ≤ $25 per day).
- Rust rules from `~/.claude/rules/rust.md` apply: no `bool` parameters in new signatures, no `unwrap` outside tests, newtypes over bare strings.

## Review Focus

1. Exact-output amount with more decimals than the output asset supports (e.g. `"0.0000001"` EURC): must be a 400 `invalid_amount` naming EURC's 6 decimals, never a silently truncated quote. Test added in Task 3.
2. Recipient equal to the maker wallet: the maker would lock funds redeemable by itself. Must be a 400 `invalid_recipient`. Test added in Task 5.
3. Recipient with no EURC token account: the redeem transaction must carry an idempotent create-ATA instruction paid by the sender, otherwise the redeem fails on chain after the sender has already locked. Unit test added in Task 5; live smoke in Task 10 uses a fresh recipient wallet.
4. Quote expiring while the sender signs: the lock must be refused by the existing pre-lock expiry path and the Pay page must return to the form with "Quote expired, request a new one", never leave the user on a dead button. Manual check in Task 7 step 9.
5. Resuming the Pay page with a different wallet connected than the one that paid: stored state must be discarded, never replayed with the wrong payer. Covered by the wallet-mismatch guard in Task 7 and its manual check.

---

### Task 1: Exact-output quote math

**Files:**
- Modify: `src/domain/quote_engine.rs` (add `calculate_quote_exact_output`, `decimal_to_raw_ceil`, tests at the bottom)

**Interfaces:**
- Consumes: `calculate_quote`, `inventory_skew_bps`, `decimal_factor`, `raw_to_decimal` already in the file.
- Produces: `pub fn calculate_quote_exact_output(pair: AssetPair, output_amount_raw: AmountRaw, reference_price: ReferencePrice, input_decimals: u8, output_decimals: u8, inventory: &InventorySnapshot, config: &QuoteEngineConfig) -> Result<QuoteComputation, QuoteMathError>`. Task 2 calls it.

- [ ] **Step 1: Write the failing tests**

Append inside the existing `mod tests` in `src/domain/quote_engine.rs`:

```rust
    fn check_exact_output_round_trip(output_raw: u64, price: Decimal, sol_balance: u64) {
        let pair = AssetPair::new(usdc(), sol());
        let reference = ReferencePrice {
            pair: pair.clone(),
            output_per_input: price,
            observed_at: now(),
        };
        let config = QuoteEngineConfig::default();
        let inventory = inventory(sol_balance, 1_000_000_000);

        let exact = calculate_quote_exact_output(
            pair.clone(),
            AmountRaw::new(output_raw),
            reference.clone(),
            6,
            9,
            &inventory,
            &config,
        )
        .expect("exact-output quote");
        assert_eq!(exact.output_amount.amount_raw, AmountRaw::new(output_raw));
        assert_eq!(exact.breakdown.reference_output_amount_raw, AmountRaw::new(output_raw));

        let replay = calculate_quote(
            pair,
            exact.input_amount.amount_raw,
            reference,
            6,
            9,
            &inventory,
            &config,
        )
        .expect("replayed exact-input quote");
        assert!(
            replay.output_amount.amount_raw.as_u64() >= output_raw,
            "output {output_raw} at price {price} with SOL balance {sol_balance}: replaying input {} pays only {}",
            exact.input_amount.amount_raw.as_u64(),
            replay.output_amount.amount_raw.as_u64()
        );
    }

    #[test]
    fn exact_output_quote_rounds_the_input_up() {
        let config = QuoteEngineConfig {
            base_spread_bps: 30,
            fee_bps: 5,
            min_profit_bps: 20,
            max_inventory_skew_bps: 50,
        };

        // 0.01 SOL at 0.01 SOL per USDC is 1 USDC before spread; 35 bps on top
        // is 1 / 0.9965 = 1_003_512.29 raw, rounded up.
        let quote = calculate_quote_exact_output(
            AssetPair::new(usdc(), sol()),
            AmountRaw::new(10_000_000),
            reference_price(),
            6,
            9,
            &inventory(1_000_000_000, 1_000_000_000),
            &config,
        )
        .expect("quote should calculate");

        assert_eq!(quote.input_amount.amount_raw, AmountRaw::new(1_003_513));
        assert_eq!(quote.output_amount.amount_raw, AmountRaw::new(10_000_000));
        assert_eq!(quote.breakdown.total_spread_bps, 35);
    }

    #[test]
    fn exact_output_quote_widens_when_output_inventory_is_under_target() {
        let config = QuoteEngineConfig {
            base_spread_bps: 30,
            fee_bps: 5,
            min_profit_bps: 20,
            max_inventory_skew_bps: 50,
        };

        let quote = calculate_quote_exact_output(
            AssetPair::new(usdc(), sol()),
            AmountRaw::new(10_000_000),
            reference_price(),
            6,
            9,
            &inventory(500_000_000, 1_000_000_000),
            &config,
        )
        .expect("quote should calculate");

        assert_eq!(quote.breakdown.total_spread_bps, 60);
        assert_eq!(quote.input_amount.amount_raw, AmountRaw::new(1_006_037));
    }

    #[test]
    fn exact_output_quote_never_under_collects() {
        check_exact_output_round_trip(10_000_000, Decimal::new(1, 2), 1_000_000_000);
        check_exact_output_round_trip(1, Decimal::new(1, 2), 1_000_000_000);
        check_exact_output_round_trip(123_456_789, Decimal::new(73, 4), 500_000_000);
        check_exact_output_round_trip(10_000_000, Decimal::new(1, 2), 2_000_000_000);
        check_exact_output_round_trip(999_999_999_999, Decimal::new(1234567, 8), 1_000_000_000);
    }

    #[test]
    fn exact_output_quote_rejects_zero_output() {
        let result = calculate_quote_exact_output(
            AssetPair::new(usdc(), sol()),
            AmountRaw::new(0),
            reference_price(),
            6,
            9,
            &inventory(1_000_000_000, 1_000_000_000),
            &QuoteEngineConfig::default(),
        );
        assert_eq!(result, Err(QuoteMathError::AmountTooSmall));
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib quote_engine 2>&1 | tail -20`
Expected: compile error `cannot find function calculate_quote_exact_output`.

- [ ] **Step 3: Implement the function**

Add after `calculate_quote` in `src/domain/quote_engine.rs`:

```rust
/// Calculate a deterministic quote for a fixed output amount.
///
/// Payments fix what the recipient receives; this computes what the taker
/// must pay. Every rounding step rounds the input up so the maker never
/// pays more than `calculate_quote` would for the same input.
///
/// # Errors
///
/// Returns a quote math error when amounts, prices, or spread values are
/// outside safe raw-token boundaries.
pub fn calculate_quote_exact_output(
    pair: AssetPair,
    output_amount_raw: AmountRaw,
    reference_price: ReferencePrice,
    input_decimals: u8,
    output_decimals: u8,
    inventory: &InventorySnapshot,
    config: &QuoteEngineConfig,
) -> Result<QuoteComputation, QuoteMathError> {
    if output_amount_raw.is_zero() {
        return Err(QuoteMathError::AmountTooSmall);
    }

    if reference_price.pair != pair || reference_price.output_per_input <= Decimal::ZERO {
        return Err(QuoteMathError::InvalidReferencePrice);
    }

    let output_ui = raw_to_decimal(output_amount_raw, output_decimals)?;
    let reference_input_ui = output_ui / reference_price.output_per_input;
    let reference_input_raw = decimal_to_raw_ceil(reference_input_ui, input_decimals)?;
    if reference_input_raw.is_zero() {
        return Err(QuoteMathError::AmountTooSmall);
    }

    let inventory_skew_bps = inventory_skew_bps(&pair, inventory, config);
    let total_before_floor = config.base_spread_bps + config.fee_bps + inventory_skew_bps;
    let total_spread_bps = total_before_floor.max(config.min_profit_bps).max(0);
    if total_spread_bps >= 10_000 {
        return Err(QuoteMathError::SpreadTooWide);
    }

    let retained_bps = Decimal::from(10_000 - total_spread_bps) / Decimal::from(10_000);
    let input_amount_raw = decimal_to_raw_ceil(
        Decimal::from(reference_input_raw.as_u64()) / retained_bps,
        0,
    )?;

    Ok(QuoteComputation {
        pair: pair.clone(),
        input_amount: TokenAmount::new(pair.input, input_amount_raw),
        output_amount: TokenAmount::new(pair.output, output_amount_raw),
        breakdown: QuoteBreakdown {
            reference_output_amount_raw: output_amount_raw,
            base_spread_bps: config.base_spread_bps,
            inventory_skew_bps,
            fee_bps: config.fee_bps,
            min_profit_bps: config.min_profit_bps,
            total_spread_bps,
            reference_price,
        },
    })
}
```

Add next to `decimal_to_raw_floor`:

```rust
fn decimal_to_raw_ceil(value: Decimal, decimals: u8) -> Result<AmountRaw, QuoteMathError> {
    if value.is_sign_negative() {
        return Err(QuoteMathError::AmountTooSmall);
    }

    let scaled = (value * decimal_factor(decimals)?).ceil();
    let raw = scaled.to_u64().ok_or(QuoteMathError::AmountOverflow)?;
    Ok(AmountRaw::new(raw))
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib quote_engine 2>&1 | tail -20`
Expected: `test result: ok.` with the four new tests listed as passing (plus the two existing ones).

- [ ] **Step 5: Commit (only after Azeem's go-ahead)**

```bash
git add src/domain/quote_engine.rs
git commit -m "Quote a fixed output amount without ever under-collecting"
```

---

### Task 2: Requested amount side in the domain RFQ

**Files:**
- Modify: `src/application/rfq.rs` (`RfqRequest`, `request_quote`, tests)
- Modify: `src/application/runtime/orchestrator.rs`, `src/interfaces/http/mod.rs`, `tests/runtime_integration.rs`, `tests/live_mainnet.rs` (literal constructions of `RfqRequest`)

**Interfaces:**
- Produces: `pub enum RequestedAmount { Input(AmountRaw), Output(AmountRaw) }` in `src/application/rfq.rs`; `RfqRequest.amount: RequestedAmount` replaces `RfqRequest.input_amount_raw`. Task 3 builds `Output` from HTTP.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module of `src/application/rfq.rs`, next to `rfq(amount)`:

```rust
    fn exact_output_rfq(output_raw: u64) -> RfqRequest {
        RfqRequest {
            input_mint: usdc_mint(),
            output_mint: sol_mint(),
            amount: RequestedAmount::Output(AmountRaw::new(output_raw)),
            taker_wallet: taker(),
            expiry_seconds: None,
        }
    }

    #[tokio::test]
    async fn rfq_quotes_the_input_cost_for_an_exact_output() {
        let outcome = request_quote(
            exact_output_rfq(10_000_000),
            &context(1_000_000_000),
            &price(now()),
            RuntimeRunId::generate(),
            now(),
        )
        .await;

        match outcome.response {
            RfqResponse::Accepted(quote) => {
                assert_eq!(quote.output_amount.amount_raw, AmountRaw::new(10_000_000));
                assert_eq!(quote.input_amount.amount_raw, AmountRaw::new(1_003_513));
                assert_eq!(quote.htlc_terms.maker_receive.amount_raw, AmountRaw::new(1_003_513));
                assert_eq!(quote.htlc_terms.maker_pay.amount_raw, AmountRaw::new(10_000_000));
            }
            RfqResponse::Rejected(rejection) => {
                panic!("expected accepted exact-output quote, got {rejection:?}");
            }
        }
    }

    #[tokio::test]
    async fn rfq_exact_output_still_enforces_the_input_asset_amount_gate() {
        // 3 USDC worth of SOL trips the 2 USDC per-asset cap on the computed input.
        let outcome = request_quote(
            exact_output_rfq(30_000_000),
            &context(1_000_000_000),
            &price(now()),
            RuntimeRunId::generate(),
            now(),
        )
        .await;

        assert_eq!(
            rejection_reason(&outcome),
            Some(&RejectionReason::AboveAssetMaxAmount)
        );
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --lib rfq 2>&1 | tail -20`
Expected: compile error `cannot find type RequestedAmount`.

- [ ] **Step 3: Change the request type and the dispatch**

In `src/application/rfq.rs` replace the `RfqRequest` struct with:

```rust
/// Which side of the trade the taker fixed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestedAmount {
    /// Taker fixes what they pay; the maker quotes what comes out.
    Input(AmountRaw),
    /// Taker fixes what must come out; the maker quotes what it costs.
    Output(AmountRaw),
}

/// RFQ request body accepted by the domain layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RfqRequest {
    /// Input token mint address.
    pub input_mint: MintAddress,
    /// Output token mint address.
    pub output_mint: MintAddress,
    /// Fixed side of the trade in raw token units.
    pub amount: RequestedAmount,
    /// Taker wallet requesting the quote.
    pub taker_wallet: WalletAddress,
    /// Optional quote expiry override in seconds.
    pub expiry_seconds: Option<u64>,
}
```

Update the import line to include the new engine function:

```rust
use crate::domain::quote_engine::{
    InventorySnapshot, QuoteBreakdown, QuoteEngineConfig, QuoteMathError, calculate_quote,
    calculate_quote_exact_output,
};
```

In `request_quote`, replace the `input_amount` binding and the `Requested` event with:

```rust
    let requested_amount = match request.amount {
        RequestedAmount::Input(raw) => TokenAmount::new(pair.input.clone(), raw),
        RequestedAmount::Output(raw) => TokenAmount::new(pair.output.clone(), raw),
    };
    let mut events = vec![RuntimeEvent::Quote(QuoteEvent::Requested {
        metadata: metadata(run_id, now),
        quote_id,
        pair: pair.clone(),
        input_amount: requested_amount,
        taker_wallet: request.taker_wallet.clone(),
        expires_at,
    })];
```

and replace the `calculate_quote(...)` call with:

```rust
    let computed = match request.amount {
        RequestedAmount::Input(raw) => calculate_quote(
            pair.clone(),
            raw,
            reference_price.clone(),
            input_asset.decimals,
            output_asset.decimals,
            &context.inventory,
            &context.quote_config,
        ),
        RequestedAmount::Output(raw) => calculate_quote_exact_output(
            pair.clone(),
            raw,
            reference_price.clone(),
            input_asset.decimals,
            output_asset.decimals,
            &context.inventory,
            &context.quote_config,
        ),
    };
    let quote = match computed {
        Ok(quote) => quote,
        Err(error) => {
            let (reason, detail) = quote_math_rejection(error);
            return rejected_outcome(quote_id, run_id, now, reason, detail, events);
        }
    };
```

The per-asset gate (`check_input_asset_amount(&quote.input_amount, input_asset)`) already runs on the computed input, so exact-output quotes get the same cap.

- [ ] **Step 4: Sweep the construction sites**

Every `RfqRequest { ... input_amount_raw: X, ... }` literal becomes `amount: RequestedAmount::Input(X)`. For the mechanical ones:

```bash
grep -rln "input_amount_raw: AmountRaw::new(" src/application/rfq.rs tests/runtime_integration.rs tests/live_mainnet.rs \
  | xargs sed -i '' 's/input_amount_raw: AmountRaw::new(\(.*\)),/amount: RequestedAmount::Input(AmountRaw::new(\1)),/'
```

Then `cargo build --tests 2>&1 | grep -E "^error" -A 5` and fix the rest by hand: the three literals in `src/interfaces/http/mod.rs` (`input_amount_raw: raw` → `amount: RequestedAmount::Input(raw)`, and the shorthand `input_amount_raw,` in the legacy branch → `amount: RequestedAmount::Input(input_amount_raw),`), the one in `src/application/runtime/orchestrator.rs`, and any read of `request.input_amount_raw` (replace with a `match request.amount { RequestedAmount::Input(raw) | RequestedAmount::Output(raw) => raw }` only where the code genuinely needs a raw number, otherwise pass `request.amount` through). Add `use firmament::rfq::RequestedAmount;` to both test files and `use crate::application::rfq::RequestedAmount;` where `src/` files need it.

- [ ] **Step 5: Run the full suite**

Run: `cargo test 2>&1 | grep -E "^test result|FAILED|panicked"`
Expected: every suite `ok`, including the two new rfq tests; 0 failed.

- [ ] **Step 6: Commit (only after Azeem's go-ahead)**

```bash
git add src/application/rfq.rs src/application/runtime/orchestrator.rs src/interfaces/http/mod.rs tests/runtime_integration.rs tests/live_mainnet.rs
git commit -m "Let an RFQ fix either side of the trade"
```

---

### Task 3: `output_amount` on the HTTP RFQ

**Files:**
- Modify: `src/interfaces/http/types.rs:24-48` (`RfqRequest`)
- Modify: `src/interfaces/http/mod.rs:1331-1453` (`parse_rfq_request`)
- Modify: `docs/openapi.yaml:713-733` (`FriendlyRfqRequest`)
- Modify: `web/app/src/api.ts:35-41` (`FriendlyRfqRequest`)
- Test: `tests/api.rs`

**Interfaces:**
- Consumes: `RequestedAmount` from Task 2.
- Produces: friendly RFQ body accepts `output_amount` (decimal string in the output asset's display units) as an alternative to `amount`.

- [ ] **Step 1: Write the failing API tests**

Append to `tests/api.rs`:

```rust
fn exact_output_rfq_request(output_amount: &str) -> Value {
    json!({
        "input_asset": "USDC",
        "output_asset": "SOL",
        "output_amount": output_amount,
        "taker_wallet": "DemoTaker111111111111111111111111111111111111",
        "expiry_seconds": 30
    })
}

async fn post_rfq(app: axum::Router, body: Value) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/rfq")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .expect("request"),
        )
        .await
        .expect("response");
    let status = response.status();
    (status, response_json(response).await)
}

#[tokio::test]
async fn api_rfq_exact_output_quotes_the_input_cost() {
    let app = test_orchestrator_router().await;

    let (status, body) = post_rfq(app, exact_output_rfq_request("0.01")).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "accepted", "{body}");
    assert_eq!(body["output"]["asset"], "SOL");
    assert_eq!(body["output"]["amount_raw"], "10000000");
    assert_eq!(body["input"]["asset"], "USDC");
    let input_raw: u64 = body["input"]["amount_raw"]
        .as_str()
        .expect("input amount_raw")
        .parse()
        .expect("numeric input amount_raw");
    assert!(input_raw > 0);
}

#[tokio::test]
async fn api_rfq_rejects_output_amount_with_too_many_decimals() {
    let app = test_orchestrator_router().await;

    let (status, body) = post_rfq(app, exact_output_rfq_request("0.0000000001")).await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "invalid_amount");
    let message = body["error"]["message"].as_str().expect("message");
    assert!(message.contains("9 decimals"), "{message}");
    assert!(message.contains("SOL"), "{message}");
}

#[tokio::test]
async fn api_rfq_rejects_both_amount_and_output_amount() {
    let app = test_orchestrator_router().await;
    let mut body = exact_output_rfq_request("0.01");
    body["amount"] = json!("1.00");

    let (status, body) = post_rfq(app, body).await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "invalid_amount");
}
```

Check the error envelope shape first: `grep -n "pub struct ErrorBody" -A 8 src/interfaces/http/types.rs`. If the code field is not at `error.code`, adjust the three assertions to the real path.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --test api exact_output 2>&1 | tail -20`
Expected: the first test fails with status 400 (`amount is required`), the other two fail on the assertions.

- [ ] **Step 3: Extend the request type**

In `src/interfaces/http/types.rs`, add after the `amount` field of `RfqRequest`:

```rust
    /// Friendly output amount in the output asset's display units. Exactly
    /// one of `amount` and `output_amount` is required.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_amount: Option<String>,
```

and update the doc comment on the struct: `- **Friendly** (preferred): input_asset, output_asset, and exactly one of amount (what the taker pays) or output_amount (what must come out).`

- [ ] **Step 4: Parse either side**

In `src/interfaces/http/mod.rs`, `parse_rfq_request`: destructure `output_amount` too, include it in `used_friendly`, and replace the body of the `if used_friendly` block from `let raw_amount = ...` through the `Ok(rfq::RfqRequest { ... })` with:

```rust
        let input_cfg = resolve_asset_alias(config, input_id)?;
        let output_cfg = resolve_asset_alias(config, output_id)?;
        if input_cfg.id == output_cfg.id {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "same_asset",
                format!(
                    "input and output assets must differ; both resolve to {}",
                    input_cfg.id
                ),
                Vec::new(),
            ));
        }

        let (raw_amount, fixed_cfg) = match (amount.as_deref(), output_amount.as_deref()) {
            (Some(raw), None) => (raw, input_cfg),
            (None, Some(raw)) => (raw, output_cfg),
            _ => {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_amount",
                    "provide exactly one of amount (what you pay) or output_amount (what must come out)",
                    Vec::new(),
                ));
            }
        };

        let amount_decimal = Decimal::from_str(raw_amount.trim()).map_err(|_| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_amount",
                format!("amount '{raw_amount}' is not a valid decimal"),
                Vec::new(),
            )
        })?;
        if amount_decimal <= Decimal::ZERO {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_amount",
                "amount must be greater than zero",
                Vec::new(),
            ));
        }

        let registry = AssetRegistry::from_config(config);
        let raw = registry
            .display_to_raw(&fixed_cfg.id, amount_decimal)
            .map_err(|err| match err {
                AssetError::PrecisionLoss { .. } => ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_amount",
                    format!(
                        "amount '{raw_amount}' has more than {} decimals (max for {})",
                        fixed_cfg.decimals, fixed_cfg.id
                    ),
                    Vec::new(),
                ),
                AssetError::NegativeAmount { .. } => ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_amount",
                    "amount must be greater than zero",
                    Vec::new(),
                ),
                _ => ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_amount",
                    err.to_string(),
                    Vec::new(),
                ),
            })?;
        let requested = if output_amount.is_some() {
            RequestedAmount::Output(raw)
        } else {
            RequestedAmount::Input(raw)
        };

        Ok(rfq::RfqRequest {
            input_mint: input_cfg.mint.clone(),
            output_mint: output_cfg.mint.clone(),
            amount: requested,
            taker_wallet,
            expiry_seconds,
        })
```

The existing `let raw_amount = amount.as_deref().ok_or_else(...)` lines are deleted; `used_friendly` becomes `input_asset.is_some() || output_asset.is_some() || amount.is_some() || output_amount.is_some()`; the mixed-shape error message gains `/output_amount`.

- [ ] **Step 5: Run the API suite**

Run: `cargo test --test api 2>&1 | grep -E "^test result|FAILED|panicked"`
Expected: `ok`, 0 failed (38 tests).

- [ ] **Step 6: Document the field**

In `docs/openapi.yaml` under `FriendlyRfqRequest`: change `required: [input_asset, output_asset, amount, taker_wallet]` to `required: [input_asset, output_asset, taker_wallet]`, add after the `amount` property:

```yaml
        output_amount:
          type: string
          description: Decimal amount in the output asset's display units. Fixes what must come out; the response's `input` is what it costs. Exactly one of `amount` and `output_amount` is required.
          example: "89.30"
```

and add to the schema `description: Exactly one of amount or output_amount.` Run `npm run docs:check` and expect `✔` with no schema errors.

In `web/app/src/api.ts` replace `FriendlyRfqRequest` with:

```ts
export type FriendlyRfqRequest = {
  input_asset: string;
  output_asset: string;
  taker_wallet: string;
  expiry_seconds?: number;
} & ({ amount: string; output_amount?: never } | { output_amount: string; amount?: never });
```

Run: `npm run app:build 2>&1 | tail -3` → expect `✓ built in`.

- [ ] **Step 7: Commit (only after Azeem's go-ahead)**

```bash
git add src/interfaces/http/types.rs src/interfaces/http/mod.rs docs/openapi.yaml web/app/src/api.ts tests/api.rs
git commit -m "Accept output_amount on the friendly RFQ shape"
```

---

### Task 4: EURC as a configured asset

**Files:**
- Modify: `config.example.toml` (asset row, weights, dust)
- Modify: `src/domain/assets.rs` (`EURC_ID`, `EURC_MINT`, `uses_stable_notional_limit`)
- Modify: `src/application/rfq.rs:78` (exposure limit choice)
- Modify: `src/application/rebalance.rs:651` (`is_non_native_non_stable_asset`)
- Modify: `web/app/src/pages/SwapPage.tsx:27-31` (`fallbackAssets`)
- Modify: `docs/maker-setup.mdx` (asset section)
- Test: `tests/config.rs`

**Interfaces:**
- Produces: `pub fn uses_stable_notional_limit(asset: &AssetId) -> bool` in `src/domain/assets.rs`, true for USDC, EURC and SOL. The two call sites replace their hard-coded matches with it.

- [ ] **Step 1: Write the failing tests**

Append to `tests/config.rs` (check the file's existing helper for loading `config.example.toml`; if there is one named differently, reuse it instead of `example_config()` below):

```rust
fn example_config() -> firmament::AppConfig {
    let raw = std::fs::read_to_string("config.example.toml").expect("read config.example.toml");
    toml::from_str(&raw).expect("parse config.example.toml")
}

#[test]
fn example_config_enables_eurc_on_the_circle_mint() {
    let config = example_config();
    let eurc = config
        .assets
        .supported
        .iter()
        .find(|asset| asset.id.as_str() == "EURC")
        .expect("EURC row");
    assert!(eurc.enabled);
    assert_eq!(eurc.mint.as_str(), "HzwqbKZw8HxMN6bF2yFZNrht3c2iXXzpKcFu7uBEDKtr");
    assert_eq!(eurc.decimals, 6);
    let weights: rust_decimal::Decimal = config.assets.supported.iter().map(|asset| asset.target_weight).sum();
    assert_eq!(weights, rust_decimal::Decimal::ONE);
}

#[test]
fn stablecoins_and_sol_use_the_stable_notional_limit() {
    use firmament::assets::uses_stable_notional_limit;
    use firmament::types::AssetId;
    for (asset, expected) in [("USDC", true), ("EURC", true), ("SOL", true), ("cbBTC", false)] {
        assert_eq!(uses_stable_notional_limit(&AssetId::from(asset)), expected, "{asset}");
    }
}
```

If `toml` is not a dev-dependency, check `grep -n "^toml" Cargo.toml`; it is a normal dependency of the crate (config loading), so the test can use it.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --test config eurc 2>&1 | tail -15`
Expected: first test fails with `EURC row` panic (or the sum assertion), second fails to compile on `uses_stable_notional_limit`.

- [ ] **Step 3: Add the config row and the helper**

`config.example.toml`: change the weights and add the row and dust threshold:

```toml
[[assets.supported]]
id = "USDC"
symbol = "USDC"
mint = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"
decimals = 6
enabled = true
target_weight = "0.45"
quoteable_threshold_raw = 2_000_000
min_trade_amount = "1"
max_trade_amount = "2"

[[assets.supported]]
id = "SOL"
symbol = "SOL"
mint = "So11111111111111111111111111111111111111112"
decimals = 9
enabled = true
target_weight = "0.30"
quoteable_threshold_raw = 10_000_000
min_trade_amount = "0.01"
max_trade_amount = "0.02"

[[assets.supported]]
id = "EURC"
symbol = "EURC"
mint = "HzwqbKZw8HxMN6bF2yFZNrht3c2iXXzpKcFu7uBEDKtr"
decimals = 6
enabled = true
target_weight = "0.20"
quoteable_threshold_raw = 2_000_000
min_trade_amount = "1"
max_trade_amount = "2"

[[assets.supported]]
id = "cbBTC"
symbol = "cbBTC"
mint = "VERIFY_CBBTC_SOLANA_MINT_BEFORE_LIVE_USE"
decimals = 8
enabled = true
target_weight = "0.05"
quoteable_threshold_raw = 1_000
min_trade_amount = "0.00001"
max_trade_amount = "0.00002"
```

and under `[reconciliation.dust]` add `EURC = 10000`.

`src/domain/assets.rs`, after `CBBTC_MINT`:

```rust
/// Stable asset identifier for Circle's euro stablecoin.
pub const EURC_ID: &str = "EURC";
/// Circle EURC mint on Solana mainnet.
pub const EURC_MINT: &str = "HzwqbKZw8HxMN6bF2yFZNrht3c2iXXzpKcFu7uBEDKtr";

/// True for assets whose notional is capped by the stable trade limit rather
/// than the non-stable exception: the stablecoins and native SOL.
#[must_use]
pub fn uses_stable_notional_limit(asset: &AssetId) -> bool {
    matches!(asset.as_str(), USDC_ID | EURC_ID | SOL_ID)
}
```

`src/application/rfq.rs:78`: replace `if matches!(asset.id.as_str(), "USDC" | "SOL")` with `if crate::domain::assets::uses_stable_notional_limit(&asset.id)`.

`src/application/rebalance.rs:651`: replace the body of `is_non_native_non_stable_asset` with `!crate::domain::assets::uses_stable_notional_limit(asset)`.

`web/app/src/pages/SwapPage.tsx` `fallbackAssets`: add after the SOL entry:

```ts
  { id: 'EURC', symbol: 'EURC', name: 'Euro Coin', mint: 'HzwqbKZw8HxMN6bF2yFZNrht3c2iXXzpKcFu7uBEDKtr', decimals: 6, min_trade_amount: '1', max_trade_amount: '2' },
```

`docs/maker-setup.mdx` section 3: after the cbBTC paragraph add:

```
EURC is Circle's euro stablecoin on Solana (mint
`HzwqbKZw8HxMN6bF2yFZNrht3c2iXXzpKcFu7uBEDKtr`, 6 decimals, legacy Token
program). The maker must hold EURC working inventory to quote USDC→EURC; buy it
through Jupiter or send it from a wallet. Rebalancing treats EURC like USDC and
SOL for notional caps.
```

and in section 4 add `- USDC to EURC and EURC to USDC` to the pair list.

- [ ] **Step 4: Run the suites**

Run: `cargo test 2>&1 | grep -E "^test result|FAILED|panicked"` and `npm run app:build 2>&1 | tail -2`
Expected: all `ok`; build `✓`.

- [ ] **Step 5: Commit (only after Azeem's go-ahead)**

```bash
git add config.example.toml src/domain/assets.rs src/application/rfq.rs src/application/rebalance.rs web/app/src/pages/SwapPage.tsx docs/maker-setup.mdx tests/config.rs
git commit -m "Quote EURC as a stablecoin leg"
```

---

### Task 5: Third-party recipient on wallet settlement

**Files:**
- Modify: `src/domain/types.rs` (add `ExternalRedeemRequest`)
- Modify: `src/ports/mod.rs:125-143,210-222` (`build_external_redeem`, `restore_wallet_settlement`)
- Modify: `src/adapters/solana/htlc/client.rs:387-411,640-666,841-895` (unsigned tx builder, redeem builder, restore)
- Modify: `src/application/runtime/orchestrator.rs:202-233,339-349,1684-1765,1837-1844,1882-1898,2239-2256` and every `RuntimeTrade {` literal
- Modify: `src/interfaces/http/types.rs:223-230,407-445,551-580`, `src/interfaces/http/mod.rs:482-510,1619-1660`, `src/interfaces/http/runtime_read.rs:189-212`
- Modify: `docs/openapi.yaml` (`WalletSettlementRequest`, `TradeResponse`, `TradeSummary`)
- Modify: `web/app/src/api.ts` (`walletSettlement` payload, `RuntimeTrade.recipient`)
- Test: `tests/runtime_integration.rs`, `tests/api.rs`, unit test in `src/adapters/solana/htlc/client.rs`

**Interfaces:**
- Produces:
  - `pub struct ExternalRedeemRequest { pub trade_id: TradeId, pub recipient: WalletAddress, pub payer: WalletAddress, pub preimage: String }` in `domain/types.rs`.
  - `HtlcClient::build_external_redeem(&self, request: ExternalRedeemRequest)`.
  - `HtlcClient::restore_wallet_settlement(&self, taker_lock, maker_lock, maker_redeemer: WalletAddress, taker_locked, maker_locked)`.
  - `RuntimeOrchestrator::start_wallet_settlement(&self, quote_id, taker_wallet, recipient: Option<WalletAddress>, secret_hash)`.
  - `RuntimeTrade.recipient: Option<WalletAddress>`; HTTP `TradeResponse.recipient` and `TradeSummary.recipient` (strings, defaulting to the taker wallet).
  - `WalletSettlementRequest.recipient: Option<WalletAddress>`.

- [ ] **Step 1: Write the failing orchestrator tests**

In `tests/runtime_integration.rs`, extend `FakeHtlcState` with two recorders and `FakeHtlcClient` with two accessors:

```rust
    external_redeemers: HashMap<TradeId, WalletAddress>,
    redeem_requests: Vec<ExternalRedeemRequest>,
```

```rust
    fn external_redeemer(&self, trade_id: TradeId) -> Option<WalletAddress> {
        self.inner
            .lock()
            .expect("htlc lock")
            .external_redeemers
            .get(&trade_id)
            .cloned()
    }

    fn last_redeem_request(&self) -> Option<ExternalRedeemRequest> {
        self.inner
            .lock()
            .expect("htlc lock")
            .redeem_requests
            .last()
            .cloned()
    }
```

Change the fake's `initiate_with_external_redeemer` to record the redeemer:

```rust
    async fn initiate_with_external_redeemer(
        &self,
        request: HtlcInitiation,
        redeemer: WalletAddress,
    ) -> Result<HtlcReceipt, AppError> {
        self.inner
            .lock()
            .expect("htlc lock")
            .external_redeemers
            .insert(request.trade_id, redeemer);
        self.initiate(request).await
    }
```

Change its `build_external_redeem` to the new signature and record the request:

```rust
    async fn build_external_redeem(
        &self,
        request: ExternalRedeemRequest,
    ) -> Result<UnsignedWalletTransaction, AppError> {
        self.inner
            .lock()
            .expect("htlc lock")
            .redeem_requests
            .push(request);
        Ok(UnsignedWalletTransaction {
            transaction_base64: "AA==".to_owned(),
            recent_blockhash: "fake-blockhash".to_owned(),
        })
    }
```

Update the fake's `restore_wallet_settlement` (around line 2227) to take `_maker_redeemer: WalletAddress` between `maker_lock` and `taker_locked`. Add `ExternalRedeemRequest` to the `firmament::types` import. Every existing call `start_wallet_settlement(quote_id, taker_wallet(), "aa".repeat(32))` gains a `None` third argument:

```bash
sed -i '' 's/\.start_wallet_settlement(\([^,]*\), \([^,]*\), \(.*\))$/.start_wallet_settlement(\1, \2, None, \3)/' tests/runtime_integration.rs
```

(then inspect `grep -n "start_wallet_settlement(" tests/runtime_integration.rs` and fix any multi-line call by hand).

Add the tests, modelled on `active_wallet_settlement_resumes_after_runtime_restart`:

```rust
fn recipient_wallet() -> WalletAddress {
    WalletAddress::new("So11111111111111111111111111111111111111112")
}

async fn settle_through_maker_lock(
    htlc: &FakeHtlcClient,
    recipient: Option<WalletAddress>,
) -> (RuntimeOrchestrator, TradeId) {
    let persistence = Arc::new(
        RuntimePersistence::open_in_memory(AssetRegistry::from_config(&relaxed_default_config()))
            .expect("open persistence"),
    );
    seed_working_custody(&persistence, &sol(), 200_000_000);
    seed_working_custody(&persistence, &usdc(), 10_000_000);
    let orchestrator = persistent_harness_with_persistence(
        FakePriceProvider::default(),
        htlc.clone(),
        FakeSwapExecutor::default(),
        FakeGatewayClient::default(),
        FakeBalanceReader::new(vec![quote_inventory()]),
        RuntimeOrchestratorOptions::default(),
        persistence,
    )
    .await;

    let quote_id = match orchestrator.request_rfq(rfq(1_000_000)).await.expect("request quote") {
        RfqResponse::Accepted(quote) => quote.quote_id,
        RfqResponse::Rejected(rejection) => panic!("expected accepted quote: {rejection:?}"),
    };
    let started = orchestrator
        .start_wallet_settlement(quote_id, taker_wallet(), recipient, "aa".repeat(32))
        .await
        .expect("start wallet settlement");
    orchestrator
        .record_wallet_taker_lock(started.trade_id, TxSignature::new("taker-lock-1"))
        .await
        .expect("record taker lock");
    (orchestrator, started.trade_id)
}

#[tokio::test]
async fn wallet_settlement_recipient_is_the_maker_lock_redeemer_and_redeem_destination() {
    let htlc = FakeHtlcClient::default();
    let (orchestrator, trade_id) = settle_through_maker_lock(&htlc, Some(recipient_wallet())).await;

    assert_eq!(htlc.external_redeemer(trade_id), Some(recipient_wallet()));

    orchestrator
        .prepare_wallet_taker_redeem(trade_id, "bb".repeat(32))
        .await
        .expect("prepare taker redeem");
    let redeem = htlc.last_redeem_request().expect("redeem request");
    assert_eq!(redeem.trade_id, trade_id);
    assert_eq!(redeem.recipient, recipient_wallet());
    assert_eq!(redeem.payer, taker_wallet());

    let trade = orchestrator.trade(trade_id).await.expect("trade");
    assert_eq!(trade.recipient, Some(recipient_wallet()));
    assert_eq!(trade.taker_wallet, Some(taker_wallet()));
}

#[tokio::test]
async fn wallet_settlement_without_recipient_pays_the_taker() {
    let htlc = FakeHtlcClient::default();
    let (orchestrator, trade_id) = settle_through_maker_lock(&htlc, None).await;

    assert_eq!(htlc.external_redeemer(trade_id), Some(taker_wallet()));

    orchestrator
        .prepare_wallet_taker_redeem(trade_id, "bb".repeat(32))
        .await
        .expect("prepare taker redeem");
    let redeem = htlc.last_redeem_request().expect("redeem request");
    assert_eq!(redeem.recipient, taker_wallet());
    assert_eq!(redeem.payer, taker_wallet());
}

#[tokio::test]
async fn wallet_settlement_rejects_the_maker_wallet_as_recipient() {
    let htlc = FakeHtlcClient::default();
    let persistence = Arc::new(
        RuntimePersistence::open_in_memory(AssetRegistry::from_config(&relaxed_default_config()))
            .expect("open persistence"),
    );
    seed_working_custody(&persistence, &sol(), 200_000_000);
    seed_working_custody(&persistence, &usdc(), 10_000_000);
    let orchestrator = persistent_harness_with_persistence(
        FakePriceProvider::default(),
        htlc.clone(),
        FakeSwapExecutor::default(),
        FakeGatewayClient::default(),
        FakeBalanceReader::new(vec![quote_inventory()]),
        RuntimeOrchestratorOptions::default(),
        persistence,
    )
    .await;
    let quote_id = match orchestrator.request_rfq(rfq(1_000_000)).await.expect("request quote") {
        RfqResponse::Accepted(quote) => quote.quote_id,
        RfqResponse::Rejected(rejection) => panic!("expected accepted quote: {rejection:?}"),
    };
    let maker = htlc.wallet_address(WalletRole::Maker).await.expect("maker wallet");

    let error = orchestrator
        .start_wallet_settlement(quote_id, taker_wallet(), Some(maker), "aa".repeat(32))
        .await
        .expect_err("maker as recipient must be refused");

    assert!(error.to_string().contains("recipient"), "{error}");
    // The quote is still consumable by a correct request.
    orchestrator
        .start_wallet_settlement(quote_id, taker_wallet(), None, "aa".repeat(32))
        .await
        .expect("quote survives a refused recipient");
}
```

If `seed_working_custody` in this file takes `&AssetId` (as in `active_wallet_settlement_resumes_after_runtime_restart`) keep the `&sol()` form; if it takes `AssetId`, drop the `&`.

- [ ] **Step 2: Write the failing API tests**

In `tests/api.rs`, update the fake's `build_external_redeem` to `request: firmament::types::ExternalRedeemRequest` (ignore it, return the same placeholder) and append:

```rust
async fn accepted_quote_id(app: &axum::Router) -> String {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/rfq")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(rfq_request().to_string()))
                .expect("request"),
        )
        .await
        .expect("response");
    let body = response_json(response).await;
    assert_eq!(body["status"], "accepted", "{body}");
    body["quote_id"].as_str().expect("quote id").to_owned()
}

async fn post_wallet_settlement(app: &axum::Router, quote_id: &str, body: Value) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(format!("/v1/quotes/{quote_id}/wallet-settlement"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .expect("request"),
        )
        .await
        .expect("response");
    let status = response.status();
    (status, response_json(response).await)
}

#[tokio::test]
async fn api_wallet_settlement_recipient_shows_on_the_trade() {
    let app = test_orchestrator_router().await;
    let quote_id = accepted_quote_id(&app).await;

    let (status, body) = post_wallet_settlement(
        &app,
        &quote_id,
        json!({
            "taker_wallet": "DemoTaker111111111111111111111111111111111111",
            "secret_hash": "0000000000000000000000000000000000000000000000000000000000000000",
            "recipient": "So11111111111111111111111111111111111111112"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let trade_id = body["trade_id"].as_str().expect("trade id");

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(format!("/v1/trades/{trade_id}/taker-lock"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({ "signature": "taker-lock-1" }).to_string()))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/v1/trades/{trade_id}"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let trade = response_json(response).await;
    assert_eq!(trade["recipient"], "So11111111111111111111111111111111111111112");
    assert_eq!(trade["taker_wallet"], "DemoTaker111111111111111111111111111111111111");
}

#[tokio::test]
async fn api_wallet_settlement_rejects_a_malformed_recipient() {
    let app = test_orchestrator_router().await;
    let quote_id = accepted_quote_id(&app).await;

    let (status, body) = post_wallet_settlement(
        &app,
        &quote_id,
        json!({
            "taker_wallet": "DemoTaker111111111111111111111111111111111111",
            "secret_hash": "0000000000000000000000000000000000000000000000000000000000000000",
            "recipient": "not-a-solana-address"
        }),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "invalid_recipient");
}
```

Check the taker-lock status code the existing handler returns (`grep -n "taker-lock" -A 3 tests/api.rs` for an existing test, or the handler's `StatusCode::`) and match it in the assertion.

- [ ] **Step 3: Run to verify they fail**

Run: `cargo test --test runtime_integration recipient 2>&1 | tail -5` and `cargo test --test api recipient 2>&1 | tail -5`
Expected: compile errors on `ExternalRedeemRequest` and the new `start_wallet_settlement` arity.

- [ ] **Step 4: Add the domain type and port signatures**

`src/domain/types.rs`, after `ExternalHtlcInitiation`:

```rust
/// Browser-signed redeem of the maker's HTLC leg: the recipient receives the
/// escrowed asset, the payer covers the transaction fee and any token
/// account the recipient is missing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalRedeemRequest {
    /// Trade whose maker leg is being redeemed.
    pub trade_id: TradeId,
    /// Wallet that receives the maker's output.
    pub recipient: WalletAddress,
    /// Browser wallet that signs and pays for the transaction.
    pub payer: WalletAddress,
    /// Hex-encoded 32-byte preimage revealed by the payer.
    pub preimage: String,
}
```

`src/ports/mod.rs`: replace the `build_external_redeem` default with

```rust
    async fn build_external_redeem(
        &self,
        request: ExternalRedeemRequest,
    ) -> Result<UnsignedWalletTransaction, AppError> {
        let _ = request;
        Err(AppError::unsupported(
            "HTLC client does not support browser redeem construction",
        ))
    }
```

and give `restore_wallet_settlement` a `maker_redeemer: WalletAddress` parameter after `maker_lock` (with `let _ = maker_redeemer;` in the default body). Add `ExternalRedeemRequest` to the imports.

- [ ] **Step 5: Implement the Solana client side**

`src/adapters/solana/htlc/client.rs`:

Generalize the unsigned builder to a slice of instructions:

```rust
    async fn build_unsigned_transaction(
        &self,
        payer: Pubkey,
        instructions: &[Instruction],
    ) -> Result<UnsignedWalletTransaction, AppError> {
        let blockhash = self
            .rpc_client
            .get_latest_blockhash()
            .await
            .map_err(|error| AppError::solana(format!("get latest blockhash: {error}")))?;
        let message = solana_sdk::message::Message::new_with_blockhash(
            instructions,
            Some(&payer),
            &blockhash,
        );
```

(the rest unchanged) and update its two other callers, `build_external_initiate` and `build_external_refund`, to pass `&[instruction]`.

Replace `build_external_redeem`:

```rust
    async fn build_external_redeem(
        &self,
        request: ExternalRedeemRequest,
    ) -> Result<UnsignedWalletTransaction, AppError> {
        let recipient = Self::pubkey_from_wallet(&request.recipient)?;
        let payer = Self::pubkey_from_wallet(&request.payer)?;
        let leg = {
            let state = self.state.lock().await;
            let trade = state.get(&request.trade_id).ok_or_else(|| {
                AppError::validation(format!("unknown HTLC trade {}", request.trade_id))
            })?;
            trade
                .legs
                .iter()
                .find(|leg| {
                    leg.request.funder == WalletRole::Maker
                        && leg.status == SettlementStatus::Initiated
                })
                .cloned()
                .ok_or_else(|| AppError::validation("no maker-funded HTLC leg is redeemable"))?
        };

        let mut instructions = Vec::with_capacity(2);
        if let (HtlcProgramKind::Spl, Some(mint), Some(token_program)) =
            (leg.kind, leg.mint, leg.token_program)
        {
            let recipient_ata = SolanaClient::derive_associated_token_address(
                &recipient,
                &mint,
                &token_program,
            );
            if !self.account_exists(&recipient_ata).await? {
                instructions.push(SolanaClient::derive_create_associated_token_account_instruction(
                    &payer,
                    &recipient,
                    &mint,
                    &token_program,
                ));
            }
        }
        instructions.push(Self::build_redeem_instruction_for_pubkey(
            &leg,
            recipient,
            &request.preimage,
        )?);
        self.build_unsigned_transaction(payer, &instructions).await
    }

    async fn account_exists(&self, account: &Pubkey) -> Result<bool, AppError> {
        let response = self
            .rpc_client
            .get_account_with_commitment(account, self.rpc_client.commitment())
            .await
            .map_err(|error| AppError::solana(format!("get account {account}: {error}")))?;
        Ok(response.value.is_some())
    }
```

`SolanaClient` is `crate::adapters::solana::client::SolanaClient` (already referenced with its full path in this file; add a `use` for it at the top). Add `ExternalRedeemRequest` to the `crate::domain::types` import.

In `restore_wallet_settlement`, add the parameter `maker_redeemer: WalletAddress` after `maker_lock`, delete the `let taker_wallet = taker_lock.funder.clone();` line, and replace the maker-leg redeemer derivation (the `Self::pubkey_from_wallet(&WalletAddress::new(taker_wallet...))` block with its comment) with `let redeemer = Self::pubkey_from_wallet(&maker_redeemer)?;`.

Add a unit test to the file's `#[cfg(test)] mod tests` (create the module at the bottom if the file has none; it needs `use super::*;`):

```rust
    #[test]
    fn spl_redeem_pays_the_recipients_token_account() {
        let program_id = Pubkey::new_unique();
        let funder = Pubkey::new_unique();
        let recipient = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let token_program = Pubkey::from_str_const(SPL_TOKEN_PROGRAM_ID);
        let secret = [7u8; 32];
        let secret_hash = hash_secret(&secret);
        let (swap_pda, _) = derive_spl_swap_pda(&program_id, &funder, &secret_hash);
        let leg = SolanaHtlcLeg {
            request: HtlcInitiation {
                trade_id: TradeId::generate(),
                funder: WalletRole::Maker,
                redeemer: WalletRole::Taker,
                amount: TokenAmount::new(AssetId::from("EURC"), AmountRaw::new(1_000_000)),
                hashlock: hex::encode(secret_hash),
                expires_at: OffsetDateTime::now_utc(),
            },
            kind: HtlcProgramKind::Spl,
            program_id,
            swap_pda,
            funder_pubkey: funder,
            redeemer_pubkey: recipient,
            mint: Some(mint),
            token_program: Some(token_program),
            status: SettlementStatus::Initiated,
        };

        let instruction =
            SolanaHtlcClient::build_redeem_instruction_for_pubkey(&leg, recipient, &hex::encode(secret))
                .expect("redeem instruction");

        let expected_ata =
            SolanaClient::derive_associated_token_address(&recipient, &mint, &token_program);
        assert_eq!(instruction.accounts[4].pubkey, expected_ata);
        assert!(instruction.accounts[4].is_writable);
        assert!(!instruction.accounts.iter().any(|meta| meta.is_signer));
    }
```

Adjust the struct name if the client type is not `SolanaHtlcClient` (`grep -n "^pub struct" src/adapters/solana/htlc/client.rs`) and the import list to whatever `use super::*` does not cover (`hex`, `OffsetDateTime`, domain types).

- [ ] **Step 6: Thread the recipient through the orchestrator**

`src/application/runtime/orchestrator.rs`:

`RuntimeTrade` gains, after `taker_wallet`:

```rust
    /// Wallet that receives the maker's output. Equal to the taker unless a
    /// third-party recipient was named at settlement start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipient: Option<WalletAddress>,
```

`WalletSettlementState` gains `#[serde(default)] recipient: Option<WalletAddress>,` after `taker_wallet`, plus:

```rust
impl WalletSettlementState {
    fn recipient(&self) -> &WalletAddress {
        self.recipient.as_ref().unwrap_or(&self.taker_wallet)
    }
}
```

`start_wallet_settlement` takes `recipient: Option<WalletAddress>` after `taker_wallet`. After the `maker_wallet` lookup add:

```rust
        if recipient.as_ref() == Some(&maker_wallet) {
            return Err(AppError::validation(
                "recipient must not be the maker wallet",
            ));
        }
```

placed before `self.take_quote(quote_id)` so a refused recipient does not consume the quote (move the `maker_wallet` lookup above `take_quote` as well). Store `recipient` in the `WalletSettlementState` literal.

Maker lock (`record_wallet_taker_lock`): `state.taker_wallet.clone()` → `state.recipient().clone()`.

`prepare_wallet_taker_redeem`:

```rust
        let taker_redeem_transaction = self
            .adapters
            .htlc_client
            .build_external_redeem(ExternalRedeemRequest {
                trade_id,
                recipient: state.recipient().clone(),
                payer: state.taker_wallet.clone(),
                preimage,
            })
            .await?;
```

Search for other `build_external_redeem(` calls (`grep -n "build_external_redeem(" src/application/runtime/orchestrator.rs`); the resume path, if it builds a redeem transaction, gets the same request shape.

`restore_wallet_settlement_htlc` passes `state.recipient().clone()` as the new third argument.

`runtime_trade_from_wallet_state` sets `recipient: Some(state.recipient().clone()),`; every other `RuntimeTrade {` literal (`grep -n "RuntimeTrade {" src/application/runtime/orchestrator.rs`) sets `recipient: None,`.

- [ ] **Step 7: Expose it over HTTP**

`src/interfaces/http/types.rs`:

`WalletSettlementRequest` gains

```rust
    /// Wallet that should receive the maker's output. Defaults to `taker_wallet`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipient: Option<WalletAddress>,
```

`TradeResponse` and `TradeSummary` each gain, after `taker_wallet`:

```rust
    /// Wallet that receives the maker's output; the taker unless a recipient was named.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recipient: Option<String>,
```

`src/interfaces/http/mod.rs`, `start_wallet_settlement` handler, before calling the orchestrator:

```rust
        if let Some(recipient) = &request.recipient {
            solana_sdk::pubkey::Pubkey::from_str(recipient.as_str()).map_err(|_| {
                ApiError::bad_request("invalid_recipient", "recipient is not a Solana address")
            })?;
        }
```

(add `use std::str::FromStr;` if missing) and pass `request.recipient` as the new argument. In `trade_to_api_response` set `recipient: trade.recipient.as_ref().or(trade.taker_wallet.as_ref()).map(|wallet| wallet.as_str().to_owned()),`. Same expression in `trade_to_summary` in `src/interfaces/http/runtime_read.rs`.

`docs/openapi.yaml`: add to `WalletSettlementRequest.properties`

```yaml
        recipient:
          $ref: "#/components/schemas/WalletAddress"
          description: Wallet that receives the maker's output. Defaults to `taker_wallet`. Must not be the maker wallet.
```

and to both `TradeResponse.properties` and `TradeSummary.properties`

```yaml
        recipient:
          type: string
          description: Wallet that receives the maker's output; equals `taker_wallet` unless a recipient was named at settlement start.
```

`web/app/src/api.ts`: `walletSettlement` payload type becomes `{ taker_wallet: string; secret_hash: string; recipient?: string }`; `RuntimeTrade` gains `recipient?: string;`.

- [ ] **Step 8: Run everything**

Run: `cargo test 2>&1 | grep -E "^test result|FAILED|panicked"`, `cargo clippy --all-targets 2>&1 | grep -E "^(warning|error)" | sort | uniq -c`, `cargo fmt --check`, `npm run docs:check`, `npm run app:build`
Expected: all suites `ok` (the three new integration tests and two new API tests pass), clippy prints no new warnings against the pre-change baseline (`git stash; cargo clippy --all-targets 2>&1 | grep -c warning; git stash pop` to compare), fmt clean, docs valid, build `✓`.

- [ ] **Step 9: Commit (only after Azeem's go-ahead)**

```bash
git add src/domain/types.rs src/ports/mod.rs src/adapters/solana/htlc/client.rs src/application/runtime/orchestrator.rs src/interfaces/http docs/openapi.yaml web/app/src/api.ts tests/runtime_integration.rs tests/api.rs
git commit -m "Settle the maker leg to a named recipient"
```

---

### Task 6: API deployment on Railway

**Files:**
- Modify: `docker/solver-entrypoint.sh` (config from env)
- Create: `railway.json` (Dockerfile path)
- Create locally, never committed: `config.toml` (production policy), `.env` (local runs only)

**Interfaces:**
- Produces: a public API URL `https://<service>.up.railway.app` that Task 9's rewrites point at. Record it in `docs/plans/deploy-notes.md` (gitignored? no: it holds no secrets, commit it).

- [ ] **Step 1: Let the container take its config from an env var**

In `docker/solver-entrypoint.sh`, before `[ -f "$CONFIG_PATH" ] || fail ...`:

```sh
if [ -n "${FIRMAMENT_CONFIG_B64:-}" ]; then
    printf '%s' "$FIRMAMENT_CONFIG_B64" | base64 -d > "$CONFIG_PATH" \
        || fail "FIRMAMENT_CONFIG_B64 is not valid base64"
fi
```

Create `railway.json` at the repo root:

```json
{
  "$schema": "https://railway.com/railway.schema.json",
  "build": { "builder": "DOCKERFILE", "dockerfilePath": "docker/solver.Dockerfile" },
  "deploy": { "healthcheckPath": "/health", "restartPolicyType": "ON_FAILURE" }
}
```

Build the image locally to prove the entrypoint change: `docker build -f docker/solver.Dockerfile -t firmament-solver . 2>&1 | tail -2` → expect `naming to docker.io/library/firmament-solver`.

- [ ] **Step 2: Write the production config**

`cp config.example.toml config.toml`, then edit `config.toml`: `[http] bind_address = "0.0.0.0"`, cbBTC `enabled = false`, `[runtime] enable_protocol_workers = true`, `database_path = "/data/runtime.sqlite"`, `[assets.policy] default_quote_expiry_seconds = 30`, caps: `max_action_notional_usd = "5"`, `max_cumulative_automation_notional_usd = "20"`, `[risk] max_quote_notional_usd = "5"`, `max_trade_notional_usd = "5"`, `max_daily_notional_usd = "25"`, per-asset `max_trade_amount`: USDC `"5"`, EURC `"5"`, SOL `"0.05"`. Confirm it parses: `cargo run -- --help >/dev/null 2>&1; cargo test --test config 2>&1 | grep "test result"` (config tests still pass) and `base64 -i config.toml | wc -c` (a few KB, fine for an env var).

- [ ] **Step 3: Create the Railway project and service**

```bash
railway init --name firmament
railway up --detach
```

`railway init` prompts for a workspace; pick the personal one. After the first deploy fails on the missing config (expected), set the non-secret variables:

```bash
railway variables set PORT=5050 FIRMAMENT_CONFIG_B64="$(base64 -i config.toml)"
railway volume add --mount-path /data
railway domain
```

Then Azeem adds the secrets in the Railway dashboard (Variables tab): `SOLANA_RPC_URL`, `MAKER_KEYPAIR_JSON` (the contents of `~/.config/solana/firmament-maker.json`), `JUPITER_API_KEY`, `CIRCLE_GATEWAY_SOLANA_ADDRESS` (= `9yovryyWxBLcmhuR92jFYq3Sc9kdmJrfFdsZ56gy5Mud`), `FIRMAMENT_ADMIN_PASSWORD_HASHES`, `FIRMAMENT_ADMIN_SESSION_SECRET`. Redeploy with `railway up --detach`.

- [ ] **Step 4: Verify**

```bash
curl -s https://<service>.up.railway.app/health
curl -s https://<service>.up.railway.app/v1/assets | jq '[.[] | {id, enabled}]'
curl -s https://<service>.up.railway.app/v1/pairs | jq '[.pairs[] | "\(.input_asset)->\(.output_asset)"]'
```

Expected: `{"status":"ok",...}`, four assets with EURC enabled, pairs including `USDC->EURC` and `EURC->USDC`. Record the URL in `docs/plans/deploy-notes.md`.

- [ ] **Step 5: Commit (only after Azeem's go-ahead)**

```bash
git add docker/solver-entrypoint.sh railway.json docs/plans/deploy-notes.md
git commit -m "Run the solver on Railway with config from the environment"
```

---

### Task 7: Pay page

**Files:**
- Create: `web/app/src/lib/format.ts`, `web/app/src/lib/wallet.ts` (helpers moved out of `SwapPage.tsx`)
- Create: `web/app/src/pages/PayPage.tsx`, `web/app/src/pages/PayRoute.tsx`
- Modify: `web/app/src/pages/SwapPage.tsx` (import the moved helpers), `web/app/src/main.tsx` (routes, nav), `web/app/src/pages/RuntimePage.tsx` (heading)
- Modify: `web/app/src/styles.css` (a few classes)

**Interfaces:**
- Consumes: `api.requestRfq` with `output_amount`, `api.walletSettlement` with `recipient`, `api.takerLock`, `api.takerRedeem`, `api.takerRefund`, `api.resumeSettlement`, `api.assets`.
- Produces: `/app` renders `PayPage`; `/app/swap` renders `SwapPage`; `/app/runtime` unchanged.

- [ ] **Step 1: Move shared helpers**

Create `web/app/src/lib/format.ts` and move these functions out of `SwapPage.tsx` verbatim, each prefixed with `export`: `parseDecimal`, `formatAssetAmount`, `formatRangeLabel`, `parseAmountRaw`, `formatRawAmount`, `formatRawBigInt`, `bytesToHex`, `hexToBytes`, `bytesFromBase64`, `shortValue`, `formatExpiry`, `quoteExpiryMs`, `isQuoteExpired`. Bring along the constants they use (search each body for identifiers defined at the top of `SwapPage.tsx`, e.g. the expiry constant used by `quoteExpiryMs`) and the `Asset`/`RfqResponse` type imports from `../api`.

Create `web/app/src/lib/wallet.ts` and move `isNativeAsset` (with `nativeSolMint`), `walletBalanceRaw`, `walletFundingIssue`, `walletFailure`, `isWalletCancellation`, each exported, plus a new helper:

```ts
import { Connection, Transaction } from '@solana/web3.js';
import { bytesFromBase64 } from './format';

export async function signAndSubmit(
  connection: Connection,
  signTransaction: (transaction: Transaction) => Promise<Transaction>,
  transactionBase64: string
) {
  const transaction = Transaction.from(bytesFromBase64(transactionBase64));
  const signed = await signTransaction(transaction);
  return connection.sendRawTransaction(signed.serialize());
}
```

In `SwapPage.tsx` delete the moved definitions and add:

```ts
import { bytesFromBase64, bytesToHex, formatAssetAmount, formatExpiry, formatRangeLabel, formatRawAmount, formatRawBigInt, hexToBytes, isQuoteExpired, parseAmountRaw, parseDecimal, quoteExpiryMs, shortValue } from '../lib/format';
import { isNativeAsset, isWalletCancellation, walletFailure, walletFundingIssue } from '../lib/wallet';
```

Run `npm run app:build 2>&1 | tail -5` → `✓ built`. If tsc reports an unused import, drop it; if it reports a missing symbol, the function is still referenced in `SwapPage.tsx` and must be in the import list.

- [ ] **Step 2: Write the Pay page**

`web/app/src/pages/PayRoute.tsx`:

```tsx
import { SolanaWalletProvider } from '../SolanaWalletProvider';
import { PayPage } from './PayPage';

export default function PayRoute() {
  return (
    <SolanaWalletProvider>
      <PayPage />
    </SolanaWalletProvider>
  );
}
```

`web/app/src/pages/PayPage.tsx`:

```tsx
import { FormEvent, useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useConnection, useWallet } from '@solana/wallet-adapter-react';
import { useWalletModal } from '@solana/wallet-adapter-react-ui';
import { PublicKey } from '@solana/web3.js';
import { api, Asset, RfqAcceptedResponse, TradeStepResponse, WalletSettlementResumeResponse } from '../api';
import { bytesToHex, formatRawAmount, parseAmountRaw, shortValue } from '../lib/format';
import { isWalletCancellation, signAndSubmit, walletFailure, walletFundingIssue } from '../lib/wallet';

const payAsset = 'USDC';
const receiveAsset = 'EURC';
const quoteExpirySeconds = 30;
const quoteDebounceMs = 500;
const storageKey = 'firmament.pay.v1';

type Phase =
  | { kind: 'form' }
  | { kind: 'quoting' }
  | { kind: 'quoted'; quote: RfqAcceptedResponse; secondsLeft: number }
  | { kind: 'rejected'; message: string }
  | { kind: 'locking'; tradeId: string }
  | { kind: 'maker_locking'; tradeId: string }
  | { kind: 'redeeming'; tradeId: string }
  | { kind: 'paid'; tradeId: string; signatures: string[] }
  | { kind: 'expired'; tradeId: string }
  | { kind: 'refunding'; tradeId: string }
  | { kind: 'refunded'; tradeId: string; signatures: string[] }
  | { kind: 'failed'; message: string };

type StoredPayment = {
  version: 1;
  wallet: string;
  recipient: string;
  eurAmount: string;
  tradeId: string;
  preimage: string;
  expiresAt: string | null;
  lockSignature: string | null;
};

function readStored(): StoredPayment | null {
  try {
    const raw = window.localStorage.getItem(storageKey);
    if (!raw) return null;
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed !== 'object' || parsed === null) return null;
    const candidate = parsed as Partial<StoredPayment>;
    if (candidate.version !== 1 || typeof candidate.tradeId !== 'string' || typeof candidate.preimage !== 'string' || typeof candidate.wallet !== 'string') {
      return null;
    }
    return candidate as StoredPayment;
  } catch {
    return null;
  }
}

function writeStored(value: StoredPayment | null) {
  try {
    if (value) window.localStorage.setItem(storageKey, JSON.stringify(value));
    else window.localStorage.removeItem(storageKey);
  } catch {
    // Private mode: the payment still completes, it just cannot resume after a refresh.
  }
}

function isValidAddress(value: string) {
  try {
    new PublicKey(value.trim());
    return true;
  } catch {
    return false;
  }
}

function solscan(signature: string) {
  return `https://solscan.io/tx/${signature}`;
}

function signaturesOf(step: TradeStepResponse) {
  return step.tx_signatures ?? [];
}

function hasKind(resume: WalletSettlementResumeResponse, kind: string) {
  return (resume.tx_signature_kinds ?? []).some((entry) => entry.kind === kind);
}

export function PayPage() {
  const { connection } = useConnection();
  const { publicKey, signTransaction } = useWallet();
  const { setVisible: setWalletModalVisible } = useWalletModal();
  const [assets, setAssets] = useState<Asset[]>([]);
  const [eurAmount, setEurAmount] = useState('');
  const [recipient, setRecipient] = useState('');
  const [phase, setPhase] = useState<Phase>({ kind: 'form' });
  const [preimage, setPreimage] = useState<string | null>(null);
  const quoteSeq = useRef(0);
  const resumedRef = useRef(false);
  const walletAddress = publicKey?.toBase58() ?? '';

  const eurc = useMemo(() => assets.find((asset) => asset.id === receiveAsset), [assets]);
  const usdc = useMemo(() => assets.find((asset) => asset.id === payAsset), [assets]);
  const amountCheck = useMemo(() => (eurAmount.trim() ? parseAmountRaw(eurAmount, eurc?.decimals ?? 6) : null), [eurAmount, eurc]);
  const recipientOk = recipient.trim() === '' || isValidAddress(recipient);
  const inFlight = phase.kind !== 'form' && phase.kind !== 'quoting' && phase.kind !== 'quoted' && phase.kind !== 'rejected';

  useEffect(() => {
    api.assets().then(setAssets).catch(() => setAssets([]));
  }, []);

  // Quote whenever the amount changes and nothing is settling.
  useEffect(() => {
    if (inFlight || !walletAddress || amountCheck?.ok !== true) return undefined;
    const seq = quoteSeq.current + 1;
    quoteSeq.current = seq;
    setPhase({ kind: 'quoting' });
    const timer = window.setTimeout(async () => {
      try {
        const response = await api.requestRfq({
          input_asset: payAsset,
          output_asset: receiveAsset,
          output_amount: eurAmount.trim(),
          taker_wallet: walletAddress,
          expiry_seconds: quoteExpirySeconds
        });
        if (quoteSeq.current !== seq) return;
        if (response.status === 'accepted') {
          setPhase({ kind: 'quoted', quote: response, secondsLeft: quoteExpirySeconds });
        } else {
          setPhase({ kind: 'rejected', message: response.message || response.reason });
        }
      } catch (error) {
        if (quoteSeq.current !== seq) return;
        setPhase({ kind: 'rejected', message: error instanceof Error ? error.message : 'Quote unavailable.' });
      }
    }, quoteDebounceMs);
    return () => window.clearTimeout(timer);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [eurAmount, walletAddress, amountCheck?.ok, inFlight]);

  // Countdown; when the quote expires, ask for a fresh one.
  useEffect(() => {
    if (phase.kind !== 'quoted') return undefined;
    const expiresAt = new Date(phase.quote.expires_at).getTime();
    const tick = window.setInterval(() => {
      const left = Math.max(0, Math.ceil((expiresAt - Date.now()) / 1000));
      if (left === 0) {
        quoteSeq.current += 1;
        setPhase({ kind: 'form' });
        setEurAmount((value) => `${value}`);
      } else {
        setPhase((current) => (current.kind === 'quoted' ? { ...current, secondsLeft: left } : current));
      }
    }, 1000);
    return () => window.clearInterval(tick);
  }, [phase.kind, phase.kind === 'quoted' ? phase.quote.expires_at : null]);

  const redeem = useCallback(async (tradeId: string, secret: string) => {
    if (!signTransaction) throw new Error('Connected wallet cannot sign transactions.');
    setPhase({ kind: 'redeeming', tradeId });
    const prepared = await api.takerRedeem(tradeId, { preimage: secret });
    if (!prepared.taker_redeem_transaction) {
      writeStored(null);
      setPhase({ kind: 'paid', tradeId, signatures: signaturesOf(prepared) });
      return;
    }
    const signature = await signAndSubmit(connection, signTransaction, prepared.taker_redeem_transaction.transaction_base64);
    const done = await api.takerRedeem(tradeId, { preimage: secret, signature });
    writeStored(null);
    setPhase({ kind: 'paid', tradeId, signatures: signaturesOf(done) });
  }, [connection, signTransaction]);

  // Resume an interrupted payment once the paying wallet reconnects.
  useEffect(() => {
    if (resumedRef.current || !walletAddress) return;
    const stored = readStored();
    if (!stored) return;
    resumedRef.current = true;
    if (stored.wallet !== walletAddress) {
      writeStored(null);
      return;
    }
    setRecipient(stored.recipient);
    setEurAmount(stored.eurAmount);
    setPreimage(stored.preimage);
    void (async () => {
      try {
        const resume = await api.resumeSettlement(stored.tradeId);
        if (hasKind(resume, 'taker_redeem') || resume.settlement_status === 'redeemed') {
          writeStored(null);
          setPhase({ kind: 'paid', tradeId: stored.tradeId, signatures: resume.tx_signatures ?? [] });
          return;
        }
        if (hasKind(resume, 'taker_refund') || resume.settlement_status === 'refunded') {
          writeStored(null);
          setPhase({ kind: 'refunded', tradeId: stored.tradeId, signatures: resume.tx_signatures ?? [] });
          return;
        }
        if (resume.taker_refund_transaction) {
          setPhase({ kind: 'expired', tradeId: stored.tradeId });
          return;
        }
        if (hasKind(resume, 'maker_lock')) {
          await redeem(stored.tradeId, stored.preimage);
          return;
        }
        if (stored.lockSignature && !hasKind(resume, 'taker_lock')) {
          setPhase({ kind: 'maker_locking', tradeId: stored.tradeId });
          await api.takerLock(stored.tradeId, { signature: stored.lockSignature });
          await redeem(stored.tradeId, stored.preimage);
          return;
        }
        if (hasKind(resume, 'taker_lock')) {
          setPhase({ kind: 'maker_locking', tradeId: stored.tradeId });
          await api.takerLock(stored.tradeId, { signature: stored.lockSignature ?? '' });
          await redeem(stored.tradeId, stored.preimage);
          return;
        }
        writeStored(null);
        setPhase({ kind: 'form' });
      } catch (error) {
        setPhase({ kind: 'failed', message: walletFailure(error, 'Could not resume the payment.') });
      }
    })();
  }, [walletAddress, redeem]);

  async function pay() {
    if (phase.kind !== 'quoted' || !publicKey || !signTransaction || !usdc) return;
    const quote = phase.quote;
    if (recipient.trim() && !isValidAddress(recipient)) return;
    quoteSeq.current += 1;
    try {
      const funding = await walletFundingIssue(connection, publicKey, usdc, quote.input.amount_raw);
      if (funding) {
        setPhase({ kind: 'failed', message: funding.detail });
        return;
      }
      const secretBytes = crypto.getRandomValues(new Uint8Array(32));
      const hash = new Uint8Array(await crypto.subtle.digest('SHA-256', secretBytes));
      const secret = bytesToHex(secretBytes);
      setPreimage(secret);
      const settlement = await api.walletSettlement(quote.quote_id, {
        taker_wallet: publicKey.toBase58(),
        secret_hash: bytesToHex(hash),
        ...(recipient.trim() ? { recipient: recipient.trim() } : {})
      });
      writeStored({
        version: 1,
        wallet: publicKey.toBase58(),
        recipient: recipient.trim(),
        eurAmount,
        tradeId: settlement.trade_id,
        preimage: secret,
        expiresAt: settlement.expires_at ?? null,
        lockSignature: null
      });
      setPhase({ kind: 'locking', tradeId: settlement.trade_id });
      const lockSignature = await signAndSubmit(connection, signTransaction, settlement.taker_lock_transaction.transaction_base64);
      const stored = readStored();
      if (stored) writeStored({ ...stored, lockSignature });
      setPhase({ kind: 'maker_locking', tradeId: settlement.trade_id });
      await api.takerLock(settlement.trade_id, { signature: lockSignature });
      await redeem(settlement.trade_id, secret);
    } catch (error) {
      if (isWalletCancellation(error) && phase.kind === 'quoted') {
        setPhase({ kind: 'form' });
        return;
      }
      setPhase({ kind: 'failed', message: walletFailure(error, 'Payment failed.') });
    }
  }

  async function refund() {
    if (phase.kind !== 'expired' || !signTransaction) return;
    const tradeId = phase.tradeId;
    setPhase({ kind: 'refunding', tradeId });
    try {
      const prepared = await api.takerRefund(tradeId, {});
      if (!prepared.taker_refund_transaction) {
        writeStored(null);
        setPhase({ kind: 'refunded', tradeId, signatures: signaturesOf(prepared) });
        return;
      }
      const signature = await signAndSubmit(connection, signTransaction, prepared.taker_refund_transaction.transaction_base64);
      const done = await api.takerRefund(tradeId, { signature });
      writeStored(null);
      setPhase({ kind: 'refunded', tradeId, signatures: signaturesOf(done) });
    } catch (error) {
      setPhase({ kind: 'failed', message: walletFailure(error, 'Refund failed.') });
    }
  }

  function startOver() {
    writeStored(null);
    setPreimage(null);
    setEurAmount('');
    setRecipient('');
    setPhase({ kind: 'form' });
  }

  function submit(event: FormEvent) {
    event.preventDefault();
    if (!walletAddress) {
      setWalletModalVisible(true);
      return;
    }
    void pay();
  }

  const cost = phase.kind === 'quoted' ? phase.quote.input.amount : null;
  const rate = phase.kind === 'quoted' && usdc && eurc
    ? (Number(phase.quote.output.amount) / Number(phase.quote.input.amount)).toFixed(4)
    : null;

  return (
    <main className="swap-page">
      <section className="swap-page-header">
        <div className="hero-brand-stack">
          <p className="eyebrow">Firm FX</p>
          <h1>Pay in euros</h1>
          <p className="lede">The rate you see is the rate they get. Locked for {quoteExpirySeconds}s, settled atomically.</p>
        </div>
        <div className="wallet-actions">
          <button className="wallet-button" onClick={() => setWalletModalVisible(true)} type="button">
            {walletAddress ? shortValue(walletAddress) : 'Connect wallet'}
          </button>
        </div>
      </section>

      <section className="swap-layout">
        <form className="swap-card" onSubmit={submit}>
          <div className="swap-card-heading"><h2>Send EURC</h2></div>

          <label className="pay-field">
            <span>Recipient gets</span>
            <input
              inputMode="decimal"
              placeholder="89.30"
              value={eurAmount}
              disabled={inFlight}
              onChange={(event) => setEurAmount(event.target.value)}
            />
            <b>EURC</b>
          </label>
          {amountCheck && !amountCheck.ok && <p className="field-error">{amountCheck.message}</p>}

          <label className="pay-field">
            <span>Recipient address</span>
            <input
              placeholder="Leave empty to receive it yourself"
              value={recipient}
              disabled={inFlight}
              onChange={(event) => setRecipient(event.target.value)}
            />
          </label>
          {!recipientOk && <p className="field-error">That is not a Solana address.</p>}

          <div className="quote-summary">
            <span>You pay</span>
            <strong>{cost ? `${cost} USDC` : phase.kind === 'quoting' ? 'Quoting…' : '—'}</strong>
            <span>Rate</span>
            <strong>{rate ? `1 USDC = ${rate} EURC` : '—'}</strong>
            {phase.kind === 'quoted' && (
              <>
                <span>Locked for</span>
                <strong>{phase.secondsLeft}s</strong>
              </>
            )}
          </div>

          {phase.kind === 'rejected' && <p className="field-error">{phase.message}</p>}
          {phase.kind === 'failed' && <p className="field-error">{phase.message}</p>}

          {(phase.kind === 'paid' || phase.kind === 'refunded' || phase.kind === 'failed') ? (
            <button type="button" className="primary-swap-button primary-swap-button-reset" onClick={startOver}>New payment</button>
          ) : phase.kind === 'expired' ? (
            <button type="button" className="primary-swap-button" onClick={() => void refund()}>Lock expired, refund my USDC</button>
          ) : (
            <button
              className="primary-swap-button"
              disabled={inFlight || (walletAddress !== '' && (phase.kind !== 'quoted' || !recipientOk))}
            >
              {!walletAddress ? 'Connect wallet' : phase.kind === 'locking' ? 'Confirm in wallet…' : phase.kind === 'maker_locking' ? 'Maker locking EURC…' : phase.kind === 'redeeming' ? 'Delivering…' : `Pay ${cost ?? ''} USDC`}
            </button>
          )}
        </form>

        <aside className="behind-scenes">
          <h3>Settlement</h3>
          <ol className="pay-steps">
            <li className={inFlight || phase.kind === 'paid' ? 'done' : ''}>Quote locked at a firm rate</li>
            <li className={['maker_locking', 'redeeming', 'paid'].includes(phase.kind) ? 'done' : ''}>Your USDC locked in escrow</li>
            <li className={['redeeming', 'paid'].includes(phase.kind) ? 'done' : ''}>Maker locked the EURC for the recipient</li>
            <li className={phase.kind === 'paid' ? 'done' : ''}>Recipient received exactly {eurAmount || '…'} EURC</li>
          </ol>
          {(phase.kind === 'paid' || phase.kind === 'refunded') && (
            <ul className="pay-receipt">
              {phase.signatures.map((signature) => (
                <li key={signature}><a href={solscan(signature)} target="_blank" rel="noreferrer">{shortValue(signature)}</a></li>
              ))}
            </ul>
          )}
          {preimage && phase.kind !== 'paid' && phase.kind !== 'form' && <p className="locked-note">Keep this tab open until delivery completes; it holds the secret that releases the EURC.</p>}
          <p className="locked-note">{eurc ? `Maker quotes up to ${eurc.max_trade_amount ?? '?'} EURC per payment on this demo.` : 'EURC is not enabled on this maker.'}</p>
        </aside>
      </section>
    </main>
  );
}
```

`formatRawAmount` is imported for the receipt if you render raw amounts; drop the import if unused.

Add to `web/app/src/styles.css`:

```css
.pay-field { display: grid; grid-template-columns: 1fr auto; gap: 0.25rem 0.5rem; align-items: center; margin-top: 1rem; }
.pay-field span { grid-column: 1 / -1; font-size: 0.75rem; letter-spacing: 0.08em; text-transform: uppercase; opacity: 0.7; }
.pay-field input { font: inherit; font-size: 1.25rem; padding: 0.6rem 0.75rem; border: 2px solid currentColor; background: transparent; color: inherit; min-width: 0; }
.pay-steps { list-style: none; padding: 0; margin: 1rem 0; display: grid; gap: 0.5rem; }
.pay-steps li { padding-left: 1.5rem; position: relative; opacity: 0.55; }
.pay-steps li::before { content: ''; position: absolute; left: 0; top: 0.35em; width: 0.8em; height: 0.8em; border: 2px solid currentColor; }
.pay-steps li.done { opacity: 1; }
.pay-steps li.done::before { background: currentColor; }
.pay-receipt { list-style: none; padding: 0; margin: 0.5rem 0; display: grid; gap: 0.25rem; font-family: var(--font-mono, monospace); }
```

- [ ] **Step 3: Route it**

`web/app/src/main.tsx`: add `const PayRoute = lazy(() => import('./pages/PayRoute'));`, change the routes to

```tsx
            <Route path="/" element={<PayRoute />} />
            <Route path="/swap" element={<SwapRoute />} />
            <Route path="/runtime" element={<RuntimePage />} />
```

and the nav to

```tsx
          <NavLink to="/" end>Pay</NavLink>
          <NavLink to="/swap">Swap</NavLink>
          <NavLink to="/runtime">Maker</NavLink>
```

Footer tagline in `ConsoleFooter`: `Firm FX for stablecoin payments on Solana.`; signals: `Firm quotes`, `Atomic settlement`, `Ledger proof`.

`web/app/src/pages/RuntimePage.tsx`: `grep -n "<h1\|eyebrow" web/app/src/pages/RuntimePage.tsx`, change the `<h1>` text to `Maker` and the eyebrow to `Firm FX runtime`.

- [ ] **Step 4: Build and lint**

Run: `npm run app:build 2>&1 | tail -5` and `npm run web:polish 2>&1 | tail -5`
Expected: `✓ built`, polish check passes (if it flags the new page for a missing font or favicon, follow its message).

- [ ] **Step 5: Run it locally against the deployed API**

`web/app/vite.config.ts` proxies `/v1` to `127.0.0.1:5050`; for a local check against Railway, run the backend locally instead with `.env` + `config.toml` (`MAKER_KEYPAIR_PATH=~/.config/solana/firmament-maker.json`, `enable_protocol_workers = true`) and `cargo run`, then `npm run dev`. Open `http://127.0.0.1:3000/app`, connect Phantom, type `1` as the amount: expect a USDC cost around 1.12 and a 30-second countdown.

- [ ] **Step 6: Manual checks (Review Focus 4 and 5)**

Let a quote expire while the Phantom prompt is open: cancel, confirm the page returns to the form with a fresh quote. Mid-payment (after lock), refresh the page: it should resume and complete. Switch to another wallet after a refresh: the stored payment must be dropped and the form shown empty.

- [ ] **Step 7: Commit (only after Azeem's go-ahead)**

```bash
git add web/app/src
git commit -m "Add the Pay page and move swap to /app/swap"
```

---

### Task 8: Landing page copy

**Files:**
- Modify: `landing/src/app/page.tsx` (constants and hero)
- Modify: `landing/AGENTS.md` (positioning)

- [ ] **Step 1: Replace the copy**

In `landing/src/app/page.tsx` set `const appHref = "https://<web-app>.vercel.app/app";` (the Task 9 URL; use `/app` until then) and replace the constants:

```ts
const proofPoints = [
  ["Quote", "Exact recipient amount"],
  ["Lock", "Rate held for 30 seconds"],
  ["Settle", "Atomic Solana HTLC"],
  ["Refill", "USDC through Circle Gateway"],
];

const runtimeBlocks = [
  { label: "Quote", title: "Exact-output RFQ", copy: "Ask for 89.30 EURC, get the USDC cost.", tone: "lime" },
  { label: "Policy", title: "Risk gate", copy: "Caps, inventory, stale prices.", tone: "amber" },
  { label: "Inventory", title: "EURC on hand", copy: "The maker holds the thin side.", tone: "cyan" },
  { label: "Settlement", title: "Solana HTLC", copy: "Both legs or neither.", tone: "blue" },
  { label: "Repair", title: "Rebalance + refill", copy: "Jupiter and Circle Gateway.", tone: "violet" },
  { label: "Ledger", title: "Double-entry proof", copy: "Every payment booked.", tone: "red" },
];

const productLanes = [
  { label: "Payment apps", title: "Promise a euro amount", copy: "Show the recipient's exact amount before the sender signs, and deliver exactly that." },
  { label: "Payroll", title: "Pay contractors in their currency", copy: "USDC in, EURC out, one transaction, no slippage to explain." },
  { label: "Makers", title: "Run the FX desk", copy: "Hold EURC inventory, quote under policy, refill USDC from Gateway." },
];

const positionReasons = [
  "USDC has $465M of Solana DEX liquidity; EURC has $0.7M (Jupiter, Oct 2026)",
  "An AMM route cannot promise a recipient amount; a maker with inventory can",
  "Firm quote, then atomic settlement: the recipient gets the number or the sender is refunded",
  "Self-hosted maker runtime: any app can run its own",
];

const runtimeSignals = [
  ["Health", "runtime status"],
  ["Inventory", "USDC / EURC / SOL"],
  ["Flow", "quotes, locks, payouts"],
  ["Repair", "Jupiter + Gateway"],
  ["Ledger", "balances + P&L"],
];

const apiProof = [
  ["Quote", "POST /v1/rfq { output_amount }"],
  ["Pay", "wallet-settlement { recipient }"],
  ["Runtime", "GET state + events"],
  ["Ledger", "balances + trades"],
];

const footerSignals = [
  { code: "01", label: "Exact recipient amounts" },
  { code: "02", label: "Firm quotes under policy" },
  { code: "03", label: "Atomic HTLC settlement" },
  { code: "04", label: "Gateway-refilled inventory" },
];
```

Hero: eyebrow `Firm FX for stablecoin payments`, `<h1>` stays `Firmament`, lede:

```
The rate you promise is the rate they get. Firmament quotes an exact recipient
amount in EURC, holds the USDC cost for 30 seconds, and settles both legs
atomically on Solana.
```

Primary button: `Try a payment` → `appHref`; secondary: `View on GitHub`. Board top: `exact-output quotes` / `USDC in, EURC out`; pieces: `EURC inventory`, `Policy`, `Jupiter`, `HTLC`, `Ledger`, `Rebalance`, `Gateway refill`, `Runtime API`; board footer: `quote when safe` / `deliver the exact amount`.

Section headings: product `Payments need a number, not a route.` with the paragraph `Swap routers optimise what you pay. Payment apps have to promise what the other side receives. Firmament is the maker that makes that promise safe to show.`; position panel `Why now` / `Stablecoin payments are going multi-currency and the non-USD side on Solana is thin.`; runtime `See the desk the maker is running.`; proof `A payment, end to end.`; API `A small API for firm FX.` with paragraph `Quote an exact output, start wallet settlement with a recipient, and read the same runtime state the maker uses.`. Footer brand line: `Firm FX for stablecoin payments on Solana.`

`landing/AGENTS.md`: replace the "Colosseum Submission Positioning" section with the World's Fair framing: target hackathon Crypto World's Fair (Sep 14–Oct 12, 2026), category Payments & Remittance, the core claim `The rate you promise is the rate they get`, the liquidity numbers, and the disclosure that pre-Sep-14 code is prior work.

- [ ] **Step 2: Build**

Run: `npm run landing:build 2>&1 | tail -5`
Expected: `✓ Generating static pages` and `landing/out/index.html` exists.

- [ ] **Step 3: Commit (only after Azeem's go-ahead)**

```bash
git add landing/src/app/page.tsx landing/AGENTS.md
git commit -m "Reposition the landing page around firm FX payments"
```

---

### Task 9: Web app and landing on Vercel

**Files:**
- Modify: `web/app/vercel.json` (rewrite targets)
- Modify: `landing/src/app/page.tsx` (`appHref`)

- [ ] **Step 1: Point the rewrites at Railway**

In `web/app/vercel.json` replace both `https://api.firmament.shaikazeem.com` destinations with the Railway URL from Task 6.

- [ ] **Step 2: Deploy the web app**

```bash
cd web/app && vercel link --yes --project firmament-app && vercel --prod --yes 2>&1 | tail -3
```

Azeem adds `VITE_SOLANA_RPC_URL` (the Helius URL) as a Vercel environment variable for Production in the dashboard before the deploy, or the app falls back to the public RPC. Expected output: `Production: https://firmament-app-….vercel.app`. Verify: `curl -s https://<app>.vercel.app/health` returns the API's health JSON (proving the rewrite), and `/app` loads the Pay page.

- [ ] **Step 3: Deploy the landing**

Set `appHref` in `landing/src/app/page.tsx` to the web app URL, rebuild, then:

```bash
cd landing && vercel link --yes --project firmament && vercel --prod --yes 2>&1 | tail -3
```

Verify the landing loads and its `Try a payment` button opens the Pay page.

- [ ] **Step 4: Commit (only after Azeem's go-ahead)**

```bash
git add web/app/vercel.json landing/src/app/page.tsx docs/plans/deploy-notes.md
git commit -m "Deploy the app and landing on Vercel against the Railway API"
```

---

### Task 10: Mainnet demo run and evidence

**Files:**
- Create (outside git): `~/firmament-demo-evidence/<run-id>/…` per `docs/demo-evidence-checklist.md`

- [ ] **Step 1: Fund and verify inventory**

After Azeem funds `9yovryyWxBLcmhuR92jFYq3Sc9kdmJrfFdsZ56gy5Mud` (≈0.1 SOL, 30 USDC, 20 EURC): `curl -s <api>/v1/runtime/state | jq .inventory` shows the three balances; `curl -s <api>/v1/runtime/ledger | jq '.balances[] | select(.account_type=="working_custody")'` matches.

- [ ] **Step 2: One live smoke**

From Phantom (a second wallet with a few USDC), pay `1` EURC to a fresh recipient wallet that has never held EURC. Expect: quote ≈ 1.12 USDC, one lock signature, maker lock, redeem with the create-ATA instruction, recipient balance exactly 1.000000 EURC. Then a second payment to yourself (no recipient). Then one deliberately expired lock (lock, wait 31 s without redeeming… the HTLC expiry is the quote expiry, so wait past it) and refund.

- [ ] **Step 3: Capture**

Follow `docs/demo-evidence-checklist.md`: config and caps snapshot, API snapshots before/after, trade JSON for each trade, screenshots of `/app`, `/app/runtime`, Solscan links. Run the redaction grep. Keep the folder outside the repo.

---

### Task 11: Demo video and pitch video

**Files:**
- Create: `video/` (Remotion project), `video/src/Root.tsx`, `video/src/Demo.tsx`, `video/public/clips/*.mov` (Azeem's screen recordings)

- [ ] **Step 1: Record the raw clips (Azeem)**

With QuickTime (`⌘⇧5`), 1920×1080, record: (a) the Pay page end to end for a 1 EURC payment to a second wallet, with Phantom visible, (b) the recipient wallet showing exactly 1 EURC landing, (c) the Maker page after the payment, (d) a Solscan page for the redeem. Save as `video/public/clips/pay.mov`, `receive.mov`, `maker.mov`, `solscan.mov`.

- [ ] **Step 2: Scaffold Remotion**

```bash
npx create-video@latest video --template blank --yes
cd video && npm install
```

`video/src/Demo.tsx`:

```tsx
import { AbsoluteFill, OffthreadVideo, Sequence, staticFile, useCurrentFrame, interpolate } from 'remotion';

const fps = 30;

function Card({ title, body }: { title: string; body: string }) {
  const frame = useCurrentFrame();
  const opacity = interpolate(frame, [0, 10], [0, 1], { extrapolateRight: 'clamp' });
  return (
    <AbsoluteFill style={{ background: '#0b0b0c', color: '#f4f1ea', justifyContent: 'center', padding: 120, fontFamily: 'IBM Plex Sans, sans-serif', opacity }}>
      <h1 style={{ fontSize: 96, margin: 0, lineHeight: 1.05 }}>{title}</h1>
      <p style={{ fontSize: 40, opacity: 0.8, marginTop: 32 }}>{body}</p>
    </AbsoluteFill>
  );
}

function Clip({ src, caption }: { src: string; caption: string }) {
  return (
    <AbsoluteFill style={{ background: '#0b0b0c' }}>
      <OffthreadVideo src={staticFile(src)} style={{ width: '100%', height: '100%', objectFit: 'contain' }} />
      <div style={{ position: 'absolute', left: 80, bottom: 60, padding: '16px 24px', background: 'rgba(0,0,0,0.7)', color: '#f4f1ea', fontSize: 36, fontFamily: 'IBM Plex Sans, sans-serif' }}>{caption}</div>
    </AbsoluteFill>
  );
}

export const Demo = () => (
  <AbsoluteFill>
    <Sequence from={0} durationInFrames={5 * fps}><Card title="Firmament" body="The rate you promise is the rate they get." /></Sequence>
    <Sequence from={5 * fps} durationInFrames={8 * fps}><Card title="USDC: $465M. EURC: $0.7M." body="Any app promising a euro amount through a Solana AMM is guessing." /></Sequence>
    <Sequence from={13 * fps} durationInFrames={60 * fps}><Clip src="clips/pay.mov" caption="Exact-output quote, locked 30s, one wallet confirmation" /></Sequence>
    <Sequence from={73 * fps} durationInFrames={15 * fps}><Clip src="clips/receive.mov" caption="Recipient receives exactly 1.000000 EURC" /></Sequence>
    <Sequence from={88 * fps} durationInFrames={20 * fps}><Clip src="clips/maker.mov" caption="The maker's inventory, ledger and refill rail" /></Sequence>
    <Sequence from={108 * fps} durationInFrames={15 * fps}><Clip src="clips/solscan.mov" caption="Both legs on chain: lock, lock, redeem, redeem" /></Sequence>
    <Sequence from={123 * fps} durationInFrames={7 * fps}><Card title="Firm FX for stablecoin payments" body="github.com/azeemshaik025/firmament" /></Sequence>
  </AbsoluteFill>
);
```

`video/src/Root.tsx` registers `<Composition id="Demo" component={Demo} durationInFrames={130 * 30} fps={30} width={1920} height={1080} />`. Trim clip lengths to the recordings (`durationInFrames` per sequence) so nothing shows a frozen last frame.

- [ ] **Step 3: Render**

```bash
cd video && npx remotion render Demo out/firmament-demo.mp4 2>&1 | tail -3
```

Expected: `Rendered out/firmament-demo.mp4`, under 3 minutes. Upload to Loom or YouTube (unlisted) and keep the link for the form.

- [ ] **Step 4: Pitch video (Azeem on camera, Loom, 2–3 min)**

Outline, one sentence per beat: who you are and that you built a maker runtime for Frontier; the problem with the two liquidity numbers; what Firmament does now (exact amount, locked rate, atomic settlement, recipient); 30 seconds of the demo clip; who pays (payment and payroll apps needing EUR/BRL/MXN legs; spread on conversions; makers can self-host); what's next (more non-USD stables, hosted maker, SDK). No slides needed beyond the demo clip.

- [ ] **Step 5: Commit the video source (only after Azeem's go-ahead)**

```bash
git add video/src video/package.json video/package-lock.json video/tsconfig.json video/remotion.config.ts
git commit -m "Add the Remotion demo composition"
```

Add `video/out/` and `video/public/clips/` to `.gitignore`.

---

### Task 12: Submission

**Files:**
- Modify: `README.md` (one paragraph on the payments framing, the Pay page, disclosure), `docs/index.mdx`
- Arena: create the project draft, fill the submission form, post builder updates

- [ ] **Step 1: README and docs**

README intro paragraph becomes the pitch (two sentences), followed by the existing self-hosting text; add a `## What's new for Crypto World's Fair` section listing EURC, exact-output quotes, third-party recipients, the Pay page, the redeploy, and the sentence `Everything before 2026-09-14 was built for the Solana Frontier Hackathon and is prior work.` `docs/index.mdx` first paragraph gets the same framing. `npm run docs:check` passes.

- [ ] **Step 2: Arena draft**

On colosseum.com/arena/hackathon, Create project: name `Firmament`, category Payments & Remittance, two-sentence description in Azeem's words. Then the submission form, field by field, Azeem writing in his own words from these points (not pasted text):

- What are you building and for whom: firm-quote FX maker for stablecoin payment apps; exact recipient amounts; atomic settlement; EURC first.
- Why now: multi-currency stablecoins arriving on Solana, thin non-USD liquidity (the numbers), payment apps multiplying (count the corridor entries in Frontier's directory).
- Technologies: Rust/axum runtime, Solana HTLC settlement, Jupiter Swap API V2, Circle Gateway, Vite/React, Next.js, Railway, Vercel, Remotion.
- Anything else judges should know: prior-work disclosure with the date line; mainnet demo caps; link to the evidence Solscan transactions.
- Repo context: which directories are new in the window (`web/app/src/pages/PayPage.tsx`, `src/domain/quote_engine.rs` exact-output, recipient plumbing, `video/`), and that commit history shows the split.
- Links: live app (Vercel), landing (Vercel), repo, demo video, pitch video, X profile.

- [ ] **Step 3: Builder updates**

Post two or three updates on the arena feed during the build (Saturday: the pivot and the liquidity numbers; Sunday: exact-output quotes and recipient settlement live on mainnet with a Solscan link; Monday/Tuesday: the Pay page and the videos). Short, in Azeem's words, one screenshot each.

- [ ] **Step 4: Final checks before submitting**

Every link in the form opens in a private window; the demo video is under 3 minutes; the pitch video is 2–3 minutes; the repo is public and the last commit is before the deadline; `cargo test` and both builds are green on the pushed commit.

---

### Task 13 (stretch, Tuesday morning only): MCP server

**Files:**
- Create: `mcp/package.json`, `mcp/src/index.ts`

Only if Tasks 1–12 are done and the videos are recorded.

- [ ] **Step 1: Scaffold**

```bash
mkdir -p mcp/src && cd mcp && npm init -y && npm install @modelcontextprotocol/sdk @solana/web3.js zod && npm install -D typescript tsx @types/node
```

- [ ] **Step 2: One tool, `pay`**

`mcp/src/index.ts`:

```ts
import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js';
import { StdioServerTransport } from '@modelcontextprotocol/sdk/server/stdio.js';
import { Connection, Keypair, Transaction } from '@solana/web3.js';
import { createHash, randomBytes } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { z } from 'zod';

const api = process.env.FIRMAMENT_API ?? 'http://127.0.0.1:5050';
const rpc = process.env.SOLANA_RPC_URL ?? 'https://api.mainnet-beta.solana.com';
const keypair = Keypair.fromSecretKey(Uint8Array.from(JSON.parse(readFileSync(process.env.PAYER_KEYPAIR_PATH!, 'utf8'))));
const connection = new Connection(rpc, 'confirmed');

async function call<T>(path: string, body?: unknown): Promise<T> {
  const response = await fetch(`${api}${path}`, { method: body ? 'POST' : 'GET', headers: { 'content-type': 'application/json' }, body: body ? JSON.stringify(body) : undefined });
  const json = await response.json();
  if (!response.ok) throw new Error(JSON.stringify(json));
  return json as T;
}

async function sign(transactionBase64: string) {
  const transaction = Transaction.from(Buffer.from(transactionBase64, 'base64'));
  transaction.sign(keypair);
  return connection.sendRawTransaction(transaction.serialize());
}

const server = new McpServer({ name: 'firmament', version: '0.1.0' });

server.tool('pay', 'Pay an exact EURC amount to a Solana address at a firm USDC rate', {
  amount: z.string().describe('EURC amount the recipient must receive, e.g. "12.50"'),
  recipient: z.string().describe('Solana address that receives the EURC')
}, async ({ amount, recipient }) => {
  const taker = keypair.publicKey.toBase58();
  const quote = await call<{ status: string; quote_id: string; input: { amount: string }; message?: string }>('/v1/rfq', { input_asset: 'USDC', output_asset: 'EURC', output_amount: amount, taker_wallet: taker, expiry_seconds: 30 });
  if (quote.status !== 'accepted') return { content: [{ type: 'text', text: `Quote refused: ${quote.message ?? quote.status}` }] };
  const secret = randomBytes(32);
  const secretHash = createHash('sha256').update(secret).digest('hex');
  const settlement = await call<{ trade_id: string; taker_lock_transaction: { transaction_base64: string } }>(`/v1/quotes/${quote.quote_id}/wallet-settlement`, { taker_wallet: taker, secret_hash: secretHash, recipient });
  const lockSignature = await sign(settlement.taker_lock_transaction.transaction_base64);
  await call(`/v1/trades/${settlement.trade_id}/taker-lock`, { signature: lockSignature });
  const prepared = await call<{ taker_redeem_transaction?: { transaction_base64: string } }>(`/v1/trades/${settlement.trade_id}/taker-redeem`, { preimage: secret.toString('hex') });
  const redeemSignature = prepared.taker_redeem_transaction ? await sign(prepared.taker_redeem_transaction.transaction_base64) : null;
  if (redeemSignature) await call(`/v1/trades/${settlement.trade_id}/taker-redeem`, { preimage: secret.toString('hex'), signature: redeemSignature });
  return { content: [{ type: 'text', text: `Paid ${amount} EURC to ${recipient} for ${quote.input.amount} USDC. Lock ${lockSignature}${redeemSignature ? `, redeem ${redeemSignature}` : ''}.` }] };
});

await server.connect(new StdioServerTransport());
```

- [ ] **Step 3: Try it from Claude Code**

`claude mcp add firmament -- npx tsx mcp/src/index.ts` with `FIRMAMENT_API`, `SOLANA_RPC_URL` and `PAYER_KEYPAIR_PATH` (a throwaway payer with a few USDC, never the maker key) in the environment; ask Claude to pay 1 EURC to an address; record the 20-second clip for the demo video's maker section.

---

## Self-review

- Spec coverage: EURC (Task 4), exact-output (Tasks 1–3), recipient (Task 5), HTTP contract and OpenAPI (Tasks 3, 5), Pay page and routing (Task 7), landing (Task 8), deployment (Tasks 6, 9), testing (each task), demo and media (Tasks 10, 11), submission and disclosure (Task 12), stretch MCP (Task 13). Gateway refill is left as-is per the spec.
- Placeholders: none; every code step carries its code. Where a line number may have drifted, the step gives a `grep` to relocate it.
- Type consistency: `RequestedAmount::{Input, Output}` (Tasks 2, 3); `ExternalRedeemRequest { trade_id, recipient, payer, preimage }` (Task 5 across ports, adapter, orchestrator, fakes); `start_wallet_settlement(quote_id, taker_wallet, recipient: Option<WalletAddress>, secret_hash)` (Task 5 tests, orchestrator, HTTP handler); `restore_wallet_settlement(taker_lock, maker_lock, maker_redeemer, taker_locked, maker_locked)` (port, adapter, fake, orchestrator); `uses_stable_notional_limit` (Task 4 both call sites and test); `signAndSubmit(connection, signTransaction, base64)` (Task 7 lib and page).
- Review Focus: 1 → Task 3 test `api_rfq_rejects_output_amount_with_too_many_decimals`; 2 → Task 5 test `wallet_settlement_rejects_the_maker_wallet_as_recipient`; 3 → Task 5 unit test plus the ATA branch and the fresh-recipient smoke in Task 10; 4 and 5 → Task 7 step 6 manual checks.
