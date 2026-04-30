//! Configuration loading and safe scaffold defaults.

use std::path::Path;

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};
use crate::types::{AmountRaw, AssetId, AssetPair, MintAddress, WalletRole};

const CONFIG_FILE: &str = "config.toml";

/// Complete application configuration after file/default merging.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    /// HTTP API bind settings.
    pub http: HttpConfig,
    /// Solana RPC and cluster settings.
    pub solana: SolanaConfig,
    /// Demo wallet secret references.
    pub wallets: WalletsConfig,
    /// Asset universe and quote policy.
    pub assets: AssetsConfig,
    /// Risk caps and stale-data limits.
    pub risk: RiskConfig,
    /// Jupiter Swap API settings.
    pub jupiter: JupiterConfig,
    /// Circle Gateway settings.
    pub gateway: GatewayConfig,
    /// Runtime scaffold and worker settings.
    pub runtime: RuntimeConfig,
}

impl AppConfig {
    /// Load `.env`, then load `config.toml` when present, otherwise use safe
    /// scaffold defaults.
    ///
    /// # Errors
    ///
    /// Returns an error when `config.toml` cannot be parsed or config values
    /// fail local validation.
    pub fn load() -> AppResult<Self> {
        let _ = dotenvy::dotenv();

        let config_path = Path::new(CONFIG_FILE);
        let mut app_config = if config_path.exists() {
            ::config::Config::builder()
                .add_source(::config::File::from(config_path).required(false))
                .build()?
                .try_deserialize::<Self>()?
        } else {
            Self::default()
        };

        app_config.normalize();
        app_config.validate()?;
        Ok(app_config)
    }

    /// Validate scaffold config values that would otherwise fail later at
    /// runtime boundaries.
    ///
    /// # Errors
    ///
    /// Returns a validation error when a local policy value is empty,
    /// contradictory, or unsafe for the scaffold.
    pub fn validate(&self) -> AppResult<()> {
        if self.http.bind_address.trim().is_empty() {
            return Err(AppError::validation("http.bind_address must not be empty"));
        }

        if self.solana.rpc_url_env.trim().is_empty() {
            return Err(AppError::validation("solana.rpc_url_env must not be empty"));
        }

        if self.assets.supported.is_empty() {
            return Err(AppError::validation(
                "assets.supported must include at least one asset",
            ));
        }

        for asset in &self.assets.supported {
            asset.validate()?;
        }

        if self.assets.pairs.is_empty() {
            return Err(AppError::validation(
                "assets.pairs must include at least one directional pair",
            ));
        }

        if self.assets.policy.default_quote_expiry_seconds == 0 {
            return Err(AppError::validation(
                "assets.policy.default_quote_expiry_seconds must be greater than zero",
            ));
        }

        if self.assets.policy.max_action_notional_usd
            > self.assets.policy.max_cumulative_automation_notional_usd
        {
            return Err(AppError::validation(
                "max action notional must not exceed cumulative automation cap",
            ));
        }

        if self.risk.max_price_staleness_seconds == 0 {
            return Err(AppError::validation(
                "risk.max_price_staleness_seconds must be greater than zero",
            ));
        }

        if self.runtime.event_capacity == 0 {
            return Err(AppError::validation(
                "runtime.event_capacity must be greater than zero",
            ));
        }

        if self.runtime.enable_protocol_workers {
            return Err(AppError::unsupported(
                "protocol workers are intentionally disabled in the Wave 0 scaffold",
            ));
        }

        Ok(())
    }

    fn normalize(&mut self) {
        if self.runtime.scaffold_hold_millis > 10_000 {
            self.runtime.scaffold_hold_millis = 10_000;
        }
    }
}

/// HTTP API settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HttpConfig {
    /// Address for future Axum bind.
    pub bind_address: String,
    /// Port for future Axum bind.
    pub port: u16,
    /// Environment variable name that stores the operator API token.
    pub operator_api_token_env: String,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            bind_address: "127.0.0.1".to_owned(),
            port: 8080,
            operator_api_token_env: "OPERATOR_API_TOKEN".to_owned(),
        }
    }
}

