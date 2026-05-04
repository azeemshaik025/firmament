//! Runtime bootstrap and live adapter assembly.

use std::env;
use std::sync::Arc;

use ed25519_dalek::SigningKey;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::{Keypair, Signer};
use time::OffsetDateTime;

use crate::adapters::circle_gateway::{
    CircleGatewayClient, CircleGatewayClientConfig, GatewayMintSubmitter, USDC_MINT,
};
use crate::adapters::jupiter::{JupiterClient, JupiterClientConfig, JupiterWallet};
use crate::adapters::solana::htlc::{
    HtlcConfirmationConfig, SolanaHtlcClient, SolanaHtlcClientConfig,
};
use crate::adapters::solana::wallets::{DemoWallets, LoadedWallet, SOLANA_RPC_URL_ENV};
use crate::config::{AppConfig, CIRCLE_GATEWAY_SOLANA_ADDRESS_ENV, ProtocolWorkerScope};
use crate::domain::assets::AssetRegistry;
use crate::domain::events::{EventMetadata, RuntimeEvent, SystemEvent};
use crate::domain::types::{RuntimeRunId, WalletRole};
use crate::error::{AppError, AppResult};

use super::orchestrator::{
    RuntimeAdapters, RuntimeOrchestrator, RuntimeOrchestratorOptions, RuntimePersistence,
};
use super::projection::{RuntimeHandle, RuntimeState};

/// Application state shared by API, web app, workers, and tests.
#[derive(Debug, Clone)]
pub struct AppState {
    config: AppConfig,
    runtime: RuntimeHandle,
    run_id: RuntimeRunId,
}

impl AppState {
    /// Borrow the loaded application config.
    #[must_use]
    pub const fn config(&self) -> &AppConfig {
        &self.config
    }

    /// Return a handle to the current runtime projection.
    #[must_use]
    pub fn runtime(&self) -> RuntimeHandle {
        self.runtime.clone()
    }

    /// Clone the latest runtime projection.
    pub async fn runtime_snapshot(&self) -> RuntimeState {
        self.runtime.snapshot().await
    }

    /// Return this process run identifier.
    #[must_use]
    pub const fn run_id(&self) -> RuntimeRunId {
        self.run_id
    }
}

/// Build the runtime state without starting protocol workers.
///
/// # Errors
///
/// Returns an error when supplied config is invalid for startup.
pub async fn bootstrap(config: AppConfig) -> AppResult<AppState> {
    config.validate()?;

    let run_id = RuntimeRunId::generate();
    let started_at = OffsetDateTime::now_utc();
    let worker_state = if config.runtime.enable_protocol_workers {
        "enabled"
    } else {
        "disabled"
    };
    let ready_event = RuntimeEvent::System(SystemEvent::RuntimeReady {
        metadata: EventMetadata::new(run_id),
        message: format!("runtime initialized; protocol workers are {worker_state}"),
    });

    let event_capacity = config.runtime.event_capacity;
    let runtime = RuntimeState::bootstrap(run_id, started_at, &config, ready_event)?;
    let runtime = RuntimeHandle::new(runtime, event_capacity);

    Ok(AppState {
        config,
        runtime,
        run_id,
    })
}

/// Fully assembled live runtime used by production startup.
#[derive(Clone)]
pub struct LiveRuntime {
    /// Shared runtime state projection.
    pub app_state: AppState,
    /// Orchestrator backed by live protocol adapters.
    pub orchestrator: Arc<RuntimeOrchestrator>,
}

/// Build the maker-only live runtime assembly.
///
/// # Errors
///
/// Returns non-secret configuration, wallet, RPC, adapter, or persistence
/// errors when live protocol workers are enabled but cannot be wired.
pub async fn bootstrap_maker_runtime(config: AppConfig) -> AppResult<LiveRuntime> {
    config.validate_protocol_workers_ready_for(ProtocolWorkerScope::Maker)?;
    ensure_live_runtime_enabled(&config)?;
    assemble_live_runtime(config, LoadedWallet::from_maker_env()?, None).await
}

/// Build the legacy two-wallet local-signing demo runtime assembly.
///
/// # Errors
///
/// Returns non-secret configuration, wallet, RPC, adapter, or persistence
/// errors when live protocol workers are enabled but cannot be wired.
pub async fn bootstrap_demo_runtime(config: AppConfig) -> AppResult<LiveRuntime> {
    config.validate_protocol_workers_ready_for(ProtocolWorkerScope::Demo)?;
    ensure_live_runtime_enabled(&config)?;
    let wallets = DemoWallets::from_env()?;
    assemble_live_runtime(config, wallets.maker, Some(wallets.taker)).await
}

/// Build the default live runtime assembly.
///
/// The web app uses connected browser wallets for taker settlement, so the
/// default live runtime only requires the maker wallet.
///
/// # Errors
///
/// Returns non-secret configuration, wallet, RPC, adapter, or persistence
/// errors when live protocol workers are enabled but cannot be wired.
pub async fn bootstrap_live_runtime(config: AppConfig) -> AppResult<LiveRuntime> {
    bootstrap_maker_runtime(config).await
}

