//! Circle Gateway Solana adapter primitives.
//!
//! This module intentionally implements only the Solana Gateway path used by the
//! demo runtime: USDC deposit instruction construction, Gateway balance reads,
//! signed Solana burn intents, transfer attestation submission/polling, and
//! `gatewayMint` instruction construction from decoded attestation elements.

use std::str::FromStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use ed25519_dalek::{Signer, SigningKey};
use reqwest::StatusCode;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
};
use tokio::time::Instant;

use crate::error::AppError;
use crate::events::{EventMetadata, GatewayEvent};
use crate::ports::GatewayClient;
use crate::types::{
    AmountRaw, AssetId, GatewayReceipt, GatewayRefillRequest, TokenAmount, TxSignature,
};

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
        }
    }
}

fn parse_pubkey(label: &'static str, value: &str) -> GatewayResult<Pubkey> {
    Pubkey::from_str(value).map_err(|source| GatewayError::InvalidPubkey { label, source })
}

fn endpoint(base_url: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base_url.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

fn truncate_for_error(body: &str) -> String {
    const MAX_ERROR_BODY: usize = 512;
    if body.len() <= MAX_ERROR_BODY {
        body.to_owned()
    } else {
        format!("{}...", &body[..MAX_ERROR_BODY])
    }
}

fn duration_millis_u64(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn decode_hex_bytes(label: &'static str, value: &str) -> GatewayResult<Vec<u8>> {
    let stripped = value.strip_prefix("0x").unwrap_or(value);
    if stripped.len() % 2 != 0 {
        return Err(GatewayError::InvalidHex {
            label,
            message: "hex string must contain an even number of digits".to_owned(),
        });
    }
    hex::decode(stripped).map_err(|source| GatewayError::InvalidHex {
        label,
        message: source.to_string(),
    })
}

fn hex_bytes32(value: [u8; 32]) -> String {
    format!("0x{}", hex::encode(value))
}

fn decimal_usdc_to_raw(value: &str) -> Result<u64, String> {
    let amount: Decimal = value
        .parse()
        .map_err(|error| format!("parse decimal USDC amount {value}: {error}"))?;
    if amount.is_sign_negative() {
        return Err(format!("USDC amount {value} must not be negative"));
    }
    let subunits = amount * Decimal::from(1_000_000u64);
    subunits
        .round()
        .to_u64()
        .ok_or_else(|| format!("USDC amount {value} out of u64 range"))
}

fn deserialize_usdc_amount_raw<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error as DeError;

    let value = String::deserialize(deserializer)?;
    decimal_usdc_to_raw(&value).map_err(DeError::custom)
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

/// Mainnet Solana Gateway program identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GatewayProgramIds {
    /// Gateway Wallet program id.
    pub wallet_program: Pubkey,
    /// Gateway Minter program id.
    pub minter_program: Pubkey,
    /// USDC mint address.
    pub usdc_mint: Pubkey,
    /// SPL Token program id.
    pub token_program: Pubkey,
    /// System program id.
    pub system_program: Pubkey,
}

impl GatewayProgramIds {
    /// Parse mainnet program identifiers.
    ///
    /// # Errors
    ///
    /// Returns an error if one of the baked-in constants is malformed.
    pub fn mainnet() -> GatewayResult<Self> {
        Ok(Self {
            wallet_program: parse_pubkey("gateway_wallet_program", GATEWAY_WALLET_PROGRAM)?,
            minter_program: parse_pubkey("gateway_minter_program", GATEWAY_MINTER_PROGRAM)?,
            usdc_mint: parse_pubkey("usdc_mint", USDC_MINT)?,
            token_program: parse_pubkey("spl_token_program", SPL_TOKEN_PROGRAM_ID)?,
            system_program: parse_pubkey("system_program", SYSTEM_PROGRAM_ID)?,
        })
    }
}

/// Derive the Gateway Wallet PDA.
#[must_use]
pub fn gateway_wallet_pda(program_id: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[b"gateway_wallet"], program_id)
}

