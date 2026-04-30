//! Jupiter transaction signing helpers.

use base64::Engine as _;
use solana_sdk::signature::{Keypair, Signature, Signer};
use solana_sdk::transaction::VersionedTransaction;

use crate::error::AppError;

/// Base64-signed transaction plus the local wallet signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedJupiterTransaction {
    /// Base64-encoded signed versioned transaction.
    pub signed_transaction: String,
    /// Signature produced by the local signer.
    pub derived_signature: String,
}

/// Prepared order ready for `/execute`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedJupiterOrder {
    /// Jupiter request id from `/order`.
    pub request_id: String,
    /// Base64-encoded signed transaction.
    pub signed_transaction: String,
    /// Signature produced by the local signer.
    pub derived_signature: String,
    /// Optional last valid block height from `/order`.
    pub last_valid_block_height: Option<String>,
}

pub(crate) fn sign_preserving_jupiter_message(
    base64_transaction: &str,
    signer: &Keypair,
) -> Result<SignedJupiterTransaction, AppError> {
    let mut transaction = decode_versioned_transaction(base64_transaction)?;
    let signer_pubkey = signer.pubkey();
    let signer_index = transaction
        .message
        .static_account_keys()
        .iter()
        .position(|pubkey| pubkey == &signer_pubkey)
        .ok_or_else(|| {
            AppError::solana(format!(
                "wallet pubkey {signer_pubkey} not found in Jupiter transaction account keys"
            ))
        })?;
    let required_signatures = usize::from(transaction.message.header().num_required_signatures);

    if signer_index >= required_signatures {
        return Err(AppError::solana(format!(
            "wallet pubkey {signer_pubkey} is not a required signer in Jupiter transaction"
        )));
    }
    if transaction.signatures.len() < required_signatures {
        transaction
            .signatures
            .resize(required_signatures, Signature::default());
    }

    let signature = signer.sign_message(&transaction.message.serialize());
    transaction.signatures[signer_index] = signature;
    let signed_transaction = encode_versioned_transaction(&transaction)?;

    Ok(SignedJupiterTransaction {
        signed_transaction,
        derived_signature: signature.to_string(),
    })
}

pub(crate) fn decode_versioned_transaction(
    base64_transaction: &str,
) -> Result<VersionedTransaction, AppError> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(base64_transaction)
        .map_err(|error| AppError::solana(format!("decode Jupiter transaction base64: {error}")))?;
    bincode::deserialize(&bytes)
        .map_err(|error| AppError::solana(format!("deserialize Jupiter transaction: {error}")))
}

pub(crate) fn encode_versioned_transaction(
    transaction: &VersionedTransaction,
) -> Result<String, AppError> {
    let bytes = bincode::serialize(transaction)
        .map_err(|error| AppError::solana(format!("serialize Jupiter transaction: {error}")))?;
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}
