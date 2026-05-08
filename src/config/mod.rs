//! Configuration loading and safe startup defaults.

use std::collections::HashMap;
use std::path::Path;

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::adapters::solana::wallets::{SOLANA_RPC_URL_ENV, keypair_source_envs};
use crate::domain::types::{AmountRaw, AssetId, AssetPair, MintAddress, WalletRole};
use crate::error::{AppError, AppResult};

const CONFIG_FILE: &str = "config.toml";

/// Environment variable containing the Jupiter API key.
pub const JUPITER_API_KEY_ENV: &str = "JUPITER_API_KEY";
/// Environment variable containing the approved Circle Gateway Solana address.
pub const CIRCLE_GATEWAY_SOLANA_ADDRESS_ENV: &str = "CIRCLE_GATEWAY_SOLANA_ADDRESS";
/// Environment variable containing username-to-Argon2id password hashes.
pub const FIRMAMENT_ADMIN_PASSWORD_HASHES_ENV: &str = "FIRMAMENT_ADMIN_PASSWORD_HASHES";
/// Environment variable containing the HMAC session-cookie signing secret.
pub const FIRMAMENT_ADMIN_SESSION_SECRET_ENV: &str = "FIRMAMENT_ADMIN_SESSION_SECRET";

/// Live protocol credential scope for runtime startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolWorkerScope {
    /// Maker runtime: API/workers with maker-owned inventory only.
    Maker,
    /// Legacy local two-wallet scripted taker flow.
    Demo,
}

impl ProtocolWorkerScope {
    /// Return the wallet roles required for this live startup scope.
    #[must_use]
    pub const fn required_wallet_roles(self) -> &'static [WalletRole] {
        match self {
            Self::Maker => &[WalletRole::Maker],
            Self::Demo => &[WalletRole::Maker, WalletRole::Taker],
        }
    }
}

/// Complete application configuration after file/default merging.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    /// HTTP API bind settings.
    pub http: HttpConfig,
    /// Solana RPC and cluster settings.
    pub solana: SolanaConfig,
    /// Asset universe and quote policy.
    pub assets: AssetsConfig,
    /// Risk caps and stale-data limits.
    pub risk: RiskConfig,
    /// Jupiter Swap API settings.
    pub jupiter: JupiterConfig,
    /// Circle Gateway settings.
    pub gateway: GatewayConfig,
    /// Admin login and session settings.
    pub admin: AdminConfig,
    /// Runtime startup and worker settings.
    pub runtime: RuntimeConfig,
    /// Always-on reconciliation worker settings.
    pub reconciliation: ReconciliationConfig,
}

impl AppConfig {
    /// Load `.env`, then load `config.toml` when present, otherwise use safe
    /// startup defaults.
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

