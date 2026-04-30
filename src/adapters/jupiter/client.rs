//! Jupiter Swap API V2 client.

use std::collections::{HashMap, HashSet};
use std::env;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use reqwest::{Client, RequestBuilder};
use rust_decimal::Decimal;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_commitment_config::CommitmentConfig;
use solana_sdk::signature::{Keypair, Signature, Signer};
use time::OffsetDateTime;
use tracing::debug;

use crate::adapters::solana::wallets::LoadedWallet;
use crate::config::{AppConfig, AssetConfig, PairConfig, WalletConfig};
use crate::domain::types::{
    AmountRaw, AssetId, AssetPair, MintAddress, ReferencePrice, SwapQuote, SwapReceipt,
    SwapRequest, TokenAmount, TxSignature, WalletAddress, WalletRole,
};
use crate::error::AppError;
use crate::ports::{PriceProvider, SupportsPair, SwapExecutor};

use super::models::{
    JupiterBuildResponse, JupiterExecuteRequest, JupiterExecuteResponse, JupiterOrderResponse,
    ensure_execute_succeeded, ensure_order_has_transaction, ensure_order_is_usable,
    jupiter_http_error, parse_build_response, parse_execute_response, parse_optional_timestamp,
    parse_order_response, parse_raw_amount,
};
use super::signing::{
    PreparedJupiterOrder, SignedJupiterTransaction, sign_preserving_jupiter_message,
};

/// Stable service label used when mapping Jupiter failures into [`AppError`].
pub const JUPITER_SERVICE: &str = "jupiter";

/// Native SOL mint address expected by Jupiter routes.
pub const JUPITER_NATIVE_SOL_MINT: &str = "So11111111111111111111111111111111111111112";

/// USDC mint address used by Jupiter routes on Solana mainnet.
pub const JUPITER_USDC_MINT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";

/// Coinbase cbBTC mint address used by Jupiter routes on Solana mainnet.
pub const JUPITER_CBBTC_MINT: &str = "cbbtcf3aa214zXHbiAZQwf4122FBYbraNdFqgw4iMij";

const DEFAULT_SWAP_V2_BASE_URL: &str = "https://api.jup.ag/swap/v2";
const SWAP_V2_PATH: &str = "/swap/v2";
const DEFAULT_HTTP_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_CONFIRMATION_ATTEMPTS: u32 = 30;
const DEFAULT_CONFIRMATION_POLL_INTERVAL: Duration = Duration::from_secs(1);
const DEFAULT_MAX_SLIPPAGE_BPS: u16 = 50;

/// One Jupiter-quoteable asset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JupiterAsset {
    /// Runtime asset identifier.
    pub id: AssetId,
    /// Jupiter mint address.
    pub mint: MintAddress,
    /// Native decimal precision.
    pub decimals: u8,
    /// Raw amount used when asking for a one-display-unit reference price.
    pub reference_amount_raw: AmountRaw,
}

impl JupiterAsset {
    /// Construct a Jupiter asset using one full display unit as the reference amount.
    ///
    /// # Errors
    ///
    /// Returns a validation error when decimal scaling overflows `u64`.
    pub fn new(
        id: impl Into<AssetId>,
        mint: impl Into<String>,
        decimals: u8,
    ) -> Result<Self, AppError> {
        Ok(Self {
            id: id.into(),
            mint: MintAddress::new(mint.into()),
            decimals,
            reference_amount_raw: AmountRaw::new(one_display_unit_raw(decimals)?),
        })
    }

    fn from_config(asset: &AssetConfig) -> Result<Self, AppError> {
        Self::new(asset.id.clone(), asset.mint.as_str(), asset.decimals)
    }
}

/// Wallet metadata and optional signer used by `/order` and `/execute`.
#[derive(Clone)]
pub struct JupiterWallet {
    role: WalletRole,
    address: WalletAddress,
    signer: Option<Arc<Keypair>>,
}

impl std::fmt::Debug for JupiterWallet {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("JupiterWallet")
            .field("role", &self.role)
            .field("address", &self.address)
            .field("has_signer", &self.signer.is_some())
            .finish()
    }
}

impl JupiterWallet {
    /// Construct a quote-only wallet address.
    #[must_use]
    pub fn new(role: WalletRole, address: WalletAddress) -> Self {
        Self {
            role,
            address,
            signer: None,
        }
    }

    /// Construct a wallet from a Solana keypair.
    #[must_use]
    pub fn from_keypair(role: WalletRole, signer: Keypair) -> Self {
        let address = WalletAddress::new(signer.pubkey().to_string());
        Self {
            role,
            address,
            signer: Some(Arc::new(signer)),
        }
    }

    /// Load a wallet signer from the configured private-key/path/JSON source.
    ///
    /// # Errors
    ///
    /// Returns a validation error when the configured env vars are missing or malformed.
    pub fn from_env_config(config: &WalletConfig) -> Result<Self, AppError> {
        let wallet = LoadedWallet::from_env_config(config).map_err(AppError::from)?;
        let keypair = wallet.try_clone_keypair().map_err(AppError::from)?;
        Ok(Self::from_keypair(config.role, keypair))
    }

    fn role(&self) -> WalletRole {
        self.role
    }

    fn address(&self) -> &WalletAddress {
        &self.address
    }

    fn signer(&self) -> Option<&Keypair> {
        self.signer.as_deref()
    }
}

/// Confirmation-polling controls for submitted Jupiter swaps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JupiterConfirmationConfig {
    /// Number of signature-status polls before timing out.
    pub max_attempts: u32,
    /// Delay between signature-status polls.
    pub poll_interval: Duration,
}

impl Default for JupiterConfirmationConfig {
    fn default() -> Self {
        Self {
            max_attempts: DEFAULT_CONFIRMATION_ATTEMPTS,
            poll_interval: DEFAULT_CONFIRMATION_POLL_INTERVAL,
        }
    }
}

/// Configuration required to build a Jupiter client.
#[derive(Debug, Clone)]
pub struct JupiterClientConfig {
    /// Jupiter Swap API V2 base URL. Root URLs are normalized by appending `/swap/v2`.
    pub base_url: String,
    /// Jupiter API key, when configured.
    pub api_key: Option<String>,
    /// Supported Jupiter assets.
    pub assets: Vec<JupiterAsset>,
    /// Enabled directional pairs.
    pub pairs: Vec<AssetPair>,
    /// Wallets available to quote and sign swaps.
    pub wallets: Vec<JupiterWallet>,
    /// Solana RPC URL used for post-execute confirmation polling.
    pub rpc_url: Option<String>,
    /// Local slippage ceiling for live swap requests.
    pub max_slippage_bps: u16,
    /// HTTP timeout for Jupiter requests.
    pub http_timeout: Duration,
    /// Confirmation-polling controls.
    pub confirmation: JupiterConfirmationConfig,
}

