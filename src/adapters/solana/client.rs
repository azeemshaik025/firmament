//! Solana RPC, balance-read, and associated-token-account helpers.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

use async_trait::async_trait;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_commitment_config::CommitmentConfig;
use solana_sdk::instruction::Instruction;
use solana_sdk::pubkey::Pubkey;
use spl_associated_token_account_interface::address::get_associated_token_address_with_program_id;
use spl_associated_token_account_interface::instruction::create_associated_token_account_idempotent;
use thiserror::Error;
use time::OffsetDateTime;

use crate::domain::assets::{AssetError, AssetMetadata, AssetRegistry};
use crate::domain::types::{AmountRaw, BalanceSnapshot, TokenAmount, WalletRole};
use crate::error::AppError;
use crate::ports::BalanceReader;

/// Legacy SPL Token program ID.
pub const LEGACY_TOKEN_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");

/// SPL Token-2022 program ID.
pub const TOKEN_2022_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb");

/// Nonblocking Solana RPC client wrapper used by live adapters.
#[derive(Clone)]
pub struct SolanaClient {
    rpc_client: Arc<RpcClient>,
}

impl SolanaClient {
    /// Construct a Solana client from an RPC URL and commitment label.
    ///
    /// # Errors
    ///
    /// Returns a Solana client error when the commitment label is unsupported.
    pub fn new(
        rpc_url: impl Into<String>,
        commitment: impl AsRef<str>,
    ) -> Result<Self, SolanaClientError> {
        let commitment = commitment_from_label(commitment.as_ref())?;
        Ok(Self {
            rpc_client: Arc::new(RpcClient::new_with_commitment(rpc_url.into(), commitment)),
        })
    }

    /// Wrap an existing nonblocking RPC client.
    #[must_use]
    pub fn from_rpc_client(rpc_client: RpcClient) -> Self {
        Self {
            rpc_client: Arc::new(rpc_client),
        }
    }

    /// Borrow the inner RPC client for future protocol-specific adapters.
    #[must_use]
    pub fn rpc_client(&self) -> Arc<RpcClient> {
        Arc::clone(&self.rpc_client)
    }

    /// Read native SOL lamports for a wallet.
    ///
    /// # Errors
    ///
    /// Returns a Solana client error when RPC balance lookup fails.
    pub async fn native_sol_balance(
        &self,
        wallet: &Pubkey,
    ) -> Result<AmountRaw, SolanaClientError> {
        let lamports =
            self.rpc_client
                .get_balance(wallet)
                .await
                .map_err(|error| SolanaClientError::Rpc {
                    operation: "get_balance",
                    message: error.to_string(),
                })?;
        Ok(AmountRaw::new(lamports))
    }

    /// Read an asset balance for a wallet, preserving native SOL vs SPL paths.
    ///
    /// # Errors
    ///
    /// Returns a Solana client error when mint parsing, ATA derivation, token
    /// program detection, or RPC balance lookup fails.
    pub async fn balance_for_asset(
        &self,
        wallet: &Pubkey,
        asset: &AssetMetadata,
    ) -> Result<AmountRaw, SolanaClientError> {
        if asset.is_native_sol() {
            return self.native_sol_balance(wallet).await;
        }

        self.spl_token_balance(wallet, asset).await
    }

    /// Read an SPL token balance through its ATA. Missing ATAs return zero.
    ///
    /// # Errors
    ///
    /// Returns a Solana client error when the mint or token account cannot be
    /// decoded or RPC returns an unexpected error.
    pub async fn spl_token_balance(
        &self,
        wallet: &Pubkey,
        asset: &AssetMetadata,
    ) -> Result<AmountRaw, SolanaClientError> {
        let ata = self.associated_token_address(wallet, asset).await?;

        match self.rpc_client.get_token_account_balance(&ata).await {
            Ok(balance) => {
                let amount = u64::from_str(&balance.amount).map_err(|error| {
                    SolanaClientError::AmountParse {
                        account: ata,
                        amount: balance.amount,
                        message: error.to_string(),
                    }
                })?;
                Ok(AmountRaw::new(amount))
            }
            Err(error) if is_missing_account_error(&error.to_string()) => Ok(AmountRaw::new(0)),
            Err(error) => Err(SolanaClientError::Rpc {
                operation: "get_token_account_balance",
                message: error.to_string(),
            }),
        }
    }

    /// Resolve the ATA for a wallet and SPL asset.
    ///
    /// # Errors
    ///
    /// Returns an error when called for native SOL or when the mint owner is not
    /// a supported token program.
    pub async fn associated_token_address(
        &self,
        wallet: &Pubkey,
        asset: &AssetMetadata,
    ) -> Result<Pubkey, SolanaClientError> {
        let mint = asset.mint_pubkey()?;
        let token_program = self.token_program_for_mint(&mint).await?;
        Ok(Self::derive_associated_token_address(
            wallet,
            &mint,
            &token_program,
        ))
    }