    /// Validate config values that would otherwise fail later at runtime
    /// boundaries.
    ///
    /// # Errors
    ///
    /// Returns a validation error when a local policy value is empty,
    /// contradictory, or unsafe for startup.
    pub fn validate(&self) -> AppResult<()> {
        if self.http.bind_address.trim().is_empty() {
            return Err(AppError::validation("http.bind_address must not be empty"));
        }

        if self.assets.supported.is_empty() {
            return Err(AppError::validation(
                "assets.supported must include at least one asset",
            ));
        }

        for asset in &self.assets.supported {
            asset.validate()?;
        }

        if self.assets.enabled_pairs().is_empty() {
            return Err(AppError::validation(
                "assets must leave at least one enabled directional pair",
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

        if self.admin.allowed_users.is_empty() {
            return Err(AppError::validation(
                "admin.allowed_users must include at least one username",
            ));
        }

        if self.admin.session_ttl_seconds == 0 {
            return Err(AppError::validation(
                "admin.session_ttl_seconds must be greater than zero",
            ));
        }

        Ok(())
    }

    /// Validate that live protocol workers can start without silently falling
    /// back to fake or placeholder adapters.
    ///
    /// # Errors
    ///
    /// Returns a non-secret config error when required live env references are
    /// missing or when live-only guards, such as placeholder mint verification, fail.
    pub fn validate_protocol_workers_ready(&self) -> AppResult<()> {
        self.validate_protocol_workers_ready_for(ProtocolWorkerScope::Maker)
    }

    /// Validate that live protocol workers can start for a specific runtime
    /// scope.
    ///
    /// # Errors
    ///
    /// Returns a non-secret config error when required live env references are
    /// missing or when live-only guards, such as placeholder mint verification, fail.
    pub fn validate_protocol_workers_ready_for(&self, scope: ProtocolWorkerScope) -> AppResult<()> {
        self.validate()?;

        if !self.runtime.enable_protocol_workers {
            return Ok(());
        }

        if let Some(asset) = self.assets.supported.iter().find(|asset| {
            asset
                .mint
                .as_str()
                .trim()
                .to_ascii_uppercase()
                .starts_with("VERIFY_")
        }) {
            return Err(AppError::validation(format!(
                "live protocol workers require a verified Solana mint for {}; replace placeholder mint {} before funding",
                asset.id, asset.mint
            )));
        }

        require_env(SOLANA_RPC_URL_ENV)?;
        for role in scope.required_wallet_roles() {
            require_wallet_env(*role)?;
        }

        if self.jupiter.enabled {
            require_env(JUPITER_API_KEY_ENV)?;
        }

        if self.gateway.enabled {
            require_env(CIRCLE_GATEWAY_SOLANA_ADDRESS_ENV)?;
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
#[serde(deny_unknown_fields)]
pub struct HttpConfig {
    /// Address for future Axum bind.
    pub bind_address: String,
    /// Port for future Axum bind.
    pub port: u16,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            bind_address: "127.0.0.1".to_owned(),
            port: 5050,
        }
    }
}

/// Solana RPC settings. Secret values are loaded from fixed environment names.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct SolanaConfig {
    /// Solana cluster label.
    pub cluster: String,
    /// Commitment level label.
    pub commitment: String,
}

impl Default for SolanaConfig {
    fn default() -> Self {
        Self {
            cluster: "mainnet-beta".to_owned(),
            commitment: "confirmed".to_owned(),
        }
    }
}

/// Asset universe, directional pair overrides, and quote policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct AssetsConfig {
    /// Supported asset metadata.
    pub supported: Vec<AssetConfig>,
    /// Directional pairs excluded from the default enabled-asset cross-product.
    pub blacklisted_pairs: Vec<PairConfig>,
    /// Non-secret quote and inventory policy.
    pub policy: PolicyConfig,
}

impl Default for AssetsConfig {
    fn default() -> Self {
        Self {
            supported: default_assets(),
            blacklisted_pairs: Vec::new(),
            policy: PolicyConfig::default(),
        }
    }
}

impl AssetsConfig {
    /// Return enabled directional pairs as every enabled asset to every other
    /// enabled asset, minus directional blacklist overrides.
    #[must_use]
    pub fn enabled_pairs(&self) -> Vec<AssetPair> {
        self.supported
            .iter()
            .filter(|asset| asset.enabled)
            .flat_map(|input| {
                self.supported
                    .iter()
                    .filter(|output| output.enabled && output.id != input.id)
                    .filter(|output| {
                        !self
                            .blacklisted_pairs
                            .iter()
                            .any(|pair| pair.input == input.id && pair.output == output.id)
                    })
                    .map(|output| AssetPair::new(input.id.clone(), output.id.clone()))
            })
            .collect()
    }
}

/// Asset metadata and inventory policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[serde(deny_unknown_fields)]
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
    /// Per-asset minimum trade notional in estimated USD. Both legs of a trade
    /// must clear their own asset's minimum.
    pub min_trade_notional_usd: Decimal,
    /// Per-asset maximum trade notional in estimated USD. Both legs of a trade
    /// must stay below their own asset's maximum.
    pub max_trade_notional_usd: Decimal,
}

/// Sane ceiling for per-asset notional caps; guards against u64 overflow.
const ASSET_NOTIONAL_USD_CEILING: u64 = 1_000_000;

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