impl JupiterClientConfig {
    /// Build adapter config from the application config and preloaded wallets.
    ///
    /// # Errors
    ///
    /// Returns a validation error when asset decimal scaling overflows.
    pub fn from_app_config(
        app_config: &AppConfig,
        wallets: Vec<JupiterWallet>,
    ) -> Result<Self, AppError> {
        let assets = app_config
            .assets
            .supported
            .iter()
            .filter(|asset| asset.enabled)
            .map(JupiterAsset::from_config)
            .collect::<Result<Vec<_>, _>>()?;
        let pairs = app_config
            .assets
            .pairs
            .iter()
            .filter(|pair| pair.enabled)
            .map(PairConfig::pair)
            .collect();
        let api_key = env_nonempty(&app_config.jupiter.api_key_env);
        let rpc_url = env_nonempty(&app_config.solana.rpc_url_env);

        Ok(Self {
            base_url: app_config.jupiter.base_url.clone(),
            api_key,
            assets,
            pairs,
            wallets,
            rpc_url,
            max_slippage_bps: app_config.jupiter.max_slippage_bps,
            http_timeout: Duration::from_millis(app_config.jupiter.quote_timeout_millis),
            confirmation: JupiterConfirmationConfig::default(),
        })
    }

    /// Small mainnet asset universe suitable for quote-only tests and demos.
    ///
    /// # Errors
    ///
    /// Returns a validation error when decimal scaling overflows.
    pub fn mainnet(api_key: Option<String>) -> Result<Self, AppError> {
        let usdc = JupiterAsset::new("USDC", JUPITER_USDC_MINT, 6)?;
        let sol = JupiterAsset::new("SOL", JUPITER_NATIVE_SOL_MINT, 9)?;
        let cbbtc = JupiterAsset::new("cbBTC", JUPITER_CBBTC_MINT, 8)?;
        let pairs = default_pairs(&usdc.id, &sol.id, &cbbtc.id);

        Ok(Self {
            base_url: DEFAULT_SWAP_V2_BASE_URL.to_owned(),
            api_key,
            assets: vec![usdc, sol, cbbtc],
            pairs,
            wallets: Vec::new(),
            rpc_url: None,
            max_slippage_bps: DEFAULT_MAX_SLIPPAGE_BPS,
            http_timeout: DEFAULT_HTTP_TIMEOUT,
            confirmation: JupiterConfirmationConfig::default(),
        })
    }
}

/// Jupiter Swap API V2 client implementing quote and swap traits.
pub struct JupiterClient {
    http: Client,
    base_url: String,
    api_key: Option<String>,
    assets_by_id: HashMap<AssetId, JupiterAsset>,
    asset_ids_by_mint: HashMap<String, AssetId>,
    pairs: HashSet<(AssetId, AssetId)>,
    wallets: HashMap<WalletRole, JupiterWallet>,
    rpc_client: Option<Arc<RpcClient>>,
    max_slippage_bps: u16,
    confirmation: JupiterConfirmationConfig,
    pending_orders: Arc<Mutex<HashMap<PendingQuoteKey, JupiterOrderResponse>>>,
}

impl std::fmt::Debug for JupiterClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("JupiterClient")
            .field("base_url", &self.base_url)
            .field("has_api_key", &self.api_key.is_some())
            .field("asset_count", &self.assets_by_id.len())
            .field("pair_count", &self.pairs.len())
            .field("wallet_count", &self.wallets.len())
            .field("has_rpc_client", &self.rpc_client.is_some())
            .field("max_slippage_bps", &self.max_slippage_bps)
            .field("confirmation", &self.confirmation)
            .finish_non_exhaustive()
    }
}

