//! Circle Gateway Solana adapter primitives.
//!
//! This module intentionally implements only the Solana Gateway path used by the
//! demo runtime: USDC deposit instruction construction, Gateway balance reads,
//! signed Solana burn intents, transfer attestation submission/polling, and
//! `gatewayMint` instruction construction from decoded attestation elements.

pub mod api;
pub mod attestation;
pub mod instructions;
pub mod signing;

use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use ed25519_dalek::SigningKey;
use reqwest::StatusCode;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    instruction::Instruction,
    pubkey::Pubkey,
    signature::{Keypair, Signer as SolanaSigner},
    transaction::Transaction,
};
use tokio::time::Instant;

use crate::domain::events::{EventMetadata, GatewayEvent};
use crate::domain::types::{
    AmountRaw, AssetId, GatewayReceipt, GatewayRefillRequest, TokenAmount, TxSignature,
};
use crate::error::AppError;
use crate::ports::GatewayClient;

pub use api::{
    AttestationEnvelope, BalanceRequest, BalanceSource, BurnIntentData, BurnIntentRequest,
    TransferFeePerIntent, TransferFees, TransferForwardingDetails, TransferResponse,
    TransferSpecData, TransferStatus, TransferStatusResponse,
};
pub use attestation::{AttestationElement, parse_attestation_elements};
pub use instructions::{
    GatewayProgramIds, build_deposit_instruction, build_gateway_mint_instruction,
    depositor_denylist_pda, gateway_deposit_pda, gateway_minter_custody_pda, gateway_minter_pda,
    gateway_wallet_custody_pda, gateway_wallet_pda, used_transfer_spec_hash_pda,
};
pub use signing::{
    TransferSpecSolana, amount_to_u256_be, build_solana_burn_intent_request, encode_burn_intent,
    encode_transfer_spec, fresh_salt_bytes, pubkey_to_bytes32, sign_burn_intent_ed25519,
    solana_burn_intent_signing_message,
};

use api::{
    BalanceResponse, TransferAcceptedResponse, decode_hex_bytes, duration_millis_u64, endpoint,
    truncate_for_error,
};

#[cfg(test)]
use solana_sdk::instruction::AccountMeta;

/// Circle Gateway Wallet program on Solana mainnet.
pub const GATEWAY_WALLET_PROGRAM: &str = "GATEwy4YxeiEbRJLwB6dXgg7q61e6zBPrMzYj5h1pRXQ";

/// Circle Gateway Minter program on Solana mainnet.
pub const GATEWAY_MINTER_PROGRAM: &str = "GATEm5SoBJiSw1v2Pz1iPBgUYkXzCUJ27XSXhDfSyzVZ";

/// USDC mint on Solana mainnet.
pub const USDC_MINT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";

/// SPL Token program.
pub const SPL_TOKEN_PROGRAM_ID: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";

/// System program.
pub const SYSTEM_PROGRAM_ID: &str = "11111111111111111111111111111111";

/// Circle Gateway domain identifier for Solana.
pub const SOLANA_DOMAIN: u32 = 5;

/// Mainnet Circle Gateway API base URL.
pub const GATEWAY_API_BASE_URL: &str = "https://gateway-api.circle.com/v1";

/// Testnet Circle Gateway API base URL.
pub const GATEWAY_TESTNET_API_BASE_URL: &str = "https://gateway-api-testnet.circle.com/v1";

const USDC_ASSET_ID: &str = "USDC";
const DEFAULT_HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_TRANSFER_STATUS_TIMEOUT: Duration = Duration::from_secs(60);
const DEFAULT_TRANSFER_STATUS_POLL_INTERVAL: Duration = Duration::from_secs(1);
const BURN_INTENT_MAGIC: u32 = 0x070a_fbc2;
const TRANSFER_SPEC_MAGIC: u32 = 0xca85_def7;
const DEPOSIT_DISCRIMINATOR: [u8; 2] = [22, 0];
const GATEWAY_MINT_DISCRIMINATOR: [u8; 2] = [12, 0];
const ATTESTATION_HEADER_SIZE: usize = 88;
const NUM_ATTESTATIONS_OFFSET: usize = 84;
const ELEM_DEST_TOKEN: usize = 0;
const ELEM_DEST_RECIPIENT: usize = 32;
const ELEM_VALUE: usize = 64;
const ELEM_TRANSFER_SPEC_HASH: usize = 72;
const ELEM_HOOK_DATA_LENGTH: usize = 104;
const ELEM_FIXED_SIZE: usize = 108;

/// Solana signing domain header for Gateway burn intents.
pub const SOLANA_BURN_INTENT_SIGNING_DOMAIN: [u8; 16] =
    [0xff, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

/// Result alias for Gateway protocol helpers.
pub type GatewayResult<T> = Result<T, GatewayError>;

/// Gateway-specific adapter errors.
#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    /// A Solana public key could not be parsed.
    #[error("invalid pubkey for {label}: {source}")]
    InvalidPubkey {
        /// Field label.
        label: &'static str,
        /// Parser source error.
        source: solana_sdk::pubkey::ParsePubkeyError,
    },

    /// Hex data could not be parsed.
    #[error("invalid hex for {label}: {message}")]
    InvalidHex {
        /// Field label.
        label: &'static str,
        /// Parser message.
        message: String,
    },

    /// An attestation could not be decoded.
    #[error("invalid attestation: {0}")]
    InvalidAttestation(String),

    /// A Circle Gateway API call returned a non-success status.
    #[error("Circle Gateway API error ({status}): {body}")]
    ApiStatus {
        /// HTTP status.
        status: StatusCode,
        /// Response body.
        body: String,
    },

    /// A Circle Gateway HTTP request failed.
    #[error("Circle Gateway HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    /// A successful response body could not be interpreted.
    #[error("{operation} returned an unreadable success response: {reason}; body: {body}")]
    SuccessResponse {
        /// Operation label.
        operation: &'static str,
        /// Parse reason.
        reason: String,
        /// Truncated response body.
        body: String,
    },

    /// A transfer reached a terminal failure state.
    #[error("{operation} transfer {transfer_id} reached terminal status {status}: {reason}")]
    TransferStatusFailed {
        /// Operation label.
        operation: &'static str,
        /// Provider transfer id.
        transfer_id: String,
        /// Terminal status.
        status: String,
        /// Failure reason.
        reason: String,
    },

    /// Polling did not yield a mintable attestation before timeout.
    #[error(
        "{operation} transfer {transfer_id} did not yield attestation before timeout ({duration_ms}ms); last status: {last_status:?}; last error: {last_error:?}"
    )]
    TransferStatusTimeout {
        /// Operation label.
        operation: &'static str,
        /// Provider transfer id.
        transfer_id: String,
        /// Timeout in milliseconds.
        duration_ms: u64,
        /// Last observed transfer status.
        last_status: Option<String>,
        /// Last polling error.
        last_error: Option<String>,
    },

    /// Only USDC Gateway operations are supported by this adapter.
    #[error("unsupported Gateway asset: {0}")]
    UnsupportedAsset(String),

    /// The client was asked to sign without a configured Solana signing key.
    #[error("missing Solana signing key for Gateway refill")]
    MissingSigningKey,

    /// Numeric conversion would lose data.
    #[error("Gateway numeric value is out of supported range: {0}")]
    NumericRange(String),

    /// Solana transaction submission failed.
    #[error("Gateway Solana transaction failed: {0}")]
    Solana(String),
}

