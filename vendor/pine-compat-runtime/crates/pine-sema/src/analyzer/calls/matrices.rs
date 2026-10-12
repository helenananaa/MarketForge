use super::arg_type_for_param_index;
use crate::prelude::*;
use crate::types::is_numeric_matrix_kind;

impl Analyzer {
    pub(crate) fn validate_matrix_concat_args(
        &mut self,
        signature: &BuiltinSignature,
        args: &[CallArg],
        arg_types: &[Option<PineType>],
    ) {
        self.validate_udt_matrix_operation(signature, args, arg_types);
        if signature.name != "matrix.concat" {
            return;
        }
        let Some(first_type) = arg_types.first().copied().flatten() else {
            return;
        };
        let Some(second_type) = arg_types.get(1).copied().flatten() else {
            return;
        };
        if !is_matrix_kind(first_type.kind)
            || !is_matrix_kind(second_type.kind)
            || first_type.kind == second_type.kind
        {
            return;
        }

        self.diagnostics.push(call_arg_expected_type_diagnostic(
            "matrix.concat",
            "id2",
            &pine_type_name(first_type),
            second_type,
            args.get(1).map_or(Span::default(), |arg| arg.span),
        ));
    }
}

pub(super) fn matrix_element_expected_label(matrix_type: PineType) -> Option<&'static str> {
    match matrix_type.kind {
        ValueKind::FloatMatrix => Some("numeric-compatible"),
        ValueKind::IntMatrix => Some("integer-compatible"),
        ValueKind::BoolMatrix => Some("bool-compatible"),
        ValueKind::StringMatrix => Some("string-compatible"),
        ValueKind::ColorMatrix => Some("color-compatible"),
        _ => None,
    }
}

pub(super) fn matrix_element_array_expected_type(matrix_type: PineType) -> Option<PineType> {
    let kind = match matrix_type.kind {
        ValueKind::FloatMatrix => ValueKind::FloatArray,
        ValueKind::IntMatrix => ValueKind::IntArray,
        ValueKind::BoolMatrix => ValueKind::BoolArray,
        ValueKind::StringMatrix => ValueKind::StringArray,
        ValueKind::ColorMatrix => ValueKind::ColorArray,
        _ => return None,
    };
    Some(PineType::new(Qualifier::Simple, kind))
}

#[derive(Clone, Copy)]
pub(super) enum MatrixPairScalarPolicy {
    Numeric,
    NumericOrNumericArray,
}

pub(super) fn matrix_pair_expected_label(
    signature: &BuiltinSignature,
    args: &[CallArg],
    arg_types: &[Option<PineType>],
    counterpart_param_index: usize,
    scalar_policy: MatrixPairScalarPolicy,
) -> Option<&'static str> {
    let counterpart_type =
        arg_type_for_param_index(signature, args, arg_types, counterpart_param_index)?;
    if !is_numeric_matrix_kind(counterpart_type.kind) {
        if matches!(scalar_policy, MatrixPairScalarPolicy::NumericOrNumericArray)
            && accepts_type(Accepts::NumericArray, counterpart_type)
        {
            return Some("numeric matrix or numeric array");
        }
        return Some("numeric matrix");
    }
    Some(match scalar_policy {
        MatrixPairScalarPolicy::Numeric => "numeric matrix or numeric-compatible",
        MatrixPairScalarPolicy::NumericOrNumericArray => {
            "numeric matrix, numeric-compatible, or numeric array"
        }
    })
}