impl JupiterClient {
    /// Construct a Jupiter client from explicit adapter config.
    ///
    /// # Errors
    ///
    /// Returns an internal error when the HTTP client cannot be built.
    pub fn new(config: JupiterClientConfig) -> Result<Self, AppError> {
        let http = Client::builder()
            .timeout(config.http_timeout)
            .build()
            .map_err(|error| AppError::internal(format!("build Jupiter HTTP client: {error}")))?;
        let base_url = normalize_swap_v2_base_url(&config.base_url);
        let assets_by_id = config
            .assets
            .into_iter()
            .map(|asset| (asset.id.clone(), asset))
            .collect::<HashMap<_, _>>();
        let asset_ids_by_mint = assets_by_id
            .values()
            .map(|asset| (asset.mint.as_str().to_owned(), asset.id.clone()))
            .collect::<HashMap<_, _>>();
        let pairs = config
            .pairs
            .into_iter()
            .map(|pair| (pair.input, pair.output))
            .collect::<HashSet<_>>();
        let wallets = config
            .wallets
            .into_iter()
            .map(|wallet| (wallet.role(), wallet))
            .collect::<HashMap<_, _>>();
        let rpc_client = config
            .rpc_url
            .filter(|url| !url.trim().is_empty())
            .map(|url| Arc::new(RpcClient::new_with_timeout(url, Duration::from_secs(30))));

        Ok(Self {
            http,
            base_url,
            api_key: config.api_key.filter(|key| !key.trim().is_empty()),
            assets_by_id,
            asset_ids_by_mint,
            pairs,
            wallets,
            rpc_client,
            max_slippage_bps: config.max_slippage_bps,
            confirmation: config.confirmation,
            pending_orders: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Fetch a Jupiter `/order` response. Without `taker`, this is quote-only.
    ///
    /// # Errors
    ///
    /// Returns an error when Jupiter rejects the request or the response cannot be parsed.
    pub async fn fetch_order(
        &self,
        input_mint: &str,
        output_mint: &str,
        amount_raw: AmountRaw,
        taker: Option<&WalletAddress>,
        slippage_bps: Option<u16>,
    ) -> Result<JupiterOrderResponse, AppError> {
        if amount_raw.is_zero() {
            return Err(AppError::validation(
                "Jupiter order amount must be greater than zero",
            ));
        }

        let mut query = vec![
            ("inputMint", input_mint.to_owned()),
            ("outputMint", output_mint.to_owned()),
            ("amount", amount_raw.as_u64().to_string()),
        ];
        if let Some(taker) = taker {
            query.push(("taker", taker.as_str().to_owned()));
        }
        if let Some(slippage_bps) = slippage_bps {
            query.push(("slippageBps", slippage_bps.to_string()));
        }

        let request = self.authed_request(self.http.get(self.endpoint("/order")).query(&query));
        let response = request
            .send()
            .await
            .map_err(|error| AppError::external_service(JUPITER_SERVICE, error.to_string()))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|error| AppError::external_service(JUPITER_SERVICE, error.to_string()))?;

        if !status.is_success() {
            return Err(jupiter_http_error(status.as_u16(), &body));
        }

        let order = parse_order_response(&body)?;
        ensure_order_is_usable(&order)?;
        Ok(order)
    }

    /// Fetch Router `/build` raw swap instructions.
    ///
    /// `/build` is intentionally not used by [`Self::execute_prepared_order`] because
    /// Jupiter documents that `/build` transactions do not have the request id
    /// needed by `/execute`.
    ///
    /// # Errors
    ///
    /// Returns an error when the pair, wallet, Jupiter request, or response fails.
    pub async fn build_swap_instructions(
        &self,
        request: &SwapRequest,
    ) -> Result<JupiterBuildResponse, AppError> {
        self.validate_swap_request(request)?;
        let input_asset = self.asset(&request.pair.input)?;
        let output_asset = self.asset(&request.pair.output)?;
        let taker = self.wallet(request.source_wallet)?;

        let query = vec![
            ("inputMint", input_asset.mint.as_str().to_owned()),
            ("outputMint", output_asset.mint.as_str().to_owned()),
            (
                "amount",
                request.input_amount.amount_raw.as_u64().to_string(),
            ),
            ("taker", taker.address().as_str().to_owned()),
            ("slippageBps", request.max_slippage_bps.to_string()),
        ];

        let response = self
            .authed_request(self.http.get(self.endpoint("/build")).query(&query))
            .send()
            .await
            .map_err(|error| AppError::external_service(JUPITER_SERVICE, error.to_string()))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|error| AppError::external_service(JUPITER_SERVICE, error.to_string()))?;

        if !status.is_success() {
            return Err(jupiter_http_error(status.as_u16(), &body));
        }

        parse_build_response(&body)
    }

    /// Sign an order transaction without replacing Jupiter's message or blockhash.
    ///
    /// # Errors
    ///
    /// Returns a Solana error when decoding, signer lookup, signing, or encoding fails.
    pub fn sign_order_transaction(
        &self,
        base64_transaction: &str,
        wallet_role: WalletRole,
    ) -> Result<SignedJupiterTransaction, AppError> {
        let wallet = self.wallet(wallet_role)?;
        let signer = wallet.signer().ok_or_else(|| {
            AppError::validation(format!(
                "wallet {wallet_role:?} does not have a signer for Jupiter execution"
            ))
        })?;
        sign_preserving_jupiter_message(base64_transaction, signer)
    }

    /// Execute a prepared `/order` transaction via Jupiter `/execute`.
    ///
    /// # Errors
    ///
    /// Returns an error when Jupiter execution or Solana confirmation fails.
    pub async fn execute_prepared_order(
        &self,
        prepared: &PreparedJupiterOrder,
    ) -> Result<JupiterExecuteResponse, AppError> {
        let body = JupiterExecuteRequest {
            signed_transaction: prepared.signed_transaction.clone(),
            request_id: prepared.request_id.clone(),
            last_valid_block_height: prepared.last_valid_block_height.clone(),
        };

        let response = self
            .authed_request(self.http.post(self.endpoint("/execute")).json(&body))
            .send()
            .await
            .map_err(|error| AppError::external_service(JUPITER_SERVICE, error.to_string()))?;
        let status = response.status();
        let response_body = response
            .text()
            .await
            .map_err(|error| AppError::external_service(JUPITER_SERVICE, error.to_string()))?;

        if !status.is_success() {
            return Err(jupiter_http_error(status.as_u16(), &response_body));
        }

        let execute = parse_execute_response(&response_body)?;
        ensure_execute_succeeded(&execute)?;
        let signature = execute.signature.as_deref().ok_or_else(|| {
            AppError::external_service(
                JUPITER_SERVICE,
                "execute response succeeded without a signature",
            )
        })?;
        self.await_confirmation(signature).await?;

        Ok(execute)
    }

    /// Poll Solana RPC until the transaction reaches confirmed commitment.
    ///
    /// # Errors
    ///
    /// Returns a Solana error for invalid signatures, RPC failures, on-chain
    /// transaction errors, or timeout.
    pub async fn await_confirmation(&self, signature: &str) -> Result<(), AppError> {
        let rpc_client = self.rpc_client.as_ref().ok_or_else(|| {
            AppError::solana("Solana RPC URL is required for Jupiter confirmation polling")
        })?;
        let signature = Signature::from_str(signature)
            .map_err(|error| AppError::solana(format!("invalid signature: {error}")))?;

        for attempt in 0..self.confirmation.max_attempts {
            tokio::time::sleep(self.confirmation.poll_interval).await;
            let response = rpc_client
                .get_signature_statuses(std::slice::from_ref(&signature))
                .await
                .map_err(|error| {
                    AppError::solana(format!("getSignatureStatuses rpc error: {error}"))
                })?;

            if let Some(Some(status)) = response.value.first() {
                if let Some(error) = &status.err {
                    return Err(confirmation_status_error(
                        &signature.to_string(),
                        &format!("{error:?}"),
                    ));
                }
                if status.satisfies_commitment(CommitmentConfig::confirmed()) {
                    debug!(
                        signature = %signature,
                        attempt,
                        "Jupiter swap confirmed on-chain"
                    );
                    return Ok(());
                }
            }
        }

        Err(confirmation_timeout_error(
            &signature.to_string(),
            self.confirmation.max_attempts,
        ))
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    fn authed_request(&self, request: RequestBuilder) -> RequestBuilder {
        if let Some(api_key) = &self.api_key {
            request.header("x-api-key", api_key)
        } else {
            request
        }
    }

    fn asset(&self, asset: &AssetId) -> Result<&JupiterAsset, AppError> {
        self.assets_by_id
            .get(asset)
            .ok_or_else(|| AppError::unsupported(format!("unsupported Jupiter asset: {asset}")))
    }

    fn wallet(&self, role: WalletRole) -> Result<&JupiterWallet, AppError> {
        self.wallets
            .get(&role)
            .ok_or_else(|| AppError::validation(format!("missing Jupiter wallet for {role:?}")))
    }

    fn validate_swap_request(&self, request: &SwapRequest) -> Result<(), AppError> {
        if request.input_amount.asset != request.pair.input {
            return Err(AppError::validation(format!(
                "swap input amount asset {} does not match pair input {}",
                request.input_amount.asset, request.pair.input
            )));
        }
        if request.input_amount.amount_raw.is_zero() {
            return Err(AppError::validation(
                "swap input amount must be greater than zero",
            ));
        }
        if !self.supports_pair(&request.pair) {
            return Err(AppError::unsupported(format!(
                "unsupported Jupiter pair: {}->{}",
                request.pair.input, request.pair.output
            )));
        }
        if request.max_slippage_bps > self.max_slippage_bps {
            return Err(AppError::validation(format!(
                "swap slippage {} bps exceeds Jupiter cap {} bps",
                request.max_slippage_bps, self.max_slippage_bps
            )));
        }
        Ok(())
    }

    fn cache_order(
        &self,
        key: PendingQuoteKey,
        order: JupiterOrderResponse,
    ) -> Result<(), AppError> {
        let mut pending = self
            .pending_orders
            .lock()
            .map_err(|_| AppError::internal("Jupiter pending order cache is poisoned"))?;
        pending.insert(key, order);
        Ok(())
    }

    fn take_cached_order(
        &self,
        key: &PendingQuoteKey,
    ) -> Result<Option<JupiterOrderResponse>, AppError> {
        let mut pending = self
            .pending_orders
            .lock()
            .map_err(|_| AppError::internal("Jupiter pending order cache is poisoned"))?;
        Ok(pending.remove(key))
    }

    fn token_amount_for_mint(&self, mint: &str, raw: &str) -> Option<TokenAmount> {
        let asset = self.asset_ids_by_mint.get(mint)?;
        let amount = parse_raw_amount(raw).ok()?;
        Some(TokenAmount::new(asset.clone(), AmountRaw::new(amount)))
    }
}

#[async_trait]
impl PriceProvider for JupiterClient {
    async fn reference_price(&self, pair: AssetPair) -> Result<ReferencePrice, AppError> {
        if !self.supports_pair(&pair) {
            return Err(AppError::unsupported(format!(
                "unsupported Jupiter pair: {}->{}",
                pair.input, pair.output
            )));
        }

        let input_asset = self.asset(&pair.input)?;
        let output_asset = self.asset(&pair.output)?;
        let order = self
            .fetch_order(
                input_asset.mint.as_str(),
                output_asset.mint.as_str(),
                input_asset.reference_amount_raw,
                None,
                None,
            )
            .await?;
        let price = reference_price_from_order(&order, input_asset, output_asset)?;

        Ok(ReferencePrice {
            pair,
            output_per_input: price,
            observed_at: OffsetDateTime::now_utc(),
        })
    }
}

#[async_trait]
impl SwapExecutor for JupiterClient {
    async fn quote_swap(&self, request: SwapRequest) -> Result<SwapQuote, AppError> {
        self.validate_swap_request(&request)?;
        let input_asset = self.asset(&request.pair.input)?;
        let output_asset = self.asset(&request.pair.output)?;
        let wallet = self.wallet(request.source_wallet)?;
        let order = self
            .fetch_order(
                input_asset.mint.as_str(),
                output_asset.mint.as_str(),
                request.input_amount.amount_raw,
                Some(wallet.address()),
                Some(request.max_slippage_bps),
            )
            .await?;
        ensure_order_has_transaction(&order)?;

        let expected_output_raw = parse_raw_amount(&order.out_amount)?;
        let expected_output = TokenAmount::new(
            request.pair.output.clone(),
            AmountRaw::new(expected_output_raw),
        );
        let estimated_fee = order
            .platform_fee
            .as_ref()
            .and_then(|fee| fee.fee_mint.as_deref().zip(fee.amount.as_deref()))
            .and_then(|(fee_mint, amount)| self.token_amount_for_mint(fee_mint, amount));
        let quote = SwapQuote {
            request,
            expected_output,
            estimated_fee,
            expires_at: parse_optional_timestamp(order.expire_at.as_deref())?,
        };

        self.cache_order(PendingQuoteKey::from_quote(&quote), order)?;
        Ok(quote)
    }

    async fn execute_swap(&self, quote: SwapQuote) -> Result<SwapReceipt, AppError> {
        self.validate_swap_request(&quote.request)?;
        let key = PendingQuoteKey::from_quote(&quote);
        let order = if let Some(order) = self.take_cached_order(&key)? {
            order
        } else {
            let input_asset = self.asset(&quote.request.pair.input)?;
            let output_asset = self.asset(&quote.request.pair.output)?;
            let wallet = self.wallet(quote.request.source_wallet)?;
            let fresh = self
                .fetch_order(
                    input_asset.mint.as_str(),
                    output_asset.mint.as_str(),
                    quote.request.input_amount.amount_raw,
                    Some(wallet.address()),
                    Some(quote.request.max_slippage_bps),
                )
                .await?;
            ensure_order_has_transaction(&fresh)?;
            let fresh_out = parse_raw_amount(&fresh.out_amount)?;
            if fresh_out < quote.expected_output.amount_raw.as_u64() {
                return Err(AppError::external_service(
                    JUPITER_SERVICE,
                    format!(
                        "fresh Jupiter order output {fresh_out} is below quoted output {}",
                        quote.expected_output.amount_raw.as_u64()
                    ),
                ));
            }
            fresh
        };

        let transaction = ensure_order_has_transaction(&order)?;
        let signed = self.sign_order_transaction(transaction, quote.request.source_wallet)?;
        let prepared = PreparedJupiterOrder {
            request_id: order.request_id.clone(),
            signed_transaction: signed.signed_transaction,
            derived_signature: signed.derived_signature,
            last_valid_block_height: order.last_valid_block_height.clone(),
        };
        let execute = self.execute_prepared_order(&prepared).await?;
        let signature = execute.signature.ok_or_else(|| {
            AppError::external_service(
                JUPITER_SERVICE,
                "execute response succeeded without a signature",
            )
        })?;
        let output_amount = execute
            .output_amount_result
            .as_deref()
            .map(parse_raw_amount)
            .transpose()?
            .map(|raw| TokenAmount::new(quote.request.pair.output, AmountRaw::new(raw)));

        Ok(SwapReceipt {
            trade_id: None,
            signature: TxSignature::new(signature),
            output_amount,
        })
    }
}

impl SupportsPair for JupiterClient {
    fn supports_pair(&self, pair: &AssetPair) -> bool {
        self.assets_by_id.contains_key(&pair.input)
            && self.assets_by_id.contains_key(&pair.output)
            && self
                .pairs
                .contains(&(pair.input.clone(), pair.output.clone()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PendingQuoteKey {
    input: AssetId,
    output: AssetId,
    input_amount_raw: u64,
    expected_output_raw: u64,
    source_wallet: WalletRole,
    destination_wallet: WalletRole,
    slippage_bps: u16,
}

impl PendingQuoteKey {
    fn from_quote(quote: &SwapQuote) -> Self {
        Self {
            input: quote.request.pair.input.clone(),
            output: quote.request.pair.output.clone(),
            input_amount_raw: quote.request.input_amount.amount_raw.as_u64(),
            expected_output_raw: quote.expected_output.amount_raw.as_u64(),
            source_wallet: quote.request.source_wallet,
            destination_wallet: quote.request.destination_wallet,
            slippage_bps: quote.request.max_slippage_bps,
        }
    }
}

fn reference_price_from_order(
    order: &JupiterOrderResponse,
    input_asset: &JupiterAsset,
    output_asset: &JupiterAsset,
) -> Result<Decimal, AppError> {
    let input_raw = parse_raw_amount(&order.in_amount)?;
    let output_raw = parse_raw_amount(&order.out_amount)?;
    if input_raw == 0 || output_raw == 0 {
        return Err(AppError::external_service(
            JUPITER_SERVICE,
            "Jupiter reference price returned zero amount",
        ));
    }

    let input_display = raw_to_decimal(input_raw, input_asset.decimals)?;
    let output_display = raw_to_decimal(output_raw, output_asset.decimals)?;
    output_display.checked_div(input_display).ok_or_else(|| {
        AppError::external_service(JUPITER_SERVICE, "Jupiter reference price division failed")
    })
}

fn raw_to_decimal(raw: u64, decimals: u8) -> Result<Decimal, AppError> {
    let scale = decimal_scale(decimals)?;
    Decimal::from(raw)
        .checked_div(scale)
        .ok_or_else(|| AppError::validation("raw amount decimal conversion failed"))
}

fn decimal_scale(decimals: u8) -> Result<Decimal, AppError> {
    let mut scale = Decimal::ONE;
    for _ in 0..decimals {
        scale = scale
            .checked_mul(Decimal::TEN)
            .ok_or_else(|| AppError::validation("decimal scale overflow"))?;
    }
    Ok(scale)
}

fn confirmation_timeout_error(signature: &str, attempts: u32) -> AppError {
    AppError::solana(format!(
        "confirmation timeout after {attempts} polls for signature {signature}"
    ))
}

fn confirmation_status_error(signature: &str, reason: &str) -> AppError {
    AppError::solana(format!(
        "on-chain transaction error for signature {signature}: {reason}"
    ))
}

fn normalize_swap_v2_base_url(base_url: &str) -> String {
    let trimmed = base_url.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return DEFAULT_SWAP_V2_BASE_URL.to_owned();
    }
    if trimmed.ends_with(SWAP_V2_PATH) {
        trimmed.to_owned()
    } else {
        format!("{trimmed}{SWAP_V2_PATH}")
    }
}

fn one_display_unit_raw(decimals: u8) -> Result<u64, AppError> {
    10_u64
        .checked_pow(u32::from(decimals))
        .ok_or_else(|| AppError::validation(format!("decimal scale overflows for {decimals}")))
}

fn env_nonempty(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn default_pairs(usdc: &AssetId, sol: &AssetId, cbbtc: &AssetId) -> Vec<AssetPair> {
    vec![
        AssetPair::new(usdc.clone(), sol.clone()),
        AssetPair::new(sol.clone(), usdc.clone()),
        AssetPair::new(usdc.clone(), cbbtc.clone()),
        AssetPair::new(cbbtc.clone(), usdc.clone()),
        AssetPair::new(sol.clone(), cbbtc.clone()),
        AssetPair::new(cbbtc.clone(), sol.clone()),
    ]
}

#[cfg(test)]
mod tests {
    use super::super::signing::{decode_versioned_transaction, encode_versioned_transaction};
    use super::super::*;
    use super::*;
    use solana_sdk::{
        hash::Hash,
        message::{Message, VersionedMessage},
        pubkey::Pubkey,
        signature::{Signature, Signer},
        transaction::VersionedTransaction,
    };

    #[test]
    fn jupiter_order_response_success_parses() {
        let json = r#"{
            "requestId": "req-123",
            "inputMint": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
            "outputMint": "So11111111111111111111111111111111111111112",
            "inAmount": "1000000",
            "outAmount": "123456789",
            "transaction": "AQAAAA==",
            "swapMode": "ExactIn",
            "slippageBps": 50,
            "router": "iris",
            "feeMint": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
            "platformFee": {
                "amount": "2000",
                "feeBps": 2,
                "feeMint": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"
            },
            "lastValidBlockHeight": "123456"
        }"#;

        let order = parse_order_response(json).expect("order parses");

        assert_eq!(order.request_id, "req-123");
        assert_eq!(order.in_amount, "1000000");
        assert_eq!(order.out_amount, "123456789");
        assert_eq!(order.transaction.as_deref(), Some("AQAAAA=="));
        assert_eq!(order.router.as_deref(), Some("iris"));
        assert_eq!(
            order
                .platform_fee
                .as_ref()
                .and_then(|fee| fee.amount.as_deref()),
            Some("2000")
        );
        assert!(ensure_order_is_usable(&order).is_ok());
    }

    #[test]
    fn jupiter_order_response_minimal_parses() {
        let json = r#"{
            "requestId": "req-quote-only",
            "inputMint": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
            "outputMint": "So11111111111111111111111111111111111111112",
            "inAmount": "1000000",
            "outAmount": "123456789",
            "transaction": null
        }"#;

        let order = parse_order_response(json).expect("minimal order parses");

        assert_eq!(order.request_id, "req-quote-only");
        assert_eq!(order.transaction, None);
        assert_eq!(order.error_code, None);
        assert_eq!(order.error_message, None);
        assert!(ensure_order_is_usable(&order).is_ok());
    }

    #[test]
    fn jupiter_api_error_maps_to_external_service() {
        let body = r#"{
            "errorCode": 42,
            "errorMessage": "No route found"
        }"#;

        let err = jupiter_http_error(400, body);

        assert!(matches!(
            err,
            crate::AppError::ExternalService { service, message }
                if service == JUPITER_SERVICE
                    && message.contains("HTTP 400")
                    && message.contains("No route found")
        ));
    }

    #[test]
    fn jupiter_order_error_code_maps_to_external_service() {
        let json = r#"{
            "requestId": "req-err",
            "inputMint": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
            "outputMint": "So11111111111111111111111111111111111111112",
            "inAmount": "1000000",
            "outAmount": "0",
            "transaction": "",
            "errorCode": 9,
            "errorMessage": "Insufficient liquidity"
        }"#;

        let order = parse_order_response(json).expect("error order still parses");
        let err = ensure_order_is_usable(&order).expect_err("error code rejected");

        assert!(matches!(
            err,
            crate::AppError::ExternalService { service, message }
                if service == JUPITER_SERVICE
                    && message.contains("errorCode=9")
                    && message.contains("Insufficient liquidity")
        ));
    }

    #[test]
    fn jupiter_execute_success_parses() {
        let json = r#"{
            "signature": "5abc123def",
            "status": "Success",
            "code": 0,
            "inputAmountResult": "1000000",
            "outputAmountResult": "123456789",
            "swapEvents": [
                {
                    "inputMint": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
                    "inputAmount": "1000000",
                    "outputMint": "So11111111111111111111111111111111111111112",
                    "outputAmount": "123456789"
                }
            ]
        }"#;

        let execute = parse_execute_response(json).expect("execute parses");

        assert_eq!(execute.status, JupiterExecuteStatus::Success);
        assert_eq!(execute.signature.as_deref(), Some("5abc123def"));
        assert_eq!(execute.output_amount_result.as_deref(), Some("123456789"));
        assert!(ensure_execute_succeeded(&execute).is_ok());
    }

    #[test]
    fn jupiter_execute_failure_maps_to_external_service() {
        let json = r#"{
            "signature": "5abc123def",
            "status": "Failed",
            "code": -2004,
            "error": "Swap rejected"
        }"#;

        let execute = parse_execute_response(json).expect("failure parses");
        let err = ensure_execute_succeeded(&execute).expect_err("failed execute rejected");

        assert!(matches!(
            err,
            crate::AppError::ExternalService { service, message }
                if service == JUPITER_SERVICE
                    && message.contains("status=Failed")
                    && message.contains("code=-2004")
                    && message.contains("Swap rejected")
        ));
    }

