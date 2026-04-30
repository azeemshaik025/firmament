//! Jupiter Swap API V2 wire models and response parsing.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::error::AppError;

use super::client::JUPITER_SERVICE;

/// Jupiter `/order` platform fee object.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct JupiterPlatformFee {
    /// Fee raw amount.
    #[serde(default)]
    pub amount: Option<String>,
    /// Fee basis points.
    #[serde(rename = "feeBps")]
    pub fee_bps: Option<u16>,
    /// Fee mint.
    #[serde(rename = "feeMint", default)]
    pub fee_mint: Option<String>,
}

/// Jupiter `/order` response.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct JupiterOrderResponse {
    /// Request id required for `/execute`.
    #[serde(rename = "requestId")]
    pub request_id: String,
    /// Input mint.
    #[serde(rename = "inputMint")]
    pub input_mint: String,
    /// Output mint.
    #[serde(rename = "outputMint")]
    pub output_mint: String,
    /// Raw input amount.
    #[serde(rename = "inAmount")]
    pub in_amount: String,
    /// Raw output amount.
    #[serde(rename = "outAmount")]
    pub out_amount: String,
    /// Base64 unsigned transaction. Null for quote-only responses.
    #[serde(default)]
    pub transaction: Option<String>,
    /// Swap mode.
    #[serde(rename = "swapMode", default)]
    pub swap_mode: Option<String>,
    /// Slippage in basis points.
    #[serde(rename = "slippageBps", default)]
    pub slippage_bps: Option<u16>,
    /// Winning router.
    #[serde(default)]
    pub router: Option<String>,
    /// Fee mint.
    #[serde(rename = "feeMint", default)]
    pub fee_mint: Option<String>,
    /// Fee bps.
    #[serde(rename = "feeBps", default)]
    pub fee_bps: Option<u16>,
    /// Platform fee details.
    #[serde(rename = "platformFee", default)]
    pub platform_fee: Option<JupiterPlatformFee>,
    /// Last valid block height for execute nonce validation.
    #[serde(rename = "lastValidBlockHeight", default)]
    pub last_valid_block_height: Option<String>,
    /// RFQ expiration timestamp.
    #[serde(rename = "expireAt", default)]
    pub expire_at: Option<String>,
    /// Jupiter order-level error code.
    #[serde(rename = "errorCode", default)]
    pub error_code: Option<i64>,
    /// Jupiter order-level error message.
    #[serde(rename = "errorMessage", default)]
    pub error_message: Option<String>,
    /// Backward-compatible error string.
    #[serde(default)]
    pub error: Option<String>,
}

/// Jupiter execute status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum JupiterExecuteStatus {
    /// Transaction landed successfully according to Jupiter.
    Success,
    /// Transaction failed according to Jupiter.
    Failed,
    /// Future/unknown status.
    #[serde(other)]
    Unknown,
}

/// Jupiter `/execute` swap event.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct JupiterSwapEvent {
    /// Input mint.
    #[serde(rename = "inputMint")]
    pub input_mint: String,
    /// Input raw amount.
    #[serde(rename = "inputAmount")]
    pub input_amount: String,
    /// Output mint.
    #[serde(rename = "outputMint")]
    pub output_mint: String,
    /// Output raw amount.
    #[serde(rename = "outputAmount")]
    pub output_amount: String,
}

/// Jupiter `/execute` response.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct JupiterExecuteResponse {
    /// Execution status.
    pub status: JupiterExecuteStatus,
    /// Transaction signature.
    #[serde(default)]
    pub signature: Option<String>,
    /// Confirmed slot.
    #[serde(default)]
    pub slot: Option<String>,
    /// Jupiter error string.
    #[serde(default)]
    pub error: Option<String>,
    /// Jupiter error code.
    #[serde(default)]
    pub code: Option<i64>,
    /// Total input amount before fees.
    #[serde(rename = "totalInputAmount", default)]
    pub total_input_amount: Option<String>,
    /// Total output amount after fees.
    #[serde(rename = "totalOutputAmount", default)]
    pub total_output_amount: Option<String>,
    /// Actual input amount used.
    #[serde(rename = "inputAmountResult", default)]
    pub input_amount_result: Option<String>,
    /// Actual output amount received.
    #[serde(rename = "outputAmountResult", default)]
    pub output_amount_result: Option<String>,
    /// Parsed swap events.
    #[serde(rename = "swapEvents", default)]
    pub swap_events: Vec<JupiterSwapEvent>,
}

