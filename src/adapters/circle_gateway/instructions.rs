//! Circle Gateway Solana PDA derivation and instruction builders.

use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
};

use super::attestation::AttestationElement;
use super::{
    DEPOSIT_DISCRIMINATOR, GATEWAY_MINT_DISCRIMINATOR, GATEWAY_MINTER_PROGRAM,
    GATEWAY_WALLET_PROGRAM, GatewayError, GatewayResult, SPL_TOKEN_PROGRAM_ID, SYSTEM_PROGRAM_ID,
    USDC_MINT, parse_pubkey,
};

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