    #[test]
    fn signing_preserves_jupiter_message_and_blockhash() {
        let signer = Keypair::new();
        let blockhash = Hash::new_unique();
        let message = Message::new_with_blockhash(&[], Some(&signer.pubkey()), &blockhash);
        let original = VersionedTransaction {
            signatures: vec![Signature::default()],
            message: VersionedMessage::Legacy(message),
        };
        let original_message = original.message.clone();
        let encoded = encode_versioned_transaction(&original).expect("encode tx");

        let signed_tx = sign_preserving_jupiter_message(&encoded, &signer).expect("sign tx");
        let decoded =
            decode_versioned_transaction(&signed_tx.signed_transaction).expect("decode tx");

        assert_eq!(decoded.message, original_message);
        assert_eq!(
            decoded.message.recent_blockhash(),
            original_message.recent_blockhash()
        );
        assert_eq!(
            decoded.signatures[0],
            signer.sign_message(&original_message.serialize())
        );
        assert_eq!(
            signed_tx.derived_signature,
            decoded.signatures[0].to_string()
        );
    }

    #[test]
    fn signing_rejects_transaction_without_wallet_signer() {
        let signer = Keypair::new();
        let missing_signer = Keypair::new();
        let blockhash = Hash::new_unique();
        let message = Message::new_with_blockhash(&[], Some(&signer.pubkey()), &blockhash);
        let original = VersionedTransaction {
            signatures: vec![Signature::default()],
            message: VersionedMessage::Legacy(message),
        };
        let encoded = encode_versioned_transaction(&original).expect("encode tx");

        let err = sign_preserving_jupiter_message(&encoded, &missing_signer)
            .expect_err("missing signer rejected");

        assert!(matches!(err, crate::AppError::Solana(message) if message.contains("not found")));
    }

