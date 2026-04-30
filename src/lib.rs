//! Shared scaffold for the Solana RFQ maker runtime.
//!
//! Wave 0 intentionally exposes stable contracts and bootstrap state only. Live
//! protocol adapters, HTTP routes, persistence, and TUI rendering are added by
//! later worktrees behind these boundaries.

pub mod api;
pub mod api_types;
pub mod assets;
pub mod config;
pub mod error;
pub mod events;
pub mod gateway;
pub mod hedge;
pub mod htlc;
pub mod inventory;
pub mod jupiter;
pub mod ports;
pub mod quote_engine;
pub mod rebalance;
pub mod rfq;
pub mod risk;
pub mod runtime;
pub mod settlement;
pub mod solana_client;
pub mod tui;
pub mod tui_state;
pub mod types;
pub mod wallets;

pub use config::AppConfig;
pub use error::{AppError, AppResult};
pub use runtime::{AppState, RuntimeState, bootstrap};
