//! CandleScope-facing integration boundary for MarketForge.
//!
//! This crate deliberately keeps the CandleScope wire protocol outside
//! `exchange-core`. The matching engine remains the source of truth while this
//! layer projects its executions and snapshots into CandleScope provider pages
//! and streams.

mod adapter;
mod error;
mod platform;
mod provider;

pub use adapter::{
    ApplyCommandResult, InstrumentBinding, MarketForgeAdapter, SessionDescription,
    SessionLoadResult,
};
pub use error::{AdapterError, ErrorKind};
pub use platform::{JsonLineServer, PlatformRuntime, RuntimeState, descriptor};
pub use provider::{
    EXCHANGE_ID, MARKET_DATA_CONTRIBUTION_ID, MARKETFORGE_CONTROL_CONTRIBUTION_ID, PluginService,
    SUPPORTED_INTERVALS, SYMBOLS_CONTRIBUTION_ID,
};