    /// Deterministically derive an ATA for a wallet, mint, and token program.
    #[must_use]
    pub fn derive_associated_token_address(
        wallet: &Pubkey,
        mint: &Pubkey,
        token_program: &Pubkey,
    ) -> Pubkey {
        get_associated_token_address_with_program_id(wallet, mint, token_program)
    }

    /// Return true when the supplied ATA account currently exists.
    ///
    /// # Errors
    ///
    /// Returns a Solana client error for RPC failures other than missing
    /// account.
    pub async fn associated_token_account_exists(
        &self,
        ata: &Pubkey,
    ) -> Result<bool, SolanaClientError> {
        match self.rpc_client.get_account(ata).await {
            Ok(_) => Ok(true),
            Err(error) if is_missing_account_error(&error.to_string()) => Ok(false),
            Err(error) => Err(SolanaClientError::Rpc {
                operation: "get_account",
                message: error.to_string(),
            }),
        }
    }

    /// Build an idempotent ATA creation instruction without submitting it.
    ///
    /// # Errors
    ///
    /// Returns an error when the asset is native SOL or the mint owner is not a
    /// supported SPL token program.
    pub async fn create_associated_token_account_instruction(
        &self,
        payer: &Pubkey,
        wallet: &Pubkey,
        asset: &AssetMetadata,
    ) -> Result<Instruction, SolanaClientError> {
        let mint = asset.mint_pubkey()?;
        let token_program = self.token_program_for_mint(&mint).await?;
        Ok(Self::derive_create_associated_token_account_instruction(
            payer,
            wallet,
            &mint,
            &token_program,
        ))
    }

    /// Build an idempotent ATA creation instruction when the token program is
    /// already known.
    #[must_use]
    pub fn derive_create_associated_token_account_instruction(
        payer: &Pubkey,
        wallet: &Pubkey,
        mint: &Pubkey,
        token_program: &Pubkey,
    ) -> Instruction {
        create_associated_token_account_idempotent(payer, wallet, mint, token_program)
    }

    /// Resolve whether a mint is owned by legacy Token or Token-2022.
    ///
    /// # Errors
    ///
    /// Returns a Solana client error when the mint account cannot be fetched or
    /// its owner is neither accepted token program.
    pub async fn token_program_for_mint(&self, mint: &Pubkey) -> Result<Pubkey, SolanaClientError> {
        let account =
            self.rpc_client
                .get_account(mint)
                .await
                .map_err(|error| SolanaClientError::Rpc {
                    operation: "get_account",
                    message: error.to_string(),
                })?;

        let owner = account.owner;
        if owner == LEGACY_TOKEN_PROGRAM_ID || owner == TOKEN_2022_PROGRAM_ID {
            Ok(owner)
        } else {
            Err(SolanaClientError::UnsupportedTokenProgram { mint: *mint, owner })
        }
    }
}

/// `BalanceReader` implementation backed by configured wallet public keys.
#[derive(Clone)]
pub struct SolanaBalanceReader {
    client: SolanaClient,
    registry: AssetRegistry,
    wallets: HashMap<WalletRole, Pubkey>,
}

impl SolanaBalanceReader {
    /// Construct a balance reader from a client, registry, and wallet map.
    #[must_use]
    pub fn new(
        client: SolanaClient,
        registry: AssetRegistry,
        wallets: impl IntoIterator<Item = (WalletRole, Pubkey)>,
    ) -> Self {
        Self {
            client,
            registry,
            wallets: wallets.into_iter().collect(),
        }
    }
}

#[async_trait]
impl BalanceReader for SolanaBalanceReader {
    async fn balances(&self, wallet: WalletRole) -> Result<BalanceSnapshot, AppError> {
        let pubkey = self
            .wallets
            .get(&wallet)
            .ok_or(SolanaClientError::WalletNotConfigured(wallet))?;
        let mut balances = Vec::new();

        for asset in self.registry.assets() {
            let amount = self.client.balance_for_asset(pubkey, asset).await?;
            balances.push(TokenAmount::new(asset.id.clone(), amount));
        }

        Ok(BalanceSnapshot {
            wallet,
            balances,
            observed_at: OffsetDateTime::now_utc(),
        })
    }
}

/// Solana adapter errors safe for operator display.
#[derive(Debug, Error)]
pub enum SolanaClientError {
    /// The configured commitment label is unsupported.
    #[error("unsupported Solana commitment: {0}")]
    UnsupportedCommitment(String),