/// Derive the Gateway Wallet custody PDA for a token mint.
#[must_use]
pub fn gateway_wallet_custody_pda(program_id: &Pubkey, token_mint: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[b"gateway_wallet_custody", token_mint.as_ref()],
        program_id,
    )
}

/// Derive the Gateway deposit PDA for a token mint and depositor.
#[must_use]
pub fn gateway_deposit_pda(
    program_id: &Pubkey,
    token_mint: &Pubkey,
    depositor: &Pubkey,
) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[b"gateway_deposit", token_mint.as_ref(), depositor.as_ref()],
        program_id,
    )
}

/// Derive the Gateway Wallet denylist PDA for a depositor.
#[must_use]
pub fn depositor_denylist_pda(program_id: &Pubkey, depositor: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[b"denylist", depositor.as_ref()], program_id)
}

/// Derive the Gateway Minter PDA.
#[must_use]
pub fn gateway_minter_pda(program_id: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[b"gateway_minter"], program_id)
}

/// Derive the Gateway Minter custody PDA for a token mint.
#[must_use]
pub fn gateway_minter_custody_pda(program_id: &Pubkey, token_mint: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[b"gateway_minter_custody", token_mint.as_ref()],
        program_id,
    )
}

/// Derive the replay-protection PDA for a transfer spec hash.
#[must_use]
pub fn used_transfer_spec_hash_pda(program_id: &Pubkey, hash: &[u8; 32]) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[b"used_transfer_spec_hash", hash], program_id)
}

fn event_authority_pda(program_id: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[b"__event_authority"], program_id)
}

/// Build a Solana Gateway Wallet `deposit` instruction.
///
/// # Errors
///
/// Returns an error if the baked-in program constants cannot be parsed.
pub fn build_deposit_instruction(
    payer: &Pubkey,
    owner: &Pubkey,
    owner_usdc_token_account: &Pubkey,
    amount: u64,
) -> GatewayResult<Instruction> {
    let ids = GatewayProgramIds::mainnet()?;
    let (gateway_wallet, _) = gateway_wallet_pda(&ids.wallet_program);
    let (custody, _) = gateway_wallet_custody_pda(&ids.wallet_program, &ids.usdc_mint);
    let (deposit, _) = gateway_deposit_pda(&ids.wallet_program, &ids.usdc_mint, owner);
    let (denylist, _) = depositor_denylist_pda(&ids.wallet_program, owner);
    let (event_authority, _) = event_authority_pda(&ids.wallet_program);

    let mut data = Vec::with_capacity(10);
    data.extend_from_slice(&DEPOSIT_DISCRIMINATOR);
    data.extend_from_slice(&amount.to_le_bytes());

    Ok(Instruction {
        program_id: ids.wallet_program,
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new_readonly(*owner, true),
            AccountMeta::new_readonly(gateway_wallet, false),
            AccountMeta::new(*owner_usdc_token_account, false),
            AccountMeta::new(custody, false),
            AccountMeta::new(deposit, false),
            AccountMeta::new_readonly(denylist, false),
            AccountMeta::new_readonly(ids.token_program, false),
            AccountMeta::new_readonly(ids.system_program, false),
            AccountMeta::new_readonly(event_authority, false),
            AccountMeta::new_readonly(ids.wallet_program, false),
        ],
        data,
    })
}

/// One decoded reduced Gateway mint attestation element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttestationElement {
    /// Destination token mint.
    pub dest_token: Pubkey,
    /// Destination recipient token account.
    pub dest_recipient: Pubkey,
    /// Raw mint amount.
    pub value: u64,
    /// `TransferSpec` hash used for replay protection.
    pub transfer_spec_hash: [u8; 32],
}