    #[test]
    fn confirmation_timeout_maps_to_solana_error() {
        let err = confirmation_timeout_error("sig-123", 30);

        assert!(matches!(
            err,
            crate::AppError::Solana(message)
                if message.contains("confirmation timeout")
                    && message.contains("sig-123")
                    && message.contains("30")
        ));
    }

    #[test]
    fn confirmation_on_chain_error_maps_to_solana_error() {
        let err = confirmation_status_error("sig-123", "InstructionError");

        assert!(matches!(
            err,
            crate::AppError::Solana(message)
                if message.contains("on-chain transaction error")
                    && message.contains("InstructionError")
        ));
    }

    #[test]
    fn timestamp_parser_accepts_jupiter_epoch_seconds_and_rfc3339() {
        let epoch = parse_optional_timestamp(Some("1777547873"))
            .expect("epoch timestamp parses")
            .expect("timestamp present");
        let rfc3339 = parse_optional_timestamp(Some("2026-04-30T12:37:53Z"))
            .expect("rfc3339 timestamp parses")
            .expect("timestamp present");

        assert_eq!(epoch.unix_timestamp(), 1_777_547_873);
        assert_eq!(rfc3339.year(), 2026);
    }

    #[test]
    fn reference_price_uses_raw_integer_amounts_and_decimals() {
        let order = JupiterOrderResponse {
            request_id: "req-price".to_owned(),
            input_mint: "USDC".to_owned(),
            output_mint: "SOL".to_owned(),
            in_amount: "1000000".to_owned(),
            out_amount: "500000000".to_owned(),
            transaction: None,
            swap_mode: None,
            slippage_bps: None,
            router: None,
            fee_mint: None,
            fee_bps: None,
            platform_fee: None,
            last_valid_block_height: None,
            expire_at: None,
            error_code: None,
            error_message: None,
            error: None,
        };
        let usdc = JupiterAsset::new("USDC", "USDC", 6).expect("usdc asset");
        let sol = JupiterAsset::new("SOL", JUPITER_NATIVE_SOL_MINT, 9).expect("sol asset");

        let price = reference_price_from_order(&order, &usdc, &sol).expect("price");

        assert_eq!(price, Decimal::new(5, 1));
    }