    /// Asset metadata failed local validation.
    #[error(transparent)]
    Asset(#[from] AssetError),

    /// Solana RPC call failed.
    #[error("Solana RPC {operation} failed: {message}")]
    Rpc {
        /// RPC method or operation label.
        operation: &'static str,
        /// RPC error message.
        message: String,
    },

    /// Mint owner is not legacy Token or Token-2022.
    #[error("mint {mint} has unsupported token program owner {owner}")]
    UnsupportedTokenProgram {
        /// Token mint.
        mint: Pubkey,
        /// On-chain owner program.
        owner: Pubkey,
    },

    /// Raw token balance did not fit in the runtime amount type.
    #[error("could not parse raw token balance {amount} for account {account}: {message}")]
    AmountParse {
        /// Token account whose balance was parsed.
        account: Pubkey,
        /// Raw amount string from RPC.
        amount: String,
        /// Parse error message.
        message: String,
    },

    /// The requested wallet role has no configured public key.
    #[error("wallet {0:?} is not configured for Solana balance reads")]
    WalletNotConfigured(WalletRole),
}

impl From<SolanaClientError> for AppError {
    fn from(error: SolanaClientError) -> Self {
        match error {
            SolanaClientError::UnsupportedCommitment(_)
            | SolanaClientError::Asset(_)
            | SolanaClientError::WalletNotConfigured(_) => Self::validation(error.to_string()),
            SolanaClientError::Rpc { .. }
            | SolanaClientError::UnsupportedTokenProgram { .. }
            | SolanaClientError::AmountParse { .. } => Self::solana(error.to_string()),
        }
    }
}

fn commitment_from_label(label: &str) -> Result<CommitmentConfig, SolanaClientError> {
    match label.trim().to_ascii_lowercase().as_str() {
        "processed" => Ok(CommitmentConfig::processed()),
        "confirmed" => Ok(CommitmentConfig::confirmed()),
        "finalized" => Ok(CommitmentConfig::finalized()),
        other => Err(SolanaClientError::UnsupportedCommitment(other.to_owned())),
    }
}

fn is_missing_account_error(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    message.contains("could not find account")
        || message.contains("accountnotfound")
        || message.contains("account not found")
        || message.contains("invalid param: could not find account")
}

#[cfg(test)]
mod tests {
    use solana_sdk::signature::Keypair;

    use super::*;
    use crate::adapters::solana::wallets::LoadedWallet;
    use crate::config::{SolanaConfig, WalletsConfig};
    use crate::domain::assets::{AssetRegistry, USDC_MINT};
    use crate::domain::types::AssetId;

    #[test]
    fn solana_client_derives_legacy_token_ata() {
        let keypair = Keypair::new_from_array([8; 32]);
        let keypair_json = serde_json::to_string(&keypair.to_bytes().to_vec()).unwrap();
        let wallet =
            LoadedWallet::from_json(crate::domain::types::WalletRole::Maker, &keypair_json)
                .unwrap();
        let registry = AssetRegistry::default();
        let usdc = registry.require_asset(&AssetId::from("USDC")).unwrap();

        let ata = SolanaClient::derive_associated_token_address(
            &wallet.pubkey(),
            &usdc.mint_pubkey().unwrap(),
            &LEGACY_TOKEN_PROGRAM_ID,
        );

        assert_ne!(ata, wallet.pubkey());
        assert_ne!(ata, usdc.mint_pubkey().unwrap());
        assert_eq!(usdc.mint().unwrap().as_str(), USDC_MINT);
    }

    #[tokio::test]
    async fn solana_client_live_balance_and_ata_reads_skip_without_env() {
        let Some((client, wallet)) = live_client_and_wallet() else {
            return;
        };

        let registry = AssetRegistry::default();
        let lamports = client.native_sol_balance(&wallet.pubkey()).await.unwrap();
        let usdc = registry.require_asset(&AssetId::from("USDC")).unwrap();
        let ata = client
            .associated_token_address(&wallet.pubkey(), usdc)
            .await
            .unwrap();
        let exists = client.associated_token_account_exists(&ata).await.unwrap();

        let _ = lamports;
        assert!(!ata.to_string().is_empty());
        let _ = exists;
    }

    fn live_client_and_wallet() -> Option<(SolanaClient, LoadedWallet)> {
        if std::env::var("RUN_LIVE_SOLANA_TESTS").ok().as_deref() != Some("1") {
            eprintln!("skipping live Solana RPC test; RUN_LIVE_SOLANA_TESTS=1 is not set");
            return None;
        }

        let solana_config = SolanaConfig::default();
        let rpc_url = match std::env::var(&solana_config.rpc_url_env) {
            Ok(value) if !value.trim().is_empty() => value,
            _ => {
                eprintln!("skipping live Solana RPC test; SOLANA_RPC_URL is not set");
                return None;
            }
        };

        let wallet_config = WalletsConfig::default().maker;
        let has_wallet = wallet_config
            .keypair_source_envs()
            .into_iter()
            .any(|name| std::env::var(name).is_ok());
        if !has_wallet {
            eprintln!("skipping live Solana RPC test; maker keypair env vars are not set");
            return None;
        }

        let wallet = LoadedWallet::from_env_config(&wallet_config).unwrap();
        let client = SolanaClient::new(rpc_url, solana_config.commitment).unwrap();
        Some((client, wallet))
    }
}
