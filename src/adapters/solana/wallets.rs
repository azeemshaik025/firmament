//! Runtime wallet loading for the two-wallet Solana demo.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::{Keypair, Signer};
use thiserror::Error;

use crate::config::{WalletConfig, WalletsConfig};
use crate::domain::types::{WalletAddress, WalletRole};
use crate::error::AppError;

/// A loaded Solana keypair tagged with its runtime role.
pub struct LoadedWallet {
    role: WalletRole,
    keypair: Keypair,
}

impl LoadedWallet {
    /// Load a wallet keypair from a configured environment-backed source.
    ///
    /// # Errors
    ///
    /// Returns a wallet error when neither configured env var is set, both are
    /// set, or the referenced keypair material cannot be parsed.
    pub fn from_env_config(config: &WalletConfig) -> Result<Self, WalletError> {
        let private_key = non_empty_env(&config.private_key_env);
        let path = non_empty_env(&config.keypair_path_env);
        let json = non_empty_env(&config.keypair_json_env);

        match (private_key, path, json) {
            (Some(private_key), None, None) => Self::from_private_key_base58(
                config.role,
                &private_key,
                config.private_key_env.as_str(),
            ),
            (None, Some(path), None) => Self::from_path(config.role, path),
            (None, None, Some(json)) => Self::from_json(config.role, &json),
            (None, None, None) => Err(WalletError::MissingKeypair {
                role: config.role,
                env_vars: config
                    .keypair_source_envs()
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
            }),
            _ => Err(WalletError::AmbiguousKeypairSources {
                role: config.role,
                env_vars: configured_source_names(config),
            }),
        }
    }

    /// Load a wallet from a base58 Solana private key string.
    ///
    /// # Errors
    ///
    /// Returns a wallet error when the base58 string is not a valid Solana
    /// keypair private key.
    pub fn from_private_key_base58(
        role: WalletRole,
        private_key: &str,
        env_name: &str,
    ) -> Result<Self, WalletError> {
        let keypair = Keypair::try_from_base58_string(private_key.trim()).map_err(|error| {
            WalletError::InvalidPrivateKey {
                role,
                env_name: env_name.to_owned(),
                message: error.to_string(),
            }
        })?;
        Ok(Self { role, keypair })
    }

