use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use firmament::assets::AssetRegistry;
use firmament::ledger::LedgerAccountId;
use firmament::rfq::{RfqRequest, RfqResponse};
use firmament::runtime::TradeSignatureKind;
use firmament::solana_client::SolanaClient;
use firmament::types::{AmountRaw, AssetId, MintAddress, SettlementStatus, WalletAddress};
use firmament::wallets::{LoadedWallet, SOLANA_RPC_URL_ENV};
use firmament::{AppConfig, AppError, AppResult, bootstrap_demo_runtime};
use rust_decimal::Decimal;
use solana_sdk::{pubkey::Pubkey, transaction::Transaction};

const USDC_MINT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
const CBBTC_MINT: &str = "cbbtcf3aa214zXHbiAZQwf4122FBYbraNdFqgw4iMij";
const DEFAULT_LIVE_CBBTC_RFQ_USDC_RAW: u64 = 100_000;
const MAX_LIVE_CBBTC_RFQ_USDC_RAW: u64 = 500_000;

#[tokio::test]
async fn live_cbbtc_rfq_accept_skips_without_explicit_opt_in() {
    if std::env::var("RUN_LIVE_SOLANA_TESTS").ok().as_deref() != Some("1")
        || std::env::var("RUN_LIVE_CBBTC_RFQ_TESTS").ok().as_deref() != Some("1")
    {
        eprintln!(
            "skipping live cbBTC RFQ: RUN_LIVE_SOLANA_TESTS=1 and RUN_LIVE_CBBTC_RFQ_TESTS=1 are required"
        );
        return;
    }

    let input_amount_raw = live_cbbtc_rfq_usdc_amount_raw();
    assert!(
        (1..=MAX_LIVE_CBBTC_RFQ_USDC_RAW).contains(&input_amount_raw),
        "LIVE_CBBTC_RFQ_USDC_RAW must be between 1 and {MAX_LIVE_CBBTC_RFQ_USDC_RAW}"
    );

    let mut config = AppConfig::load().expect("load live config");
    config.runtime.database_path = temp_live_db_path();
    config.assets.policy.max_action_notional_usd = Decimal::ZERO;
    config.assets.policy.max_cumulative_automation_notional_usd = Decimal::ZERO;
    config.assets.policy.non_stable_asset_exception_notional_usd = Decimal::ZERO;
    // The live cbBTC RFQ test sends well under $1, which would otherwise trip
    // the per-asset minimum-notional gate. Lower USDC and cbBTC minimums for
    // this test specifically; production config still enforces the $1 floor.
    for asset in &mut config.assets.supported {
        if matches!(asset.id.as_str(), "USDC" | "cbBTC") {
            asset.min_trade_notional_usd = Decimal::new(1, 6); // $0.000001
        }
    }

    let maker = LoadedWallet::from_maker_env().expect("load maker wallet");
    let taker = LoadedWallet::from_taker_env().expect("load taker wallet");
    ensure_token_account_exists(&config, &maker, &taker.pubkey(), &AssetId::from("cbBTC"))
        .await
        .expect("ensure taker cbBTC ATA");

    let live = bootstrap_demo_runtime(config)
        .await
        .expect("bootstrap demo runtime");
    let response = live
        .orchestrator
        .request_rfq(RfqRequest {
            input_mint: MintAddress::new(USDC_MINT),
            output_mint: MintAddress::new(CBBTC_MINT),
            input_amount_raw: AmountRaw::new(input_amount_raw),
            taker_wallet: WalletAddress::new(taker.pubkey().to_string()),
            expiry_seconds: Some(45),
        })
        .await
        .expect("live cbBTC RFQ request");

    let quote = match response {
        RfqResponse::Accepted(quote) => quote,
        RfqResponse::Rejected(rejection) => {
            panic!("live cbBTC RFQ was rejected: {:?}", rejection.reason);
        }
    };
    assert_eq!(quote.output_amount.asset.as_str(), "cbBTC");
    assert!(quote.output_amount.amount_raw.as_u64() > 0);

    let trade = live
        .orchestrator
        .accept_quote(quote.quote_id)
        .await
        .expect("accept live cbBTC quote");

    assert_eq!(trade.settlement_status, SettlementStatus::Redeemed);
    assert_eq!(trade.output_amount.asset.as_str(), "cbBTC");
    assert!(trade.tx_signatures.len() >= 4);

    let summary = live.orchestrator.ledger_summary().expect("ledger summary");
    assert!(summary.balanced);
}