impl From<GatewayError> for AppError {
    fn from(error: GatewayError) -> Self {
        match error {
            GatewayError::UnsupportedAsset(message) => AppError::unsupported(message),
            GatewayError::InvalidPubkey { .. }
            | GatewayError::InvalidHex { .. }
            | GatewayError::InvalidAttestation(_)
            | GatewayError::NumericRange(_)
            | GatewayError::MissingSigningKey => AppError::validation(error.to_string()),
            GatewayError::ApiStatus { .. }
            | GatewayError::Http(_)
            | GatewayError::SuccessResponse { .. }
            | GatewayError::TransferStatusFailed { .. }
            | GatewayError::TransferStatusTimeout { .. } => {
                AppError::external_service("circle_gateway", error.to_string())
            }
            GatewayError::Solana(_) => AppError::solana(error.to_string()),
        }
    }
}

fn parse_pubkey(label: &'static str, value: &str) -> GatewayResult<Pubkey> {
    Pubkey::from_str(value).map_err(|source| GatewayError::InvalidPubkey { label, source })
}

fn hex_bytes32(value: [u8; 32]) -> String {
    format!("0x{}", hex::encode(value))
}

fn u256_be_to_u64(value: &[u8; 32]) -> GatewayResult<u64> {
    if value[..24].iter().any(|byte| *byte != 0) {
        return Err(GatewayError::NumericRange(
            "u256 value exceeds u64 Gateway demo support".to_owned(),
        ));
    }
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&value[24..]);
    Ok(u64::from_be_bytes(bytes))
}

/// Fully prepared refill: Circle transfer plus Solana mint instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayRefillPlan {
    /// Amount requested.
    pub amount: TokenAmount,
    /// Provider transfer id.
    pub provider_transfer_id: Option<String>,
    /// Reduced attestation bytes.
    pub attestation: Vec<u8>,
    /// Circle attestation signature bytes.
    pub attestation_signature: Vec<u8>,
    /// Solana instruction the caller can broadcast with the configured payer.
    pub mint_instruction: Instruction,
}

/// Submits Gateway mint instructions on Solana.
#[derive(Clone)]
pub struct GatewayMintSubmitter {
    rpc_client: Arc<RpcClient>,
    payer: Arc<Keypair>,
}

impl GatewayMintSubmitter {
    /// Create a Solana mint submitter.
    #[must_use]
    pub fn new(rpc_client: Arc<RpcClient>, payer: Arc<Keypair>) -> Self {
        Self { rpc_client, payer }
    }

    /// Sign, submit, and confirm a prepared Gateway mint instruction.
    ///
    /// # Errors
    ///
    /// Returns a Gateway Solana error when blockhash lookup, submission, or
    /// confirmation fails.
    pub async fn submit(&self, instruction: Instruction) -> GatewayResult<TxSignature> {
        let blockhash = self
            .rpc_client
            .get_latest_blockhash()
            .await
            .map_err(|error| GatewayError::Solana(format!("get latest blockhash: {error}")))?;
        let transaction = Transaction::new_signed_with_payer(
            &[instruction],
            Some(&self.payer.pubkey()),
            &[self.payer.as_ref()],
            blockhash,
        );
        let signature = self
            .rpc_client
            .send_and_confirm_transaction(&transaction)
            .await
            .map_err(|error| {
                GatewayError::Solana(format!("send and confirm gatewayMint: {error}"))
            })?;
        Ok(TxSignature::new(signature.to_string()))
    }
}

impl std::fmt::Debug for GatewayMintSubmitter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GatewayMintSubmitter")
            .field("payer", &self.payer.pubkey())
            .finish_non_exhaustive()
    }
}

/// Submits Gateway Wallet deposit instructions on Solana.
#[derive(Clone)]
pub struct GatewayDepositSubmitter {
    rpc_client: Arc<RpcClient>,
    payer: Arc<Keypair>,
}

impl GatewayDepositSubmitter {
    /// Create a Solana deposit submitter.
    #[must_use]
    pub fn new(rpc_client: Arc<RpcClient>, payer: Arc<Keypair>) -> Self {
        Self { rpc_client, payer }
    }

    /// Build, sign, submit, and confirm a Gateway USDC deposit from the payer.
    ///
    /// # Errors
    ///
    /// Returns a Gateway Solana error when instruction construction, blockhash
    /// lookup, submission, or confirmation fails.
    pub async fn submit(
        &self,
        owner_usdc_token_account: &Pubkey,
        amount: u64,
    ) -> GatewayResult<TxSignature> {
        let payer = self.payer.pubkey();
        let instruction =
            build_deposit_instruction(&payer, &payer, owner_usdc_token_account, amount)?;
        let blockhash = self
            .rpc_client
            .get_latest_blockhash()
            .await
            .map_err(|error| GatewayError::Solana(format!("get latest blockhash: {error}")))?;
        let transaction = Transaction::new_signed_with_payer(
            &[instruction],
            Some(&payer),
            &[self.payer.as_ref()],
            blockhash,
        );
        let signature = self
            .rpc_client
            .send_and_confirm_transaction(&transaction)
            .await
            .map_err(|error| {
                GatewayError::Solana(format!("send and confirm Gateway deposit: {error}"))
            })?;
        Ok(TxSignature::new(signature.to_string()))
    }
}

impl std::fmt::Debug for GatewayDepositSubmitter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GatewayDepositSubmitter")
            .field("payer", &self.payer.pubkey())
            .finish_non_exhaustive()
    }
}

/// Circle Gateway Solana client configuration.
#[derive(Clone)]
pub struct CircleGatewayClientConfig {
    /// Gateway API base URL.
    pub api_base_url: String,
    /// Gateway balance owner and Solana signing identity.
    pub depositor: Pubkey,
    /// Destination initialized USDC token account or ATA.
    pub destination_recipient_token_account: Pubkey,
    /// Optional signing key required for refill transfer requests.
    pub signing_key: Option<SigningKey>,
    /// Maximum fee in raw USDC units for burn intent signing.
    pub max_fee_raw: u64,
    /// Transfer status polling timeout.
    pub transfer_status_timeout: Duration,
    /// Transfer status poll interval.
    pub transfer_status_poll_interval: Duration,
    /// Program ids.
    pub program_ids: GatewayProgramIds,
    /// Optional live Solana submitter for prepared `gatewayMint` instructions.
    pub mint_submitter: Option<GatewayMintSubmitter>,
}

