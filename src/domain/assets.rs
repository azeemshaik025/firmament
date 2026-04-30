//! Static Solana asset registry and integer/Decimal amount helpers.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use solana_sdk::pubkey::Pubkey;
use thiserror::Error;

use crate::config::AppConfig;
use crate::domain::types::{AmountRaw, AssetId, AssetPair, MintAddress};
use crate::error::AppError;

/// Stable asset identifier for native SOL.
pub const SOL_ID: &str = "SOL";
/// Stable asset identifier for USDC.
pub const USDC_ID: &str = "USDC";
/// Stable asset identifier for Coinbase wrapped BTC on Solana.
pub const CBBTC_ID: &str = "cbBTC";

/// Circle USDC mint on Solana mainnet.
pub const USDC_MINT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
/// Coinbase cbBTC mint on Solana mainnet.
pub const CBBTC_MINT: &str = "cbbtcf3aa214zXHbiAZQwf4122FBYbraNdFqgw4iMij";

/// Whether an asset is native SOL or an SPL-compatible token mint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssetKind {
    /// Native SOL tracked in lamports and kept separate from token mints.
    NativeSol,
    /// SPL token with a Solana mint address.
    SplToken {
        /// Token mint address.
        mint: MintAddress,
    },
}

/// Canonical metadata for one supported runtime asset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetMetadata {
    /// Stable API/config identifier.
    pub id: AssetId,
    /// Human-facing ticker.
    pub symbol: String,
    /// Number of raw subunits per display unit, expressed as base-10 decimals.
    pub decimals: u8,
    kind: AssetKind,
}

impl AssetMetadata {
    /// Construct metadata for native SOL.
    #[must_use]
    pub fn native_sol() -> Self {
        Self {
            id: AssetId::from(SOL_ID),
            symbol: SOL_ID.to_owned(),
            decimals: 9,
            kind: AssetKind::NativeSol,
        }
    }

    /// Construct metadata for an SPL token.
    #[must_use]
    pub fn spl_token(
        id: impl Into<AssetId>,
        symbol: impl Into<String>,
        mint: impl Into<String>,
        decimals: u8,
    ) -> Self {
        Self {
            id: id.into(),
            symbol: symbol.into(),
            decimals,
            kind: AssetKind::SplToken {
                mint: MintAddress::new(mint.into()),
            },
        }
    }

    /// True when this metadata represents native SOL.
    #[must_use]
    pub const fn is_native_sol(&self) -> bool {
        matches!(self.kind, AssetKind::NativeSol)
    }

    /// True when this metadata represents an SPL token mint.
    #[must_use]
    pub const fn is_spl_token(&self) -> bool {
        matches!(self.kind, AssetKind::SplToken { .. })
    }

    /// Return the SPL mint address, if the asset is a token.
    #[must_use]
    pub const fn mint(&self) -> Option<&MintAddress> {
        match &self.kind {
            AssetKind::NativeSol => None,
            AssetKind::SplToken { mint } => Some(mint),
        }
    }

    /// Parse the SPL mint as a Solana public key.
    ///
    /// # Errors
    ///
    /// Returns an asset error when the asset is native SOL or the configured
    /// mint string is not a valid Solana public key.
    pub fn mint_pubkey(&self) -> Result<Pubkey, AssetError> {
        let mint = self
            .mint()
            .ok_or_else(|| AssetError::NativeAssetHasNoMint(self.id.clone()))?;

        Pubkey::from_str(mint.as_str()).map_err(|error| AssetError::InvalidMint {
            asset: self.id.clone(),
            mint: mint.clone(),
            message: error.to_string(),
        })
    }
}

/// Registry errors that can be shown safely to API/TUI consumers.
#[derive(Debug, Error)]
pub enum AssetError {
    /// The asset is not part of the supported runtime universe.
    #[error("unsupported asset: {0}")]
    UnsupportedAsset(AssetId),

    /// The directional pair is not configured.
    #[error("unsupported pair: {input}->{output}")]
    UnsupportedPair {
        /// Requested input asset.
        input: AssetId,
        /// Requested output asset.
        output: AssetId,
    },

    /// Native SOL does not have an SPL token mint.
    #[error("native asset {0} does not have an SPL mint")]
    NativeAssetHasNoMint(AssetId),

    /// Configured mint failed Solana public-key parsing.
    #[error("invalid mint for asset {asset}: {mint} ({message})")]
    InvalidMint {
        /// Asset with the invalid mint.
        asset: AssetId,
        /// Invalid mint value.
        mint: MintAddress,
        /// Parser message.
        message: String,
    },

