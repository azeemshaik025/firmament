//! Solana RFQ maker runtime.

pub mod adapters;
pub mod application;
pub mod config;
pub mod domain;
pub mod error;
pub mod interfaces;
pub mod ports;

pub use adapters::circle_gateway as gateway;
pub use adapters::jupiter;
pub use adapters::persistence::{db, ledger, pnl};
pub use adapters::solana::{client as solana_client, htlc, wallets};
pub use application::{hedge, rebalance, rfq, runtime};
pub use config::AppConfig;
pub use domain::{assets, events, inventory, quote_engine, risk, settlement, types};
pub use error::{AppError, AppResult};
pub use interfaces::http as api;
pub use interfaces::http::types as api_types;
pub use interfaces::tui;
pub use interfaces::tui::state as tui_state;
pub use runtime::{AppState, RuntimeState, bootstrap, bootstrap_live_runtime};
