use std::fs;

use firmament::assets::{AssetError, AssetRegistry, CBBTC_MINT, USDC_MINT};
use firmament::config::SolanaConfig;
use firmament::solana_client::{LEGACY_TOKEN_PROGRAM_ID, SolanaClient};
use firmament::types::{AmountRaw, AssetId, AssetPair, WalletRole};
use firmament::wallets::{DemoWallets, LoadedWallet, SOLANA_RPC_URL_ENV, keypair_source_envs};
use rust_decimal::Decimal;
use solana_sdk::signature::{Keypair, Signer};

fn keypair_json(keypair: &Keypair) -> String {
    serde_json::to_string(&keypair.to_bytes().to_vec()).expect("serialize keypair")
}

#[test]
fn assets_registry_contains_required_assets() {
    let registry = AssetRegistry::default();

    let sol = registry
        .require_asset(&AssetId::from("SOL"))
        .expect("SOL asset");
    assert!(sol.is_native_sol());
    assert_eq!(sol.decimals, 9);
    assert_eq!(sol.mint(), None);

    let usdc = registry
        .require_asset(&AssetId::from("USDC"))
        .expect("USDC asset");
    assert!(usdc.is_spl_token());
    assert_eq!(usdc.decimals, 6);
    assert_eq!(usdc.mint().expect("USDC mint").as_str(), USDC_MINT);

    let cbbtc = registry
        .require_asset(&AssetId::from("cbBTC"))
        .expect("cbBTC asset");
    assert!(cbbtc.is_spl_token());
    assert_eq!(cbbtc.decimals, 8);
    assert_eq!(cbbtc.mint().expect("cbBTC mint").as_str(), CBBTC_MINT);
}

#[test]
fn assets_supported_pairs_are_directional_and_complete() {
    let registry = AssetRegistry::default();
    let usdc = AssetId::from("USDC");
    let sol = AssetId::from("SOL");
    let cbbtc = AssetId::from("cbBTC");

    for pair in [
        AssetPair::new(usdc.clone(), sol.clone()),
        AssetPair::new(sol.clone(), usdc.clone()),
        AssetPair::new(usdc.clone(), cbbtc.clone()),
        AssetPair::new(cbbtc.clone(), usdc),
        AssetPair::new(sol.clone(), cbbtc.clone()),
        AssetPair::new(cbbtc, sol.clone()),
    ] {
        assert!(registry.validate_pair(&pair).is_ok(), "{pair:?}");
    }

    let same_asset = AssetPair::new(sol.clone(), sol);
    assert!(matches!(
        registry.validate_pair(&same_asset),
        Err(AssetError::UnsupportedPair { .. })
    ));

    let unsupported = AssetPair::new(AssetId::from("USDC"), AssetId::from("BONK"));
    assert!(matches!(
        registry.validate_pair(&unsupported),
        Err(AssetError::UnsupportedAsset(_))
    ));
}

#[test]
fn assets_amount_conversion_uses_decimal_math_and_rejects_precision_loss() {
    let registry = AssetRegistry::default();

    let usdc = registry
        .raw_to_display(&AssetId::from("USDC"), AmountRaw::new(1_234_567))
        .expect("raw to display");
    assert_eq!(usdc, Decimal::new(1_234_567, 6));

    let sol = registry
        .raw_to_display(&AssetId::from("SOL"), AmountRaw::new(1_500_000_000))
        .expect("raw to display");
    assert_eq!(sol, Decimal::new(15, 1));

    let exact = registry
        .display_to_raw(&AssetId::from("cbBTC"), Decimal::new(12_345_678, 8))
        .expect("exact raw conversion");
    assert_eq!(exact, AmountRaw::new(12_345_678));

    let too_precise = registry.display_to_raw(&AssetId::from("USDC"), Decimal::new(1, 7));
    assert!(matches!(too_precise, Err(AssetError::PrecisionLoss { .. })));
}

#[test]
fn assets_quoteable_inventory_excludes_protected_sol_gas_buffer() {
    let registry = AssetRegistry::default();

    let quoteable_sol = registry
        .quoteable_inventory(
            &AssetId::from("SOL"),
            AmountRaw::new(1_500_000_000),
            AmountRaw::new(500_000_000),
        )
        .expect("quoteable SOL");
    assert_eq!(quoteable_sol, AmountRaw::new(1_000_000_000));

    let depleted_sol = registry
        .quoteable_inventory(
            &AssetId::from("SOL"),
            AmountRaw::new(250_000_000),
            AmountRaw::new(500_000_000),
        )
        .expect("depleted SOL");
    assert_eq!(depleted_sol, AmountRaw::new(0));

    let quoteable_usdc = registry
        .quoteable_inventory(
            &AssetId::from("USDC"),
            AmountRaw::new(2_000_000),
            AmountRaw::new(500_000_000),
        )
        .expect("quoteable USDC");
    assert_eq!(quoteable_usdc, AmountRaw::new(2_000_000));
}

#[test]
fn wallets_load_keypair_from_json_without_exposing_secret_in_debug() {
    let expected = Keypair::new();
    let wallet = LoadedWallet::from_json(WalletRole::Maker, &keypair_json(&expected))
        .expect("load keypair JSON");

    assert_eq!(wallet.role(), WalletRole::Maker);
    assert_eq!(wallet.pubkey(), expected.pubkey());

    let debug = format!("{wallet:?}");
    assert!(debug.contains("Maker"));
    assert!(debug.contains(&expected.pubkey().to_string()));
    assert!(!debug.contains(&keypair_json(&expected)));
}