/// Solana RPC settings. Secret values remain referenced by environment name.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SolanaConfig {
    /// Solana cluster label.
    pub cluster: String,
    /// Environment variable name that stores the RPC URL.
    pub rpc_url_env: String,
    /// Commitment level label.
    pub commitment: String,
}

impl Default for SolanaConfig {
    fn default() -> Self {
        Self {
            cluster: "mainnet-beta".to_owned(),
            rpc_url_env: "SOLANA_RPC_URL".to_owned(),
            commitment: "confirmed".to_owned(),
        }
    }
}

/// Demo wallet config.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WalletsConfig {
    /// Maker/operator wallet secret references.
    pub maker: WalletConfig,
    /// Taker/app wallet secret references.
    pub taker: WalletConfig,
}

impl Default for WalletsConfig {
    fn default() -> Self {
        Self {
            maker: WalletConfig {
                role: WalletRole::Maker,
                keypair_path_env: "MAKER_KEYPAIR_PATH".to_owned(),
                keypair_json_env: "MAKER_KEYPAIR_JSON".to_owned(),
            },
            taker: WalletConfig {
                role: WalletRole::Taker,
                keypair_path_env: "TAKER_KEYPAIR_PATH".to_owned(),
                keypair_json_env: "TAKER_KEYPAIR_JSON".to_owned(),
            },
        }
    }
}

/// Secret references for one wallet role.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WalletConfig {
    /// Role this wallet config applies to.
    pub role: WalletRole,
    /// Environment variable name containing a keypair path.
    pub keypair_path_env: String,
    /// Environment variable name containing keypair JSON.
    pub keypair_json_env: String,
}

impl Default for WalletConfig {
    fn default() -> Self {
        Self {
            role: WalletRole::Maker,
            keypair_path_env: "MAKER_KEYPAIR_PATH".to_owned(),
            keypair_json_env: "MAKER_KEYPAIR_JSON".to_owned(),
        }
    }
}

/// Asset universe, directional pair support, and quote policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AssetsConfig {
    /// Supported asset metadata.
    pub supported: Vec<AssetConfig>,
    /// Directional pair enablement.
    pub pairs: Vec<PairConfig>,
    /// Non-secret quote and inventory policy.
    pub policy: PolicyConfig,
}

impl Default for AssetsConfig {
    fn default() -> Self {
        Self {
            supported: default_assets(),
            pairs: default_pairs(),
            policy: PolicyConfig::default(),
        }
    }
}

/// Asset metadata and inventory policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AssetConfig {
    /// Asset identifier used in API and events.
    pub id: AssetId,
    /// Human-readable ticker.
    pub symbol: String,
    /// Solana mint address or explicit placeholder for unverified assets.
    pub mint: MintAddress,
    /// Native token decimals.
    pub decimals: u8,
    /// Whether this asset can be used by future workers.
    pub enabled: bool,
    /// Target inventory weight from zero to one.
    pub target_weight: Decimal,
    /// Minimum raw working inventory required before quoting this asset.
    pub quoteable_threshold_raw: AmountRaw,
}

impl AssetConfig {
    /// Validate local asset metadata.
    ///
    /// # Errors
    ///
    /// Returns a validation error when required metadata is empty or outside
    /// sensible token policy bounds.
    pub fn validate(&self) -> AppResult<()> {
        if self.id.as_str().trim().is_empty() {
            return Err(AppError::validation("asset id must not be empty"));
        }

        if self.symbol.trim().is_empty() {
            return Err(AppError::validation(format!(
                "asset {} symbol must not be empty",
                self.id
            )));
        }

        if self.mint.as_str().trim().is_empty() {
            return Err(AppError::validation(format!(
                "asset {} mint must not be empty",
                self.id
            )));
        }

        if self.decimals > 18 {
            return Err(AppError::validation(format!(
                "asset {} decimals must be 18 or lower",
                self.id
            )));
        }

        if self.target_weight.is_sign_negative() || self.target_weight > Decimal::ONE {
            return Err(AppError::validation(format!(
                "asset {} target_weight must be between 0 and 1",
                self.id
            )));
        }

        Ok(())
    }
}