    #[tokio::test]
    async fn jupiter_live_quote_fetch_skips_without_env() {
        let Some(client) = live_quote_client() else {
            return;
        };
        let order = client
            .fetch_order(
                JUPITER_NATIVE_SOL_MINT,
                "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
                AmountRaw::new(1_000_000),
                None,
                None,
            )
            .await
            .expect("live quote fetch");

        assert!(parse_raw_amount(&order.out_amount).expect("raw out") > 0);
        assert_eq!(order.transaction, None);
    }

    #[tokio::test]
    async fn jupiter_live_order_fetch_skips_without_env() {
        let Some(client) = live_client_with_maker(false) else {
            return;
        };
        let wallet = client.wallet(WalletRole::Maker).expect("maker wallet");
        let order = client
            .fetch_order(
                JUPITER_NATIVE_SOL_MINT,
                "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
                AmountRaw::new(1_000_000),
                Some(wallet.address()),
                Some(50),
            )
            .await
            .expect("live order fetch");

        assert!(parse_raw_amount(&order.out_amount).expect("raw out") > 0);
        assert!(
            order
                .transaction
                .as_deref()
                .is_some_and(|tx| !tx.is_empty())
        );
    }

    #[tokio::test]
    async fn jupiter_live_cbbtc_route_fetch_skips_without_env() {
        let Some(client) = live_quote_client() else {
            return;
        };
        let amount_raw = live_cbbtc_quote_usdc_amount_raw();
        assert!(
            (1..=MAX_LIVE_CBBTC_QUOTE_USDC_RAW).contains(&amount_raw),
            "LIVE_CBBTC_QUOTE_USDC_RAW must be between 1 and {MAX_LIVE_CBBTC_QUOTE_USDC_RAW}"
        );

        let order = client
            .fetch_order(
                JUPITER_USDC_MINT,
                JUPITER_CBBTC_MINT,
                AmountRaw::new(amount_raw),
                None,
                Some(50),
            )
            .await
            .expect("live USDC->cbBTC quote fetch");

        assert!(parse_raw_amount(&order.out_amount).expect("raw cbBTC out") > 0);
        assert_eq!(order.transaction, None);
    }