/// Parse all reduced Solana mint attestation elements.
///
/// # Errors
///
/// Returns an error when the payload is truncated or internally inconsistent.
pub fn parse_attestation_elements(attestation: &[u8]) -> GatewayResult<Vec<AttestationElement>> {
    if attestation.len() < ATTESTATION_HEADER_SIZE {
        return Err(GatewayError::InvalidAttestation(format!(
            "payload must be at least {ATTESTATION_HEADER_SIZE} bytes"
        )));
    }

    let count_bytes: [u8; 4] = attestation[NUM_ATTESTATIONS_OFFSET..ATTESTATION_HEADER_SIZE]
        .try_into()
        .map_err(|_| GatewayError::InvalidAttestation("missing attestation count".to_owned()))?;
    let count = u32::from_be_bytes(count_bytes) as usize;
    let max_possible_count = (attestation.len() - ATTESTATION_HEADER_SIZE) / ELEM_FIXED_SIZE;
    if count > max_possible_count {
        return Err(GatewayError::InvalidAttestation(format!(
            "header declares {count} attestations but payload can contain at most {max_possible_count}"
        )));
    }

    let mut cursor = ATTESTATION_HEADER_SIZE;
    let mut elements = Vec::with_capacity(count);
    for index in 0..count {
        if attestation.len() < cursor + ELEM_FIXED_SIZE {
            return Err(GatewayError::InvalidAttestation(format!(
                "attestation element {index} is truncated"
            )));
        }

        let element = &attestation[cursor..cursor + ELEM_FIXED_SIZE];
        let dest_token = Pubkey::try_from(&element[ELEM_DEST_TOKEN..ELEM_DEST_TOKEN + 32])
            .map_err(|error| GatewayError::InvalidAttestation(error.to_string()))?;
        let dest_recipient =
            Pubkey::try_from(&element[ELEM_DEST_RECIPIENT..ELEM_DEST_RECIPIENT + 32])
                .map_err(|error| GatewayError::InvalidAttestation(error.to_string()))?;

        let mut value_bytes = [0u8; 8];
        value_bytes.copy_from_slice(&element[ELEM_VALUE..ELEM_VALUE + 8]);
        let value = u64::from_be_bytes(value_bytes);

        let mut transfer_spec_hash = [0u8; 32];
        transfer_spec_hash
            .copy_from_slice(&element[ELEM_TRANSFER_SPEC_HASH..ELEM_TRANSFER_SPEC_HASH + 32]);

        let hook_data_len_bytes: [u8; 4] = element[ELEM_HOOK_DATA_LENGTH..ELEM_FIXED_SIZE]
            .try_into()
            .map_err(|_| GatewayError::InvalidAttestation("missing hook length".to_owned()))?;
        let hook_data_len = u32::from_be_bytes(hook_data_len_bytes) as usize;
        let total_len = ELEM_FIXED_SIZE.checked_add(hook_data_len).ok_or_else(|| {
            GatewayError::InvalidAttestation("hook data length overflow".to_owned())
        })?;
        let next_cursor = cursor.checked_add(total_len).ok_or_else(|| {
            GatewayError::InvalidAttestation("attestation cursor overflow".to_owned())
        })?;
        if next_cursor > attestation.len() {
            return Err(GatewayError::InvalidAttestation(format!(
                "attestation element {index} hook data is truncated"
            )));
        }

        elements.push(AttestationElement {
            dest_token,
            dest_recipient,
            value,
            transfer_spec_hash,
        });
        cursor = next_cursor;
    }

    Ok(elements)
}

