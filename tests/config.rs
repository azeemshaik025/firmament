use std::path::Path;

use config::FileFormat;
use firmament::{
    AppConfig,
    config::{PairConfig, ProtocolWorkerScope},
    types::{AssetId, MintAddress, WalletRole},
};
use rust_decimal::Decimal;

fn example_config() -> AppConfig {
    let settings = ::config::Config::builder()
        .add_source(::config::File::from(Path::new("config.example.toml")))
        .build()
        .expect("build config.example.toml settings");

    settings
        .try_deserialize::<AppConfig>()
        .expect("deserialize config.example.toml")
}

#[test]
fn config_example_parses_and_validates() {
    let config = example_config();

    config.validate().expect("config.example.toml validates");
    assert_eq!(config.assets.supported.len(), 4);
    assert!(config.assets.blacklisted_pairs.is_empty());
    assert_eq!(config.assets.enabled_pairs().len(), 12);
    assert_eq!(config.solana.cluster, "mainnet-beta");
}

#[test]
fn config_blacklisted_pairs_remove_only_the_matching_direction() {
    let mut config = AppConfig::default();
    config.assets.blacklisted_pairs = vec![PairConfig {
        input: AssetId::from("USDC"),
        output: AssetId::from("SOL"),
    }];

    let pairs = config.assets.enabled_pairs();

    assert_eq!(pairs.len(), 5);
    assert!(
        !pairs
            .iter()
            .any(|pair| pair.input.as_str() == "USDC" && pair.output.as_str() == "SOL")
    );
    assert!(
        pairs
            .iter()
            .any(|pair| pair.input.as_str() == "SOL" && pair.output.as_str() == "USDC")
    );
}

#[test]
fn config_example_preserves_tiny_demo_caps() {
    let config = example_config();

    assert_eq!(
        config.assets.policy.max_action_notional_usd,
        Decimal::from(2)
    );
    assert_eq!(
        config.assets.policy.max_cumulative_automation_notional_usd,
        Decimal::from(15)
    );
    assert_eq!(
        config.assets.policy.non_stable_asset_exception_notional_usd,
        Decimal::from(5)
    );
    assert_eq!(config.risk.max_quote_notional_usd, Decimal::from(2));
    assert_eq!(
        config.risk.max_non_stable_asset_notional_usd,
        Decimal::from(5)
    );
    assert_eq!(config.gateway.max_refill_notional_usd, Decimal::from(2));
    assert_eq!(config.gateway.max_refill_fee_raw.as_u64(), 250_000);

    let sol = config
        .assets
        .supported
        .iter()
        .find(|asset| asset.id.as_str() == "SOL")
        .expect("SOL asset");
    assert_eq!(sol.min_trade_amount, Decimal::new(1, 2));
    assert_eq!(sol.max_trade_amount, Decimal::new(2, 2));

    let cbbtc = config
        .assets
        .supported
        .iter()
        .find(|asset| asset.id.as_str() == "cbBTC")
        .expect("cbBTC asset");
    assert_eq!(cbbtc.min_trade_amount, Decimal::new(1, 5));
    assert_eq!(cbbtc.max_trade_amount, Decimal::new(2, 5));
}

#[test]
fn config_validation_rejects_action_cap_above_cumulative_cap() {
    let mut config = AppConfig::default();
    config.assets.policy.max_action_notional_usd = Decimal::from(16);
    config.assets.policy.max_cumulative_automation_notional_usd = Decimal::from(15);

    let error = config
        .validate()
        .expect_err("action cap above cumulative cap is invalid");

    assert!(
        error
            .to_string()
            .contains("max action notional must not exceed cumulative automation cap")
    );
}

#[test]
fn config_defaults_do_not_model_secret_env_var_names() {
    let config = AppConfig::default();
    let value = serde_json::to_value(&config).expect("serialize default config");

    assert!(value.get("wallets").is_none());
    assert!(value["solana"].get("rpc_url_env").is_none());
    assert!(value["jupiter"].get("api_key_env").is_none());
    assert!(value["gateway"].get("solana_address_env").is_none());
}

#[test]
fn config_rejects_secret_env_var_name_fields() {
    let legacy_config = r#"
[solana]
rpc_url_env = "SOLANA_RPC_URL"

[wallets.maker]
private_key_env = "MAKER_PRIVATE_KEY"
keypair_path_env = "MAKER_KEYPAIR_PATH"
keypair_json_env = "MAKER_KEYPAIR_JSON"

[jupiter]
api_key_env = "JUPITER_API_KEY"

[gateway]
solana_address_env = "CIRCLE_GATEWAY_SOLANA_ADDRESS"
"#;

    let error = ::config::Config::builder()
        .add_source(::config::File::from_str(legacy_config, FileFormat::Toml))
        .build()
        .expect("build legacy config")
        .try_deserialize::<AppConfig>()
        .expect_err("legacy env-var-name fields are no longer accepted");

    let message = error.to_string();
    assert!(
        message.contains("unknown field") || message.contains("unexpected"),
        "{message}"
    );
}

#[test]
fn config_example_omits_secret_env_var_name_fields() {
    let example = std::fs::read_to_string("config.example.toml").expect("read config.example.toml");

    for forbidden in [
        "[wallets.maker]",
        "[wallets.taker]",
        "private_key_env",
        "keypair_path_env",
        "keypair_json_env",
        "rpc_url_env",
        "api_key_env",
        "solana_address_env",
        "cbbtc_exception_notional_usd",
        "max_cbbtc_notional_usd",
    ] {
        assert!(
            !example.contains(forbidden),
            "config.example.toml must not contain {forbidden}"
        );
    }
}

