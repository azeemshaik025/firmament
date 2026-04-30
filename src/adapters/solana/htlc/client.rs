//! Live Solana HTLC client glue.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_commitment_config::CommitmentConfig;
use solana_sdk::instruction::Instruction;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::{Keypair, Signature, Signer};
use solana_sdk::transaction::Transaction;
use tokio::sync::Mutex;

use crate::adapters::solana::client::{
    LEGACY_TOKEN_PROGRAM_ID, TOKEN_2022_PROGRAM_ID as SOLANA_TOKEN_2022_PROGRAM_ID,
};
use crate::domain::assets::AssetRegistry;
use crate::domain::types::{
    HtlcInitiation, HtlcReceipt, SettlementStatus, TradeId, TxSignature, WalletRole,
};
use crate::error::AppError;
use crate::ports::HtlcClient;

use super::encoding::{
    HtlcProgramKind, InitiateNativeData, InitiateSplData, RedeemData, decode_native_swap_account,
    decode_spl_swap_account, parse_secret_hash, parse_secret_preimage,
};
use super::instructions::{
    NATIVE_SOL_HTLC_PROGRAM_ID, SPL_TOKEN_HTLC_PROGRAM_ID, build_initiate_native,
    build_initiate_spl, build_redeem_native, build_redeem_spl, build_refund_native,
    build_refund_spl, derive_native_swap_pda, derive_spl_swap_pda,
};

/// Confirmation-polling controls for submitted HTLC transactions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HtlcConfirmationConfig {
    /// Number of signature-status polls before timing out.
    pub max_attempts: u32,
    /// Delay between signature-status polls.
    pub poll_interval: Duration,
}

impl Default for HtlcConfirmationConfig {
    fn default() -> Self {
        Self {
            max_attempts: 30,
            poll_interval: Duration::from_secs(1),
        }
    }
}

/// Live Solana HTLC client configuration.
pub struct SolanaHtlcClientConfig {
    /// Shared nonblocking Solana RPC client.
    pub rpc_client: Arc<RpcClient>,
    /// Runtime asset registry.
    pub registry: AssetRegistry,
    /// Signers keyed by runtime wallet role.
    pub wallets: Vec<(WalletRole, Arc<Keypair>)>,
    /// Confirmation-polling controls.
    pub confirmation: HtlcConfirmationConfig,
}

/// Live Solana HTLC client backed by the Munger deployed programs.
pub struct SolanaHtlcClient {
    rpc_client: Arc<RpcClient>,
    registry: AssetRegistry,
    wallets: HashMap<WalletRole, Arc<Keypair>>,
    native_program_id: Pubkey,
    spl_program_id: Pubkey,
    confirmation: HtlcConfirmationConfig,
    state: Arc<Mutex<HashMap<TradeId, SolanaHtlcTradeState>>>,
}

#[derive(Debug, Clone, Default)]
struct SolanaHtlcTradeState {
    legs: Vec<SolanaHtlcLeg>,
    redeemed_count: usize,
}

#[derive(Debug, Clone)]
struct SolanaHtlcLeg {
    request: HtlcInitiation,
    kind: HtlcProgramKind,
    program_id: Pubkey,
    swap_pda: Pubkey,
    mint: Option<Pubkey>,
    token_program: Option<Pubkey>,
    status: SettlementStatus,
}

impl std::fmt::Debug for SolanaHtlcClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SolanaHtlcClient")
            .field("asset_count", &self.registry.assets().count())
            .field("wallet_roles", &self.wallets.keys().collect::<Vec<_>>())
            .field("native_program_id", &self.native_program_id)
            .field("spl_program_id", &self.spl_program_id)
            .field("confirmation", &self.confirmation)
            .finish_non_exhaustive()
    }
}