    /// Decimal scaling exceeded the supported Decimal range.
    #[error("decimal scale overflow for {decimals} decimals")]
    ScaleOverflow {
        /// Asset decimal precision.
        decimals: u8,
    },

    /// Display amount cannot be represented in raw token units without loss.
    #[error("precision loss converting {amount} {asset} to raw units")]
    PrecisionLoss {
        /// Asset being converted.
        asset: AssetId,
        /// Display amount that had fractional raw subunits.
        amount: Decimal,
    },

    /// Negative token amounts are invalid at runtime boundaries.
    #[error("negative amount {amount} for asset {asset}")]
    NegativeAmount {
        /// Asset being converted.
        asset: AssetId,
        /// Negative amount.
        amount: Decimal,
    },

    /// Conversion exceeded `u64` raw amount storage.
    #[error("amount overflow converting {amount} {asset} to raw units")]
    AmountOverflow {
        /// Asset being converted.
        asset: AssetId,
        /// Display amount that overflowed.
        amount: Decimal,
    },
}

impl From<AssetError> for AppError {
    fn from(error: AssetError) -> Self {
        match error {
            AssetError::UnsupportedAsset(_) | AssetError::UnsupportedPair { .. } => {
                Self::unsupported(error.to_string())
            }
            AssetError::NativeAssetHasNoMint(_)
            | AssetError::InvalidMint { .. }
            | AssetError::ScaleOverflow { .. }
            | AssetError::PrecisionLoss { .. }
            | AssetError::NegativeAmount { .. }
            | AssetError::AmountOverflow { .. } => Self::validation(error.to_string()),
        }
    }
}

/// Static registry for runtime assets and enabled directional pairs.
#[derive(Debug, Clone)]
pub struct AssetRegistry {
    assets: BTreeMap<AssetId, AssetMetadata>,
    pairs: BTreeSet<(AssetId, AssetId)>,
}

impl AssetRegistry {
    /// Build a registry from asset metadata and directional pairs.
    #[must_use]
    pub fn new(assets: Vec<AssetMetadata>, pairs: Vec<AssetPair>) -> Self {
        let assets = assets
            .into_iter()
            .map(|asset| (asset.id.clone(), asset))
            .collect();
        let pairs = pairs
            .into_iter()
            .map(|pair| (pair.input, pair.output))
            .collect();

        Self { assets, pairs }
    }

    /// Build a registry from enabled application config rows.
    #[must_use]
    pub fn from_config(config: &AppConfig) -> Self {
        let assets = config
            .assets
            .supported
            .iter()
            .filter(|asset| asset.enabled)
            .map(|asset| {
                if asset.id.as_str() == SOL_ID {
                    AssetMetadata::native_sol()
                } else {
                    AssetMetadata::spl_token(
                        asset.id.clone(),
                        asset.symbol.clone(),
                        asset.mint.as_str(),
                        asset.decimals,
                    )
                }
            })
            .collect();
        let pairs = config
            .assets
            .pairs
            .iter()
            .filter(|pair| pair.enabled)
            .map(|pair| AssetPair::new(pair.input.clone(), pair.output.clone()))
            .collect();

        Self::new(assets, pairs)
    }

    /// Return all supported asset metadata in stable key order.
    pub fn assets(&self) -> impl Iterator<Item = &AssetMetadata> {
        self.assets.values()
    }

    /// Return metadata for an asset if it is supported.
    #[must_use]
    pub fn asset(&self, asset: &AssetId) -> Option<&AssetMetadata> {
        self.assets.get(asset)
    }

    /// Return metadata for an asset or a typed unsupported-asset error.
    ///
    /// # Errors
    ///
    /// Returns [`AssetError::UnsupportedAsset`] when the asset is absent.
    pub fn require_asset(&self, asset: &AssetId) -> Result<&AssetMetadata, AssetError> {
        self.asset(asset)
            .ok_or_else(|| AssetError::UnsupportedAsset(asset.clone()))
    }

    /// True when a directional pair is configured and both assets are supported.
    #[must_use]
    pub fn supports_pair(&self, pair: &AssetPair) -> bool {
        self.assets.contains_key(&pair.input)
            && self.assets.contains_key(&pair.output)
            && self
                .pairs
                .contains(&(pair.input.clone(), pair.output.clone()))
    }