#[test]
fn config_example_marks_cbbtc_mint_for_live_verification() {
    let config = example_config();
    let cbbtc = config
        .assets
        .supported
        .iter()
        .find(|asset| asset.id.as_str() == "cbBTC")
        .expect("cbBTC config row");

    assert_eq!(cbbtc.decimals, 8);
    assert_eq!(
        cbbtc.mint.as_str(),
        "VERIFY_CBBTC_SOLANA_MINT_BEFORE_LIVE_USE"
    );
}

#[test]
fn config_allows_protocol_workers_flag_but_live_guard_blocks_placeholder_asset_mint() {
    let mut config = AppConfig::default();
    config.runtime.enable_protocol_workers = true;

    config
        .validate()
        .expect("base validation allows protocol workers");
    let error = config
        .validate_protocol_workers_ready()
        .expect_err("live guard rejects placeholder asset mint");

    assert!(error.to_string().contains("verified Solana mint"));
}

#[test]
fn protocol_worker_scope_separates_maker_and_demo_wallet_requirements() {
    assert_eq!(
        ProtocolWorkerScope::Maker.required_wallet_roles(),
        &[WalletRole::Maker]
    );
    assert_eq!(
        ProtocolWorkerScope::Demo.required_wallet_roles(),
        &[WalletRole::Maker, WalletRole::Taker]
    );
}

#[test]
fn config_default_reconciliation_uses_design_doc_values() {
    let config = AppConfig::default();
    let recon = &config.reconciliation;
    assert_eq!(recon.interval_seconds, 10);
    assert_eq!(recon.consecutive_ticks_for_adjustment, 3);
    assert!(recon.emit_event_on_skip);
    assert_eq!(recon.dust.get("USDC").copied(), Some(10_000));
    assert_eq!(recon.dust.get("SOL").copied(), Some(100_000));
    assert_eq!(recon.dust.get("cbBTC").copied(), Some(100));
}

#[test]
fn config_example_reconciliation_section_matches_defaults() {
    let config = example_config();
    let recon = &config.reconciliation;
    assert_eq!(recon.interval_seconds, 10);
    assert_eq!(recon.consecutive_ticks_for_adjustment, 3);
    assert!(recon.emit_event_on_skip);
    assert_eq!(recon.dust.get("USDC").copied(), Some(10_000));
    assert_eq!(recon.dust.get("SOL").copied(), Some(100_000));
    assert_eq!(recon.dust.get("cbBTC").copied(), Some(100));
}

#[test]
fn config_without_reconciliation_section_falls_back_to_defaults() {
    // Stripping the [reconciliation] section entirely must keep the worker
    // operable on safe defaults — operators should not need to hand-write
    // the section.
    let toml_without_section = r"
[runtime]
event_capacity = 64
        ";
    let settings = ::config::Config::builder()
        .add_source(::config::File::from_str(
            toml_without_section,
            FileFormat::Toml,
        ))
        .build()
        .expect("build minimal config");
    let config: AppConfig = settings.try_deserialize().expect("deserialize");
    assert_eq!(config.reconciliation.interval_seconds, 10);
    assert_eq!(config.reconciliation.consecutive_ticks_for_adjustment, 3);
    assert!(config.reconciliation.emit_event_on_skip);
    // Default dust map still applies.
    assert_eq!(
        config.reconciliation.dust.get("USDC").copied(),
        Some(10_000)
    );
}

#[test]
fn config_example_enables_eurc_on_the_circle_mint() {
    let config = example_config();
    let eurc = config
        .assets
        .supported
        .iter()
        .find(|asset| asset.id.as_str() == "EURC")
        .expect("EURC row");
    assert!(eurc.enabled);
    assert_eq!(
        eurc.mint.as_str(),
        "HzwqbKZw8HxMN6bF2yFZNrht3c2iXXzpKcFu7uBEDKtr"
    );
    assert_eq!(eurc.decimals, 6);
    let weights: Decimal = config
        .assets
        .supported
        .iter()
        .map(|asset| asset.target_weight)
        .sum();
    assert_eq!(weights, Decimal::ONE);
}

#[test]
fn stablecoins_and_sol_use_the_stable_notional_limit() {
    use firmament::assets::uses_stable_notional_limit;
    use firmament::types::AssetId;

    for (asset, expected) in [
        ("USDC", true),
        ("EURC", true),
        ("SOL", true),
        ("cbBTC", false),
    ] {
        assert_eq!(
            uses_stable_notional_limit(&AssetId::from(asset)),
            expected,
            "{asset}"
        );
    }
}

#[test]
fn live_guard_does_not_require_a_jupiter_api_key() {
    let mut config = AppConfig::default();
    config.runtime.enable_protocol_workers = true;
    config.gateway.enabled = false;
    for asset in &mut config.assets.supported {
        if asset.id.as_str() == "cbBTC" {
            asset.mint = MintAddress::new("cbbtcf3aa214zXHbiAZQwf4122FBYbraNdFqgw4iMij");
        }
    }

    // SAFETY: no other test in this binary reads these variables.
    unsafe {
        std::env::set_var("SOLANA_RPC_URL", "https://solana-rpc.publicnode.com");
        std::env::set_var("MAKER_PRIVATE_KEY", "11111111111111111111111111111111");
        std::env::remove_var("JUPITER_API_KEY");
    }
    let result = config.validate_protocol_workers_ready_for(ProtocolWorkerScope::Maker);
    // SAFETY: same variables as above; no other test reads them.
    unsafe {
        std::env::remove_var("SOLANA_RPC_URL");
        std::env::remove_var("MAKER_PRIVATE_KEY");
    }

    result.expect("Jupiter answers keyless on its public tier, so the key is optional");
}