        if self.min_trade_notional_usd <= Decimal::ZERO {
            return Err(AppError::validation(format!(
                "asset {} min_trade_notional_usd must be greater than zero",
                self.id
            )));
        }

        if self.max_trade_notional_usd < self.min_trade_notional_usd {
            return Err(AppError::validation(format!(
                "asset {} max_trade_notional_usd must be at least min_trade_notional_usd",
                self.id
            )));
        }

        if self.max_trade_notional_usd > Decimal::from(ASSET_NOTIONAL_USD_CEILING) {
            return Err(AppError::validation(format!(
                "asset {} max_trade_notional_usd exceeds ceiling {ASSET_NOTIONAL_USD_CEILING}",
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
            min_trade_notional_usd: Decimal::ONE,
            max_trade_notional_usd: Decimal::new(2, 0),
        }
    }
}

/// Directional pair override config.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct PairConfig {
    /// Input asset.
    pub input: AssetId,
    /// Output asset.
    pub output: AssetId,
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
        }
    }
}

/// Non-secret action caps and quote defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct PolicyConfig {
    /// Default per-action cap in estimated USD.
    pub max_action_notional_usd: Decimal,
    /// Cumulative automation cap in estimated USD.
    pub max_cumulative_automation_notional_usd: Decimal,
    /// One-off route-minimum exception cap for non-native, non-stable assets.
    pub non_stable_asset_exception_notional_usd: Decimal,
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
            non_stable_asset_exception_notional_usd: Decimal::new(5, 0),
            default_quote_expiry_seconds: 45,
            base_spread_bps: 35,
        }
    }
}

/// Risk limit settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct RiskConfig {
    /// Maximum quote notional in estimated USD.
    pub max_quote_notional_usd: Decimal,
    /// Maximum accepted trade notional in estimated USD.
    pub max_trade_notional_usd: Decimal,
    /// Maximum daily notional in estimated USD.
    pub max_daily_notional_usd: Decimal,
    /// Maximum exposure notional for non-native, non-stable assets.
    pub max_non_stable_asset_notional_usd: Decimal,
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
            max_non_stable_asset_notional_usd: Decimal::new(5, 0),
            max_inventory_drift_bps: 1_500,
            max_price_staleness_seconds: 20,
            require_taker_allowlist: false,
        }
    }
}

/// Jupiter Swap API V2 config.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct JupiterConfig {
    /// Whether Jupiter-backed actions are enabled for future workers.
    pub enabled: bool,
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
            base_url: "https://lite-api.jup.ag".to_owned(),
            max_slippage_bps: 50,
            quote_timeout_millis: 2_000,
        }
    }
}

/// Circle Gateway non-secret policy config.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct GatewayConfig {
    /// Whether Gateway-backed actions are enabled for future workers.
    pub enabled: bool,
    /// Refill threshold in raw USDC units.
    pub usdc_refill_threshold_raw: AmountRaw,
    /// Refill target in raw USDC units.
    pub usdc_refill_target_raw: AmountRaw,
    /// Maximum refill notional in estimated USD.
    pub max_refill_notional_usd: Decimal,
    /// Maximum Circle Gateway transfer fee in raw USDC units.
    pub max_refill_fee_raw: AmountRaw,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            usdc_refill_threshold_raw: AmountRaw::new(3_000_000),
            usdc_refill_target_raw: AmountRaw::new(10_000_000),
            max_refill_notional_usd: Decimal::new(2, 0),
            max_refill_fee_raw: AmountRaw::new(250_000),
        }
    }
}

/// Admin login/session config. Secrets are supplied through env vars.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct AdminConfig {
    /// Usernames that are allowed to authenticate as demo operators.
    pub allowed_users: Vec<String>,
    /// Session cookie lifetime.
    pub session_ttl_seconds: u64,
    /// Whether the session cookie should include the Secure attribute.
    pub cookie_secure: bool,
}