impl CircleGatewayClientConfig {
    /// Build a mainnet Solana Gateway client config.
    ///
    /// # Errors
    ///
    /// Returns an error if built-in program identifiers cannot be parsed.
    pub fn mainnet(
        depositor: Pubkey,
        destination_recipient_token_account: Pubkey,
        signing_key: Option<SigningKey>,
        max_fee_raw: u64,
    ) -> GatewayResult<Self> {
        Ok(Self {
            api_base_url: GATEWAY_API_BASE_URL.to_owned(),
            depositor,
            destination_recipient_token_account,
            signing_key,
            max_fee_raw,
            transfer_status_timeout: DEFAULT_TRANSFER_STATUS_TIMEOUT,
            transfer_status_poll_interval: DEFAULT_TRANSFER_STATUS_POLL_INTERVAL,
            program_ids: GatewayProgramIds::mainnet()?,
            mint_submitter: None,
        })
    }
}

/// Circle Gateway HTTP client and Solana instruction planner.
#[derive(Clone)]
pub struct CircleGatewayClient {
    http: reqwest::Client,
    config: CircleGatewayClientConfig,
}

impl CircleGatewayClient {
    /// Create a client from explicit config.
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP client cannot be constructed.
    pub fn new(config: CircleGatewayClientConfig) -> GatewayResult<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(DEFAULT_HTTP_CONNECT_TIMEOUT)
            .build()?;
        Ok(Self { http, config })
    }

    /// Build a Gateway balance request for the configured Solana depositor.
    #[must_use]
    pub fn balance_request(&self) -> BalanceRequest {
        BalanceRequest {
            token: USDC_ASSET_ID.to_owned(),
            sources: vec![BalanceSource {
                domain: SOLANA_DOMAIN,
                depositor: self.config.depositor.to_string(),
            }],
        }
    }

    /// Submit signed burn intents to Circle Gateway.
    ///
    /// # Errors
    ///
    /// Returns errors for transport failures, non-success statuses, unreadable
    /// success bodies, terminal transfer failures, or polling timeouts.
    pub async fn submit_transfer(
        &self,
        requests: Vec<BurnIntentRequest>,
    ) -> GatewayResult<TransferResponse> {
        let url = endpoint(&self.config.api_base_url, "transfer");
        let response = self.http.post(url).json(&requests).send().await?;
        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            return Err(GatewayError::ApiStatus { status, body });
        }

        match TransferResponse::from_api_body(&body) {
            Ok(response) => self.refresh_transfer_response(response).await,
            Err(parse_error) => {
                let transfer_id = TransferAcceptedResponse::from_api_body(&body)
                    .ok()
                    .and_then(|response| response.transfer_id)
                    .ok_or_else(|| GatewayError::SuccessResponse {
                        operation: "submit_transfer",
                        reason: parse_error.to_string(),
                        body: truncate_for_error(&body),
                    })?;
                self.poll_transfer_status(&transfer_id).await
            }
        }
    }

    async fn refresh_transfer_response(
        &self,
        response: TransferResponse,
    ) -> GatewayResult<TransferResponse> {
        let Some(transfer_id) = response.transfer_id.clone() else {
            return Ok(response);
        };
        if response.fees.is_some() {
            return Ok(response);
        }

        match self.fetch_transfer_status(&transfer_id).await {
            Ok(status_response) => {
                let mut response = response;
                if response.fees.is_none() {
                    response.fees = status_response.fees;
                }
                if response.expiration_block.is_none() {
                    response.expiration_block = status_response
                        .attestation
                        .and_then(|attestation| attestation.expiration_block);
                }
                Ok(response)
            }
            Err(_) => Ok(response),
        }
    }

    /// Fetch detailed transfer status by provider id.
    ///
    /// # Errors
    ///
    /// Returns transport, status, or schema errors from the Gateway API.
    pub async fn fetch_transfer_status(
        &self,
        transfer_id: &str,
    ) -> GatewayResult<TransferStatusResponse> {
        let url = endpoint(
            &self.config.api_base_url,
            &format!("transfer/{transfer_id}"),
        );
        let response = self
            .http
            .get(url)
            .timeout(self.config.transfer_status_timeout)
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            return Err(GatewayError::ApiStatus { status, body });
        }

        TransferStatusResponse::from_api_body(&body).map_err(|error| {
            GatewayError::SuccessResponse {
                operation: "get_transfer",
                reason: error.to_string(),
                body: truncate_for_error(&body),
            }
        })
    }

    async fn poll_transfer_status(&self, transfer_id: &str) -> GatewayResult<TransferResponse> {
        let started = Instant::now();
        let mut last_status = None;
        let mut last_error = None;

        loop {
            match self.fetch_transfer_status(transfer_id).await {
                Ok(status_response) => {
                    let status = status_response.status;
                    last_status = Some(status.as_str().to_owned());

                    if let Some(response) = TransferResponse::from_status_response(
                        transfer_id.to_owned(),
                        status_response.clone(),
                    ) {
                        return Ok(response);
                    }

                    if let Some(reason) = status_response.terminal_failure_reason() {
                        return Err(GatewayError::TransferStatusFailed {
                            operation: "submit_transfer",
                            transfer_id: transfer_id.to_owned(),
                            status: status.as_str().to_owned(),
                            reason,
                        });
                    }
                }
                Err(error) => {
                    last_error = Some(error.to_string());
                }
            }

            if started.elapsed() >= self.config.transfer_status_timeout {
                return Err(GatewayError::TransferStatusTimeout {
                    operation: "submit_transfer",
                    transfer_id: transfer_id.to_owned(),
                    duration_ms: duration_millis_u64(self.config.transfer_status_timeout),
                    last_status,
                    last_error,
                });
            }

            tokio::time::sleep(self.config.transfer_status_poll_interval).await;
        }
    }

    fn solana_refill_spec(&self, amount_raw: u64, salt: [u8; 32]) -> TransferSpecSolana {
        TransferSpecSolana {
            version: 1,
            source_domain: SOLANA_DOMAIN,
            dest_domain: SOLANA_DOMAIN,
            source_contract: pubkey_to_bytes32(&self.config.program_ids.wallet_program),
            dest_contract: pubkey_to_bytes32(&self.config.program_ids.minter_program),
            source_token: pubkey_to_bytes32(&self.config.program_ids.usdc_mint),
            dest_token: pubkey_to_bytes32(&self.config.program_ids.usdc_mint),
            source_depositor: pubkey_to_bytes32(&self.config.depositor),
            dest_recipient: pubkey_to_bytes32(&self.config.destination_recipient_token_account),
            source_signer: pubkey_to_bytes32(&self.config.depositor),
            dest_caller: [0u8; 32],
            value: amount_to_u256_be(amount_raw),
            salt,
            hook_data: Vec::new(),
        }
    }

    /// Submit a same-domain Solana Gateway transfer and prepare its mint instruction.
    ///
    /// # Errors
    ///
    /// Returns an error for non-USDC assets, missing signing key, Circle API
    /// failures, invalid attestation hex, or invalid attestation structure.
    pub async fn prepare_refill(
        &self,
        request: GatewayRefillRequest,
    ) -> GatewayResult<GatewayRefillPlan> {
        if request.amount.asset.as_str() != USDC_ASSET_ID {
            return Err(GatewayError::UnsupportedAsset(
                request.amount.asset.to_string(),
            ));
        }
        let signing_key = self
            .config
            .signing_key
            .as_ref()
            .ok_or(GatewayError::MissingSigningKey)?;

        let spec = self.solana_refill_spec(request.amount.amount_raw.as_u64(), fresh_salt_bytes());
        let burn_request = build_solana_burn_intent_request(
            signing_key,
            u64::MAX,
            self.config.max_fee_raw,
            &spec,
        )?;
        let transfer = self.submit_transfer(vec![burn_request]).await?;
        let attestation = decode_hex_bytes("attestation", &transfer.attestation)?;
        let attestation_signature = decode_hex_bytes("attestation_signature", &transfer.signature)?;
        let elements = parse_attestation_elements(&attestation)?;
        let mint_instruction = build_gateway_mint_instruction(
            &self.config.depositor,
            &attestation,
            &attestation_signature,
            &elements,
        )?;

        Ok(GatewayRefillPlan {
            amount: request.amount,
            provider_transfer_id: transfer.transfer_id,
            attestation,
            attestation_signature,
            mint_instruction,
        })
    }
}