    /// Validate a pair and distinguish unsupported assets from unsupported pairs.
    ///
    /// # Errors
    ///
    /// Returns an asset error when either asset is unknown or the direction is
    /// not enabled.
    pub fn validate_pair(&self, pair: &AssetPair) -> Result<(), AssetError> {
        self.require_asset(&pair.input)?;
        self.require_asset(&pair.output)?;

        if self.supports_pair(pair) {
            Ok(())
        } else {
            Err(AssetError::UnsupportedPair {
                input: pair.input.clone(),
                output: pair.output.clone(),
            })
        }
    }

    /// Convert raw token units to display units using checked Decimal math.
    ///
    /// # Errors
    ///
    /// Returns an asset error when the asset is unsupported or decimal scaling
    /// overflows.
    pub fn raw_to_display(
        &self,
        asset: &AssetId,
        amount: AmountRaw,
    ) -> Result<Decimal, AssetError> {
        let metadata = self.require_asset(asset)?;
        let scale = decimal_scale(metadata.decimals)?;
        Decimal::from(amount.as_u64())
            .checked_div(scale)
            .ok_or(AssetError::AmountOverflow {
                asset: asset.clone(),
                amount: Decimal::from(amount.as_u64()),
            })
    }

    /// Convert display units to raw units without truncating precision.
    ///
    /// # Errors
    ///
    /// Returns precision-loss, overflow, negative-amount, or unsupported-asset
    /// errors. This intentionally rejects fractional raw subunits.
    pub fn display_to_raw(
        &self,
        asset: &AssetId,
        amount: Decimal,
    ) -> Result<AmountRaw, AssetError> {
        let metadata = self.require_asset(asset)?;
        if amount.is_sign_negative() {
            return Err(AssetError::NegativeAmount {
                asset: asset.clone(),
                amount,
            });
        }

        let scale = decimal_scale(metadata.decimals)?;
        let scaled = amount
            .checked_mul(scale)
            .ok_or_else(|| AssetError::AmountOverflow {
                asset: asset.clone(),
                amount,
            })?;
        let truncated = scaled.trunc();

        if scaled != truncated {
            return Err(AssetError::PrecisionLoss {
                asset: asset.clone(),
                amount,
            });
        }

        let raw = truncated
            .to_u64()
            .ok_or_else(|| AssetError::AmountOverflow {
                asset: asset.clone(),
                amount,
            })?;
        Ok(AmountRaw::new(raw))
    }

    /// Return the quoteable balance after protecting the native SOL gas buffer.
    ///
    /// SPL token balances are returned unchanged. Native SOL saturates at zero
    /// when the balance is below the configured protected gas buffer.
    ///
    /// # Errors
    ///
    /// Returns an unsupported-asset error when `asset` is absent.
    pub fn quoteable_inventory(
        &self,
        asset: &AssetId,
        working_balance: AmountRaw,
        protected_sol_gas_buffer: AmountRaw,
    ) -> Result<AmountRaw, AssetError> {
        let metadata = self.require_asset(asset)?;
        if metadata.is_native_sol() {
            Ok(AmountRaw::new(
                working_balance
                    .as_u64()
                    .saturating_sub(protected_sol_gas_buffer.as_u64()),
            ))
        } else {
            Ok(working_balance)
        }
    }
}

impl Default for AssetRegistry {
    fn default() -> Self {
        let usdc = AssetId::from(USDC_ID);
        let sol = AssetId::from(SOL_ID);
        let cbbtc = AssetId::from(CBBTC_ID);

        Self::new(
            vec![
                AssetMetadata::native_sol(),
                AssetMetadata::spl_token(USDC_ID, USDC_ID, USDC_MINT, 6),
                AssetMetadata::spl_token(CBBTC_ID, CBBTC_ID, CBBTC_MINT, 8),
            ],
            vec![
                AssetPair::new(usdc.clone(), sol.clone()),
                AssetPair::new(sol.clone(), usdc.clone()),
                AssetPair::new(usdc.clone(), cbbtc.clone()),
                AssetPair::new(cbbtc.clone(), usdc),
                AssetPair::new(sol.clone(), cbbtc.clone()),
                AssetPair::new(cbbtc, sol),
            ],
        )
    }
}

fn decimal_scale(decimals: u8) -> Result<Decimal, AssetError> {
    let mut scale = Decimal::ONE;
    for _ in 0..decimals {
        scale = scale
            .checked_mul(Decimal::TEN)
            .ok_or(AssetError::ScaleOverflow { decimals })?;
    }
    Ok(scale)
}