/// Build a Solana Gateway Minter `gatewayMint` instruction from decoded elements.
///
/// # Errors
///
/// Returns an error if the baked-in program constants cannot be parsed.
pub fn build_gateway_mint_instruction(
    payer: &Pubkey,
    attestation: &[u8],
    signature: &[u8],
    elements: &[AttestationElement],
) -> GatewayResult<Instruction> {
    let ids = GatewayProgramIds::mainnet()?;
    let attestation_len = u32::try_from(attestation.len())
        .map_err(|_| GatewayError::NumericRange("attestation length exceeds u32".to_owned()))?;
    let signature_len = u32::try_from(signature.len())
        .map_err(|_| GatewayError::NumericRange("signature length exceeds u32".to_owned()))?;
    let (gateway_minter, _) = gateway_minter_pda(&ids.minter_program);
    let (event_authority, _) = event_authority_pda(&ids.minter_program);

    let mut accounts = vec![
        AccountMeta::new(*payer, true),
        AccountMeta::new_readonly(*payer, true),
        AccountMeta::new_readonly(gateway_minter, false),
        AccountMeta::new_readonly(ids.system_program, false),
        AccountMeta::new_readonly(ids.token_program, false),
        AccountMeta::new_readonly(event_authority, false),
        AccountMeta::new_readonly(ids.minter_program, false),
    ];

    for element in elements {
        let (custody, _) = gateway_minter_custody_pda(&ids.minter_program, &element.dest_token);
        let (used_hash, _) =
            used_transfer_spec_hash_pda(&ids.minter_program, &element.transfer_spec_hash);
        accounts.push(AccountMeta::new(custody, false));
        accounts.push(AccountMeta::new(element.dest_recipient, false));
        accounts.push(AccountMeta::new(used_hash, false));
    }

    let mut data = Vec::with_capacity(10 + attestation.len() + signature.len());
    data.extend_from_slice(&GATEWAY_MINT_DISCRIMINATOR);
    data.extend_from_slice(&attestation_len.to_le_bytes());
    data.extend_from_slice(attestation);
    data.extend_from_slice(&signature_len.to_le_bytes());
    data.extend_from_slice(signature);

    Ok(Instruction {
        program_id: ids.minter_program,
        accounts,
        data,
    })
}

/// Solana-native Gateway transfer spec fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferSpecSolana {
    /// Gateway transfer spec version.
    pub version: u32,
    /// Source Circle domain.
    pub source_domain: u32,
    /// Destination Circle domain.
    pub dest_domain: u32,
    /// Source Gateway Wallet program.
    pub source_contract: [u8; 32],
    /// Destination Gateway Minter program.
    pub dest_contract: [u8; 32],
    /// Source token.
    pub source_token: [u8; 32],
    /// Destination token.
    pub dest_token: [u8; 32],
    /// Source depositor.
    pub source_depositor: [u8; 32],
    /// Destination recipient token account.
    pub dest_recipient: [u8; 32],
    /// Source signer.
    pub source_signer: [u8; 32],
    /// Destination caller constraint, zeroed when anyone may submit.
    pub dest_caller: [u8; 32],
    /// Amount as u256 big-endian bytes.
    pub value: [u8; 32],
    /// 32-byte salt.
    pub salt: [u8; 32],
    /// Optional hook data.
    pub hook_data: Vec<u8>,
}

/// Convert a Solana pubkey to Gateway's 32-byte address field.
#[must_use]
pub fn pubkey_to_bytes32(pubkey: &Pubkey) -> [u8; 32] {
    pubkey.to_bytes()
}

/// Convert a raw u64 amount to Gateway's u256 big-endian field.
#[must_use]
pub fn amount_to_u256_be(amount: u64) -> [u8; 32] {
    let mut result = [0u8; 32];
    result[24..].copy_from_slice(&amount.to_be_bytes());
    result
}

/// Generate a fresh 32-byte salt.
#[must_use]
pub fn fresh_salt_bytes() -> [u8; 32] {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or_else(
        |error| error.duration().as_nanos(),
        |duration| duration.as_nanos(),
    );
    let hash = Sha256::digest(nanos.to_le_bytes());
    let mut salt = [0u8; 32];
    salt.copy_from_slice(&hash);
    salt
}

