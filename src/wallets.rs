//! Runtime wallet loading for the two-wallet Solana demo.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::{Keypair, Signer};
use thiserror::Error;

use crate::config::{WalletConfig, WalletsConfig};
use crate::error::AppError;
use crate::types::{WalletAddress, WalletRole};

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
        let path = non_empty_env(&config.keypair_path_env);
        let json = non_empty_env(&config.keypair_json_env);

        match (path, json) {
            (Some(_), Some(_)) => Err(WalletError::AmbiguousKeypairSources {
                role: config.role,
                path_env: config.keypair_path_env.clone(),
                json_env: config.keypair_json_env.clone(),
            }),
            (Some(path), None) => Self::from_path(config.role, path),
            (None, Some(json)) => Self::from_json(config.role, &json),
            (None, None) => Err(WalletError::MissingKeypair {
                role: config.role,
                path_env: config.keypair_path_env.clone(),
                json_env: config.keypair_json_env.clone(),
            }),
        }
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
        "missing keypair for {role:?}: set either env var '{path_env}' or env var '{json_env}'"
    )]
    MissingKeypair {
        /// Runtime wallet role.
        role: WalletRole,
        /// Env var that should contain the keypair path.
        path_env: String,
        /// Env var that should contain raw keypair JSON.
        json_env: String,
    },

    /// Both path and JSON sources were provided for one role.
    #[error(
        "ambiguous keypair sources for {role:?}: both env vars '{path_env}' and '{json_env}' are set"
    )]
    AmbiguousKeypairSources {
        /// Runtime wallet role.
        role: WalletRole,
        /// Env var containing a keypair path.
        path_env: String,
        /// Env var containing raw keypair JSON.
        json_env: String,
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
}

impl From<WalletError> for AppError {
    fn from(error: WalletError) -> Self {
        Self::validation(error.to_string())
    }
}

fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use solana_sdk::signature::{Keypair, Signer};

    use super::*;
    use crate::config::WalletConfig;
    use crate::types::WalletRole;

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
    fn wallets_env_loader_reports_missing_secret_references() {
        let config = WalletConfig {
            role: WalletRole::Maker,
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
        let maker_secret_configured = std::env::var(&config.maker.keypair_path_env).is_ok()
            || std::env::var(&config.maker.keypair_json_env).is_ok();
        let taker_secret_configured = std::env::var(&config.taker.keypair_path_env).is_ok()
            || std::env::var(&config.taker.keypair_json_env).is_ok();

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