    /// Load a wallet from a Solana CLI-style JSON keypair file.
    ///
    /// # Errors
    ///
    /// Returns a wallet error when the file cannot be read or parsed.
    pub fn from_path(role: WalletRole, path: impl AsRef<Path>) -> Result<Self, WalletError> {
        let path = path.as_ref();
        let json = fs::read_to_string(path).map_err(|error| WalletError::ReadKeypairFile {
            role,
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
        Self::from_json(role, &json)
    }

    /// Load a wallet from Solana CLI-style JSON keypair bytes.
    ///
    /// # Errors
    ///
    /// Returns a wallet error when the JSON is invalid or the bytes do not form
    /// a valid Solana keypair.
    pub fn from_json(role: WalletRole, json: &str) -> Result<Self, WalletError> {
        let bytes: Vec<u8> =
            serde_json::from_str(json).map_err(|error| WalletError::InvalidKeypairJson {
                role,
                message: error.to_string(),
            })?;
        let keypair = Keypair::try_from(bytes.as_slice()).map_err(|error| {
            WalletError::InvalidKeypairBytes {
                role,
                message: error.to_string(),
            }
        })?;
        Ok(Self { role, keypair })
    }

    /// Return the runtime role for this wallet.
    #[must_use]
    pub const fn role(&self) -> WalletRole {
        self.role
    }

    /// Return the Solana public key for this wallet.
    #[must_use]
    pub fn pubkey(&self) -> Pubkey {
        self.keypair.pubkey()
    }

    /// Return the public address as the crate's shared value object.
    #[must_use]
    pub fn address(&self) -> WalletAddress {
        WalletAddress::new(self.pubkey().to_string())
    }

    /// Borrow the signing keypair for future transaction builders.
    #[must_use]
    pub const fn keypair(&self) -> &Keypair {
        &self.keypair
    }

    /// Duplicate the keypair for adapter owners that need independent signer
    /// handles. Secret bytes are copied in memory only and are never formatted.
    ///
    /// # Errors
    ///
    /// Returns a wallet error if the in-memory Solana keypair bytes cannot be
    /// reconstituted.
    pub fn try_clone_keypair(&self) -> Result<Keypair, WalletError> {
        let bytes = self.keypair.to_bytes();
        Keypair::try_from(bytes.as_slice()).map_err(|error| WalletError::InvalidKeypairBytes {
            role: self.role,
            message: error.to_string(),
        })
    }
}

impl fmt::Debug for LoadedWallet {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LoadedWallet")
            .field("role", &self.role)
            .field("pubkey", &self.pubkey().to_string())
            .field("keypair", &"[REDACTED]")
            .finish()
    }
}

/// Maker/operator and taker/app wallets used by the demo.
#[derive(Debug)]
pub struct DemoWallets {
    /// Maker/operator wallet that owns working liquidity.
    pub maker: LoadedWallet,
    /// Taker/app wallet used for settlement demos.
    pub taker: LoadedWallet,
}

impl DemoWallets {
    /// Load both demo wallets from configured environment-backed sources.
    ///
    /// # Errors
    ///
    /// Returns a wallet error when either wallet cannot be loaded.
    pub fn from_env_config(config: &WalletsConfig) -> Result<Self, WalletError> {
        Ok(Self {
            maker: LoadedWallet::from_env_config(&config.maker)?,
            taker: LoadedWallet::from_env_config(&config.taker)?,
        })
    }
}

/// Safe wallet-loading errors. Secret values are never stored in these variants.
#[derive(Debug, Error)]
pub enum WalletError {
    /// Neither configured keypair env var was set.
    #[error(
        "missing keypair for {role:?}: set exactly one keypair source env var from {env_vars:?}"
    )]
    MissingKeypair {
        /// Runtime wallet role.
        role: WalletRole,
        /// Accepted env vars for this wallet.
        env_vars: Vec<String>,
    },

    /// More than one secret source was provided for one role.
    #[error(
        "ambiguous keypair sources for {role:?}: set only one keypair source env var; currently set {env_vars:?}"
    )]
    AmbiguousKeypairSources {
        /// Runtime wallet role.
        role: WalletRole,
        /// Env vars that were configured.
        env_vars: Vec<String>,
    },

    /// Keypair file could not be read.
    #[error("failed to read keypair file for {role:?} at {}: {message}", path.display())]
    ReadKeypairFile {
        /// Runtime wallet role.
        role: WalletRole,
        /// Path that failed to load.
        path: PathBuf,
        /// Read error message.
        message: String,
    },

    /// Keypair JSON was invalid.
    #[error("invalid keypair JSON for {role:?}: {message}")]
    InvalidKeypairJson {
        /// Runtime wallet role.
        role: WalletRole,
        /// Parser message.
        message: String,
    },

    /// Keypair bytes failed Solana validation.
    #[error("invalid keypair bytes for {role:?}: {message}")]
    InvalidKeypairBytes {
        /// Runtime wallet role.
        role: WalletRole,
        /// Parser message.
        message: String,
    },

    /// A base58 private key env var was invalid.
    #[error("invalid base58 private key in env var '{env_name}' for {role:?}: {message}")]
    InvalidPrivateKey {
        /// Runtime wallet role.
        role: WalletRole,
        /// Env var containing the private key.
        env_name: String,
        /// Parser message.
        message: String,
    },
}

impl From<WalletError> for AppError {
    fn from(error: WalletError) -> Self {
        Self::validation(error.to_string())
    }
}

