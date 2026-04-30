//! Circle Gateway HTTP API wire models and helpers.

use std::time::Duration;

use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Deserializer, Serialize};

use super::{GatewayError, GatewayResult};

pub(crate) fn endpoint(base_url: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base_url.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

pub(crate) fn truncate_for_error(body: &str) -> String {
    const MAX_ERROR_BODY: usize = 512;
    if body.len() <= MAX_ERROR_BODY {
        body.to_owned()
    } else {
        format!("{}...", &body[..MAX_ERROR_BODY])
    }
}

pub(crate) fn duration_millis_u64(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

pub(crate) fn decode_hex_bytes(label: &'static str, value: &str) -> GatewayResult<Vec<u8>> {
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
pub(crate) struct BalanceResponse {
    pub(crate) token: String,
    pub(crate) balances: Vec<BalanceEntry>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub(crate) struct BalanceEntry {
    pub(crate) domain: u32,
    pub(crate) depositor: String,
    #[serde(rename = "balance", deserialize_with = "deserialize_usdc_amount_raw")]
    pub(crate) amount_raw: u64,
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

    pub(crate) fn from_status_response(
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
    pub(crate) transfer_id: Option<String>,
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
pub(crate) struct TransferAcceptedResponse {
    pub(crate) transfer_id: Option<String>,
}

impl TransferAcceptedResponse {
    pub(crate) fn from_api_body(body: &str) -> Result<Self, serde_json::Error> {
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
    pub(crate) fn as_str(self) -> &'static str {
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
