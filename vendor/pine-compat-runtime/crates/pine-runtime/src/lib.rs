//! Historical runtime scaffolding.

pub use pine_ir::ValueKind;
use pine_ir::{CallSiteId, HirExpr, PersistenceKind, SeriesId, SymbolId, VarSlotId};

mod algorithms;
mod bar;
mod builtins;
mod error;
mod external;
pub use external::{
    ExternalAccountFrame, ExternalOrderIntent, ExternalPass, ExternalRequestStream,
};
mod host_requirements;
mod input_metadata;
mod magnifier;
mod output;
mod profile;
mod request;
mod retention;
mod runtime;
mod series;
mod session_windows;
mod strategy;
mod value;

pub use bar::{Bar, BarUpdate, BarUpdateKind, RealtimeUpdateContext};
pub use error::RuntimeError;
pub use host_requirements::{
    AccountInputContract, ChartInputContract, ChartInputDefaults, ExecutionInputContract,
    HOST_REQUIREMENTS_SCHEMA_VERSION, HostCallSite, HostRequirements, HostSourceLocation,
    RequestArgument, RequestRequirement, host_requirements, host_requirements_json,
};
pub use input_metadata::{InputCall, input_calls};
pub use magnifier::{
    MAGNIFIER_SCHEMA_VERSION, MAX_MAGNIFIER_INTRABARS, MagnifierChartBarInput, MagnifierFallback,
    MagnifierHostTicks, MagnifierInput, MagnifierInputError, MagnifierTickSource,
    magnifier_absence_diagnostic, magnifier_gap_diagnostic, magnifier_host_ticks,
    magnifier_input_from_groups, magnifier_input_from_json, magnifier_input_from_v1,
};
pub use output::alerts::AlertEvent;
pub use output::changes::{
    DrawingAction, DrawingChange, DrawingFamily, DrawingObject, EventAction, EventChange,
    FillAction, FillChange, HLineAction, HLineChange, ListSplice,
    PUBLIC_RUNTIME_CHANGES_SCHEMA_VERSION, RuntimeChanges, SeriesChange, SeriesChangeOp,
    SeriesFamily, SeriesFields, SeriesHeader, StrategyChanges, StreamingVisibility,
};
pub use output::drawings::{
    BoxOutput, BoxSnapshot, LabelOutput, LabelSnapshot, LineFillOutput, LineFillSnapshot,
    LineOutput, LineSnapshot, PolylineOutput, PolylineSnapshot, TableCellSnapshot,
    TableMergedCellSnapshot, TableOutput, TableSnapshot,
};
pub use output::json::{
    into_public_runtime_result_json, public_runtime_changes_json,
    public_runtime_profiled_result_json, public_runtime_result_json,
    public_runtime_result_view_json, write_public_runtime_changes_json,
    write_public_runtime_result_json, write_public_runtime_result_view_json,
};
pub use output::model::{
    ColorSeries, FillGradientSample, FillOutput, HLineOutput, OutputMetadata,
    PUBLIC_MATRIX_SCHEMA_VERSION, PUBLIC_OUTPUT_SCHEMA_VERSION, PUBLIC_RENDER_METADATA_VERSION,
    PUBLIC_RUNTIME_SCHEMA_VERSION, PlotArrowSeries, PlotBarSeries, PlotCandleSeries,
    PlotCharSeries, PlotSeries, PlotShapeSeries, RuntimeDiagnostic, RuntimeResult,
};
pub use output::parse::{runtime_changes_from_json, runtime_result_from_json};
pub use output::strategy::{
    StrategyEquitySnapshot, StrategyOrderEvent, StrategyOrderFillAlertOutput,
    StrategyPositionSnapshot, StrategyResult, StrategyTrade,
};
pub use output::strategy_alert_templates::{
    STRATEGY_ORDER_ALERT_MESSAGE_PLACEHOLDER, StrategyOrderFillAlertTemplateError,
    render_strategy_order_fill_alert_template,
};
pub use output::view::*;
pub use profile::{RuntimeProfile, RuntimeProfiledResult};
pub(crate) use request::RequestCacheKey;
pub use request::{
    ChartContext, InMemoryRequestDataProvider, NoRequestDataProvider, RequestDataError,
    RequestDataProvider, RequestEnvironment, RequestKey, RequestTimeframe, RequestTimeframeError,
    validate_requested_bars,
};
pub use retention::{HistoryRetentionMode, OutputRetention};
pub use runtime::HistoricalDataset;
pub use runtime::PreparedProgram;
pub use runtime::execution::ExecutionLimits;
pub use runtime::historical::{
    HistoricalRuntime, InputOverrides, run_historical, run_historical_profiled,
    run_historical_profiled_with_execution_times, run_historical_profiled_with_request_environment,
    run_historical_profiled_with_request_environment_and_input_overrides,
    run_historical_profiled_with_request_environment_and_input_overrides_and_execution_times,
    run_historical_with_execution_times, run_historical_with_input_overrides,
    run_historical_with_request_environment,
    run_historical_with_request_environment_and_input_overrides,
    run_historical_with_request_environment_and_input_overrides_and_execution_times,
};
pub use runtime::realtime::RealtimeRuntime;
pub use runtime::resource_limits::ResourceLimits;
pub use runtime::valuewhen_limits::ValueWhenLimits;
pub use series::SeriesStore;
pub use session_windows::{
    SESSION_WINDOW_SCHEMA_VERSION, SessionWindowIds, SessionWindowInput, SessionWindowInputError,
    session_window_input_from_bars, session_window_input_from_json, session_window_input_from_v1,
};
pub use strategy::BrokerState;
pub use value::{
    ChartPointValue, PineValue, chart_source_input_override, encode_color_literal,
    encode_color_rgba, is_valid_public_color,
};

use algorithms::numeric::finite_float_or_na;
use algorithms::rolling_window::{
    RisingFallingMode, RollingWindowKey, RollingWindowState, WindowExtreme,
};
use builtins::args::output_id;
use builtins::arrays::{ArrayElementKind, ArrayPercentileMode, ArraySlice};
use builtins::maps::MapStorage;
use builtins::matrices::MatrixStorage;
use output::align::finalize_bar_aligned_outputs;
use output::collect::{finalize_plot_values, finalize_series_values};
use retention::SeriesRetention;
use runtime::expressions::values_equal;
use runtime::statements::StmtControl;

#[cfg(test)]
use builtins::colors::apply_transparency;
#[cfg(test)]
use builtins::ta::{PivotPointPeriod, pivot_na_levels, pivot_point_levels};

const MAX_WHILE_ITERATIONS: usize = 100_000;
const MAX_RUNTIME_EVAL_DEPTH: u32 = 256;
const MAX_ARRAY_ELEMENTS: usize = 100_000;
const MAX_STRING_CHARS: usize = 40_960;
const MAX_SERIES_HISTORY_VALUES: usize = 1_000_000;
const DEFAULT_MAX_LABELS: usize = 50;
const MAX_LABELS: usize = 500;
const DEFAULT_MAX_LINES: usize = 50;
const MAX_LINES: usize = 500;
const MAX_LINEFILLS: usize = 500;
const DEFAULT_MAX_POLYLINES: usize = 50;
const MAX_POLYLINES: usize = 100;
const DEFAULT_MAX_BOXES: usize = 50;
const MAX_BOXES: usize = 500;
const MAX_TABLE_CELLS: i64 = 1_000;
const DEFAULT_CHART_TIMEFRAME: &str = "1";

#[cfg(test)]
mod tests;

pub use output::replica::RuntimeReplica;