/// Raw Jupiter instruction returned by `/build`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct JupiterInstruction {
    /// Program id.
    #[serde(rename = "programId")]
    pub program_id: String,
    /// Instruction accounts.
    #[serde(default)]
    pub accounts: Vec<JupiterInstructionAccount>,
    /// Base64 instruction data.
    pub data: String,
}

/// Jupiter instruction account.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct JupiterInstructionAccount {
    /// Account pubkey.
    pub pubkey: String,
    /// Whether the account is writable.
    #[serde(rename = "isWritable")]
    pub is_writable: bool,
    /// Whether the account is a signer.
    #[serde(rename = "isSigner")]
    pub is_signer: bool,
}

/// Blockhash metadata returned by `/build`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct JupiterBlockhashWithMetadata {
    /// Raw blockhash representation from Jupiter.
    pub blockhash: serde_json::Value,
    /// Last valid block height.
    #[serde(rename = "lastValidBlockHeight")]
    pub last_valid_block_height: u64,
}

/// Jupiter `/build` response with raw router instructions.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct JupiterBuildResponse {
    /// Input mint.
    #[serde(rename = "inputMint")]
    pub input_mint: String,
    /// Output mint.
    #[serde(rename = "outputMint")]
    pub output_mint: String,
    /// Raw input amount.
    #[serde(rename = "inAmount")]
    pub in_amount: String,
    /// Raw output amount.
    #[serde(rename = "outAmount")]
    pub out_amount: String,
    /// Slippage threshold.
    #[serde(rename = "otherAmountThreshold", default)]
    pub other_amount_threshold: Option<String>,
    /// Swap mode.
    #[serde(rename = "swapMode", default)]
    pub swap_mode: Option<String>,
    /// Slippage bps.
    #[serde(rename = "slippageBps", default)]
    pub slippage_bps: Option<u16>,
    /// Compute budget instructions.
    #[serde(rename = "computeBudgetInstructions", default)]
    pub compute_budget_instructions: Vec<JupiterInstruction>,
    /// Setup instructions.
    #[serde(rename = "setupInstructions", default)]
    pub setup_instructions: Vec<JupiterInstruction>,
    /// Main swap instruction.
    #[serde(rename = "swapInstruction")]
    pub swap_instruction: JupiterInstruction,
    /// Cleanup instruction.
    #[serde(rename = "cleanupInstruction", default)]
    pub cleanup_instruction: Option<JupiterInstruction>,
    /// Other instructions.
    #[serde(rename = "otherInstructions", default)]
    pub other_instructions: Vec<JupiterInstruction>,
    /// Tip instruction.
    #[serde(rename = "tipInstruction", default)]
    pub tip_instruction: Option<JupiterInstruction>,
    /// Lookup table mapping.
    #[serde(rename = "addressesByLookupTableAddress", default)]
    pub addresses_by_lookup_table_address: serde_json::Value,
    /// Blockhash metadata.
    #[serde(rename = "blockhashWithMetadata", default)]
    pub blockhash_with_metadata: Option<JupiterBlockhashWithMetadata>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct JupiterExecuteRequest {
    #[serde(rename = "signedTransaction")]
    pub(crate) signed_transaction: String,
    #[serde(rename = "requestId")]
    pub(crate) request_id: String,
    #[serde(
        rename = "lastValidBlockHeight",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) last_valid_block_height: Option<String>,
}

pub(crate) fn parse_order_response(body: &str) -> Result<JupiterOrderResponse, AppError> {
    serde_json::from_str(body).map_err(|error| {
        AppError::external_service(JUPITER_SERVICE, format!("decode /order response: {error}"))
    })
}

pub(crate) fn parse_execute_response(body: &str) -> Result<JupiterExecuteResponse, AppError> {
    serde_json::from_str(body).map_err(|error| {
        AppError::external_service(
            JUPITER_SERVICE,
            format!("decode /execute response: {error}"),
        )
    })
}

pub(crate) fn parse_build_response(body: &str) -> Result<JupiterBuildResponse, AppError> {
    serde_json::from_str(body).map_err(|error| {
        AppError::external_service(JUPITER_SERVICE, format!("decode /build response: {error}"))
    })
}