impl Default for AssetConfig {
    fn default() -> Self {
        Self {
            id: AssetId::from("USDC"),
            symbol: "USDC".to_owned(),
            mint: MintAddress::new("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"),
            decimals: 6,
            enabled: true,
            target_weight: Decimal::new(60, 2),
            quoteable_threshold_raw: AmountRaw::new(2_000_000),
        }
    }
}

/// Directional pair support config.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PairConfig {
    /// Input asset.
    pub input: AssetId,
    /// Output asset.
    pub output: AssetId,
    /// Whether this pair can be quoted.
    pub enabled: bool,
}

impl PairConfig {
    /// Return the domain pair represented by this config row.
    #[must_use]
    pub fn pair(&self) -> AssetPair {
        AssetPair::new(self.input.clone(), self.output.clone())
    }
}

impl Default for PairConfig {
    fn default() -> Self {
        Self {
            input: AssetId::from("USDC"),
            output: AssetId::from("SOL"),
            enabled: true,
        }
    }
}

/// Non-secret action caps and quote defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PolicyConfig {
    /// Default per-action cap in estimated USD.
    pub max_action_notional_usd: Decimal,
    /// Cumulative automation cap in estimated USD.
    pub max_cumulative_automation_notional_usd: Decimal,
    /// cbBTC demo exception cap in estimated USD.
    pub cbbtc_exception_notional_usd: Decimal,
    /// Default quote expiry.
    pub default_quote_expiry_seconds: u64,
    /// Base maker spread in basis points.
    pub base_spread_bps: u16,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            max_action_notional_usd: Decimal::new(2, 0),
            max_cumulative_automation_notional_usd: Decimal::new(15, 0),
            cbbtc_exception_notional_usd: Decimal::new(5, 0),
            default_quote_expiry_seconds: 45,
            base_spread_bps: 35,
        }
    }
}

/// Risk limit settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RiskConfig {
    /// Maximum quote notional in estimated USD.
    pub max_quote_notional_usd: Decimal,
    /// Maximum accepted trade notional in estimated USD.
    pub max_trade_notional_usd: Decimal,
    /// Maximum daily notional in estimated USD.
    pub max_daily_notional_usd: Decimal,
    /// cbBTC-specific exception cap.
    pub max_cbbtc_notional_usd: Decimal,
    /// Maximum inventory drift before rebalancing is needed.
    pub max_inventory_drift_bps: u16,
    /// Maximum allowed reference price age.
    pub max_price_staleness_seconds: u64,
    /// Whether taker allowlist enforcement is active.
    pub require_taker_allowlist: bool,
}

impl Default for RiskConfig {
    fn default() -> Self {
        Self {
            max_quote_notional_usd: Decimal::new(2, 0),
            max_trade_notional_usd: Decimal::new(2, 0),
            max_daily_notional_usd: Decimal::new(15, 0),
            max_cbbtc_notional_usd: Decimal::new(5, 0),
            max_inventory_drift_bps: 1_500,
            max_price_staleness_seconds: 20,
            require_taker_allowlist: false,
        }
    }
}

/// Jupiter Swap API V2 config.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct JupiterConfig {
    /// Whether Jupiter-backed actions are enabled for future workers.
    pub enabled: bool,
    /// Environment variable name containing the Jupiter API key.
    pub api_key_env: String,
    /// Base URL for Jupiter Swap API V2.
    pub base_url: String,
    /// Maximum slippage cap in basis points.
    pub max_slippage_bps: u16,
    /// Quote timeout for future HTTP calls.
    pub quote_timeout_millis: u64,
}