#[cfg(test)]
mod tests {
    use rust_decimal::Decimal;

    use super::*;
    use crate::domain::types::{AmountRaw, AssetId, AssetPair};

    #[test]
    fn assets_registry_contains_required_assets() {
        let registry = AssetRegistry::default();

        let sol = registry.require_asset(&AssetId::from("SOL")).unwrap();
        assert!(sol.is_native_sol());
        assert_eq!(sol.decimals, 9);
        assert_eq!(sol.mint(), None);

        let usdc = registry.require_asset(&AssetId::from("USDC")).unwrap();
        assert!(usdc.is_spl_token());
        assert_eq!(usdc.decimals, 6);
        assert_eq!(usdc.mint().unwrap().as_str(), USDC_MINT);

        let cbbtc = registry.require_asset(&AssetId::from("cbBTC")).unwrap();
        assert!(cbbtc.is_spl_token());
        assert_eq!(cbbtc.decimals, 8);
        assert_eq!(cbbtc.mint().unwrap().as_str(), CBBTC_MINT);
    }

    #[test]
    fn assets_supported_pairs_are_directional_and_complete() {
        let registry = AssetRegistry::default();
        let usdc = AssetId::from("USDC");
        let sol = AssetId::from("SOL");
        let cbbtc = AssetId::from("cbBTC");

        for pair in [
            AssetPair::new(usdc.clone(), sol.clone()),
            AssetPair::new(sol.clone(), usdc.clone()),
            AssetPair::new(usdc.clone(), cbbtc.clone()),
            AssetPair::new(cbbtc.clone(), usdc),
            AssetPair::new(sol.clone(), cbbtc.clone()),
            AssetPair::new(cbbtc, sol.clone()),
        ] {
            assert!(registry.validate_pair(&pair).is_ok(), "{pair:?}");
        }

        let same_asset = AssetPair::new(sol.clone(), sol);
        assert!(matches!(
            registry.validate_pair(&same_asset),
            Err(AssetError::UnsupportedPair { .. })
        ));

        let unsupported = AssetPair::new(AssetId::from("USDC"), AssetId::from("BONK"));
        assert!(matches!(
            registry.validate_pair(&unsupported),
            Err(AssetError::UnsupportedAsset(_))
        ));
    }

    #[test]
    fn assets_raw_to_display_uses_decimal_math() {
        let registry = AssetRegistry::default();

        let usdc = registry
            .raw_to_display(&AssetId::from("USDC"), AmountRaw::new(1_234_567))
            .unwrap();
        assert_eq!(usdc, Decimal::new(1_234_567, 6));

        let sol = registry
            .raw_to_display(&AssetId::from("SOL"), AmountRaw::new(1_500_000_000))
            .unwrap();
        assert_eq!(sol, Decimal::new(15, 1));
    }

    #[test]
    fn assets_display_to_raw_rejects_precision_loss() {
        let registry = AssetRegistry::default();

        let exact = registry
            .display_to_raw(&AssetId::from("cbBTC"), Decimal::new(12_345_678, 8))
            .unwrap();
        assert_eq!(exact, AmountRaw::new(12_345_678));

        let too_precise = registry.display_to_raw(&AssetId::from("USDC"), Decimal::new(1, 7));
        assert!(matches!(too_precise, Err(AssetError::PrecisionLoss { .. })));
    }

    #[test]
    fn assets_quoteable_inventory_excludes_protected_sol_gas_buffer() {
        let registry = AssetRegistry::default();

        let quoteable_sol = registry
            .quoteable_inventory(
                &AssetId::from("SOL"),
                AmountRaw::new(1_500_000_000),
                AmountRaw::new(500_000_000),
            )
            .unwrap();
        assert_eq!(quoteable_sol, AmountRaw::new(1_000_000_000));

        let depleted_sol = registry
            .quoteable_inventory(
                &AssetId::from("SOL"),
                AmountRaw::new(250_000_000),
                AmountRaw::new(500_000_000),
            )
            .unwrap();
        assert_eq!(depleted_sol, AmountRaw::new(0));

        let quoteable_usdc = registry
            .quoteable_inventory(
                &AssetId::from("USDC"),
                AmountRaw::new(2_000_000),
                AmountRaw::new(500_000_000),
            )
            .unwrap();
        assert_eq!(quoteable_usdc, AmountRaw::new(2_000_000));
    }
}
