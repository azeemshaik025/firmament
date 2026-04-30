//! HTLC data encoding, decoding, discriminators, and hashlock helpers.

use borsh::{BorshDeserialize, BorshSerialize};
use rand::RngCore;
use sha2::{Digest, Sha256};
use solana_sdk::pubkey::Pubkey;
use thiserror::Error;

/// Anchor discriminator for `initiate`.
pub const INITIATE_DISC: [u8; 8] = [5, 63, 123, 113, 153, 75, 148, 14];

/// Anchor discriminator for `redeem`.
pub const REDEEM_DISC: [u8; 8] = [184, 12, 86, 149, 70, 196, 97, 225];

/// Anchor discriminator for `refund`.
pub const REFUND_DISC: [u8; 8] = [2, 96, 183, 251, 63, 208, 46, 46];

/// Anchor discriminator for `instant_refund`.
pub const INSTANT_REFUND_DISC: [u8; 8] = [211, 202, 103, 41, 183, 147, 59, 251];

/// Anchor discriminator for the on-chain swap account.
pub const SWAP_ACCOUNT_DISC: [u8; 8] = [53, 126, 9, 14, 14, 197, 105, 182];

/// Errors produced while encoding or decoding HTLC data.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum HtlcEncodingError {
    /// Instruction or account data did not contain enough bytes.
    #[error("data too short: expected at least {expected} bytes, got {actual}")]
    DataTooShort {
        /// Minimum expected byte count.
        expected: usize,
        /// Actual byte count received.
        actual: usize,
    },

    /// The first 8 bytes did not match the expected Anchor discriminator.
    #[error("unexpected discriminator: expected {expected:?}, got {actual:?}")]
    UnexpectedDiscriminator {
        /// Expected Anchor discriminator.
        expected: [u8; 8],
        /// Actual Anchor discriminator.
        actual: [u8; 8],
    },

    /// Hex parsing failed.
    #[error("invalid hex: {0}")]
    InvalidHex(String),

    /// A hex preimage or hash was not 32 bytes after decoding.
    #[error("expected 32 bytes, got {0}")]
    InvalidSecretLength(usize),

    /// Borsh decoding failed.
    #[error("borsh decode failed: {0}")]
    BorshDecode(String),
}

/// Native or SPL HTLC program family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HtlcProgramKind {
    /// Native SOL swaps program.
    Native,
    /// SPL token swaps program.
    Spl,
}

/// HTLC instruction detected from the Anchor discriminator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HtlcInstructionType {
    /// `initiate`.
    Initiate,
    /// `redeem`.
    Redeem,
    /// `refund`.
    Refund,
    /// `instant_refund`.
    InstantRefund,
}

/// Arguments for native SOL `initiate`.
#[derive(BorshSerialize, BorshDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct InitiateNativeData {
    /// Lamports placed into escrow.
    pub amount_lamports: u64,
    /// Relative expiry in Solana slots.
    pub expires_in_slots: u64,
    /// Wallet allowed to redeem with the secret.
    pub redeemer: Pubkey,
    /// SHA-256 hash of the 32-byte secret.
    pub secret_hash: [u8; 32],
}

/// Arguments for SPL `initiate`.
#[derive(BorshSerialize, BorshDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct InitiateSplData {
    /// Relative expiry in Solana slots.
    pub expires_in_slots: u64,
    /// Wallet allowed to redeem with the secret.
    pub redeemer: Pubkey,
    /// SHA-256 hash of the 32-byte secret.
    pub secret_hash: [u8; 32],
    /// Raw token amount placed into escrow.
    pub swap_amount: u64,
    /// Munger destination data passthrough. `None` is the v1 runtime default.
    pub destination_data: Option<Vec<u8>>,
}

/// Arguments for native/SPL `redeem`.
#[derive(BorshSerialize, BorshDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct RedeemData {
    /// 32-byte preimage whose SHA-256 hash matches the escrow hashlock.
    pub secret: [u8; 32],
}

/// Decoded native swap account.
#[derive(BorshSerialize, BorshDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct NativeSwapAccount {
    /// Lamports in escrow.
    pub amount_lamports: u64,
    /// Absolute expiry slot stored on-chain.
    pub expiry_slot: u64,
    /// Original funder.
    pub initiator: Pubkey,
    /// Authorized redeemer.
    pub redeemer: Pubkey,
    /// Hashlock.
    pub secret_hash: [u8; 32],
}

/// Decoded SPL swap account.
#[derive(BorshSerialize, BorshDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct SplSwapAccount {
    /// Token mint escrowed.
    pub mint: Pubkey,
    /// Absolute expiry slot stored on-chain.
    pub expiry_slot: u64,
    /// Original funder.
    pub initiator: Pubkey,
    /// Authorized redeemer.
    pub redeemer: Pubkey,
    /// Hashlock.
    pub secret_hash: [u8; 32],
    /// Raw token amount in escrow.
    pub swap_amount: u64,
    /// Bump for the SPL identity PDA.
    pub identity_pda_bump: u8,
    /// Sponsor account used by the SPL program.
    pub sponsor: Pubkey,
}