#[test]
fn wallets_load_keypair_from_path() {
    let expected = Keypair::new();
    let path = std::env::temp_dir().join(format!("rfq-maker-wallet-{}.json", uuid::Uuid::now_v7()));
    fs::write(&path, keypair_json(&expected)).expect("write temp keypair");

    let wallet = LoadedWallet::from_path(WalletRole::Taker, &path).expect("load keypair path");
    fs::remove_file(&path).expect("remove temp keypair");

    assert_eq!(wallet.role(), WalletRole::Taker);
    assert_eq!(wallet.pubkey(), expected.pubkey());
}

#[test]
fn wallets_load_keypair_from_base58_private_key() {
    let expected = Keypair::new();
    let wallet = LoadedWallet::from_private_key_base58(
        WalletRole::Maker,
        &expected.to_base58_string(),
        "MAKER_PRIVATE_KEY",
    )
    .expect("load base58 private key");

    assert_eq!(wallet.role(), WalletRole::Maker);
    assert_eq!(wallet.pubkey(), expected.pubkey());
}

#[test]
fn wallets_env_loader_uses_fixed_secret_references() {
    assert_eq!(
        keypair_source_envs(WalletRole::Maker),
        [
            "MAKER_PRIVATE_KEY",
            "MAKER_KEYPAIR_PATH",
            "MAKER_KEYPAIR_JSON"
        ]
    );
    assert_eq!(
        keypair_source_envs(WalletRole::Taker),
        [
            "TAKER_PRIVATE_KEY",
            "TAKER_KEYPAIR_PATH",
            "TAKER_KEYPAIR_JSON"
        ]
    );
}

#[test]
fn solana_client_derives_legacy_token_ata() {
    let keypair = Keypair::new_from_array([8; 32]);
    let wallet = LoadedWallet::from_json(WalletRole::Maker, &keypair_json(&keypair))
        .expect("load static keypair");
    let registry = AssetRegistry::default();
    let usdc = registry
        .require_asset(&AssetId::from("USDC"))
        .expect("USDC asset");

    let ata = SolanaClient::derive_associated_token_address(
        &wallet.pubkey(),
        &usdc.mint_pubkey().expect("USDC mint pubkey"),
        &LEGACY_TOKEN_PROGRAM_ID,
    );

    assert_ne!(ata, wallet.pubkey());
    assert_ne!(ata, usdc.mint_pubkey().expect("USDC mint pubkey"));
}

#[test]
fn wallets_live_loading_skips_without_env() {
    if std::env::var("RUN_LIVE_SOLANA_TESTS").ok().as_deref() != Some("1") {
        eprintln!("skipping live wallet loading; RUN_LIVE_SOLANA_TESTS=1 is not set");
        return;
    }

    let maker_secret_configured = keypair_source_envs(WalletRole::Maker)
        .into_iter()
        .any(|name| std::env::var(name).is_ok());
    let taker_secret_configured = keypair_source_envs(WalletRole::Taker)
        .into_iter()
        .any(|name| std::env::var(name).is_ok());

    if !maker_secret_configured || !taker_secret_configured {
        eprintln!("skipping live wallet loading; maker/taker keypair env vars are not both set");
        return;
    }

    let wallets = DemoWallets::from_env().expect("load live demo wallets");
    assert_eq!(wallets.maker.role(), WalletRole::Maker);
    assert_eq!(wallets.taker.role(), WalletRole::Taker);
}

#[tokio::test]
async fn solana_client_live_balance_and_ata_reads_skip_without_env() {
    let Some((client, wallet)) = live_client_and_wallet() else {
        return;
    };

    let registry = AssetRegistry::default();
    let lamports = client
        .native_sol_balance(&wallet.pubkey())
        .await
        .expect("read live SOL balance");
    let usdc = registry
        .require_asset(&AssetId::from("USDC"))
        .expect("USDC asset");
    let ata = client
        .associated_token_address(&wallet.pubkey(), usdc)
        .await
        .expect("derive live USDC ATA");
    let exists = client
        .associated_token_account_exists(&ata)
        .await
        .expect("check live ATA existence");

    let _ = lamports;
    assert!(!ata.to_string().is_empty());
    let _ = exists;
}

fn live_client_and_wallet() -> Option<(SolanaClient, LoadedWallet)> {
    if std::env::var("RUN_LIVE_SOLANA_TESTS").ok().as_deref() != Some("1") {
        eprintln!("skipping live Solana RPC test; RUN_LIVE_SOLANA_TESTS=1 is not set");
        return None;
    }

    let solana_config = SolanaConfig::default();
    let rpc_url = match std::env::var(SOLANA_RPC_URL_ENV) {
        Ok(value) if !value.trim().is_empty() => value,
        _ => {
            eprintln!("skipping live Solana RPC test; SOLANA_RPC_URL is not set");
            return None;
        }
    };

    let has_wallet = keypair_source_envs(WalletRole::Maker)
        .into_iter()
        .any(|name| std::env::var(name).is_ok());
    if !has_wallet {
        eprintln!("skipping live Solana RPC test; maker keypair env vars are not set");
        return None;
    }

    let wallet = LoadedWallet::from_maker_env().expect("load live maker wallet");
    let client = SolanaClient::new(rpc_url, solana_config.commitment).expect("build Solana client");
    Some((client, wallet))
}