impl Default for AdminConfig {
    fn default() -> Self {
        Self {
            allowed_users: vec!["admin".to_owned()],
            session_ttl_seconds: 8 * 60 * 60,
            cookie_secure: false,
        }
    }
}

/// Runtime startup controls.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct RuntimeConfig {
    /// Maximum recent events held in the in-memory projection.
    pub event_capacity: usize,
    /// Optional startup hold delay, retained under the existing config key.
    pub scaffold_hold_millis: u64,
    /// When true, production startup builds real protocol adapters.
    pub enable_protocol_workers: bool,
    /// Local `SQLite` path used by production ledger and P&L persistence.
    pub database_path: String,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            event_capacity: 256,
            scaffold_hold_millis: 0,
            enable_protocol_workers: false,
            database_path: "runtime.sqlite".to_owned(),
        }
    }
}

/// Always-on reconciliation worker settings.
///
/// Dust thresholds are stored as `u64` because the `config` crate's TOML
/// deserializer does not support `u128`. Solana SPL token mints cap raw
/// amounts at `u64::MAX`, so this is sufficient for every asset Firmament
/// currently quotes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct ReconciliationConfig {
    /// Tick cadence in seconds. Defaults to 10.
    pub interval_seconds: u64,
    /// Number of consecutive in-window observations required to trigger an
    /// adjustment. Defaults to 3.
    pub consecutive_ticks_for_adjustment: u8,
    /// Whether to emit a `Skipped` `ReconciliationEvent` on guard-blocked
    /// adjustments. Defaults to true.
    pub emit_event_on_skip: bool,
    /// Per-asset dust threshold (raw native units). Drifts at or below the
    /// dust threshold are ignored.
    pub dust: HashMap<String, u64>,
}

impl Default for ReconciliationConfig {
    fn default() -> Self {
        let mut dust = HashMap::new();
        dust.insert("USDC".to_owned(), 10_000);
        dust.insert("SOL".to_owned(), 100_000);
        dust.insert("cbBTC".to_owned(), 100);
        Self {
            interval_seconds: 10,
            consecutive_ticks_for_adjustment: 3,
            emit_event_on_skip: true,
            dust,
        }
    }
}

fn require_env(name: &str) -> AppResult<()> {
    if std::env::var(name)
        .ok()
        .is_some_and(|value| !value.trim().is_empty())
    {
        return Ok(());
    }

    Err(AppError::config(format!(
        "live protocol workers require non-empty env var {name}"
    )))
}

fn require_wallet_env(role: WalletRole) -> AppResult<()> {
    let source_envs = keypair_source_envs(role);
    let present_sources = source_envs
        .iter()
        .filter(|name| env_is_present(name))
        .copied()
        .collect::<Vec<_>>();

    match present_sources.len() {
        1 => Ok(()),
        0 => Err(AppError::config(format!(
            "live protocol workers require a keypair source for {:?}: set exactly one of {}",
            role,
            source_envs.join(", ")
        ))),
        _ => Err(AppError::config(format!(
            "live protocol workers require exactly one keypair source for {:?}: set only one of {}; currently set {}",
            role,
            source_envs.join(", "),
            present_sources.join(", ")
        ))),
    }
}

fn env_is_present(name: &str) -> bool {
    !name.trim().is_empty()
        && std::env::var(name)
            .ok()
            .is_some_and(|value| !value.trim().is_empty())
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
            min_trade_notional_usd: Decimal::ONE,
            max_trade_notional_usd: Decimal::new(2, 0),
        },
        AssetConfig {
            id: AssetId::from("cbBTC"),
            symbol: "cbBTC".to_owned(),
            mint: MintAddress::new("VERIFY_CBBTC_SOLANA_MINT_BEFORE_LIVE_USE"),
            decimals: 8,
            enabled: true,
            target_weight: Decimal::new(5, 2),
            quoteable_threshold_raw: AmountRaw::new(1_000),
            min_trade_notional_usd: Decimal::ONE,
            max_trade_notional_usd: Decimal::new(5, 0),
        },
    ]
}
