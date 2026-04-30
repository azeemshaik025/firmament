use std::path::Path;

use rust_decimal::Decimal;
use tbd_rfq_maker_runtime::AppConfig;

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
    assert_eq!(config.assets.supported.len(), 3);
    assert_eq!(config.assets.pairs.len(), 6);
    assert_eq!(config.solana.cluster, "mainnet-beta");
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
        config.assets.policy.cbbtc_exception_notional_usd,
        Decimal::from(5)
    );
    assert_eq!(config.risk.max_quote_notional_usd, Decimal::from(2));
    assert_eq!(config.risk.max_cbbtc_notional_usd, Decimal::from(5));
    assert_eq!(config.gateway.max_refill_notional_usd, Decimal::from(2));
    assert_eq!(config.gateway.max_refill_fee_raw.as_u64(), 250_000);
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
fn config_env_names_match_runbook_contract() {
    let config = AppConfig::default();

    assert_eq!(config.solana.rpc_url_env, "SOLANA_RPC_URL");
    assert_eq!(config.wallets.maker.private_key_env, "MAKER_PRIVATE_KEY");
    assert_eq!(config.wallets.maker.keypair_path_env, "MAKER_KEYPAIR_PATH");
    assert_eq!(config.wallets.maker.keypair_json_env, "MAKER_KEYPAIR_JSON");
    assert_eq!(config.wallets.taker.private_key_env, "TAKER_PRIVATE_KEY");
    assert_eq!(config.wallets.taker.keypair_path_env, "TAKER_KEYPAIR_PATH");
    assert_eq!(config.wallets.taker.keypair_json_env, "TAKER_KEYPAIR_JSON");
    assert_eq!(config.jupiter.api_key_env, "JUPITER_API_KEY");
    assert_eq!(
        config.gateway.solana_address_env,
        "CIRCLE_GATEWAY_SOLANA_ADDRESS"
    );
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
fn config_allows_protocol_workers_flag_but_live_guard_blocks_placeholder_cbbtc() {
    let mut config = AppConfig::default();
    config.runtime.enable_protocol_workers = true;

    config
        .validate()
        .expect("base validation allows protocol workers");
    let error = config
        .validate_protocol_workers_ready()
        .expect_err("live guard rejects unverified cbBTC mint");

    assert!(error.to_string().contains("verified cbBTC Solana mint"));
}
