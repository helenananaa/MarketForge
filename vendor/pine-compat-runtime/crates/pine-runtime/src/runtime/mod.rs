pub(crate) mod append_history;
pub(crate) mod array_values;
pub(crate) mod call_context;
mod call_plan;
pub(crate) mod calls;
pub(crate) mod collection_gc;
pub(crate) mod context;
pub(crate) mod display_retention;
mod display_value;
pub(crate) mod drawing_history;
pub(crate) mod execution;
pub(crate) mod expressions;
pub(crate) mod historical;
mod historical_dataset;
pub use historical_dataset::HistoricalDataset;
pub(crate) mod hir_walk;
pub(crate) mod history;
pub(crate) mod id_store;
mod metadata;
mod prepared_program;
pub use prepared_program::PreparedProgram;
mod objects;
pub(crate) mod persistence;
pub(crate) mod plot_history;
mod profile;
pub(crate) mod realtime;
pub(crate) mod resource_limits;
mod result_view;
pub(crate) mod statements;
mod strategy_history;
pub(crate) mod strategy_path;
pub(crate) mod strategy_scheduler;
pub(crate) mod streaming;
pub(crate) mod valuewhen_history;
pub(crate) mod valuewhen_limits;

#[cfg(test)]
mod window_length_tests;

#[cfg(test)]
mod ta_state_identity_tests;