/// Encode a Solana Gateway transfer spec.
///
/// # Errors
///
/// Returns an error when hook data length exceeds the protocol's u32 length
/// prefix.
pub fn encode_transfer_spec(spec: &TransferSpecSolana) -> GatewayResult<Vec<u8>> {
    let hook_data_len = u32::try_from(spec.hook_data.len())
        .map_err(|_| GatewayError::NumericRange("hook data length exceeds u32".to_owned()))?;
    let mut buffer = Vec::with_capacity(296 + spec.hook_data.len());
    buffer.extend_from_slice(&TRANSFER_SPEC_MAGIC.to_be_bytes());
    buffer.extend_from_slice(&spec.version.to_be_bytes());
    buffer.extend_from_slice(&spec.source_domain.to_be_bytes());
    buffer.extend_from_slice(&spec.dest_domain.to_be_bytes());
    buffer.extend_from_slice(&spec.source_contract);
    buffer.extend_from_slice(&spec.dest_contract);
    buffer.extend_from_slice(&spec.source_token);
    buffer.extend_from_slice(&spec.dest_token);
    buffer.extend_from_slice(&spec.source_depositor);
    buffer.extend_from_slice(&spec.dest_recipient);
    buffer.extend_from_slice(&spec.source_signer);
    buffer.extend_from_slice(&spec.dest_caller);
    buffer.extend_from_slice(&spec.value);
    buffer.extend_from_slice(&spec.salt);
    buffer.extend_from_slice(&hook_data_len.to_be_bytes());
    buffer.extend_from_slice(&spec.hook_data);
    Ok(buffer)
}

/// Encode a Solana Gateway burn intent.
///
/// # Errors
///
/// Returns an error when the nested transfer spec cannot be encoded or exceeds
/// the protocol's u32 length prefix.
pub fn encode_burn_intent(
    max_block_height: &[u8; 32],
    max_fee: &[u8; 32],
    transfer_spec: &TransferSpecSolana,
) -> GatewayResult<Vec<u8>> {
    let spec_bytes = encode_transfer_spec(transfer_spec)?;
    let spec_len = u32::try_from(spec_bytes.len()).map_err(|_| {
        GatewayError::NumericRange("encoded transfer spec length exceeds u32".to_owned())
    })?;
    let mut buffer = Vec::with_capacity(72 + spec_bytes.len());
    buffer.extend_from_slice(&BURN_INTENT_MAGIC.to_be_bytes());
    buffer.extend_from_slice(max_block_height);
    buffer.extend_from_slice(max_fee);
    buffer.extend_from_slice(&spec_len.to_be_bytes());
    buffer.extend_from_slice(&spec_bytes);
    Ok(buffer)
}

/// Build the exact bytes signed by the Solana Ed25519 key.
#[must_use]
pub fn solana_burn_intent_signing_message(burn_intent_bytes: &[u8]) -> Vec<u8> {
    let mut message =
        Vec::with_capacity(SOLANA_BURN_INTENT_SIGNING_DOMAIN.len() + burn_intent_bytes.len());
    message.extend_from_slice(&SOLANA_BURN_INTENT_SIGNING_DOMAIN);
    message.extend_from_slice(burn_intent_bytes);
    message
}

/// Sign a Solana burn intent with Ed25519.
#[must_use]
pub fn sign_burn_intent_ed25519(signing_key: &SigningKey, burn_intent_bytes: &[u8]) -> Vec<u8> {
    let message = solana_burn_intent_signing_message(burn_intent_bytes);
    signing_key.sign(&message).to_bytes().to_vec()
}

/// Convert a Solana transfer spec into Circle Gateway API JSON fields.
///
/// # Errors
///
/// Returns an error when the u256 amount exceeds the demo adapter's u64 range.
pub fn transfer_spec_to_api_data(spec: &TransferSpecSolana) -> GatewayResult<TransferSpecData> {
    Ok(TransferSpecData {
        version: spec.version,
        source_domain: spec.source_domain,
        destination_domain: spec.dest_domain,
        source_contract: hex_bytes32(spec.source_contract),
        destination_contract: hex_bytes32(spec.dest_contract),
        source_token: hex_bytes32(spec.source_token),
        destination_token: hex_bytes32(spec.dest_token),
        source_depositor: hex_bytes32(spec.source_depositor),
        destination_recipient: hex_bytes32(spec.dest_recipient),
        source_signer: hex_bytes32(spec.source_signer),
        destination_caller: hex_bytes32(spec.dest_caller),
        value: u256_be_to_u64(&spec.value)?.to_string(),
        salt: hex_bytes32(spec.salt),
        hook_data: format!("0x{}", hex::encode(&spec.hook_data)),
    })
}