    #[tokio::test]
    async fn jupiter_live_swap_skips_without_explicit_mutating_opt_in() {
        let Some(client) = live_client_with_maker(true) else {
            return;
        };
        let amount_raw = live_swap_amount_raw();
        assert!(
            amount_raw <= 10_000_000,
            "live swap amount must stay at or below the tiny 0.01 SOL test cap"
        );
        let request = SwapRequest {
            pair: AssetPair::new(AssetId::from("SOL"), AssetId::from("USDC")),
            input_amount: TokenAmount::new(AssetId::from("SOL"), AmountRaw::new(amount_raw)),
            source_wallet: WalletRole::Maker,
            destination_wallet: WalletRole::Maker,
            max_slippage_bps: 50,
        };

        let quote = client.quote_swap(request).await.expect("live swap quote");
        let receipt = client.execute_swap(quote).await.expect("live Jupiter swap");

        assert!(!receipt.signature.as_str().is_empty());
    }

    #[tokio::test]
    async fn jupiter_live_cbbtc_swap_skips_without_explicit_mutating_opt_in() {
        let Some(client) = live_client_with_maker_for_cbbtc_swap() else {
            return;
        };
        let amount_raw = live_cbbtc_swap_usdc_amount_raw();
        assert!(
            (1..=MAX_LIVE_CBBTC_SWAP_USDC_RAW).contains(&amount_raw),
            "LIVE_CBBTC_SWAP_USDC_RAW must be between 1 and {MAX_LIVE_CBBTC_SWAP_USDC_RAW}"
        );

        let wallet = client.wallet(WalletRole::Maker).expect("maker wallet");
        let maker = wallet.signer().expect("maker signer").pubkey();
        let cbbtc_mint = Pubkey::from_str(JUPITER_CBBTC_MINT).expect("valid cbBTC mint");
        let cbbtc_ata =
            crate::adapters::solana::client::SolanaClient::derive_associated_token_address(
                &maker,
                &cbbtc_mint,
                &crate::adapters::solana::client::LEGACY_TOKEN_PROGRAM_ID,
            );
        let rpc_client = client
            .rpc_client
            .as_ref()
            .expect("mutating Jupiter client has RPC client");
        let before_raw = live_optional_token_balance_raw(rpc_client, &cbbtc_ata).await;

        let request = SwapRequest {
            pair: AssetPair::new(AssetId::from("USDC"), AssetId::from("cbBTC")),
            input_amount: TokenAmount::new(AssetId::from("USDC"), AmountRaw::new(amount_raw)),
            source_wallet: WalletRole::Maker,
            destination_wallet: WalletRole::Maker,
            max_slippage_bps: 50,
        };
        let quote = client
            .quote_swap(request)
            .await
            .expect("live cbBTC swap quote");
        let expected_raw = quote.expected_output.amount_raw.as_u64();
        assert!(expected_raw > 0, "live cbBTC quote must return output");

        let receipt = client
            .execute_swap(quote)
            .await
            .expect("live Jupiter USDC->cbBTC swap");

        assert!(!receipt.signature.as_str().is_empty());
        eprintln!("live Jupiter cbBTC swap submitted: {}", receipt.signature);
        assert!(
            receipt.output_amount.as_ref().is_some_and(
                |amount| amount.asset.as_str() == "cbBTC" && amount.amount_raw.as_u64() > 0
            )
        );

        let after_raw = wait_for_token_balance_increase(rpc_client, &cbbtc_ata, before_raw).await;
        assert!(
            after_raw > before_raw,
            "maker cbBTC ATA should increase after live swap; before={before_raw}, after={after_raw}"
        );
    }

