//! Solana HTLC instruction encoding and account decoding.
//!
//! This module preserves the Munger-native Anchor protocol surface for the two
//! deployed Solana HTLC programs. It builds instructions only; signing,
//! submission, confirmation, and ledger writes stay outside this module.

use borsh::{BorshDeserialize, BorshSerialize};
use rand::RngCore;
use sha2::{Digest, Sha256};
use solana_sdk::instruction::{AccountMeta, Instruction};
use solana_sdk::pubkey::Pubkey;
use thiserror::Error;

/// Munger native SOL HTLC program deployed on Solana mainnet.
pub const NATIVE_SOL_HTLC_PROGRAM_ID: &str = "2bag6xpshpvPe7SJ9nSDLHpxqhEAoHPGpEkjNSv7gxoF";

/// Munger SPL token HTLC program deployed on Solana mainnet.
pub const SPL_TOKEN_HTLC_PROGRAM_ID: &str = "gdnvdMCHJgnidtU7SL8RkRshHPvDJU1pdfZEpoLvqdU";

/// Legacy SPL token program ID.
pub const SPL_TOKEN_PROGRAM_ID: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";

/// Token-2022 program ID, accepted by the SPL instruction builders.
pub const TOKEN_2022_PROGRAM_ID: &str = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";

/// System program ID.
pub const SYSTEM_PROGRAM_ID: Pubkey = Pubkey::from_str_const("11111111111111111111111111111111");

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

/// Derive the native SOL swap account PDA.
///
/// Munger/Anchor seeds: `["swap_account", initiator, secret_hash]`.
#[must_use]
pub fn derive_native_swap_pda(
    program_id: &Pubkey,
    initiator: &Pubkey,
    secret_hash: &[u8; 32],
) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[b"swap_account", initiator.as_ref(), secret_hash.as_ref()],
        program_id,
    )
}

/// Derive the SPL swap data PDA.
///
/// Munger/Anchor seeds: `[initiator, secret_hash]`.
#[must_use]
pub fn derive_spl_swap_pda(
    program_id: &Pubkey,
    initiator: &Pubkey,
    secret_hash: &[u8; 32],
) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[initiator.as_ref(), secret_hash.as_ref()], program_id)
}

/// Derive the SPL identity PDA.
///
/// Munger/Anchor seeds: `[]`.
#[must_use]
pub fn derive_spl_identity_pda(program_id: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[], program_id)
}

/// Derive the SPL token vault PDA.
///
/// Munger/Anchor seeds: `[mint]`.
#[must_use]
pub fn derive_spl_token_vault(program_id: &Pubkey, mint: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[mint.as_ref()], program_id)
}

/// Build a native SOL `initiate` instruction.
#[must_use]
pub fn build_initiate_native(
    program_id: &Pubkey,
    initiator: &Pubkey,
    data: &InitiateNativeData,
) -> Instruction {
    let (swap_account, _) = derive_native_swap_pda(program_id, initiator, &data.secret_hash);
    Instruction {
        program_id: *program_id,
        accounts: vec![
            AccountMeta::new(swap_account, false),
            AccountMeta::new(*initiator, true),
            AccountMeta::new_readonly(SYSTEM_PROGRAM_ID, false),
        ],
        data: serialize_with_disc(INITIATE_DISC, data),
    }
}

