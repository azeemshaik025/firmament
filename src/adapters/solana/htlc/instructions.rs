//! Munger HTLC PDA derivation and instruction builders.

use solana_sdk::instruction::{AccountMeta, Instruction};
use solana_sdk::pubkey::Pubkey;

use super::encoding::{
    HtlcProgramKind, INITIATE_DISC, INSTANT_REFUND_DISC, InitiateNativeData, InitiateSplData,
    REDEEM_DISC, REFUND_DISC, RedeemData, serialize_with_disc,
};

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
