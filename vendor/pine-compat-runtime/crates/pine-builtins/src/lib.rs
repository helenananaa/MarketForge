//! Built-in registry scaffolding.

mod constants;
mod display;
mod drawing_styles;
pub use display::combine_display_values;
mod history;
mod namespaces;
mod registry;
mod returns;
mod signature;

pub use constants::{
    NAMED_COLORS, NamedColor, builtin_series_value_type, named_color, named_float_constant,
    named_int_constant, named_string_constant, registered_value_names,
};
pub use drawing_styles::{LABEL_STYLES, LINE_STYLES};
pub use history::{
    BUILTIN_HISTORY_METADATA, BuiltinHistoryMetadata, BuiltinHistoryRequirement,
    BuiltinSeriesHistoryRequirement, builtin_history_requirement,
};
pub use namespaces::strings::is_pure_scalar_string_builtin;
pub use registry::{PHASE_1_BUILTINS, get_phase_1_builtin, is_phase_1_builtin};
pub use returns::{
    change_return_for_arg, color_return_for_arg, fallback_bool_for_arg, input_return_for_arg,
    tuple_return_type,
};
pub use signature::{
    Accepts, BuiltinParam, BuiltinPhase, BuiltinSignature, QualifierBoundScalar, QualifierRelation,
    ReturnSpec, ScalarKind,
};

/// Select the vertical-gradient overload from source or normalized HIR args.
pub fn is_gradient_fill_call<'a>(
    args: impl IntoIterator<Item = (Option<&'a str>, Option<pine_ir::ValueKind>)>,
) -> bool {
    let args: Vec<_> = args.into_iter().collect();
    args.iter().any(|(name, _)| {
        matches!(
            name,
            Some("top_value" | "bottom_value" | "top_color" | "bottom_color")
        )
    }) || (args.len() >= 4
        && [2, 3].into_iter().all(|index| {
            args[index].0.is_none()
                && matches!(
                    args[index].1,
                    Some(
                        pine_ir::ValueKind::Int
                            | pine_ir::ValueKind::Float
                            | pine_ir::ValueKind::Na
                    )
                )
        }))
}

pub fn gradient_fill_signature() -> &'static BuiltinSignature {
    &namespaces::outputs::GRADIENT_FILL_SIGNATURE
}
