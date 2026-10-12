use crate::prelude::*;

impl Analyzer {
    pub(super) fn analyze_drawing_call_result_method(
        &mut self,
        callee: &Expr,
        args: &[CallArg],
        arg_types: &[Option<PineType>],
    ) -> Option<Option<PineType>> {
        let (_, method_name) = postfix_call_result_method_parts(callee, args)?;
        let Some(receiver_type) = arg_types.first().copied().flatten() else {
            return Some(None);
        };
        let builtin_name = drawing_call_result_builtin_name(receiver_type.kind, method_name)?;
        if self.reject_legacy_builtin_method_syntax("box call-result", method_name, callee.span) {
            return Some(None);
        }
        if let Some(min_version) =
            crate::PineDialect::qualified_builtin_min_version(&builtin_name, true)
            && self.legacy.dialect().version() < min_version
        {
            self.reject_unavailable_legacy_builtin(&builtin_name, min_version, callee.span);
            return Some(None);
        }
        let signature = pine_builtins::get_phase_1_builtin(&builtin_name)
            .expect("drawing call-result helper returned registered builtin");
        self.check_feature_name(&builtin_name, callee.span);

        if self.function_depth > 0
            && is_output_or_declaration_builtin(&builtin_name)
            && !self.allows_udf_output_or_declaration_side_effect(&builtin_name)
        {
            self.unsupported(
                "function_side_effect",
                "indicator, strategy, input, plot, plotchar, plotshape, plotarrow, plotbar, plotcandle, hline, fill, bgcolor, barcolor, alert, alertcondition, drawing calls, and strategy order calls are not supported inside user-defined functions",
                callee.span,
            );
        }

        self.validate_call_args(signature, args, arg_types);
        Some(self.return_type_for_call(signature, args, arg_types))
    }
}