    fn live_quote_client() -> Option<JupiterClient> {
        if env::var("RUN_LIVE_JUPITER_TESTS").ok().as_deref() != Some("1") {
            eprintln!("skipping live Jupiter quote test; RUN_LIVE_JUPITER_TESTS=1 is not set");
            return None;
        }
        let Some(api_key) = env_nonempty("JUPITER_API_KEY") else {
            eprintln!("skipping live Jupiter quote test; JUPITER_API_KEY is not set");
            return None;
        };
        let mut config = JupiterClientConfig::mainnet(Some(api_key)).expect("mainnet config");
        if let Some(base_url) = env_nonempty("JUPITER_BASE_URL") {
            config.base_url = base_url;
        }
        Some(JupiterClient::new(config).expect("client"))
    }

    fn live_client_with_maker(mutating: bool) -> Option<JupiterClient> {
        if mutating {
            if env::var("RUN_LIVE_SOLANA_TESTS").ok().as_deref() != Some("1")
                || env::var("RUN_LIVE_JUPITER_SWAP_TESTS").ok().as_deref() != Some("1")
            {
                eprintln!(
                    "skipping live Jupiter swap; RUN_LIVE_SOLANA_TESTS=1 and RUN_LIVE_JUPITER_SWAP_TESTS=1 are required"
                );
                return None;
            }
        } else if env::var("RUN_LIVE_JUPITER_TESTS").ok().as_deref() != Some("1") {
            eprintln!("skipping live Jupiter order test; RUN_LIVE_JUPITER_TESTS=1 is not set");
            return None;
        }

        let Some(api_key) = env_nonempty("JUPITER_API_KEY") else {
            eprintln!("skipping live Jupiter test; JUPITER_API_KEY is not set");
            return None;
        };
        let wallet_config = crate::config::WalletsConfig::default().maker;
        let Ok(wallet) = JupiterWallet::from_env_config(&wallet_config) else {
            eprintln!("skipping live Jupiter test; maker keypair env vars are not set");
            return None;
        };
        let mut config = JupiterClientConfig::mainnet(Some(api_key)).expect("mainnet config");
        config.wallets = vec![wallet];
        if mutating {
            let Some(rpc_url) = env_nonempty("SOLANA_RPC_URL") else {
                eprintln!("skipping live Jupiter swap; SOLANA_RPC_URL is not set");
                return None;
            };
            config.rpc_url = Some(rpc_url);
        }
        if let Some(base_url) = env_nonempty("JUPITER_BASE_URL") {
            config.base_url = base_url;
        }
        Some(JupiterClient::new(config).expect("client"))
    }

    fn live_client_with_maker_for_cbbtc_swap() -> Option<JupiterClient> {
        if env::var("RUN_LIVE_SOLANA_TESTS").ok().as_deref() != Some("1")
            || env::var("RUN_LIVE_JUPITER_CBBTC_SWAP_TESTS")
                .ok()
                .as_deref()
                != Some("1")
        {
            eprintln!(
                "skipping live Jupiter cbBTC swap; RUN_LIVE_SOLANA_TESTS=1 and RUN_LIVE_JUPITER_CBBTC_SWAP_TESTS=1 are required"
            );
            return None;
        }

        let Some(api_key) = env_nonempty("JUPITER_API_KEY") else {
            eprintln!("skipping live Jupiter cbBTC swap; JUPITER_API_KEY is not set");
            return None;
        };
        let Some(rpc_url) = env_nonempty("SOLANA_RPC_URL") else {
            eprintln!("skipping live Jupiter cbBTC swap; SOLANA_RPC_URL is not set");
            return None;
        };
        let wallet_config = crate::config::WalletsConfig::default().maker;
        let Ok(wallet) = JupiterWallet::from_env_config(&wallet_config) else {
            eprintln!("skipping live Jupiter cbBTC swap; maker keypair env vars are not set");
            return None;
        };
        let mut config = JupiterClientConfig::mainnet(Some(api_key)).expect("mainnet config");
        config.wallets = vec![wallet];
        config.rpc_url = Some(rpc_url);
        if let Some(base_url) = env_nonempty("JUPITER_BASE_URL") {
            config.base_url = base_url;
        }
        Some(JupiterClient::new(config).expect("client"))
    }

    async fn live_optional_token_balance_raw(
        rpc_client: &RpcClient,
        token_account: &Pubkey,
    ) -> u64 {
        rpc_client
            .get_token_account_balance(token_account)
            .await
            .ok()
            .and_then(|balance| balance.amount.parse::<u64>().ok())
            .unwrap_or(0)
    }

    async fn live_required_token_balance_raw(
        rpc_client: &RpcClient,
        token_account: &Pubkey,
    ) -> u64 {
        rpc_client
            .get_token_account_balance(token_account)
            .await
            .expect("token account must be readable")
            .amount
            .parse::<u64>()
            .expect("token amount should parse as u64")
    }

    async fn wait_for_token_balance_increase(
        rpc_client: &RpcClient,
        token_account: &Pubkey,
        before_raw: u64,
    ) -> u64 {
        let mut last_raw = 0;
        for _ in 0..45 {
            last_raw = live_required_token_balance_raw(rpc_client, token_account).await;
            if last_raw > before_raw {
                return last_raw;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        last_raw
    }

    fn live_swap_amount_raw() -> u64 {
        env::var("LIVE_JUPITER_SWAP_AMOUNT_RAW")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(1_000_000)
    }

    const DEFAULT_LIVE_CBBTC_QUOTE_USDC_RAW: u64 = 5_000_000;
    const MAX_LIVE_CBBTC_QUOTE_USDC_RAW: u64 = 5_000_000;

    fn live_cbbtc_quote_usdc_amount_raw() -> u64 {
        env::var("LIVE_CBBTC_QUOTE_USDC_RAW")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(DEFAULT_LIVE_CBBTC_QUOTE_USDC_RAW)
    }

    const DEFAULT_LIVE_CBBTC_SWAP_USDC_RAW: u64 = 4_000_000;
    const MAX_LIVE_CBBTC_SWAP_USDC_RAW: u64 = 5_000_000;

    fn live_cbbtc_swap_usdc_amount_raw() -> u64 {
        env::var("LIVE_CBBTC_SWAP_USDC_RAW")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(DEFAULT_LIVE_CBBTC_SWAP_USDC_RAW)
    }
}