pub(crate) fn ensure_order_is_usable(order: &JupiterOrderResponse) -> Result<(), AppError> {
    if order.error_code.is_some() || order.error_message.is_some() || order.error.is_some() {
        let code = order
            .error_code
            .map(|code| format!("errorCode={code}: "))
            .unwrap_or_default();
        let message = order
            .error_message
            .as_deref()
            .or(order.error.as_deref())
            .unwrap_or("unknown Jupiter order error");
        return Err(AppError::external_service(
            JUPITER_SERVICE,
            format!("{code}{message}"),
        ));
    }
    if order.transaction.as_deref() == Some("") {
        return Err(AppError::external_service(
            JUPITER_SERVICE,
            "Jupiter order returned an empty transaction",
        ));
    }
    parse_raw_amount(&order.in_amount)?;
    parse_raw_amount(&order.out_amount)?;
    Ok(())
}

pub(crate) fn ensure_order_has_transaction(order: &JupiterOrderResponse) -> Result<&str, AppError> {
    let transaction = order.transaction.as_deref().ok_or_else(|| {
        AppError::external_service(JUPITER_SERVICE, "Jupiter order has no transaction to sign")
    })?;
    if transaction.trim().is_empty() {
        return Err(AppError::external_service(
            JUPITER_SERVICE,
            "Jupiter order returned an empty transaction",
        ));
    }
    Ok(transaction)
}

pub(crate) fn ensure_execute_succeeded(execute: &JupiterExecuteResponse) -> Result<(), AppError> {
    if execute.status == JupiterExecuteStatus::Success && execute.code.unwrap_or(0) == 0 {
        if execute
            .signature
            .as_deref()
            .is_some_and(|signature| !signature.trim().is_empty())
        {
            return Ok(());
        }
        return Err(AppError::external_service(
            JUPITER_SERVICE,
            "execute response succeeded without a signature",
        ));
    }

    let code = execute
        .code
        .map(|code| format!("code={code}: "))
        .unwrap_or_default();
    let error = execute
        .error
        .as_deref()
        .unwrap_or("unknown execute failure");
    Err(AppError::external_service(
        JUPITER_SERVICE,
        format!("status={:?}: {code}{error}", execute.status),
    ))
}

pub(crate) fn jupiter_http_error(status: u16, body: &str) -> AppError {
    AppError::external_service(
        JUPITER_SERVICE,
        format!("HTTP {status}: {}", summarize_error_body(body)),
    )
}

fn summarize_error_body(body: &str) -> String {
    if body.trim().is_empty() {
        return "empty response body".to_owned();
    }

    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return body.trim().to_owned();
    };

    let code = value
        .get("errorCode")
        .or_else(|| value.get("code"))
        .and_then(serde_json::Value::as_i64)
        .map(|code| format!("errorCode={code}: "))
        .unwrap_or_default();
    let message = value
        .get("errorMessage")
        .or_else(|| value.get("message"))
        .or_else(|| value.get("error"))
        .and_then(serde_json::Value::as_str)
        .map_or_else(|| value.to_string(), str::to_owned);

    format!("{code}{message}")
}

pub(crate) fn parse_raw_amount(value: &str) -> Result<u64, AppError> {
    value.parse::<u64>().map_err(|error| {
        AppError::external_service(
            JUPITER_SERVICE,
            format!("invalid raw token amount '{value}': {error}"),
        )
    })
}

pub(crate) fn parse_optional_timestamp(
    value: Option<&str>,
) -> Result<Option<OffsetDateTime>, AppError> {
    value
        .map(|timestamp| {
            if let Ok(epoch_seconds) = timestamp.parse::<i64>() {
                return OffsetDateTime::from_unix_timestamp(epoch_seconds).map_err(|error| {
                    AppError::external_service(
                        JUPITER_SERVICE,
                        format!("invalid Jupiter epoch timestamp '{timestamp}': {error}"),
                    )
                });
            }

            OffsetDateTime::parse(timestamp, &time::format_description::well_known::Rfc3339)
                .map_err(|error| {
                    AppError::external_service(
                        JUPITER_SERVICE,
                        format!("invalid Jupiter timestamp '{timestamp}': {error}"),
                    )
                })
        })
        .transpose()
}