async fn assemble_live_runtime(
    config: AppConfig,
    maker_wallet: LoadedWallet,
    taker_wallet: Option<LoadedWallet>,
) -> AppResult<LiveRuntime> {
    let app_state = bootstrap(config.clone()).await?;
    let registry = AssetRegistry::from_config(&config);
    let solana = crate::adapters::solana::client::SolanaClient::new(
        env_required(SOLANA_RPC_URL_ENV)?,
        &config.solana.commitment,
    )?;

    let demo_taker_wallet = taker_wallet.as_ref().map(LoadedWallet::address);
    let maker_keypair = Arc::new(maker_wallet.try_clone_keypair()?);
    let taker_keypair = taker_wallet
        .map(|wallet| wallet.try_clone_keypair().map(Arc::new))
        .transpose()?;
    let allow_local_taker_settlement = taker_keypair.is_some();

    let mut wallet_pubkeys = vec![(WalletRole::Maker, maker_keypair.pubkey())];
    let mut jupiter_wallets = vec![JupiterWallet::from_keypair(
        WalletRole::Maker,
        clone_keypair(maker_keypair.as_ref(), WalletRole::Maker)?,
    )];
    let mut htlc_wallets = vec![(WalletRole::Maker, Arc::clone(&maker_keypair))];

    if let Some(taker_keypair) = &taker_keypair {
        wallet_pubkeys.push((WalletRole::Taker, taker_keypair.pubkey()));
        jupiter_wallets.push(JupiterWallet::from_keypair(
            WalletRole::Taker,
            clone_keypair(taker_keypair.as_ref(), WalletRole::Taker)?,
        ));
        htlc_wallets.push((WalletRole::Taker, Arc::clone(taker_keypair)));
    }

    let jupiter = Arc::new(JupiterClient::new(JupiterClientConfig::from_app_config(
        &config,
        jupiter_wallets,
    )?)?);

    let balance_reader = Arc::new(crate::adapters::solana::client::SolanaBalanceReader::new(
        solana.clone(),
        registry.clone(),
        wallet_pubkeys,
    ));
    let htlc_client = Arc::new(SolanaHtlcClient::new(SolanaHtlcClientConfig {
        rpc_client: solana.rpc_client(),
        registry: registry.clone(),
        wallets: htlc_wallets,
        confirmation: HtlcConfirmationConfig::default(),
    })?);

    let gateway = Arc::new(build_gateway_client(
        &config,
        solana.rpc_client(),
        Arc::clone(&maker_keypair),
    )?);
    let persistence = Arc::new(RuntimePersistence::open(
        &config.runtime.database_path,
        registry,
    )?);

    let orchestrator = Arc::new(RuntimeOrchestrator::new_with_persistence(
        app_state.clone(),
        RuntimeAdapters {
            price_provider: jupiter.clone(),
            htlc_client,
            swap_executor: jupiter,
            gateway_client: gateway,
            balance_reader,
        },
        persistence,
        RuntimeOrchestratorOptions {
            demo_taker_wallet,
            allow_local_taker_settlement,
            ..RuntimeOrchestratorOptions::default()
        },
    ));

    Ok(LiveRuntime {
        app_state,
        orchestrator,
    })
}

fn ensure_live_runtime_enabled(config: &AppConfig) -> AppResult<()> {
    if config.runtime.enable_protocol_workers {
        return Ok(());
    }

    Err(AppError::config(
        "runtime.enable_protocol_workers must be true for live runtime assembly",
    ))
}

fn build_gateway_client(
    config: &AppConfig,
    rpc_client: Arc<solana_client::nonblocking::rpc_client::RpcClient>,
    maker_keypair: Arc<Keypair>,
) -> AppResult<CircleGatewayClient> {
    let depositor = env_required(CIRCLE_GATEWAY_SOLANA_ADDRESS_ENV)?
        .parse::<Pubkey>()
        .map_err(|error| {
            AppError::config(format!(
                "{CIRCLE_GATEWAY_SOLANA_ADDRESS_ENV} must contain a valid Solana pubkey: {error}"
            ))
        })?;
    let usdc_mint = USDC_MINT
        .parse::<Pubkey>()
        .map_err(|error| AppError::internal(format!("parse USDC mint: {error}")))?;
    let destination_recipient_token_account =
        crate::adapters::solana::client::SolanaClient::derive_associated_token_address(
            &maker_keypair.pubkey(),
            &usdc_mint,
            &crate::adapters::solana::client::LEGACY_TOKEN_PROGRAM_ID,
        );
    let signing_key = signing_key_from_solana_keypair(maker_keypair.as_ref());
    let mut gateway_config = CircleGatewayClientConfig::mainnet(
        depositor,
        destination_recipient_token_account,
        Some(signing_key),
        config.gateway.max_refill_fee_raw.as_u64(),
    )?;
    gateway_config.mint_submitter = Some(GatewayMintSubmitter::new(rpc_client, maker_keypair));

    CircleGatewayClient::new(gateway_config).map_err(AppError::from)
}

fn env_required(name: &str) -> AppResult<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::config(format!("required env var {name} is not set")))
}

fn clone_keypair(keypair: &Keypair, role: WalletRole) -> AppResult<Keypair> {
    Keypair::try_from(keypair.to_bytes().as_slice()).map_err(|error| {
        AppError::validation(format!("clone {role:?} keypair for adapter owner: {error}"))
    })
}

fn signing_key_from_solana_keypair(keypair: &Keypair) -> SigningKey {
    let bytes = keypair.to_bytes();
    let mut secret = [0_u8; 32];
    secret.copy_from_slice(&bytes[..32]);
    SigningKey::from_bytes(&secret)
}