/// Build an SPL token `initiate` instruction.
#[must_use]
pub fn build_initiate_spl(
    program_id: &Pubkey,
    initiator: &Pubkey,
    mint: &Pubkey,
    initiator_token_account: &Pubkey,
    sponsor: &Pubkey,
    token_program: Pubkey,
    data: &InitiateSplData,
) -> Instruction {
    let (identity_pda, _) = derive_spl_identity_pda(program_id);
    let (swap_data, _) = derive_spl_swap_pda(program_id, initiator, &data.secret_hash);
    let (token_vault, _) = derive_spl_token_vault(program_id, mint);

    Instruction {
        program_id: *program_id,
        accounts: vec![
            AccountMeta::new(identity_pda, false),
            AccountMeta::new(swap_data, false),
            AccountMeta::new(token_vault, false),
            AccountMeta::new_readonly(*initiator, true),
            AccountMeta::new(*initiator_token_account, false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new(*sponsor, true),
            AccountMeta::new_readonly(token_program, false),
            AccountMeta::new_readonly(SYSTEM_PROGRAM_ID, false),
        ],
        data: serialize_with_disc(INITIATE_DISC, data),
    }
}

/// Build a native SOL `redeem` instruction.
#[must_use]
pub fn build_redeem_native(
    program_id: &Pubkey,
    swap_account: &Pubkey,
    initiator: &Pubkey,
    redeemer: &Pubkey,
    data: &RedeemData,
) -> Instruction {
    Instruction {
        program_id: *program_id,
        accounts: vec![
            AccountMeta::new(*swap_account, false),
            AccountMeta::new(*initiator, false),
            AccountMeta::new(*redeemer, false),
        ],
        data: serialize_with_disc(REDEEM_DISC, data),
    }
}

/// Build an SPL token `redeem` instruction.
#[must_use]
pub fn build_redeem_spl(
    program_id: &Pubkey,
    swap_data: &Pubkey,
    mint: &Pubkey,
    redeemer_token_account: &Pubkey,
    sponsor: &Pubkey,
    token_program: Pubkey,
    data: &RedeemData,
) -> Instruction {
    let (identity_pda, _) = derive_spl_identity_pda(program_id);
    let (token_vault, _) = derive_spl_token_vault(program_id, mint);

    Instruction {
        program_id: *program_id,
        accounts: vec![
            AccountMeta::new_readonly(identity_pda, false),
            AccountMeta::new(*swap_data, false),
            AccountMeta::new(token_vault, false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new(*redeemer_token_account, false),
            AccountMeta::new(*sponsor, false),
            AccountMeta::new_readonly(token_program, false),
        ],
        data: serialize_with_disc(REDEEM_DISC, data),
    }
}

/// Build a native SOL `refund` instruction.
#[must_use]
pub fn build_refund_native(
    program_id: &Pubkey,
    swap_account: &Pubkey,
    initiator: &Pubkey,
) -> Instruction {
    Instruction {
        program_id: *program_id,
        accounts: vec![
            AccountMeta::new(*swap_account, false),
            AccountMeta::new(*initiator, false),
        ],
        data: REFUND_DISC.to_vec(),
    }
}

/// Build an SPL token `refund` instruction.
#[must_use]
pub fn build_refund_spl(
    program_id: &Pubkey,
    swap_data: &Pubkey,
    mint: &Pubkey,
    initiator_token_account: &Pubkey,
    sponsor: &Pubkey,
    token_program: Pubkey,
) -> Instruction {
    let (identity_pda, _) = derive_spl_identity_pda(program_id);
    let (token_vault, _) = derive_spl_token_vault(program_id, mint);

    Instruction {
        program_id: *program_id,
        accounts: vec![
            AccountMeta::new_readonly(identity_pda, false),
            AccountMeta::new(*swap_data, false),
            AccountMeta::new(token_vault, false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new(*initiator_token_account, false),
            AccountMeta::new(*sponsor, false),
            AccountMeta::new_readonly(token_program, false),
        ],
        data: REFUND_DISC.to_vec(),
    }
}

/// Build a native SOL `instant_refund` instruction.
#[must_use]
pub fn build_instant_refund_native(
    program_id: &Pubkey,
    swap_account: &Pubkey,
    initiator: &Pubkey,
    redeemer: &Pubkey,
) -> Instruction {
    Instruction {
        program_id: *program_id,
        accounts: vec![
            AccountMeta::new(*swap_account, false),
            AccountMeta::new(*initiator, false),
            AccountMeta::new_readonly(*redeemer, true),
        ],
        data: INSTANT_REFUND_DISC.to_vec(),
    }
}

/// Build an SPL token `instant_refund` instruction.
#[must_use]
pub fn build_instant_refund_spl(
    program_id: &Pubkey,
    swap_data: &Pubkey,
    mint: &Pubkey,
    initiator_token_account: &Pubkey,
    redeemer: &Pubkey,
    sponsor: &Pubkey,
    token_program: Pubkey,
) -> Instruction {
    let (identity_pda, _) = derive_spl_identity_pda(program_id);
    let (token_vault, _) = derive_spl_token_vault(program_id, mint);

    Instruction {
        program_id: *program_id,
        accounts: vec![
            AccountMeta::new_readonly(identity_pda, false),
            AccountMeta::new(*swap_data, false),
            AccountMeta::new(token_vault, false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new(*initiator_token_account, false),
            AccountMeta::new(*redeemer, true),
            AccountMeta::new(*sponsor, false),
            AccountMeta::new_readonly(token_program, false),
        ],
        data: INSTANT_REFUND_DISC.to_vec(),
    }
}

/// Compute the on-chain HTLC ID, represented by the swap PDA address.
#[must_use]
pub fn compute_htlc_id(
    kind: HtlcProgramKind,
    program_id: &Pubkey,
    initiator: &Pubkey,
    secret_hash: &[u8; 32],
) -> String {
    let (pda, _) = match kind {
        HtlcProgramKind::Native => derive_native_swap_pda(program_id, initiator, secret_hash),
        HtlcProgramKind::Spl => derive_spl_swap_pda(program_id, initiator, secret_hash),
    };
    pda.to_string()
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

fn serialize_with_disc(disc: [u8; 8], data: &impl BorshSerialize) -> Vec<u8> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use borsh::BorshSerialize;
    use solana_sdk::instruction::AccountMeta;
    use solana_sdk::pubkey::Pubkey;
    use std::str::FromStr;

    fn native_program() -> Pubkey {
        Pubkey::from_str(NATIVE_SOL_HTLC_PROGRAM_ID).expect("valid native program id")
    }

    fn spl_program() -> Pubkey {
        Pubkey::from_str(SPL_TOKEN_HTLC_PROGRAM_ID).expect("valid spl program id")
    }

    fn token_program() -> Pubkey {
        Pubkey::from_str(SPL_TOKEN_PROGRAM_ID).expect("valid token program id")
    }

    fn initiator() -> Pubkey {
        Pubkey::from_str("11111111111111111111111111111112").expect("valid pubkey")
    }

    fn redeemer() -> Pubkey {
        Pubkey::from_str("11111111111111111111111111111113").expect("valid pubkey")
    }

    fn secret_hash() -> [u8; 32] {
        let mut hash = [0u8; 32];
        hash[0] = 0xab;
        hash[31] = 0xcd;
        hash
    }

    fn meta_tuple(meta: &AccountMeta) -> (Pubkey, bool, bool) {
        (meta.pubkey, meta.is_writable, meta.is_signer)
    }

    #[test]
    fn native_pda_derivation_uses_munger_seeds() {
        let program_id = native_program();
        let initiator = initiator();
        let secret_hash = secret_hash();

        let (actual, actual_bump) = derive_native_swap_pda(&program_id, &initiator, &secret_hash);
        let (expected, expected_bump) = Pubkey::find_program_address(
            &[b"swap_account", initiator.as_ref(), secret_hash.as_ref()],
            &program_id,
        );

        assert_eq!(actual, expected);
        assert_eq!(actual_bump, expected_bump);
    }

    #[test]
    fn spl_pda_derivation_uses_munger_seeds() {
        let program_id = spl_program();
        let initiator = initiator();
        let secret_hash = secret_hash();

        let (actual, actual_bump) = derive_spl_swap_pda(&program_id, &initiator, &secret_hash);
        let (expected, expected_bump) =
            Pubkey::find_program_address(&[initiator.as_ref(), secret_hash.as_ref()], &program_id);

        assert_eq!(actual, expected);
        assert_eq!(actual_bump, expected_bump);
    }

    #[test]
    fn native_initiate_instruction_matches_munger_account_order_and_data() {
        let program_id = native_program();
        let initiator = initiator();
        let redeemer = redeemer();
        let secret_hash = secret_hash();
        let data = InitiateNativeData {
            amount_lamports: 1_234_567,
            expires_in_slots: 88,
            redeemer,
            secret_hash,
        };

        let instruction = build_initiate_native(&program_id, &initiator, &data);
        let (swap_pda, _) = derive_native_swap_pda(&program_id, &initiator, &secret_hash);

        assert_eq!(instruction.program_id, program_id);
        assert_eq!(
            instruction
                .accounts
                .iter()
                .map(meta_tuple)
                .collect::<Vec<_>>(),
            vec![
                (swap_pda, true, false),
                (initiator, true, true),
                (SYSTEM_PROGRAM_ID, false, false),
            ]
        );

        let mut expected = INITIATE_DISC.to_vec();
        expected.extend_from_slice(&1_234_567u64.to_le_bytes());
        expected.extend_from_slice(&88u64.to_le_bytes());
        expected.extend_from_slice(redeemer.as_ref());
        expected.extend_from_slice(&secret_hash);
        assert_eq!(instruction.data, expected);
    }

    #[test]
    fn native_redeem_instruction_matches_munger_account_order_and_data() {
        let program_id = native_program();
        let swap_pda = Pubkey::new_unique();
        let initiator = initiator();
        let redeemer = redeemer();
        let secret = [0x42; 32];

        let instruction = build_redeem_native(
            &program_id,
            &swap_pda,
            &initiator,
            &redeemer,
            &RedeemData { secret },
        );

        assert_eq!(instruction.program_id, program_id);
        assert_eq!(
            instruction
                .accounts
                .iter()
                .map(meta_tuple)
                .collect::<Vec<_>>(),
            vec![
                (swap_pda, true, false),
                (initiator, true, false),
                (redeemer, true, false),
            ]
        );

        let mut expected = REDEEM_DISC.to_vec();
        expected.extend_from_slice(&secret);
        assert_eq!(instruction.data, expected);
    }

    #[test]
    fn native_refund_instruction_matches_munger_account_order_and_data() {
        let program_id = native_program();
        let swap_pda = Pubkey::new_unique();
        let initiator = initiator();

        let instruction = build_refund_native(&program_id, &swap_pda, &initiator);

        assert_eq!(instruction.program_id, program_id);
        assert_eq!(
            instruction
                .accounts
                .iter()
                .map(meta_tuple)
                .collect::<Vec<_>>(),
            vec![(swap_pda, true, false), (initiator, true, false)]
        );
        assert_eq!(instruction.data, REFUND_DISC.to_vec());
    }

    #[test]
    fn spl_initiate_instruction_matches_munger_account_order_and_data() {
        let program_id = spl_program();
        let initiator = initiator();
        let redeemer = redeemer();
        let mint = Pubkey::new_unique();
        let initiator_token_account = Pubkey::new_unique();
        let sponsor = Pubkey::new_unique();
        let secret_hash = secret_hash();
        let data = InitiateSplData {
            expires_in_slots: 144,
            redeemer,
            secret_hash,
            swap_amount: 9_876_543,
            destination_data: None,
        };

        let instruction = build_initiate_spl(
            &program_id,
            &initiator,
            &mint,
            &initiator_token_account,
            &sponsor,
            token_program(),
            &data,
        );
        let (identity_pda, _) = derive_spl_identity_pda(&program_id);
        let (swap_pda, _) = derive_spl_swap_pda(&program_id, &initiator, &secret_hash);
        let (token_vault, _) = derive_spl_token_vault(&program_id, &mint);

        assert_eq!(instruction.program_id, program_id);
        assert_eq!(
            instruction
                .accounts
                .iter()
                .map(meta_tuple)
                .collect::<Vec<_>>(),
            vec![
                (identity_pda, true, false),
                (swap_pda, true, false),
                (token_vault, true, false),
                (initiator, false, true),
                (initiator_token_account, true, false),
                (mint, false, false),
                (sponsor, true, true),
                (token_program(), false, false),
                (SYSTEM_PROGRAM_ID, false, false),
            ]
        );

        let mut expected = INITIATE_DISC.to_vec();
        expected.extend_from_slice(&144u64.to_le_bytes());
        expected.extend_from_slice(redeemer.as_ref());
        expected.extend_from_slice(&secret_hash);
        expected.extend_from_slice(&9_876_543u64.to_le_bytes());
        expected.push(0);
        assert_eq!(instruction.data, expected);
    }

    #[test]
    fn spl_redeem_instruction_matches_munger_account_order_and_data() {
        let program_id = spl_program();
        let swap_pda = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let redeemer_token_account = Pubkey::new_unique();
        let sponsor = Pubkey::new_unique();
        let secret = [0x24; 32];

        let instruction = build_redeem_spl(
            &program_id,
            &swap_pda,
            &mint,
            &redeemer_token_account,
            &sponsor,
            token_program(),
            &RedeemData { secret },
        );
        let (identity_pda, _) = derive_spl_identity_pda(&program_id);
        let (token_vault, _) = derive_spl_token_vault(&program_id, &mint);

        assert_eq!(instruction.program_id, program_id);
        assert_eq!(
            instruction
                .accounts
                .iter()
                .map(meta_tuple)
                .collect::<Vec<_>>(),
            vec![
                (identity_pda, false, false),
                (swap_pda, true, false),
                (token_vault, true, false),
                (mint, false, false),
                (redeemer_token_account, true, false),
                (sponsor, true, false),
                (token_program(), false, false),
            ]
        );

        let mut expected = REDEEM_DISC.to_vec();
        expected.extend_from_slice(&secret);
        assert_eq!(instruction.data, expected);
    }

    #[test]
    fn spl_refund_instruction_matches_munger_account_order_and_data() {
        let program_id = spl_program();
        let swap_pda = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let initiator_token_account = Pubkey::new_unique();
        let sponsor = Pubkey::new_unique();

        let instruction = build_refund_spl(
            &program_id,
            &swap_pda,
            &mint,
            &initiator_token_account,
            &sponsor,
            token_program(),
        );
        let (identity_pda, _) = derive_spl_identity_pda(&program_id);
        let (token_vault, _) = derive_spl_token_vault(&program_id, &mint);

        assert_eq!(instruction.program_id, program_id);
        assert_eq!(
            instruction
                .accounts
                .iter()
                .map(meta_tuple)
                .collect::<Vec<_>>(),
            vec![
                (identity_pda, false, false),
                (swap_pda, true, false),
                (token_vault, true, false),
                (mint, false, false),
                (initiator_token_account, true, false),
                (sponsor, true, false),
                (token_program(), false, false),
            ]
        );
        assert_eq!(instruction.data, REFUND_DISC.to_vec());
    }

    #[test]
    fn discriminator_detection_matches_munger_names() {
        assert_eq!(
            detect_instruction_type(&INITIATE_DISC),
            Some(HtlcInstructionType::Initiate)
        );
        assert_eq!(
            detect_instruction_type(&REDEEM_DISC),
            Some(HtlcInstructionType::Redeem)
        );
        assert_eq!(
            detect_instruction_type(&REFUND_DISC),
            Some(HtlcInstructionType::Refund)
        );
        assert_eq!(
            detect_instruction_type(&INSTANT_REFUND_DISC),
            Some(HtlcInstructionType::InstantRefund)
        );
        assert_eq!(detect_instruction_type(&[0; 7]), None);
        assert_eq!(detect_instruction_type(&[0; 8]), None);
    }

    #[test]
    fn secret_hash_parsing_accepts_hex_with_optional_prefix() {
        let plain = "ab000000000000000000000000000000000000000000000000000000000000cd";
        let prefixed = format!("0x{plain}");

        assert_eq!(parse_secret_hash(plain).expect("plain hash"), secret_hash());
        assert_eq!(
            parse_secret_hash(&prefixed).expect("prefixed hash"),
            secret_hash()
        );
        assert!(parse_secret_hash("abcd").is_err());
        assert!(
            parse_secret_hash("zz00000000000000000000000000000000000000000000000000000000000000")
                .is_err()
        );
    }

    #[test]
    fn htlc_id_is_swap_pda_for_each_solana_program() {
        let initiator = initiator();
        let secret_hash = secret_hash();
        let native_program = native_program();
        let spl_program = spl_program();

        let native_id = compute_htlc_id(
            HtlcProgramKind::Native,
            &native_program,
            &initiator,
            &secret_hash,
        );
        let spl_id = compute_htlc_id(HtlcProgramKind::Spl, &spl_program, &initiator, &secret_hash);

        assert_eq!(
            native_id,
            derive_native_swap_pda(&native_program, &initiator, &secret_hash)
                .0
                .to_string()
        );
        assert_eq!(
            spl_id,
            derive_spl_swap_pda(&spl_program, &initiator, &secret_hash)
                .0
                .to_string()
        );
    }

    #[test]
    fn decode_helpers_roundtrip_instruction_payloads() {
        let native_data = InitiateNativeData {
            amount_lamports: 77,
            expires_in_slots: 123,
            redeemer: redeemer(),
            secret_hash: secret_hash(),
        };
        let native_instruction =
            build_initiate_native(&native_program(), &initiator(), &native_data);
        assert_eq!(
            decode_initiate_native_data(&native_instruction.data).expect("decode native initiate"),
            native_data
        );

        let spl_data = InitiateSplData {
            expires_in_slots: 456,
            redeemer: redeemer(),
            secret_hash: secret_hash(),
            swap_amount: 88,
            destination_data: Some(vec![1, 2, 3]),
        };
        let spl_instruction = build_initiate_spl(
            &spl_program(),
            &initiator(),
            &Pubkey::new_unique(),
            &Pubkey::new_unique(),
            &Pubkey::new_unique(),
            token_program(),
            &spl_data,
        );
        assert_eq!(
            decode_initiate_spl_data(&spl_instruction.data).expect("decode spl initiate"),
            spl_data
        );

        let redeem_data = RedeemData { secret: [0x11; 32] };
        let redeem_instruction = build_redeem_native(
            &native_program(),
            &Pubkey::new_unique(),
            &initiator(),
            &redeemer(),
            &redeem_data,
        );
        assert_eq!(
            decode_redeem_data(&redeem_instruction.data).expect("decode redeem"),
            redeem_data
        );
    }

    #[test]
    fn decode_helpers_roundtrip_swap_accounts() {
        let native = NativeSwapAccount {
            amount_lamports: 1_000,
            expiry_slot: 22,
            initiator: initiator(),
            redeemer: redeemer(),
            secret_hash: secret_hash(),
        };
        let mut native_bytes = SWAP_ACCOUNT_DISC.to_vec();
        native
            .serialize(&mut native_bytes)
            .expect("serialize native swap account");
        assert_eq!(
            decode_native_swap_account(&native_bytes).expect("decode native swap account"),
            native
        );

        let spl = SplSwapAccount {
            mint: Pubkey::new_unique(),
            expiry_slot: 33,
            initiator: initiator(),
            redeemer: redeemer(),
            secret_hash: secret_hash(),
            swap_amount: 2_000,
            identity_pda_bump: 254,
            sponsor: Pubkey::new_unique(),
        };
        let mut spl_bytes = SWAP_ACCOUNT_DISC.to_vec();
        spl.serialize(&mut spl_bytes)
            .expect("serialize spl swap account");
        assert_eq!(
            decode_spl_swap_account(&spl_bytes).expect("decode spl swap account"),
            spl
        );

        assert!(decode_native_swap_account(&REFUND_DISC).is_err());
        assert!(decode_spl_swap_account(&[1, 2, 3]).is_err());
    }

    #[test]
    fn live_htlc_smoke_tests_skip_without_explicit_opt_in() {
        let enabled = std::env::var("RUN_LIVE_SOLANA_TESTS").as_deref() == Ok("1")
            && std::env::var("RUN_LIVE_HTLC_TESTS").as_deref() == Ok("1")
            && std::env::var("SOLANA_RPC_URL").is_ok()
            && std::env::var("MAKER_KEYPAIR").is_ok()
            && std::env::var("TAKER_KEYPAIR").is_ok();

        if !enabled {
            eprintln!(
                "skipping live Solana HTLC smoke tests; set RUN_LIVE_SOLANA_TESTS=1, RUN_LIVE_HTLC_TESTS=1, SOLANA_RPC_URL, MAKER_KEYPAIR, and TAKER_KEYPAIR"
            );
            return;
        }

        panic!(
            "live HTLC smoke path is not implemented in the offline instruction-builder crate yet"
        );
    }
}
