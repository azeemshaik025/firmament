//! Circle Gateway Solana burn-intent encoding and signing.

use std::time::{SystemTime, UNIX_EPOCH};

use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};
use solana_sdk::pubkey::Pubkey;

use super::api::{BurnIntentData, BurnIntentRequest, TransferSpecData};
use super::{
    BURN_INTENT_MAGIC, GatewayError, GatewayResult, SOLANA_BURN_INTENT_SIGNING_DOMAIN,
    TRANSFER_SPEC_MAGIC, hex_bytes32, u256_be_to_u64,
};

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