/// Parse a hex-encoded 32-byte secret hash or preimage.
///
/// A leading `0x` prefix is accepted.
///
/// # Errors
///
/// Returns an error when the input is not valid hex or does not decode to 32
/// bytes.
pub fn parse_secret_hash(hex_str: &str) -> Result<[u8; 32], HtlcEncodingError> {
    parse_32_byte_hex(hex_str)
}

/// Parse a hex-encoded 32-byte secret preimage.
///
/// # Errors
///
/// Returns an error when the input is not valid hex or does not decode to 32
/// bytes.
pub fn parse_secret_preimage(hex_str: &str) -> Result<[u8; 32], HtlcEncodingError> {
    parse_32_byte_hex(hex_str)
}

/// Generate a random 32-byte HTLC secret.
#[must_use]
pub fn generate_secret() -> [u8; 32] {
    let mut secret = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut secret);
    secret
}

/// Hash a 32-byte secret with SHA-256.
#[must_use]
pub fn hash_secret(secret: &[u8; 32]) -> [u8; 32] {
    let digest = Sha256::digest(secret);
    digest.into()
}

/// Detect the instruction type from its Anchor discriminator.
#[must_use]
pub fn detect_instruction_type(ix_data: &[u8]) -> Option<HtlcInstructionType> {
    let disc = read_disc(ix_data).ok()?;
    match disc {
        INITIATE_DISC => Some(HtlcInstructionType::Initiate),
        REDEEM_DISC => Some(HtlcInstructionType::Redeem),
        REFUND_DISC => Some(HtlcInstructionType::Refund),
        INSTANT_REFUND_DISC => Some(HtlcInstructionType::InstantRefund),
        _ => None,
    }
}

/// Decode native `initiate` instruction data.
///
/// # Errors
///
/// Returns an error when the discriminator or Borsh payload is invalid.
pub fn decode_initiate_native_data(
    ix_data: &[u8],
) -> Result<InitiateNativeData, HtlcEncodingError> {
    decode_with_disc(ix_data, INITIATE_DISC)
}

/// Decode SPL `initiate` instruction data.
///
/// # Errors
///
/// Returns an error when the discriminator or Borsh payload is invalid.
pub fn decode_initiate_spl_data(ix_data: &[u8]) -> Result<InitiateSplData, HtlcEncodingError> {
    decode_with_disc(ix_data, INITIATE_DISC)
}

/// Decode `redeem` instruction data.
///
/// # Errors
///
/// Returns an error when the discriminator or Borsh payload is invalid.
pub fn decode_redeem_data(ix_data: &[u8]) -> Result<RedeemData, HtlcEncodingError> {
    decode_with_disc(ix_data, REDEEM_DISC)
}

/// Decode a native swap account.
///
/// # Errors
///
/// Returns an error when the account discriminator or Borsh payload is invalid.
pub fn decode_native_swap_account(data: &[u8]) -> Result<NativeSwapAccount, HtlcEncodingError> {
    decode_with_disc(data, SWAP_ACCOUNT_DISC)
}

/// Decode an SPL swap account.
///
/// # Errors
///
/// Returns an error when the account discriminator or Borsh payload is invalid.
pub fn decode_spl_swap_account(data: &[u8]) -> Result<SplSwapAccount, HtlcEncodingError> {
    decode_with_disc(data, SWAP_ACCOUNT_DISC)
}

fn parse_32_byte_hex(hex_str: &str) -> Result<[u8; 32], HtlcEncodingError> {
    let stripped = hex_str.strip_prefix("0x").unwrap_or(hex_str);
    let bytes =
        hex::decode(stripped).map_err(|error| HtlcEncodingError::InvalidHex(error.to_string()))?;
    let length = bytes.len();
    bytes
        .try_into()
        .map_err(|_| HtlcEncodingError::InvalidSecretLength(length))
}

pub(crate) fn serialize_with_disc(disc: [u8; 8], data: &impl BorshSerialize) -> Vec<u8> {
    let mut buffer = disc.to_vec();
    data.serialize(&mut buffer)
        .expect("serializing HTLC instruction data into Vec cannot fail");
    buffer
}

fn decode_with_disc<T: BorshDeserialize>(
    data: &[u8],
    expected: [u8; 8],
) -> Result<T, HtlcEncodingError> {
    let actual = read_disc(data)?;
    if actual != expected {
        return Err(HtlcEncodingError::UnexpectedDiscriminator { expected, actual });
    }
    T::try_from_slice(&data[8..]).map_err(|error| HtlcEncodingError::BorshDecode(error.to_string()))
}

fn read_disc(data: &[u8]) -> Result<[u8; 8], HtlcEncodingError> {
    data.get(..8)
        .ok_or(HtlcEncodingError::DataTooShort {
            expected: 8,
            actual: data.len(),
        })?
        .try_into()
        .map_err(|_| HtlcEncodingError::DataTooShort {
            expected: 8,
            actual: data.len(),
        })
}