#[async_trait]
impl GatewayClient for CircleGatewayClient {
    async fn balance(&self, asset: AssetId) -> Result<GatewayReceipt, AppError> {
        if asset.as_str() != USDC_ASSET_ID {
            return Err(GatewayError::UnsupportedAsset(asset.to_string()).into());
        }

        let url = endpoint(&self.config.api_base_url, "balances");
        let response = self
            .http
            .post(url)
            .json(&self.balance_request())
            .send()
            .await
            .map_err(GatewayError::from)?;
        let status = response.status();
        let body = response.text().await.map_err(GatewayError::from)?;
        if !status.is_success() {
            return Err(GatewayError::ApiStatus { status, body }.into());
        }

        let balance_response: BalanceResponse =
            serde_json::from_str(&body).map_err(|error| GatewayError::SuccessResponse {
                operation: "balance",
                reason: error.to_string(),
                body: truncate_for_error(&body),
            })?;
        let total = balance_response
            .balances
            .iter()
            .filter(|entry| {
                entry.domain == SOLANA_DOMAIN
                    && entry.depositor == self.config.depositor.to_string()
            })
            .try_fold(0u64, |acc, entry| {
                acc.checked_add(entry.amount_raw).ok_or_else(|| {
                    GatewayError::NumericRange("Gateway balance sum overflowed u64".to_owned())
                })
            })?;

        Ok(GatewayReceipt {
            amount: TokenAmount::new(asset, AmountRaw::new(total)),
            provider_transfer_id: None,
            signature: None,
        })
    }

    async fn request_refill(
        &self,
        request: GatewayRefillRequest,
    ) -> Result<GatewayReceipt, AppError> {
        let plan = self.prepare_refill(request).await?;
        let submitter = self.config.mint_submitter.as_ref().ok_or_else(|| {
            AppError::unsupported(format!(
                "Gateway refill prepared but Solana gatewayMint submission is not configured; provider_transfer_id={}",
                plan.provider_transfer_id.as_deref().unwrap_or("unknown")
            ))
        })?;
        let signature = submitter.submit(plan.mint_instruction).await?;
        Ok(GatewayReceipt {
            amount: plan.amount,
            provider_transfer_id: plan.provider_transfer_id,
            signature: Some(signature),
        })
    }
}

/// Build a Gateway balance event from a receipt.
#[must_use]
pub fn balance_checked_event(metadata: EventMetadata, receipt: &GatewayReceipt) -> GatewayEvent {
    GatewayEvent::BalanceChecked {
        metadata,
        balance: receipt.amount.clone(),
    }
}

/// Build a Gateway refill completed event from a receipt.
#[must_use]
pub fn refill_completed_event(metadata: EventMetadata, receipt: GatewayReceipt) -> GatewayEvent {
    GatewayEvent::RefillCompleted { metadata, receipt }
}

/// Build a Gateway failure event.
#[must_use]
pub fn gateway_failed_event(metadata: EventMetadata, reason: impl Into<String>) -> GatewayEvent {
    GatewayEvent::Failed {
        metadata,
        reason: reason.into(),
    }
}