async fn ensure_token_account_exists(
    config: &AppConfig,
    payer: &LoadedWallet,
    wallet: &Pubkey,
    asset_id: &AssetId,
) -> AppResult<Pubkey> {
    let solana = SolanaClient::new(env_required(SOLANA_RPC_URL_ENV)?, &config.solana.commitment)?;
    let registry = AssetRegistry::from_config(config);
    let asset = registry.require_asset(asset_id)?;
    let ata = solana.associated_token_address(wallet, asset).await?;
    if solana.associated_token_account_exists(&ata).await? {
        return Ok(ata);
    }

    let instruction = solana
        .create_associated_token_account_instruction(&payer.pubkey(), wallet, asset)
        .await?;
    let rpc_client = solana.rpc_client();
    let blockhash = rpc_client
        .get_latest_blockhash()
        .await
        .map_err(|error| AppError::solana(format!("get latest blockhash: {error}")))?;
    let transaction = Transaction::new_signed_with_payer(
        &[instruction],
        Some(&payer.pubkey()),
        &[payer.keypair()],
        blockhash,
    );
    rpc_client
        .send_and_confirm_transaction(&transaction)
        .await
        .map_err(|error| AppError::solana(format!("create token account: {error}")))?;

    Ok(ata)
}

/// Live smoke test for Phase 4 Gap 1: forces a Gateway-backed cbBTC RFQ by
/// draining `working_custody:USDC` to zero before the maker bootstraps. The
/// runtime must select the GatewayToDex execution path, drive the real
/// `request_refill` and `execute_swap` adapter calls, and produce trade
/// signatures that include `GatewayBurn`, `GatewayMint`, and `JupiterSwap`
/// alongside the four HTLC signatures.
///
/// Gated behind `RUN_LIVE_GATEWAY_TESTS=1` AND `RUN_LIVE_CBBTC_RFQ_TESTS=1`
/// so `cargo test` never accidentally hits mainnet.
#[tokio::test]
async fn live_gateway_backed_cbbtc_rfq_skips_without_explicit_opt_in() {
    if std::env::var("RUN_LIVE_GATEWAY_TESTS").ok().as_deref() != Some("1")
        || std::env::var("RUN_LIVE_CBBTC_RFQ_TESTS").ok().as_deref() != Some("1")
    {
        eprintln!(
            "skipping live gateway-backed cbBTC RFQ: RUN_LIVE_GATEWAY_TESTS=1 and RUN_LIVE_CBBTC_RFQ_TESTS=1 are required"
        );
        return;
    }

    let input_amount_raw = live_cbbtc_rfq_usdc_amount_raw();
    assert!(
        (1..=MAX_LIVE_CBBTC_RFQ_USDC_RAW).contains(&input_amount_raw),
        "LIVE_CBBTC_RFQ_USDC_RAW must be between 1 and {MAX_LIVE_CBBTC_RFQ_USDC_RAW}"
    );

    let mut config = AppConfig::load().expect("load live config");
    config.runtime.database_path = temp_live_db_path();
    config.assets.policy.max_action_notional_usd = Decimal::ZERO;
    config.assets.policy.max_cumulative_automation_notional_usd = Decimal::ZERO;
    config.assets.policy.non_stable_asset_exception_notional_usd = Decimal::ZERO;
    for asset in &mut config.assets.supported {
        if matches!(asset.id.as_str(), "USDC" | "cbBTC") {
            asset.min_trade_notional_usd = Decimal::new(1, 6);
        }
    }

    let maker = LoadedWallet::from_maker_env().expect("load maker wallet");
    let taker = LoadedWallet::from_taker_env().expect("load taker wallet");
    ensure_token_account_exists(&config, &maker, &taker.pubkey(), &AssetId::from("cbBTC"))
        .await
        .expect("ensure taker cbBTC ATA");

    let live = bootstrap_demo_runtime(config)
        .await
        .expect("bootstrap demo runtime");

    // Drain working_custody:USDC to zero before the RFQ so the path resolver
    // MUST select Gateway-backed (no inventory available for the trade).
    let persistence = live
        .orchestrator
        .persistence()
        .expect("live runtime has persistence");
    let usdc = AssetId::from("USDC");
    let working_usdc = persistence
        .account_balance(&LedgerAccountId::working(usdc.clone()))
        .expect("read working USDC balance");
    if working_usdc > 0 {
        let raw = u64::try_from(working_usdc).expect("working USDC fits u64");
        let drain = firmament::ledger::LedgerTransactionBuilder::new(
            "live_gateway_path_drain",
            uuid::Uuid::now_v7(),
        )
        .description("drain working USDC for live gateway-backed RFQ test")
        .idempotency_key(format!("live:drain:working_usdc:{}", uuid::Uuid::now_v7()))
        .debit(
            LedgerAccountId::external(usdc.clone(), "live_drain"),
            AmountRaw::new(raw),
        )
        .credit(LedgerAccountId::working(usdc), AmountRaw::new(raw))
        .build()
        .expect("balanced drain");
        persistence
            .save_ledger_transaction(&drain)
            .expect("save drain transaction");
    }

    let response = live
        .orchestrator
        .request_rfq(RfqRequest {
            input_mint: MintAddress::new(USDC_MINT),
            output_mint: MintAddress::new(CBBTC_MINT),
            input_amount_raw: AmountRaw::new(input_amount_raw),
            taker_wallet: WalletAddress::new(taker.pubkey().to_string()),
            expiry_seconds: Some(45),
        })
        .await
        .expect("live cbBTC RFQ request");

    let quote = match response {
        RfqResponse::Accepted(quote) => quote,
        RfqResponse::Rejected(rejection) => {
            panic!(
                "live gateway-backed cbBTC RFQ was rejected: {:?}",
                rejection.reason
            );
        }
    };
    assert_eq!(
        quote.execution_path,
        firmament::types::ExecutionPath::GatewayToDex,
        "expected GatewayToDex path; working_custody:USDC was drained"
    );
    assert_eq!(quote.output_amount.asset.as_str(), "cbBTC");

    let trade = live
        .orchestrator
        .accept_quote(quote.quote_id)
        .await
        .expect("accept live gateway-backed cbBTC quote");

    assert_eq!(trade.settlement_status, SettlementStatus::Redeemed);
    assert_eq!(
        trade.execution_path,
        firmament::types::ExecutionPath::GatewayToDex
    );
    assert_eq!(trade.output_amount.asset.as_str(), "cbBTC");
    // 4 HTLC signatures (taker lock, maker lock, taker redeem, maker redeem)
    // + Gateway burn + Gateway mint + Jupiter swap = 7.
    assert!(
        trade.tx_signatures.len() >= 7,
        "expected >=7 signatures (4 HTLC + 3 Gateway/Jupiter), got {}",
        trade.tx_signatures.len()
    );

    let kinds: Vec<TradeSignatureKind> = trade.tx_signature_kinds.iter().map(|s| s.kind).collect();
    assert!(
        kinds.contains(&TradeSignatureKind::GatewayBurn),
        "expected GatewayBurn in signature kinds: {kinds:?}",
    );
    assert!(
        kinds.contains(&TradeSignatureKind::GatewayMint),
        "expected GatewayMint in signature kinds: {kinds:?}",
    );
    assert!(
        kinds.contains(&TradeSignatureKind::JupiterSwap),
        "expected JupiterSwap (cbBTC output is non-USDC): {kinds:?}",
    );
    assert!(
        kinds.contains(&TradeSignatureKind::TakerLock),
        "expected TakerLock: {kinds:?}",
    );
    assert!(
        kinds.contains(&TradeSignatureKind::MakerLock),
        "expected MakerLock: {kinds:?}",
    );

    let summary = live.orchestrator.ledger_summary().expect("ledger summary");
    assert!(summary.balanced);
}

fn live_cbbtc_rfq_usdc_amount_raw() -> u64 {
    std::env::var("LIVE_CBBTC_RFQ_USDC_RAW")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(DEFAULT_LIVE_CBBTC_RFQ_USDC_RAW)
}

fn temp_live_db_path() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let mut path: PathBuf = std::env::temp_dir();
    path.push(format!("colosseum-live-cbbtc-rfq-{nanos}.sqlite"));
    path.to_string_lossy().into_owned()
}

fn env_required(name: &str) -> AppResult<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::config(format!("{name} is required")))
}