fn non_empty_env(name: &str) -> Option<String> {
    if name.trim().is_empty() {
        return None;
    }

    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn configured_source_names(config: &WalletConfig) -> Vec<String> {
    config
        .keypair_source_envs()
        .into_iter()
        .filter(|name| non_empty_env(name).is_some())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use solana_sdk::signature::{Keypair, Signer};

    use super::*;
    use crate::config::WalletConfig;
    use crate::domain::types::WalletRole;

    fn keypair_json(keypair: &Keypair) -> String {
        serde_json::to_string(&keypair.to_bytes().to_vec()).unwrap()
    }

    #[test]
    fn wallets_load_keypair_from_json_without_exposing_secret_in_debug() {
        let expected = Keypair::new();
        let wallet = LoadedWallet::from_json(WalletRole::Maker, &keypair_json(&expected)).unwrap();

        assert_eq!(wallet.role(), WalletRole::Maker);
        assert_eq!(wallet.pubkey(), expected.pubkey());

        let debug = format!("{wallet:?}");
        assert!(debug.contains("Maker"));
        assert!(debug.contains(&expected.pubkey().to_string()));
        assert!(!debug.contains(&keypair_json(&expected)));
    }

    #[test]
    fn wallets_load_keypair_from_path() {
        let expected = Keypair::new();
        let path =
            std::env::temp_dir().join(format!("rfq-maker-wallet-{}.json", uuid::Uuid::now_v7()));
        fs::write(&path, keypair_json(&expected)).unwrap();

        let wallet = LoadedWallet::from_path(WalletRole::Taker, &path).unwrap();
        fs::remove_file(&path).unwrap();

        assert_eq!(wallet.role(), WalletRole::Taker);
        assert_eq!(wallet.pubkey(), expected.pubkey());
    }

    #[test]
    fn wallets_load_keypair_from_base58_private_key() {
        let expected = Keypair::new();
        let wallet = LoadedWallet::from_private_key_base58(
            WalletRole::Maker,
            &expected.to_base58_string(),
            "MAKER_PRIVATE_KEY",
        )
        .unwrap();

        assert_eq!(wallet.role(), WalletRole::Maker);
        assert_eq!(wallet.pubkey(), expected.pubkey());
    }

    #[test]
    fn wallets_env_loader_reports_missing_secret_references() {
        let config = WalletConfig {
            role: WalletRole::Maker,
            private_key_env: format!("MISSING_PRIVATE_KEY_{}", uuid::Uuid::now_v7().simple()),
            keypair_path_env: format!("MISSING_PATH_{}", uuid::Uuid::now_v7().simple()),
            keypair_json_env: format!("MISSING_JSON_{}", uuid::Uuid::now_v7().simple()),
        };

        assert!(matches!(
            LoadedWallet::from_env_config(&config),
            Err(WalletError::MissingKeypair { .. })
        ));
    }

    #[test]
    fn wallets_live_loading_skips_without_env() {
        if std::env::var("RUN_LIVE_SOLANA_TESTS").ok().as_deref() != Some("1") {
            eprintln!("skipping live wallet loading; RUN_LIVE_SOLANA_TESTS=1 is not set");
            return;
        }

        let config = crate::config::WalletsConfig::default();
        let maker_secret_configured = config
            .maker
            .keypair_source_envs()
            .into_iter()
            .any(|name| std::env::var(name).is_ok());
        let taker_secret_configured = config
            .taker
            .keypair_source_envs()
            .into_iter()
            .any(|name| std::env::var(name).is_ok());

        if !maker_secret_configured || !taker_secret_configured {
            eprintln!(
                "skipping live wallet loading; maker/taker keypair env vars are not both set"
            );
            return;
        }

        let wallets = DemoWallets::from_env_config(&config).unwrap();
        assert_eq!(wallets.maker.role(), WalletRole::Maker);
        assert_eq!(wallets.taker.role(), WalletRole::Taker);
    }
}
