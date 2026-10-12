use crate::output::view::*;
use crate::{PineValue, RuntimeProfile};

mod delta_writer;
mod drawings_writer;
mod float_writer;
mod metadata_writer;
mod profile;
mod records_writer;
mod series_writer;
mod value_writer;
mod writer;
pub use delta_writer::{public_runtime_changes_json, write_public_runtime_changes_json};
pub use writer::{
    into_public_runtime_result_json, public_runtime_result_view_json,
    write_public_runtime_result_json, write_public_runtime_result_view_json,
};

use super::alerts::AlertEvent;
use super::changes::{
    DrawingAction, DrawingObject, FillAction, HLineAction, RuntimeChanges, SeriesChange,
    SeriesHeader, StrategyChanges,
};
#[cfg(test)]
use super::model::PlotSeries;
use super::model::{
    HLineOutput, PUBLIC_RENDER_METADATA_VERSION, PUBLIC_RUNTIME_SCHEMA_VERSION, RuntimeResult,
};
use profile::profile_json;

/// Serialize the public owned result without complete per-family buffers.
pub fn public_runtime_result_json(result: &RuntimeResult) -> String {
    public_runtime_result_view_json(&result.view())
}

pub fn public_runtime_profiled_result_json(
    result: &RuntimeResult,
    profile: &RuntimeProfile,
) -> String {
    let mut output = public_runtime_result_json(result);
    output.pop();
    output.push_str(",\"profile\":");
    output.push_str(&profile_json(profile));
    output.push('}');
    output
}

#[cfg(test)]
mod tests;
