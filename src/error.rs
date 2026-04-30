//! Application error categories shared by runtime modules and adapters.

use thiserror::Error;

/// Crate-wide result alias using the typed application error.
pub type AppResult<T> = Result<T, AppError>;

/// Error categories that keep protocol, persistence, and UI layers aligned.
#[derive(Debug, Error)]
pub enum AppError {
    /// Configuration loading or deserialization failed.
    #[error("configuration error: {0}")]
    Config(#[from] ::config::ConfigError),

    /// User input or local policy validation failed.
    #[error("validation error: {0}")]
    Validation(String),

    /// A third-party service returned an error or unusable response.
    #[error("external service error from {service}: {message}")]
    ExternalService { service: String, message: String },

    /// Solana RPC, wallet, transaction, or program interaction failed.
    #[error("solana error: {0}")]
    Solana(String),

    /// Durable storage failed or was unavailable.
    #[error("persistence error: {0}")]
    Persistence(String),

    /// The requested operation is outside the current runtime capability.
    #[error("unsupported operation: {0}")]
    Unsupported(String),

    /// Internal invariant or unexpected runtime failure.
    #[error("internal error: {0}")]
    Internal(String),
}

impl AppError {
    /// Build a configuration error from displayable context without exposing
    /// secret values.
    #[must_use]
    pub fn config(message: impl Into<String>) -> Self {
        Self::Config(::config::ConfigError::Message(message.into()))
    }

    /// Build a validation error from displayable context.
    #[must_use]
    pub fn validation(message: impl Into<String>) -> Self {
        Self::Validation(message.into())
    }

    /// Build an external service error with a stable service label.
    #[must_use]
    pub fn external_service(service: impl Into<String>, message: impl Into<String>) -> Self {
        Self::ExternalService {
            service: service.into(),
            message: message.into(),
        }
    }

    /// Build a Solana-facing error from displayable context.
    #[must_use]
    pub fn solana(message: impl Into<String>) -> Self {
        Self::Solana(message.into())
    }

    /// Build a persistence error from displayable context.
    #[must_use]
    pub fn persistence(message: impl Into<String>) -> Self {
        Self::Persistence(message.into())
    }

    /// Build an unsupported-operation error from displayable context.
    #[must_use]
    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::Unsupported(message.into())
    }

    /// Build an internal error from displayable context.
    #[must_use]
    pub fn internal(message: impl Into<String>) -> Self {
        Self::Internal(message.into())
    }
}