/// Build and sign a Circle Gateway Solana burn intent request.
///
/// # Errors
///
/// Returns an error when API formatting cannot be produced.
pub fn build_solana_burn_intent_request(
    signing_key: &SigningKey,
    max_block_height: u64,
    max_fee: u64,
    spec: &TransferSpecSolana,
) -> GatewayResult<BurnIntentRequest> {
    let burn_intent_bytes = encode_burn_intent(
        &amount_to_u256_be(max_block_height),
        &amount_to_u256_be(max_fee),
        spec,
    )?;
    let signature = sign_burn_intent_ed25519(signing_key, &burn_intent_bytes);

    Ok(BurnIntentRequest {
        burn_intent: BurnIntentData {
            max_block_height: max_block_height.to_string(),
            max_fee: max_fee.to_string(),
            spec: transfer_spec_to_api_data(spec)?,
        },
        signature: format!("0x{}", hex::encode(signature)),
    })
}

/// Request body for Gateway balance lookups.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct BalanceRequest {
    /// Token symbol. Only USDC is supported by Gateway.
    pub token: String,
    /// Sources to query.
    pub sources: Vec<BalanceSource>,
}

/// Balance lookup source.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct BalanceSource {
    /// Circle domain id.
    pub domain: u32,
    /// Depositor address.
    pub depositor: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
struct BalanceResponse {
    token: String,
    balances: Vec<BalanceEntry>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
struct BalanceEntry {
    domain: u32,
    depositor: String,
    #[serde(rename = "balance", deserialize_with = "deserialize_usdc_amount_raw")]
    amount_raw: u64,
}

/// Signed burn intent request accepted by `POST /v1/transfer`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BurnIntentRequest {
    /// Burn intent data.
    pub burn_intent: BurnIntentData,
    /// User signature over the encoded burn intent.
    pub signature: String,
}

/// Gateway burn intent JSON shape.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BurnIntentData {
    /// Expiration block/slot.
    pub max_block_height: String,
    /// Maximum fee in raw USDC units.
    pub max_fee: String,
    /// Transfer spec fields.
    pub spec: TransferSpecData,
}

/// Gateway transfer spec JSON shape.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TransferSpecData {
    /// Gateway transfer spec version.
    pub version: u32,
    /// Source domain.
    pub source_domain: u32,
    /// Destination domain.
    pub destination_domain: u32,
    /// Source contract as 0x-prefixed bytes32 hex.
    pub source_contract: String,
    /// Destination contract as 0x-prefixed bytes32 hex.
    pub destination_contract: String,
    /// Source token as 0x-prefixed bytes32 hex.
    pub source_token: String,
    /// Destination token as 0x-prefixed bytes32 hex.
    pub destination_token: String,
    /// Source depositor as 0x-prefixed bytes32 hex.
    pub source_depositor: String,
    /// Destination recipient as 0x-prefixed bytes32 hex.
    pub destination_recipient: String,
    /// Source signer as 0x-prefixed bytes32 hex.
    pub source_signer: String,
    /// Destination caller as 0x-prefixed bytes32 hex.
    pub destination_caller: String,
    /// Transfer value in raw token units.
    pub value: String,
    /// Salt as 0x-prefixed bytes32 hex.
    pub salt: String,
    /// Hook data as 0x-prefixed hex.
    pub hook_data: String,
}

/// Successful Gateway transfer response with mint attestation data.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TransferResponse {
    /// Provider transfer id.
    pub transfer_id: Option<String>,
    /// Reduced attestation bytes as 0x-prefixed hex.
    pub attestation: String,
    /// Circle signature over the attestation as 0x-prefixed hex.
    pub signature: String,
    /// Fee breakdown, when supplied.
    pub fees: Option<TransferFees>,
    /// Destination expiration block/slot, when supplied.
    pub expiration_block: Option<String>,
}