impl SolanaHtlcClient {
    /// Build a live HTLC client.
    ///
    /// # Errors
    ///
    /// Returns a validation error when built-in program ids cannot be parsed.
    pub fn new(config: SolanaHtlcClientConfig) -> Result<Self, AppError> {
        Ok(Self {
            rpc_client: config.rpc_client,
            registry: config.registry,
            wallets: config.wallets.into_iter().collect(),
            native_program_id: Pubkey::from_str(NATIVE_SOL_HTLC_PROGRAM_ID).map_err(|error| {
                AppError::validation(format!("parse native HTLC program id: {error}"))
            })?,
            spl_program_id: Pubkey::from_str(SPL_TOKEN_HTLC_PROGRAM_ID).map_err(|error| {
                AppError::validation(format!("parse SPL HTLC program id: {error}"))
            })?,
            confirmation: config.confirmation,
            state: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Build a native-SOL initiate transaction without submitting it.
    ///
    /// This is used by unit tests to verify transaction construction remains
    /// signer- and blockhash-correct without touching live RPC.
    ///
    /// # Errors
    ///
    /// Returns a validation error when the request is not a native SOL HTLC.
    pub fn build_native_initiate_transaction(
        &self,
        request: &HtlcInitiation,
        recent_blockhash: solana_sdk::hash::Hash,
    ) -> Result<Transaction, AppError> {
        let funder = self.wallet(request.funder)?;
        let redeemer = self.wallet(request.redeemer)?;
        let secret_hash = parse_secret_hash(&request.hashlock)
            .map_err(|error| AppError::validation(error.to_string()))?;
        let asset = self.registry.require_asset(&request.amount.asset)?;
        if !asset.is_native_sol() {
            return Err(AppError::validation(
                "native HTLC transaction builder requires SOL amount",
            ));
        }
        let instruction = build_initiate_native(
            &self.native_program_id,
            &funder.pubkey(),
            &InitiateNativeData {
                amount_lamports: request.amount.amount_raw.as_u64(),
                expires_in_slots: expires_in_slots(request.expires_at),
                redeemer: redeemer.pubkey(),
                secret_hash,
            },
        );
        Ok(Transaction::new_signed_with_payer(
            &[instruction],
            Some(&funder.pubkey()),
            &[funder.as_ref()],
            recent_blockhash,
        ))
    }

    async fn build_initiate_instruction(
        &self,
        request: &HtlcInitiation,
    ) -> Result<(Instruction, SolanaHtlcLeg), AppError> {
        let funder = self.wallet(request.funder)?;
        let redeemer = self.wallet(request.redeemer)?;
        let secret_hash = parse_secret_hash(&request.hashlock)
            .map_err(|error| AppError::validation(error.to_string()))?;
        let asset = self.registry.require_asset(&request.amount.asset)?;

        if asset.is_native_sol() {
            let instruction = build_initiate_native(
                &self.native_program_id,
                &funder.pubkey(),
                &InitiateNativeData {
                    amount_lamports: request.amount.amount_raw.as_u64(),
                    expires_in_slots: expires_in_slots(request.expires_at),
                    redeemer: redeemer.pubkey(),
                    secret_hash,
                },
            );
            let (swap_pda, _) =
                derive_native_swap_pda(&self.native_program_id, &funder.pubkey(), &secret_hash);
            return Ok((
                instruction,
                SolanaHtlcLeg {
                    request: request.clone(),
                    kind: HtlcProgramKind::Native,
                    program_id: self.native_program_id,
                    swap_pda,
                    mint: None,
                    token_program: None,
                    status: SettlementStatus::Pending,
                },
            ));
        }

        let mint = asset.mint_pubkey()?;
        let token_program = self.token_program_for_mint(&mint).await?;
        let funder_ata =
            crate::adapters::solana::client::SolanaClient::derive_associated_token_address(
                &funder.pubkey(),
                &mint,
                &token_program,
            );
        let instruction = build_initiate_spl(
            &self.spl_program_id,
            &funder.pubkey(),
            &mint,
            &funder_ata,
            &funder.pubkey(),
            token_program,
            &InitiateSplData {
                expires_in_slots: expires_in_slots(request.expires_at),
                redeemer: redeemer.pubkey(),
                secret_hash,
                swap_amount: request.amount.amount_raw.as_u64(),
                destination_data: None,
            },
        );
        let (swap_pda, _) =
            derive_spl_swap_pda(&self.spl_program_id, &funder.pubkey(), &secret_hash);
        Ok((
            instruction,
            SolanaHtlcLeg {
                request: request.clone(),
                kind: HtlcProgramKind::Spl,
                program_id: self.spl_program_id,
                swap_pda,
                mint: Some(mint),
                token_program: Some(token_program),
                status: SettlementStatus::Pending,
            },
        ))
    }

    fn build_redeem_instruction(
        &self,
        leg: &SolanaHtlcLeg,
        preimage: &str,
    ) -> Result<Instruction, AppError> {
        let redeemer = self.wallet(leg.request.redeemer)?;
        let secret = parse_secret_preimage(preimage)
            .map_err(|error| AppError::validation(error.to_string()))?;
        let data = RedeemData { secret };

        match leg.kind {
            HtlcProgramKind::Native => {
                let initiator = self.wallet(leg.request.funder)?.pubkey();
                Ok(build_redeem_native(
                    &leg.program_id,
                    &leg.swap_pda,
                    &initiator,
                    &redeemer.pubkey(),
                    &data,
                ))
            }
            HtlcProgramKind::Spl => {
                let mint = leg
                    .mint
                    .ok_or_else(|| AppError::internal("SPL HTLC leg missing mint"))?;
                let token_program = leg
                    .token_program
                    .ok_or_else(|| AppError::internal("SPL HTLC leg missing token program"))?;
                let sponsor = self.wallet(leg.request.funder)?.pubkey();
                let redeemer_ata =
                    crate::adapters::solana::client::SolanaClient::derive_associated_token_address(
                        &redeemer.pubkey(),
                        &mint,
                        &token_program,
                    );
                Ok(build_redeem_spl(
                    &leg.program_id,
                    &leg.swap_pda,
                    &mint,
                    &redeemer_ata,
                    &sponsor,
                    token_program,
                    &data,
                ))
            }
        }
    }

    fn build_refund_instruction(&self, leg: &SolanaHtlcLeg) -> Result<Instruction, AppError> {
        let funder = self.wallet(leg.request.funder)?;
        match leg.kind {
            HtlcProgramKind::Native => Ok(build_refund_native(
                &leg.program_id,
                &leg.swap_pda,
                &funder.pubkey(),
            )),
            HtlcProgramKind::Spl => {
                let mint = leg
                    .mint
                    .ok_or_else(|| AppError::internal("SPL HTLC leg missing mint"))?;
                let token_program = leg
                    .token_program
                    .ok_or_else(|| AppError::internal("SPL HTLC leg missing token program"))?;
                let funder_ata =
                    crate::adapters::solana::client::SolanaClient::derive_associated_token_address(
                        &funder.pubkey(),
                        &mint,
                        &token_program,
                    );
                Ok(build_refund_spl(
                    &leg.program_id,
                    &leg.swap_pda,
                    &mint,
                    &funder_ata,
                    &funder.pubkey(),
                    token_program,
                ))
            }
        }
    }

    async fn submit(
        &self,
        payer_role: WalletRole,
        instruction: Instruction,
    ) -> Result<TxSignature, AppError> {
        let payer = self.wallet(payer_role)?;
        let blockhash = self
            .rpc_client
            .get_latest_blockhash()
            .await
            .map_err(|error| AppError::solana(format!("get latest blockhash: {error}")))?;
        let transaction = Transaction::new_signed_with_payer(
            &[instruction],
            Some(&payer.pubkey()),
            &[payer.as_ref()],
            blockhash,
        );
        let signature = self
            .rpc_client
            .send_and_confirm_transaction(&transaction)
            .await
            .map_err(|error| {
                AppError::solana(format!("send and confirm HTLC transaction: {error}"))
            })?;
        self.await_confirmation(&signature).await?;
        Ok(TxSignature::new(signature.to_string()))
    }

    async fn await_confirmation(&self, signature: &Signature) -> Result<(), AppError> {
        for _ in 0..self.confirmation.max_attempts {
            tokio::time::sleep(self.confirmation.poll_interval).await;
            let response = self
                .rpc_client
                .get_signature_statuses(std::slice::from_ref(signature))
                .await
                .map_err(|error| AppError::solana(format!("getSignatureStatuses: {error}")))?;
            if let Some(Some(status)) = response.value.first() {
                if let Some(error) = &status.err {
                    return Err(AppError::solana(format!(
                        "HTLC transaction {signature} failed on-chain: {error:?}"
                    )));
                }
                if status.satisfies_commitment(CommitmentConfig::confirmed()) {
                    return Ok(());
                }
            }
        }

        Err(AppError::solana(format!(
            "confirmation timeout for HTLC transaction {signature}"
        )))
    }

    async fn token_program_for_mint(&self, mint: &Pubkey) -> Result<Pubkey, AppError> {
        let account = self
            .rpc_client
            .get_account(mint)
            .await
            .map_err(|error| AppError::solana(format!("get mint account {mint}: {error}")))?;
        if account.owner == LEGACY_TOKEN_PROGRAM_ID || account.owner == SOLANA_TOKEN_2022_PROGRAM_ID
        {
            Ok(account.owner)
        } else {
            Err(AppError::solana(format!(
                "mint {mint} has unsupported token program owner {}",
                account.owner
            )))
        }
    }

    fn wallet(&self, role: WalletRole) -> Result<Arc<Keypair>, AppError> {
        self.wallets
            .get(&role)
            .cloned()
            .ok_or_else(|| AppError::validation(format!("missing HTLC wallet for {role:?}")))
    }
}

#[async_trait]
impl HtlcClient for SolanaHtlcClient {
    async fn initiate(&self, request: HtlcInitiation) -> Result<HtlcReceipt, AppError> {
        let (instruction, mut leg) = self.build_initiate_instruction(&request).await?;
        let signature = self.submit(request.funder, instruction).await?;
        leg.status = SettlementStatus::Initiated;
        self.state
            .lock()
            .await
            .entry(request.trade_id)
            .or_default()
            .legs
            .push(leg);

        Ok(HtlcReceipt {
            trade_id: request.trade_id,
            status: SettlementStatus::Initiated,
            signature: Some(signature),
        })
    }

    async fn redeem(&self, trade_id: TradeId, preimage: String) -> Result<HtlcReceipt, AppError> {
        let leg = {
            let state = self.state.lock().await;
            let trade = state
                .get(&trade_id)
                .ok_or_else(|| AppError::validation(format!("unknown HTLC trade {trade_id}")))?;
            redeem_leg_for_count(trade)?
        };
        let instruction = self.build_redeem_instruction(&leg, &preimage)?;
        let signature = self.submit(leg.request.redeemer, instruction).await?;

        let mut state = self.state.lock().await;
        if let Some(trade) = state.get_mut(&trade_id) {
            trade.redeemed_count = trade.redeemed_count.saturating_add(1);
            if let Some(stored_leg) = trade
                .legs
                .iter_mut()
                .find(|stored| stored.swap_pda == leg.swap_pda)
            {
                stored_leg.status = SettlementStatus::Redeemed;
            }
        }

        Ok(HtlcReceipt {
            trade_id,
            status: SettlementStatus::Redeemed,
            signature: Some(signature),
        })
    }

    async fn refund(&self, trade_id: TradeId) -> Result<HtlcReceipt, AppError> {
        let leg = {
            let state = self.state.lock().await;
            let trade = state
                .get(&trade_id)
                .ok_or_else(|| AppError::validation(format!("unknown HTLC trade {trade_id}")))?;
            trade
                .legs
                .iter()
                .find(|leg| leg.status == SettlementStatus::Initiated)
                .cloned()
                .ok_or_else(|| {
                    AppError::validation(format!("no refundable HTLC leg for {trade_id}"))
                })?
        };
        let instruction = self.build_refund_instruction(&leg)?;
        let signature = self.submit(leg.request.funder, instruction).await?;

        Ok(HtlcReceipt {
            trade_id,
            status: SettlementStatus::Refunded,
            signature: Some(signature),
        })
    }

    async fn status(&self, trade_id: TradeId) -> Result<SettlementStatus, AppError> {
        let legs = self
            .state
            .lock()
            .await
            .get(&trade_id)
            .map(|trade| trade.legs.clone())
            .unwrap_or_default();
        if legs.is_empty() {
            return Ok(SettlementStatus::Pending);
        }

        for leg in &legs {
            if let Ok(account) = self.rpc_client.get_account(&leg.swap_pda).await {
                decode_live_swap_account(leg.kind, &account.data)?;
                return Ok(SettlementStatus::Initiated);
            }
        }

        if legs
            .iter()
            .all(|leg| leg.status == SettlementStatus::Redeemed)
        {
            Ok(SettlementStatus::Redeemed)
        } else {
            Ok(SettlementStatus::Pending)
        }
    }
}

fn redeem_leg_for_count(trade: &SolanaHtlcTradeState) -> Result<SolanaHtlcLeg, AppError> {
    let preferred_funder = if trade.redeemed_count == 0 {
        WalletRole::Maker
    } else {
        WalletRole::Taker
    };
    trade
        .legs
        .iter()
        .find(|leg| {
            leg.request.funder == preferred_funder && leg.status == SettlementStatus::Initiated
        })
        .or_else(|| {
            trade
                .legs
                .iter()
                .find(|leg| leg.status == SettlementStatus::Initiated)
        })
        .cloned()
        .ok_or_else(|| AppError::validation("no redeemable HTLC leg found"))
}

fn decode_live_swap_account(kind: HtlcProgramKind, data: &[u8]) -> Result<(), AppError> {
    match kind {
        HtlcProgramKind::Native => decode_native_swap_account(data)
            .map(|_| ())
            .map_err(|error| AppError::solana(format!("decode native HTLC account: {error}"))),
        HtlcProgramKind::Spl => decode_spl_swap_account(data)
            .map(|_| ())
            .map_err(|error| AppError::solana(format!("decode SPL HTLC account: {error}"))),
    }
}

fn expires_in_slots(expires_at: time::OffsetDateTime) -> u64 {
    let seconds = (expires_at - time::OffsetDateTime::now_utc())
        .whole_seconds()
        .max(1);
    u64::try_from(seconds).unwrap_or(1).saturating_mul(3)
}

#[cfg(test)]
mod tests {
    use super::super::*;
    use super::*;
    use crate::domain::types::{AmountRaw, AssetId, TokenAmount, TradeId};
    use borsh::BorshSerialize;
    use solana_client::nonblocking::rpc_client::RpcClient;
    use solana_sdk::hash::Hash;
    use solana_sdk::instruction::AccountMeta;
    use solana_sdk::pubkey::Pubkey;
    use solana_sdk::signature::Keypair;
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

    fn client_with_wallets(maker: Arc<Keypair>, taker: Arc<Keypair>) -> SolanaHtlcClient {
        SolanaHtlcClient::new(SolanaHtlcClientConfig {
            rpc_client: Arc::new(RpcClient::new("http://127.0.0.1:8899".to_owned())),
            registry: AssetRegistry::default(),
            wallets: vec![(WalletRole::Maker, maker), (WalletRole::Taker, taker)],
            confirmation: HtlcConfirmationConfig {
                max_attempts: 0,
                poll_interval: Duration::from_millis(1),
            },
        })
        .expect("client")
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
    fn solana_htlc_client_builds_native_initiate_transaction_without_rpc() {
        let maker = Arc::new(Keypair::new());
        let taker = Arc::new(Keypair::new());
        let client = client_with_wallets(Arc::clone(&maker), Arc::clone(&taker));
        let secret = [7u8; 32];
        let request = HtlcInitiation {
            trade_id: TradeId::generate(),
            funder: WalletRole::Maker,
            redeemer: WalletRole::Taker,
            amount: TokenAmount::new(AssetId::from("SOL"), AmountRaw::new(100_000)),
            hashlock: hex::encode(hash_secret(&secret)),
            expires_at: time::OffsetDateTime::now_utc() + time::Duration::seconds(30),
        };

        let transaction = client
            .build_native_initiate_transaction(&request, Hash::new_unique())
            .expect("transaction");

        assert_eq!(transaction.signatures.len(), 1);
        assert_ne!(
            transaction.signatures[0],
            solana_sdk::signature::Signature::default()
        );
        let message = transaction.message;
        assert_eq!(message.account_keys[0], maker.pubkey());
        assert_eq!(message.instructions.len(), 1);
        let instruction = &message.instructions[0];
        let decoded = decode_initiate_native_data(&instruction.data).expect("decode initiate");
        assert_eq!(decoded.redeemer, taker.pubkey());
    }

    #[test]
    fn solana_htlc_client_spl_redeem_uses_original_funder_as_sponsor() {
        let maker = Arc::new(Keypair::new());
        let taker = Arc::new(Keypair::new());
        let client = client_with_wallets(Arc::clone(&maker), Arc::clone(&taker));
        let secret = [9u8; 32];
        let secret_hash = hash_secret(&secret);
        let mint = crate::domain::assets::USDC_MINT
            .parse::<Pubkey>()
            .expect("USDC mint pubkey");
        let (swap_pda, _) = derive_spl_swap_pda(&spl_program(), &taker.pubkey(), &secret_hash);
        let leg = SolanaHtlcLeg {
            request: HtlcInitiation {
                trade_id: TradeId::generate(),
                funder: WalletRole::Taker,
                redeemer: WalletRole::Maker,
                amount: TokenAmount::new(AssetId::from("USDC"), AmountRaw::new(100_000)),
                hashlock: hex::encode(secret_hash),
                expires_at: time::OffsetDateTime::now_utc() + time::Duration::seconds(30),
            },
            kind: HtlcProgramKind::Spl,
            program_id: spl_program(),
            swap_pda,
            mint: Some(mint),
            token_program: Some(token_program()),
            status: SettlementStatus::Initiated,
        };

        let instruction = client
            .build_redeem_instruction(&leg, &hex::encode(secret))
            .expect("redeem instruction");

        let maker_ata =
            crate::adapters::solana::client::SolanaClient::derive_associated_token_address(
                &maker.pubkey(),
                &mint,
                &token_program(),
            );
        assert_eq!(instruction.accounts[4], AccountMeta::new(maker_ata, false));
        assert_eq!(
            instruction.accounts[5],
            AccountMeta::new(taker.pubkey(), false)
        );
        assert_ne!(instruction.accounts[5].pubkey, maker.pubkey());
    }

    #[tokio::test]
    async fn live_htlc_smoke_tests_skip_without_explicit_opt_in() {
        if std::env::var("RUN_LIVE_SOLANA_TESTS").as_deref() != Ok("1")
            || std::env::var("RUN_LIVE_HTLC_TESTS").as_deref() != Ok("1")
        {
            eprintln!(
                "skipping live Solana HTLC smoke tests; set RUN_LIVE_SOLANA_TESTS=1 and RUN_LIVE_HTLC_TESTS=1"
            );
            return;
        }

        if std::env::var("SOLANA_RPC_URL").is_err() {
            eprintln!("skipping live Solana HTLC smoke tests; SOLANA_RPC_URL is not set");
            return;
        }

        let maker_wallet_present = std::env::var("MAKER_PRIVATE_KEY").is_ok()
            || std::env::var("MAKER_KEYPAIR_JSON").is_ok()
            || std::env::var("MAKER_KEYPAIR_PATH").is_ok();
        let taker_wallet_present = std::env::var("TAKER_PRIVATE_KEY").is_ok()
            || std::env::var("TAKER_KEYPAIR_JSON").is_ok()
            || std::env::var("TAKER_KEYPAIR_PATH").is_ok();
        if !(maker_wallet_present && taker_wallet_present) {
            eprintln!(
                "skipping live Solana HTLC smoke tests; maker/taker keypair env vars are not both set"
            );
            return;
        }

        let amount_lamports = std::env::var("LIVE_HTLC_AMOUNT_LAMPORTS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(5_000);
        assert!(
            amount_lamports <= 100_000,
            "LIVE_HTLC_AMOUNT_LAMPORTS must stay at or below 100000 lamports"
        );

        let wallets = crate::adapters::solana::wallets::DemoWallets::from_env_config(
            &crate::config::WalletsConfig::default(),
        )
        .expect("load live demo wallets");
        let rpc_url = std::env::var("SOLANA_RPC_URL").expect("SOLANA_RPC_URL checked");
        let client = SolanaHtlcClient::new(SolanaHtlcClientConfig {
            rpc_client: Arc::new(RpcClient::new(rpc_url)),
            registry: AssetRegistry::default(),
            wallets: vec![
                (
                    WalletRole::Maker,
                    Arc::new(wallets.maker.try_clone_keypair().expect("clone maker")),
                ),
                (
                    WalletRole::Taker,
                    Arc::new(wallets.taker.try_clone_keypair().expect("clone taker")),
                ),
            ],
            confirmation: HtlcConfirmationConfig::default(),
        })
        .expect("live HTLC client");

        let secret = generate_secret();
        let trade_id = TradeId::generate();
        let initiate = client
            .initiate(HtlcInitiation {
                trade_id,
                funder: WalletRole::Maker,
                redeemer: WalletRole::Taker,
                amount: TokenAmount::new(AssetId::from("SOL"), AmountRaw::new(amount_lamports)),
                hashlock: hex::encode(hash_secret(&secret)),
                expires_at: time::OffsetDateTime::now_utc() + time::Duration::seconds(45),
            })
            .await
            .expect("initiate live native HTLC");
        assert!(initiate.signature.is_some());

        let redeem = client
            .redeem(trade_id, hex::encode(secret))
            .await
            .expect("redeem live native HTLC");
        assert!(redeem.signature.is_some());

        let refund_secret = generate_secret();
        let refund_trade_id = TradeId::generate();
        let refund_initiate = client
            .initiate(HtlcInitiation {
                trade_id: refund_trade_id,
                funder: WalletRole::Maker,
                redeemer: WalletRole::Taker,
                amount: TokenAmount::new(AssetId::from("SOL"), AmountRaw::new(amount_lamports)),
                hashlock: hex::encode(hash_secret(&refund_secret)),
                expires_at: time::OffsetDateTime::now_utc() + time::Duration::seconds(1),
            })
            .await
            .expect("initiate refundable live native HTLC");
        assert!(refund_initiate.signature.is_some());

        tokio::time::sleep(Duration::from_secs(4)).await;
        let refund = client
            .refund(refund_trade_id)
            .await
            .expect("refund live native HTLC");
        assert!(refund.signature.is_some());
    }
}
