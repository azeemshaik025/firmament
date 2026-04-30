//! Solana HTLC instruction encoding, account decoding, and live client glue.
//!
//! This module preserves the Munger-native Anchor protocol surface for the two
//! deployed Solana HTLC programs and adds a small demo client around the
//! builders for signing, submission, confirmation, and status lookup.

pub mod client;
pub mod encoding;
pub mod instructions;

pub use client::{HtlcConfirmationConfig, SolanaHtlcClient, SolanaHtlcClientConfig};
pub use encoding::{
    HtlcEncodingError, HtlcInstructionType, HtlcProgramKind, INITIATE_DISC, INSTANT_REFUND_DISC,
    InitiateNativeData, InitiateSplData, NativeSwapAccount, REDEEM_DISC, REFUND_DISC, RedeemData,
    SWAP_ACCOUNT_DISC, SplSwapAccount, decode_initiate_native_data, decode_initiate_spl_data,
    decode_native_swap_account, decode_redeem_data, decode_spl_swap_account,
    detect_instruction_type, generate_secret, hash_secret, parse_secret_hash,
    parse_secret_preimage,
};
pub use instructions::{
    NATIVE_SOL_HTLC_PROGRAM_ID, SPL_TOKEN_HTLC_PROGRAM_ID, SPL_TOKEN_PROGRAM_ID, SYSTEM_PROGRAM_ID,
    TOKEN_2022_PROGRAM_ID, build_initiate_native, build_initiate_spl, build_instant_refund_native,
    build_instant_refund_spl, build_redeem_native, build_redeem_spl, build_refund_native,
    build_refund_spl, compute_htlc_id, derive_native_swap_pda, derive_spl_identity_pda,
    derive_spl_swap_pda, derive_spl_token_vault,
};