impl TransferResponse {
    /// Parse either flat or nested attestation response variants.
    ///
    /// # Errors
    ///
    /// Returns the JSON parser error when neither variant matches.
    pub fn from_api_body(body: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str::<TransferResponseWire>(body).map(Into::into)
    }

    fn from_status_response(
        transfer_id: String,
        status_response: TransferStatusResponse,
    ) -> Option<Self> {
        status_response.attestation.map(|attestation| Self {
            transfer_id: Some(transfer_id),
            attestation: attestation.payload,
            signature: attestation.signature,
            fees: status_response.fees,
            expiration_block: attestation.expiration_block,
        })
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
enum TransferResponseWire {
    Flat(TransferResponseFlat),
    Nested(TransferResponseNested),
}

impl From<TransferResponseWire> for TransferResponse {
    fn from(value: TransferResponseWire) -> Self {
        match value {
            TransferResponseWire::Flat(flat) => Self {
                transfer_id: flat.transfer_id,
                attestation: flat.attestation,
                signature: flat.signature,
                fees: flat.fees,
                expiration_block: flat.expiration_block,
            },
            TransferResponseWire::Nested(nested) => Self {
                transfer_id: nested.transfer_id,
                attestation: nested.attestation.payload,
                signature: nested.attestation.signature,
                fees: nested.fees,
                expiration_block: nested.attestation.expiration_block,
            },
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct TransferResponseFlat {
    transfer_id: Option<String>,
    attestation: String,
    signature: String,
    fees: Option<TransferFees>,
    #[serde(default, deserialize_with = "stringish_opt")]
    expiration_block: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct TransferResponseNested {
    transfer_id: Option<String>,
    attestation: AttestationEnvelope,
    fees: Option<TransferFees>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct TransferAcceptedResponse {
    transfer_id: Option<String>,
}

impl TransferAcceptedResponse {
    fn from_api_body(body: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(body)
    }
}

/// Gateway transfer fee breakdown.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TransferFees {
    /// Total fee.
    pub total: String,
    /// Fee token.
    pub token: String,
    /// Per-intent fee breakdown.
    #[serde(default)]
    pub per_intent: Vec<TransferFeePerIntent>,
    /// Optional forwarding fee.
    pub forwarding_fee: Option<String>,
}

/// Gateway transfer fee for a single burn intent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TransferFeePerIntent {
    /// Transfer spec hash.
    pub transfer_spec_hash: String,
    /// Source domain.
    pub domain: u32,
    /// Base fee.
    pub base_fee: String,
    /// Transfer fee.
    pub transfer_fee: String,
}

/// Detailed transfer status from `GET /v1/transfer/{id}`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TransferStatusResponse {
    /// Transfer status.
    pub status: TransferStatus,
    /// Forwarding service details, when present.
    #[serde(default)]
    pub forwarding_details: Option<TransferForwardingDetails>,
    /// Fee breakdown, when present.
    #[serde(default)]
    pub fees: Option<TransferFees>,
    /// Attestation envelope, when manual minting is available.
    #[serde(default)]
    pub attestation: Option<AttestationEnvelope>,
}

impl TransferStatusResponse {
    /// Parse a status response body.
    ///
    /// # Errors
    ///
    /// Returns the JSON parser error when the body does not match the schema.
    pub fn from_api_body(body: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(body)
    }

    /// Return a terminal failure reason if this status is failed or expired.
    #[must_use]
    pub fn terminal_failure_reason(&self) -> Option<String> {
        match self.status {
            TransferStatus::Failed | TransferStatus::Expired => Some(
                self.forwarding_details
                    .as_ref()
                    .and_then(|details| details.failure_reason.clone())
                    .unwrap_or_else(|| {
                        "transfer status reached terminal state without attestation".to_owned()
                    }),
            ),
            TransferStatus::Pending | TransferStatus::Confirmed | TransferStatus::Finalized => None,
        }
    }
}

/// Forwarding details returned by Gateway.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TransferForwardingDetails {
    /// Whether forwarding was enabled.
    #[serde(default)]
    pub forwarding_enabled: Option<bool>,
    /// Forwarding failure reason.
    #[serde(default)]
    pub failure_reason: Option<String>,
}

/// Transfer status labels returned by Gateway.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TransferStatus {
    /// Transfer accepted but not finished.
    Pending,
    /// Destination mint transaction confirmed.
    Confirmed,
    /// Destination mint transaction finalized.
    Finalized,
    /// Transfer failed.
    Failed,
    /// Attestation expired.
    Expired,
}

impl TransferStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Confirmed => "confirmed",
            Self::Finalized => "finalized",
            Self::Failed => "failed",
            Self::Expired => "expired",
        }
    }
}