impl Default for JupiterConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            api_key_env: "JUPITER_API_KEY".to_owned(),
            base_url: "https://lite-api.jup.ag".to_owned(),
            max_slippage_bps: 50,
            quote_timeout_millis: 2_000,
        }
    }
}

/// Circle Gateway config. Values are environment variable names, not secrets.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GatewayConfig {
    /// Whether Gateway-backed actions are enabled for future workers.
    pub enabled: bool,
    /// Environment variable name containing a Circle API key.
    pub api_key_env: String,
    /// Environment variable name containing the Circle entity secret.
    pub entity_secret_env: String,
    /// Environment variable name containing the Gateway wallet ID.
    pub wallet_id_env: String,
    /// Environment variable name containing the Gateway Solana address.
    pub solana_address_env: String,
    /// Refill threshold in raw USDC units.
    pub usdc_refill_threshold_raw: AmountRaw,
    /// Refill target in raw USDC units.
    pub usdc_refill_target_raw: AmountRaw,
    /// Maximum refill notional in estimated USD.
    pub max_refill_notional_usd: Decimal,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            api_key_env: "CIRCLE_API_KEY".to_owned(),
            entity_secret_env: "CIRCLE_ENTITY_SECRET".to_owned(),
            wallet_id_env: "CIRCLE_GATEWAY_WALLET_ID".to_owned(),
            solana_address_env: "CIRCLE_GATEWAY_SOLANA_ADDRESS".to_owned(),
            usdc_refill_threshold_raw: AmountRaw::new(3_000_000),
            usdc_refill_target_raw: AmountRaw::new(10_000_000),
            max_refill_notional_usd: Decimal::new(2, 0),
        }
    }
}

/// Runtime scaffold controls.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RuntimeConfig {
    /// Maximum recent events held in the in-memory projection.
    pub event_capacity: usize,
    /// Optional delay before scaffold process exits.
    pub scaffold_hold_millis: u64,
    /// Future guard for live workers. Wave 0 rejects true.
    pub enable_protocol_workers: bool,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            event_capacity: 256,
            scaffold_hold_millis: 0,
            enable_protocol_workers: false,
        }
    }
}

fn default_assets() -> Vec<AssetConfig> {
    vec![
        AssetConfig::default(),
        AssetConfig {
            id: AssetId::from("SOL"),
            symbol: "SOL".to_owned(),
            mint: MintAddress::new("So11111111111111111111111111111111111111112"),
            decimals: 9,
            enabled: true,
            target_weight: Decimal::new(35, 2),
            quoteable_threshold_raw: AmountRaw::new(10_000_000),
        },
        AssetConfig {
            id: AssetId::from("cbBTC"),
            symbol: "cbBTC".to_owned(),
            mint: MintAddress::new("VERIFY_CBBTC_SOLANA_MINT_BEFORE_LIVE_USE"),
            decimals: 8,
            enabled: true,
            target_weight: Decimal::new(5, 2),
            quoteable_threshold_raw: AmountRaw::new(1_000),
        },
    ]
}

fn default_pairs() -> Vec<PairConfig> {
    let usdc = AssetId::from("USDC");
    let sol = AssetId::from("SOL");
    let cbbtc = AssetId::from("cbBTC");

    vec![
        PairConfig {
            input: usdc.clone(),
            output: sol.clone(),
            enabled: true,
        },
        PairConfig {
            input: sol.clone(),
            output: usdc.clone(),
            enabled: true,
        },
        PairConfig {
            input: usdc.clone(),
            output: cbbtc.clone(),
            enabled: true,
        },
        PairConfig {
            input: cbbtc.clone(),
            output: usdc,
            enabled: true,
        },
        PairConfig {
            input: sol.clone(),
            output: cbbtc.clone(),
            enabled: true,
        },
        PairConfig {
            input: cbbtc,
            output: sol,
            enabled: true,
        },
    ]
}