/// Build a Gateway receipt for a submitted Solana transaction.
#[must_use]
pub fn gateway_receipt_with_signature(
    amount: TokenAmount,
    provider_transfer_id: Option<String>,
    signature: impl Into<String>,
) -> GatewayReceipt {
    GatewayReceipt {
        amount,
        provider_transfer_id,
        signature: Some(TxSignature::new(signature)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::Verifier;

    type SampleAttestationElement = (Pubkey, Pubkey, u64, [u8; 32], Vec<u8>);

    fn mainnet_ids() -> GatewayProgramIds {
        GatewayProgramIds::mainnet().expect("mainnet ids")
    }

    fn sample_attestation(elements: &[SampleAttestationElement]) -> Vec<u8> {
        let mut payload = vec![0u8; ATTESTATION_HEADER_SIZE];
        payload[0..4].copy_from_slice(&0x10cb_b1ecu32.to_be_bytes());
        payload[4..8].copy_from_slice(&1u32.to_be_bytes());
        payload[8..12].copy_from_slice(&SOLANA_DOMAIN.to_be_bytes());
        let element_count = u32::try_from(elements.len()).expect("sample element count fits u32");
        payload[84..88].copy_from_slice(&element_count.to_be_bytes());

        for (dest_token, dest_recipient, value, hash, hook_data) in elements {
            payload.extend_from_slice(&dest_token.to_bytes());
            payload.extend_from_slice(&dest_recipient.to_bytes());
            payload.extend_from_slice(&value.to_be_bytes());
            payload.extend_from_slice(hash);
            let hook_data_len =
                u32::try_from(hook_data.len()).expect("sample hook data length fits u32");
            payload.extend_from_slice(&hook_data_len.to_be_bytes());
            payload.extend_from_slice(hook_data);
        }

        payload
    }

    #[test]
    fn pda_derivation_matches_munger_and_circle_docs() {
        let ids = mainnet_ids();
        let owner = Pubkey::new_unique();
        let hash = [42u8; 32];

        assert_eq!(
            gateway_wallet_pda(&ids.wallet_program),
            Pubkey::find_program_address(&[b"gateway_wallet"], &ids.wallet_program)
        );
        assert_eq!(
            gateway_wallet_custody_pda(&ids.wallet_program, &ids.usdc_mint),
            Pubkey::find_program_address(
                &[b"gateway_wallet_custody", ids.usdc_mint.as_ref()],
                &ids.wallet_program
            )
        );
        assert_eq!(
            gateway_deposit_pda(&ids.wallet_program, &ids.usdc_mint, &owner),
            Pubkey::find_program_address(
                &[b"gateway_deposit", ids.usdc_mint.as_ref(), owner.as_ref()],
                &ids.wallet_program
            )
        );
        assert_eq!(
            depositor_denylist_pda(&ids.wallet_program, &owner),
            Pubkey::find_program_address(&[b"denylist", owner.as_ref()], &ids.wallet_program)
        );
        assert_eq!(
            gateway_minter_pda(&ids.minter_program),
            Pubkey::find_program_address(&[b"gateway_minter"], &ids.minter_program)
        );
        assert_eq!(
            gateway_minter_custody_pda(&ids.minter_program, &ids.usdc_mint),
            Pubkey::find_program_address(
                &[b"gateway_minter_custody", ids.usdc_mint.as_ref()],
                &ids.minter_program
            )
        );
        assert_eq!(
            used_transfer_spec_hash_pda(&ids.minter_program, &hash),
            Pubkey::find_program_address(&[b"used_transfer_spec_hash", &hash], &ids.minter_program)
        );
    }

    #[test]
    fn deposit_instruction_accounts_and_data_match_gateway_idl() {
        let ids = mainnet_ids();
        let payer = Pubkey::new_unique();
        let owner = payer;
        let owner_ata = Pubkey::new_unique();
        let amount = 1_234_567u64;
        let instruction =
            build_deposit_instruction(&payer, &owner, &owner_ata, amount).expect("instruction");

        assert_eq!(instruction.program_id, ids.wallet_program);
        assert_eq!(instruction.accounts.len(), 11);
        assert_eq!(instruction.accounts[0], AccountMeta::new(payer, true));
        assert_eq!(
            instruction.accounts[1],
            AccountMeta::new_readonly(owner, true)
        );
        assert_eq!(instruction.accounts[3], AccountMeta::new(owner_ata, false));
        assert!(instruction.accounts[4].is_writable);
        assert!(instruction.accounts[5].is_writable);
        assert_eq!(instruction.accounts[7].pubkey, ids.token_program);
        assert_eq!(instruction.accounts[8].pubkey, ids.system_program);
        assert_eq!(instruction.accounts[10].pubkey, ids.wallet_program);
        assert_eq!(&instruction.data[..2], &DEPOSIT_DISCRIMINATOR);
        assert_eq!(
            u64::from_le_bytes(instruction.data[2..10].try_into().unwrap()),
            amount
        );
    }

    #[test]
    fn attestation_parser_reads_multiple_elements_and_hook_offsets() {
        let token_a = parse_pubkey("usdc", USDC_MINT).unwrap();
        let recipient_a = Pubkey::new_unique();
        let hash_a = [0xabu8; 32];
        let token_b = Pubkey::new_unique();
        let recipient_b = Pubkey::new_unique();
        let hash_b = [0xcdu8; 32];
        let payload = sample_attestation(&[
            (token_a, recipient_a, 5_000_000, hash_a, Vec::new()),
            (token_b, recipient_b, 7_000_000, hash_b, vec![1, 2, 3, 4]),
        ]);

        let elements = parse_attestation_elements(&payload).expect("elements");

        assert_eq!(elements.len(), 2);
        assert_eq!(elements[0].dest_token, token_a);
        assert_eq!(elements[0].dest_recipient, recipient_a);
        assert_eq!(elements[0].value, 5_000_000);
        assert_eq!(elements[0].transfer_spec_hash, hash_a);
        assert_eq!(elements[1].dest_token, token_b);
        assert_eq!(elements[1].dest_recipient, recipient_b);
        assert_eq!(elements[1].value, 7_000_000);
        assert_eq!(elements[1].transfer_spec_hash, hash_b);
    }

    #[test]
    fn attestation_parser_rejects_truncated_hook_data() {
        let token = parse_pubkey("usdc", USDC_MINT).unwrap();
        let recipient = Pubkey::new_unique();
        let mut payload =
            sample_attestation(&[(token, recipient, 1_000_000, [0xaau8; 32], vec![1, 2, 3, 4])]);
        payload.truncate(payload.len() - 2);

        assert!(matches!(
            parse_attestation_elements(&payload),
            Err(GatewayError::InvalidAttestation(_))
        ));
    }

    #[test]
    fn gateway_mint_instruction_uses_remaining_accounts_from_attestation() {
        let ids = mainnet_ids();
        let payer = Pubkey::new_unique();
        let dest_token = parse_pubkey("usdc", USDC_MINT).unwrap();
        let dest_recipient = Pubkey::new_unique();
        let transfer_hash = [0x42u8; 32];
        let attestation = sample_attestation(&[(
            dest_token,
            dest_recipient,
            1_000_000,
            transfer_hash,
            Vec::new(),
        )]);
        let elements = parse_attestation_elements(&attestation).expect("elements");
        let signature = vec![0x55; 65];

        let instruction =
            build_gateway_mint_instruction(&payer, &attestation, &signature, &elements)
                .expect("mint instruction");

        let (expected_custody, _) = gateway_minter_custody_pda(&ids.minter_program, &dest_token);
        let (expected_used_hash, _) =
            used_transfer_spec_hash_pda(&ids.minter_program, &transfer_hash);
        assert_eq!(instruction.program_id, ids.minter_program);
        assert_eq!(instruction.accounts.len(), 10);
        assert_eq!(instruction.accounts[0], AccountMeta::new(payer, true));
        assert_eq!(
            instruction.accounts[1],
            AccountMeta::new_readonly(payer, true)
        );
        assert_eq!(
            instruction.accounts[7],
            AccountMeta::new(expected_custody, false)
        );
        assert_eq!(
            instruction.accounts[8],
            AccountMeta::new(dest_recipient, false)
        );
        assert_eq!(
            instruction.accounts[9],
            AccountMeta::new(expected_used_hash, false)
        );
        assert_eq!(&instruction.data[..2], &GATEWAY_MINT_DISCRIMINATOR);
        let attestation_len =
            u32::from_le_bytes(instruction.data[2..6].try_into().unwrap()) as usize;
        assert_eq!(attestation_len, attestation.len());
        let sig_offset = 6 + attestation_len;
        let signature_len = u32::from_le_bytes(
            instruction.data[sig_offset..sig_offset + 4]
                .try_into()
                .unwrap(),
        ) as usize;
        assert_eq!(signature_len, signature.len());
    }

    #[test]
    fn burn_intent_encoding_uses_magic_big_endian_and_transfer_spec_length() {
        let spec = TransferSpecSolana {
            version: 1,
            source_domain: SOLANA_DOMAIN,
            dest_domain: SOLANA_DOMAIN,
            source_contract: [1; 32],
            dest_contract: [2; 32],
            source_token: [3; 32],
            dest_token: [4; 32],
            source_depositor: [5; 32],
            dest_recipient: [6; 32],
            source_signer: [7; 32],
            dest_caller: [0; 32],
            value: amount_to_u256_be(1_000_000),
            salt: [9; 32],
            hook_data: vec![0xaa, 0xbb],
        };

        let encoded =
            encode_burn_intent(&amount_to_u256_be(u64::MAX), &amount_to_u256_be(5), &spec)
                .expect("burn intent encoding");

        assert_eq!(&encoded[0..4], &BURN_INTENT_MAGIC.to_be_bytes());
        assert_eq!(&encoded[4..36], &amount_to_u256_be(u64::MAX));
        assert_eq!(&encoded[36..68], &amount_to_u256_be(5));
        let spec_len = u32::from_be_bytes(encoded[68..72].try_into().unwrap()) as usize;
        let spec_start = 72;
        assert_eq!(spec_len, encoded.len() - spec_start);
        assert_eq!(
            &encoded[spec_start..spec_start + 4],
            &TRANSFER_SPEC_MAGIC.to_be_bytes()
        );
        assert_eq!(
            &encoded[spec_start + 4..spec_start + 8],
            &spec.version.to_be_bytes()
        );
    }

    #[test]
    fn ed25519_signing_uses_solana_gateway_prefix_and_api_hex_format() {
        let signing_key = SigningKey::from_bytes(&[42; 32]);
        let burn_intent = vec![1, 2, 3, 4, 5];
        let message = solana_burn_intent_signing_message(&burn_intent);
        assert_eq!(&message[..16], &SOLANA_BURN_INTENT_SIGNING_DOMAIN);
        assert_eq!(&message[16..], burn_intent.as_slice());

        let signature_bytes = sign_burn_intent_ed25519(&signing_key, &burn_intent);
        let signature =
            ed25519_dalek::Signature::from_bytes(signature_bytes.as_slice().try_into().unwrap());
        assert!(
            signing_key
                .verifying_key()
                .verify(&message, &signature)
                .is_ok()
        );

        let spec = TransferSpecSolana {
            version: 1,
            source_domain: SOLANA_DOMAIN,
            dest_domain: SOLANA_DOMAIN,
            source_contract: [1; 32],
            dest_contract: [2; 32],
            source_token: [3; 32],
            dest_token: [4; 32],
            source_depositor: [5; 32],
            dest_recipient: [6; 32],
            source_signer: [7; 32],
            dest_caller: [0; 32],
            value: amount_to_u256_be(1),
            salt: [9; 32],
            hook_data: Vec::new(),
        };
        let request =
            build_solana_burn_intent_request(&signing_key, u64::MAX, 2_000_000, &spec).unwrap();
        assert!(request.signature.starts_with("0x"));
        assert_eq!(request.signature.len(), 130);
        assert_eq!(
            request.burn_intent.spec.destination_caller,
            hex_bytes32([0; 32])
        );
        assert_eq!(request.burn_intent.spec.value, "1");
    }

    #[test]
    fn balance_response_parses_decimal_usdc_strings_to_raw_units() {
        let body = r#"{
            "token":"USDC",
            "balances":[
                {"domain":5,"depositor":"abc","balance":"4.000001"},
                {"domain":0,"depositor":"def","balance":"0"}
            ]
        }"#;

        let response: BalanceResponse = serde_json::from_str(body).expect("balance response");

        assert_eq!(response.token, "USDC");
        assert_eq!(response.balances[0].amount_raw, 4_000_001);
        assert_eq!(response.balances[1].amount_raw, 0);
    }

    #[test]
    fn transfer_response_accepts_flat_and_nested_variants_and_missing_fees() {
        let flat = TransferResponse::from_api_body(
            r#"{
                "transferId":"tr_flat",
                "attestation":"0xaaaa",
                "signature":"0xbbbb",
                "expirationBlock":123
            }"#,
        )
        .expect("flat response");
        let nested = TransferResponse::from_api_body(
            r#"{
                "transferId":"tr_nested",
                "attestation":{
                    "payload":"0xcccc",
                    "signature":"0xdddd",
                    "expirationBlock":"456"
                },
                "fees":{"total":"1.178","token":"USDC","perIntent":[]}
            }"#,
        )
        .expect("nested response");

        assert_eq!(flat.transfer_id.as_deref(), Some("tr_flat"));
        assert_eq!(flat.attestation, "0xaaaa");
        assert!(flat.fees.is_none());
        assert_eq!(flat.expiration_block.as_deref(), Some("123"));
        assert_eq!(nested.transfer_id.as_deref(), Some("tr_nested"));
        assert_eq!(nested.attestation, "0xcccc");
        assert_eq!(nested.signature, "0xdddd");
        assert!(nested.fees.is_some());
    }

    #[test]
    fn transfer_id_only_response_and_terminal_failure_status_parse() {
        let accepted = TransferAcceptedResponse::from_api_body(r#"{"transferId":"tr_only"}"#)
            .expect("accepted response");
        let failed = TransferStatusResponse::from_api_body(
            r#"{
                "destinationDomain":5,
                "status":"failed",
                "burnIntents":[],
                "forwardingDetails":{
                    "forwardingEnabled":true,
                    "failureReason":"destination caller mismatch"
                }
            }"#,
        )
        .expect("failed status");
        let expired = TransferStatusResponse::from_api_body(r#"{"status":"expired"}"#)
            .expect("expired status");

        assert_eq!(accepted.transfer_id.as_deref(), Some("tr_only"));
        assert_eq!(
            failed.terminal_failure_reason().as_deref(),
            Some("destination caller mismatch")
        );
        assert_eq!(
            expired.terminal_failure_reason().as_deref(),
            Some("transfer status reached terminal state without attestation")
        );
    }

    #[tokio::test]
    async fn live_gateway_balance_read_skips_without_env() {
        if std::env::var("RUN_LIVE_GATEWAY_TESTS").ok().as_deref() != Some("1") {
            eprintln!("skipping live Gateway balance read: RUN_LIVE_GATEWAY_TESTS is not 1");
            return;
        }

        let Ok(depositor) = std::env::var("CIRCLE_GATEWAY_SOLANA_ADDRESS") else {
            eprintln!("skipping live Gateway balance read: CIRCLE_GATEWAY_SOLANA_ADDRESS missing");
            return;
        };
        let depositor = parse_pubkey("CIRCLE_GATEWAY_SOLANA_ADDRESS", &depositor)
            .expect("valid depositor pubkey");
        let mut config = CircleGatewayClientConfig::mainnet(depositor, depositor, None, 0)
            .expect("mainnet config");
        if let Ok(base_url) = std::env::var("CIRCLE_GATEWAY_API_BASE_URL") {
            config.api_base_url = base_url;
        }
        let client = CircleGatewayClient::new(config).expect("client");

        let receipt = client
            .balance(AssetId::from("USDC"))
            .await
            .expect("balance");

        assert_eq!(receipt.amount.asset.as_str(), "USDC");
    }

    #[tokio::test]
    async fn live_gateway_deposit_usdc_skips_without_explicit_opt_in() {
        if std::env::var("RUN_LIVE_SOLANA_TESTS").ok().as_deref() != Some("1")
            || std::env::var("RUN_LIVE_GATEWAY_TESTS").ok().as_deref() != Some("1")
            || std::env::var("RUN_LIVE_GATEWAY_DEPOSIT_TESTS")
                .ok()
                .as_deref()
                != Some("1")
        {
            eprintln!(
                "skipping live Gateway deposit: RUN_LIVE_SOLANA_TESTS=1, RUN_LIVE_GATEWAY_TESTS=1, and RUN_LIVE_GATEWAY_DEPOSIT_TESTS=1 are required"
            );
            return;
        }

        let Some(rpc_url) = nonempty_test_env("SOLANA_RPC_URL") else {
            eprintln!("skipping live Gateway deposit: SOLANA_RPC_URL is not set");
            return;
        };
        if !crate::adapters::solana::wallets::keypair_source_envs(
            crate::domain::types::WalletRole::Maker,
        )
        .into_iter()
        .any(|name| std::env::var(name).is_ok())
        {
            eprintln!("skipping live Gateway deposit: maker keypair env vars are not set");
            return;
        }
        let Some(depositor) = nonempty_test_env("CIRCLE_GATEWAY_SOLANA_ADDRESS") else {
            eprintln!("skipping live Gateway deposit: CIRCLE_GATEWAY_SOLANA_ADDRESS is not set");
            return;
        };

        let maker =
            crate::adapters::solana::wallets::LoadedWallet::from_maker_env().expect("load maker");
        let depositor = parse_pubkey("CIRCLE_GATEWAY_SOLANA_ADDRESS", &depositor)
            .expect("valid Gateway depositor pubkey");
        assert_eq!(
            depositor,
            maker.pubkey(),
            "CIRCLE_GATEWAY_SOLANA_ADDRESS must be the approved maker/depositor wallet for this smoke test"
        );

        let amount_raw = live_gateway_deposit_amount_raw();
        assert!(
            (1..=MAX_LIVE_GATEWAY_DEPOSIT_USDC_RAW).contains(&amount_raw),
            "LIVE_GATEWAY_DEPOSIT_USDC_RAW must be between 1 and {MAX_LIVE_GATEWAY_DEPOSIT_USDC_RAW}"
        );

        let solana_config = crate::config::SolanaConfig::default();
        let solana_client =
            crate::adapters::solana::client::SolanaClient::new(rpc_url, solana_config.commitment)
                .expect("live Solana client");
        let rpc_client = solana_client.rpc_client();
        let usdc_mint = parse_pubkey("USDC_MINT", USDC_MINT).expect("USDC mint");
        let maker_usdc_ata =
            crate::adapters::solana::client::SolanaClient::derive_associated_token_address(
                &maker.pubkey(),
                &usdc_mint,
                &crate::adapters::solana::client::LEGACY_TOKEN_PROGRAM_ID,
            );
        let token_balance = rpc_client
            .get_token_account_balance(&maker_usdc_ata)
            .await
            .expect("maker USDC ATA must exist and be readable before Gateway deposit");
        let available_raw = token_balance
            .amount
            .parse::<u64>()
            .expect("USDC token balance amount should parse as u64");
        assert!(
            available_raw >= amount_raw,
            "maker USDC ATA {maker_usdc_ata} has {available_raw} raw USDC, need {amount_raw} raw USDC for Gateway deposit"
        );

        let payer = Arc::new(maker.try_clone_keypair().expect("clone maker"));
        let submitter = GatewayDepositSubmitter::new(rpc_client, payer);
        let signature = submitter
            .submit(&maker_usdc_ata, amount_raw)
            .await
            .expect("submit live Gateway deposit");

        assert!(!signature.as_str().is_empty());
        eprintln!("live Gateway deposit submitted: {}", signature.as_str());
    }

    #[tokio::test]
    async fn live_gateway_refill_usdc_skips_without_explicit_opt_in() {
        if std::env::var("RUN_LIVE_SOLANA_TESTS").ok().as_deref() != Some("1")
            || std::env::var("RUN_LIVE_GATEWAY_TESTS").ok().as_deref() != Some("1")
            || std::env::var("RUN_LIVE_GATEWAY_REFILL_TESTS")
                .ok()
                .as_deref()
                != Some("1")
        {
            eprintln!(
                "skipping live Gateway refill: RUN_LIVE_SOLANA_TESTS=1, RUN_LIVE_GATEWAY_TESTS=1, and RUN_LIVE_GATEWAY_REFILL_TESTS=1 are required"
            );
            return;
        }

        let Some(fixture) = live_gateway_refill_fixture() else {
            return;
        };
        let before_raw = live_usdc_token_balance_raw(
            &fixture.rpc_client,
            &fixture.maker_usdc_ata,
            "before Gateway refill",
        )
        .await;
        assert_gateway_has_refill_capacity(
            &fixture.client,
            fixture.amount_raw,
            fixture.max_fee_raw,
        )
        .await;

        let receipt = fixture
            .client
            .request_refill(GatewayRefillRequest {
                amount: TokenAmount::new(AssetId::from("USDC"), AmountRaw::new(fixture.amount_raw)),
                destination: crate::domain::types::WalletRole::Maker,
            })
            .await
            .expect("submit live Gateway refill");
        assert_gateway_refill_receipt(&receipt, fixture.amount_raw);

        let after_raw = live_usdc_token_balance_raw(
            &fixture.rpc_client,
            &fixture.maker_usdc_ata,
            "after Gateway refill",
        )
        .await;
        assert_live_refill_increased_working_usdc(before_raw, after_raw, fixture.amount_raw);

        if let Some(signature) = receipt.signature.as_ref() {
            eprintln!("live Gateway refill submitted: {}", signature.as_str());
        }
    }

    struct LiveGatewayRefillFixture {
        rpc_client: Arc<RpcClient>,
        maker_usdc_ata: Pubkey,
        client: CircleGatewayClient,
        amount_raw: u64,
        max_fee_raw: u64,
    }

    fn live_gateway_refill_fixture() -> Option<LiveGatewayRefillFixture> {
        let Some(rpc_url) = nonempty_test_env("SOLANA_RPC_URL") else {
            eprintln!("skipping live Gateway refill: SOLANA_RPC_URL is not set");
            return None;
        };
        if !crate::adapters::solana::wallets::keypair_source_envs(
            crate::domain::types::WalletRole::Maker,
        )
        .into_iter()
        .any(|name| std::env::var(name).is_ok())
        {
            eprintln!("skipping live Gateway refill: maker keypair env vars are not set");
            return None;
        }
        let Some(depositor) = nonempty_test_env("CIRCLE_GATEWAY_SOLANA_ADDRESS") else {
            eprintln!("skipping live Gateway refill: CIRCLE_GATEWAY_SOLANA_ADDRESS is not set");
            return None;
        };

        let maker =
            crate::adapters::solana::wallets::LoadedWallet::from_maker_env().expect("load maker");
        let depositor = parse_pubkey("CIRCLE_GATEWAY_SOLANA_ADDRESS", &depositor)
            .expect("valid Gateway depositor pubkey");
        assert_eq!(
            depositor,
            maker.pubkey(),
            "CIRCLE_GATEWAY_SOLANA_ADDRESS must be the approved maker/depositor wallet for this smoke test"
        );

        let amount_raw = live_gateway_refill_amount_raw();
        assert!(
            (1..=MAX_LIVE_GATEWAY_REFILL_USDC_RAW).contains(&amount_raw),
            "LIVE_GATEWAY_REFILL_USDC_RAW must be between 1 and {MAX_LIVE_GATEWAY_REFILL_USDC_RAW}"
        );
        let max_fee_raw = live_gateway_refill_max_fee_raw();
        assert!(
            (MIN_LIVE_GATEWAY_REFILL_MAX_FEE_RAW..=MAX_LIVE_GATEWAY_REFILL_MAX_FEE_RAW)
                .contains(&max_fee_raw),
            "LIVE_GATEWAY_REFILL_MAX_FEE_RAW must be between {MIN_LIVE_GATEWAY_REFILL_MAX_FEE_RAW} and {MAX_LIVE_GATEWAY_REFILL_MAX_FEE_RAW}"
        );

        let solana_config = crate::config::SolanaConfig::default();
        let solana_client =
            crate::adapters::solana::client::SolanaClient::new(rpc_url, solana_config.commitment)
                .expect("live Solana client");
        let rpc_client = solana_client.rpc_client();
        let usdc_mint = parse_pubkey("USDC_MINT", USDC_MINT).expect("USDC mint");
        let maker_usdc_ata =
            crate::adapters::solana::client::SolanaClient::derive_associated_token_address(
                &maker.pubkey(),
                &usdc_mint,
                &crate::adapters::solana::client::LEGACY_TOKEN_PROGRAM_ID,
            );

        let mut config = CircleGatewayClientConfig::mainnet(
            depositor,
            maker_usdc_ata,
            Some(signing_key_from_test_keypair(maker.keypair())),
            max_fee_raw,
        )
        .expect("mainnet config");
        if let Ok(base_url) = std::env::var("CIRCLE_GATEWAY_API_BASE_URL") {
            config.api_base_url = base_url;
        }
        config.mint_submitter = Some(GatewayMintSubmitter::new(
            Arc::clone(&rpc_client),
            Arc::new(maker.try_clone_keypair().expect("clone maker")),
        ));
        let client = CircleGatewayClient::new(config).expect("client");

        Some(LiveGatewayRefillFixture {
            rpc_client,
            maker_usdc_ata,
            client,
            amount_raw,
            max_fee_raw,
        })
    }

    async fn live_usdc_token_balance_raw(
        rpc_client: &RpcClient,
        token_account: &Pubkey,
        label: &'static str,
    ) -> u64 {
        let token_balance = rpc_client
            .get_token_account_balance(token_account)
            .await
            .unwrap_or_else(|_| {
                panic!("maker USDC ATA must exist and be readable {label}");
            });
        token_balance
            .amount
            .parse::<u64>()
            .expect("USDC token balance amount should parse as u64")
    }

    async fn assert_gateway_has_refill_capacity(
        client: &CircleGatewayClient,
        amount_raw: u64,
        max_fee_raw: u64,
    ) {
        let before_gateway = client
            .balance(AssetId::from("USDC"))
            .await
            .expect("Gateway balance before refill");
        let gateway_available_raw = before_gateway.amount.amount_raw.as_u64();
        let gateway_needed_raw = amount_raw
            .checked_add(max_fee_raw)
            .expect("test Gateway amount and max fee addition should not overflow");
        assert!(
            gateway_available_raw >= gateway_needed_raw,
            "Gateway balance has {gateway_available_raw} raw USDC, need at least {gateway_needed_raw} raw USDC for refill amount plus max fee"
        );
    }

    fn assert_gateway_refill_receipt(receipt: &GatewayReceipt, amount_raw: u64) {
        assert_eq!(receipt.amount.asset.as_str(), "USDC");
        assert_eq!(receipt.amount.amount_raw.as_u64(), amount_raw);
        assert!(
            receipt
                .provider_transfer_id
                .as_deref()
                .is_some_and(|id| !id.is_empty())
        );
        assert!(
            receipt
                .signature
                .as_ref()
                .is_some_and(|signature| !signature.as_str().is_empty())
        );
    }

    fn assert_live_refill_increased_working_usdc(before_raw: u64, after_raw: u64, amount_raw: u64) {
        let expected_min_raw = before_raw
            .checked_add(amount_raw)
            .expect("test balance addition should not overflow");
        assert!(
            after_raw >= expected_min_raw,
            "maker USDC ATA should increase by at least {amount_raw} raw USDC after Gateway refill; before={before_raw}, after={after_raw}"
        );
    }

    #[tokio::test]
    async fn live_mutating_gateway_tests_are_explicitly_gated() {
        let solana_enabled = std::env::var("RUN_LIVE_SOLANA_TESTS").ok().as_deref() == Some("1");
        let gateway_enabled = std::env::var("RUN_LIVE_GATEWAY_TESTS").ok().as_deref() == Some("1");
        let mutating_enabled = std::env::var("RUN_LIVE_GATEWAY_MUTATING_TESTS")
            .ok()
            .as_deref()
            == Some("1");
        if !(solana_enabled && gateway_enabled && mutating_enabled) {
            eprintln!(
                "skipping mutating Gateway smoke: RUN_LIVE_SOLANA_TESTS, RUN_LIVE_GATEWAY_TESTS, and RUN_LIVE_GATEWAY_MUTATING_TESTS must all be 1"
            );
            return;
        }

        let has_wallet = std::env::var("MAKER_PRIVATE_KEY").is_ok()
            || std::env::var("MAKER_KEYPAIR_JSON").is_ok()
            || std::env::var("MAKER_KEYPAIR_PATH").is_ok();
        let has_depositor = std::env::var("CIRCLE_GATEWAY_SOLANA_ADDRESS").is_ok();
        if !(has_wallet && has_depositor) {
            eprintln!(
                "skipping mutating Gateway smoke: maker keypair and CIRCLE_GATEWAY_SOLANA_ADDRESS are required"
            );
            return;
        }

        eprintln!("mutating Gateway smoke is intentionally not executed by unit tests");
    }

    const DEFAULT_LIVE_GATEWAY_DEPOSIT_USDC_RAW: u64 = 1_000_000;
    const MAX_LIVE_GATEWAY_DEPOSIT_USDC_RAW: u64 = 1_000_000;
    const DEFAULT_LIVE_GATEWAY_REFILL_USDC_RAW: u64 = 1_000_000;
    const MAX_LIVE_GATEWAY_REFILL_USDC_RAW: u64 = 1_000_000;
    const DEFAULT_LIVE_GATEWAY_REFILL_MAX_FEE_RAW: u64 = 250_000;
    const MIN_LIVE_GATEWAY_REFILL_MAX_FEE_RAW: u64 = 150_000;
    const MAX_LIVE_GATEWAY_REFILL_MAX_FEE_RAW: u64 = 500_000;

    fn live_gateway_deposit_amount_raw() -> u64 {
        std::env::var("LIVE_GATEWAY_DEPOSIT_USDC_RAW")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(DEFAULT_LIVE_GATEWAY_DEPOSIT_USDC_RAW)
    }

    fn live_gateway_refill_amount_raw() -> u64 {
        std::env::var("LIVE_GATEWAY_REFILL_USDC_RAW")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(DEFAULT_LIVE_GATEWAY_REFILL_USDC_RAW)
    }

    fn live_gateway_refill_max_fee_raw() -> u64 {
        std::env::var("LIVE_GATEWAY_REFILL_MAX_FEE_RAW")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(DEFAULT_LIVE_GATEWAY_REFILL_MAX_FEE_RAW)
    }

    fn signing_key_from_test_keypair(keypair: &Keypair) -> SigningKey {
        let bytes = keypair.to_bytes();
        let mut secret = [0_u8; 32];
        secret.copy_from_slice(&bytes[..32]);
        SigningKey::from_bytes(&secret)
    }

    fn nonempty_test_env(name: &str) -> Option<String> {
        std::env::var(name)
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
    }
}