/// Attestation envelope returned by transfer status or nested transfer responses.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AttestationEnvelope {
    /// Reduced attestation bytes as 0x-prefixed hex.
    pub payload: String,
    /// Circle signature over the attestation as 0x-prefixed hex.
    pub signature: String,
    /// Destination expiration block/slot.
    #[serde(default, deserialize_with = "stringish_opt")]
    pub expiration_block: Option<String>,
}

fn stringish_opt<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Stringish {
        String(String),
        Number(u64),
    }

    Option::<Stringish>::deserialize(deserializer).map(|value| match value {
        Some(Stringish::String(value)) => Some(value),
        Some(Stringish::Number(value)) => Some(value.to_string()),
        None => None,
    })
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
    /// Optional bearer token. Gateway is permissionless, so this is normally unset.
    pub api_key: Option<String>,
    /// Transfer status polling timeout.
    pub transfer_status_timeout: Duration,
    /// Transfer status poll interval.
    pub transfer_status_poll_interval: Duration,
    /// Program ids.
    pub program_ids: GatewayProgramIds,
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
            api_key: None,
            transfer_status_timeout: DEFAULT_TRANSFER_STATUS_TIMEOUT,
            transfer_status_poll_interval: DEFAULT_TRANSFER_STATUS_POLL_INTERVAL,
            program_ids: GatewayProgramIds::mainnet()?,
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
        let mut request = self.http.post(url).json(&requests);
        if let Some(api_key) = &self.config.api_key {
            request = request.bearer_auth(api_key);
        }

        let response = request.send().await?;
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
        let mut request = self
            .http
            .get(url)
            .timeout(self.config.transfer_status_timeout);
        if let Some(api_key) = &self.config.api_key {
            request = request.bearer_auth(api_key);
        }

        let response = request.send().await?;
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
        let mut request = self.http.post(url).json(&self.balance_request());
        if let Some(api_key) = &self.config.api_key {
            request = request.bearer_auth(api_key);
        }

        let response = request.send().await.map_err(GatewayError::from)?;
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
        Ok(GatewayReceipt {
            amount: plan.amount,
            provider_transfer_id: plan.provider_transfer_id,
            signature: None,
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
        if let Ok(api_key) = std::env::var("CIRCLE_API_KEY") {
            config.api_key = Some(api_key);
        }
        let client = CircleGatewayClient::new(config).expect("client");

        let receipt = client
            .balance(AssetId::from("USDC"))
            .await
            .expect("balance");

        assert_eq!(receipt.amount.asset.as_str(), "USDC");
    }

    #[tokio::test]
    async fn live_mutating_gateway_tests_are_explicitly_gated() {
        let solana_enabled = std::env::var("RUN_LIVE_SOLANA_TESTS").ok().as_deref() == Some("1");
        let gateway_enabled = std::env::var("RUN_LIVE_GATEWAY_TESTS").ok().as_deref() == Some("1");
        if !(solana_enabled && gateway_enabled) {
            eprintln!(
                "skipping mutating Gateway smoke: RUN_LIVE_SOLANA_TESTS and RUN_LIVE_GATEWAY_TESTS must both be 1"
            );
            return;
        }

        let has_wallet = std::env::var("MAKER_KEYPAIR_JSON").is_ok()
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
}
