use crate::prelude::*;

impl Analyzer {
    pub(crate) fn select_fill_signature(
        &mut self,
        signature: &'static BuiltinSignature,
        args: &[CallArg],
        types: &[Option<PineType>],
    ) -> &'static BuiltinSignature {
        if signature.name == "fill"
            && pine_builtins::is_gradient_fill_call(args.iter().enumerate().map(|(i, arg)| {
                (
                    arg.name.as_deref(),
                    types.get(i).copied().flatten().map(|ty| ty.kind),
                )
            }))
        {
            if self.legacy.dialect() < crate::PineDialect::V5 {
                self.unsupported(
                    "fill.gradient",
                    "vertical gradient fills require Pine v5 or v6",
                    args.first().map_or(Span::default(), |arg| arg.span),
                );
            }
            pine_builtins::gradient_fill_signature()
        } else {
            signature
        }
    }

    pub(crate) fn validate_versioned_input_output_metadata(
        &mut self,
        signature: &BuiltinSignature,
        args: &[CallArg],
        arg_types: &[Option<PineType>],
    ) {
        if self.legacy.dialect() >= crate::PineDialect::V6 {
            return;
        }
        for (index, arg) in args.iter().enumerate() {
            if signature.name.starts_with("input.") && is_call_arg(signature, arg, index, "active")
            {
                self.diagnostics.push(Diagnostic::error(
                    "E_CALL_ARG_NAME",
                    format!("`{}` argument `active` requires Pine v6", signature.name),
                    arg.span,
                ));
            }
            if matches!(signature.name, "plot" | "plotshape" | "fill") {
                let old_const_metadata = is_call_arg(signature, arg, index, "editable")
                    || (self.legacy.dialect() < crate::PineDialect::V5
                        && is_call_arg(signature, arg, index, "display"));
                if old_const_metadata
                    && arg_types
                        .get(index)
                        .copied()
                        .flatten()
                        .is_some_and(|ty| ty.qualifier != Qualifier::Const)
                {
                    self.diagnostics.push(Diagnostic::error(
                        "E_CALL_ARG_TYPE",
                        format!(
                            "`{}` metadata argument requires a const value in this Pine version",
                            signature.name
                        ),
                        arg.span,
                    ));
                }
            }
        }
    }

    pub(crate) fn validate_indicator_timeframe_args(&mut self, args: &[CallArg]) {
        let signature =
            pine_builtins::get_phase_1_builtin("indicator").expect("indicator signature");
        for (index, arg) in args.iter().enumerate() {
            if is_call_arg(signature, arg, index, "timeframe")
                && self.known_const_string_value(&arg.value).as_deref() != Some("")
            {
                self.unsupported(
                    "indicator.timeframe",
                    "only an empty timeframe inheriting the host chart is supported; non-empty program-level timeframes require separate execution and output alignment",
                    arg.span,
                );
            }
        }
    }

    pub(crate) fn validate_label_string_arg(
        &mut self,
        signature: &BuiltinSignature,
        args: &[CallArg],
        index: usize,
        name: &str,
        allowed: &[&str],
    ) {
        for (arg_index, arg) in args.iter().enumerate() {
            let is_target = arg.name.as_deref() == Some(name)
                || (arg.name.is_none()
                    && signature
                        .params
                        .get(arg_index)
                        .is_some_and(|param| param.name == name && index == arg_index));
            if !is_target {
                continue;
            }
            let Some(value) = self.known_const_string_value(&arg.value) else {
                continue;
            };
            if !allowed.iter().any(|allowed_value| *allowed_value == value) {
                self.diagnostics.push(Diagnostic::error(
                    "E_CALL_ARG_VALUE",
                    format!(
                        "`{}` argument `{name}` only supports {}",
                        signature.name,
                        allowed.join(", ")
                    ),
                    arg.span,
                ));
            }
        }
    }

    pub(crate) fn validate_indicator_args(
        &mut self,
        signature: &BuiltinSignature,
        args: &[CallArg],
    ) {
        if signature.name != "indicator" {
            return;
        }

        self.validate_label_string_arg(
            signature,
            args,
            3,
            "format",
            &[
                "format.inherit",
                "format.price",
                "format.percent",
                "format.volume",
            ],
        );
        self.validate_label_string_arg(
            signature,
            args,
            5,
            "scale",
            &["scale.left", "scale.right", "scale.none"],
        );

        for (index, arg) in args.iter().enumerate() {
            let is_precision = arg.name.as_deref() == Some("precision")
                || (arg.name.is_none()
                    && signature
                        .params
                        .get(index)
                        .is_some_and(|param| param.name == "precision"));
            if is_precision {
                if let Some(value) = self.known_const_int_for_validation(&arg.value)
                    && match value {
                        Ok(value) => !(0..=16).contains(&value),
                        Err(()) => true,
                    }
                {
                    self.diagnostics.push(Diagnostic::error(
                        "E_CALL_ARG_VALUE",
                        "`indicator` argument `precision` must be between 0 and 16",
                        arg.span,
                    ));
                }
                continue;
            }

            if self.validate_indicator_drawing_count_arg(signature, arg, index) {
                continue;
            }

            if is_call_arg(signature, arg, index, "calc_bars_count") {
                self.validate_calc_bars_count_arg("indicator", arg);
                continue;
            }

            let is_max_bars_back = arg.name.as_deref() == Some("max_bars_back")
                || (arg.name.is_none()
                    && signature
                        .params
                        .get(index)
                        .is_some_and(|param| param.name == "max_bars_back"));
            if !is_max_bars_back {
                continue;
            }

            self.validate_max_bars_back_bound_value("indicator", "max_bars_back", arg);
        }
    }

    pub(crate) fn validate_max_bars_back_bound_value(
        &mut self,
        call_name: &str,
        arg_name: &str,
        arg: &CallArg,
    ) {
        let Some(value) = self.known_const_int_for_validation(&arg.value) else {
            return;
        };
        let message = if value.is_err() {
            Some(format!(
                "`{call_name}` argument `{arg_name}` must fit in a 32-bit unsigned history bound"
            ))
        } else if value.is_ok_and(|value| value < 0) {
            Some(format!(
                "`{call_name}` argument `{arg_name}` must be non-negative"
            ))
        } else if value.is_ok_and(|value| u32::try_from(value).is_err()) {
            Some(format!(
                "`{call_name}` argument `{arg_name}` must fit in a 32-bit unsigned history bound"
            ))
        } else {
            None
        };
        if let Some(message) = message {
            self.diagnostics
                .push(Diagnostic::error("E_CALL_ARG_VALUE", message, arg.span));
        }
    }

    pub(crate) fn validate_calc_bars_count_arg(&mut self, call_name: &str, arg: &CallArg) {
        let Some(value) = self.known_const_int_for_validation(&arg.value) else {
            return;
        };
        match value
            .ok()
            .filter(|value| *value >= 0)
            .and_then(|value| u32::try_from(value).ok())
        {
            Some(value) => self.calc_bars_count = Some(value),
            None => self.diagnostics.push(Diagnostic::error(
                "E_CALL_ARG_VALUE",
                format!(
                    "`{call_name}` argument `calc_bars_count` must be a non-negative 32-bit integer"
                ),
                arg.span,
            )),
        }
    }

    pub(crate) fn validate_max_bars_back_args(
        &mut self,
        signature: &BuiltinSignature,
        args: &[CallArg],
    ) {
        if signature.name != "max_bars_back" {
            return;
        }

        for (index, arg) in args.iter().enumerate() {
            let is_num = is_call_arg(signature, arg, index, "num");
            if !is_num {
                continue;
            }

            self.validate_max_bars_back_bound_value("max_bars_back", "num", arg);
        }
    }
}

fn is_call_arg(signature: &BuiltinSignature, arg: &CallArg, index: usize, name: &str) -> bool {
    arg.name.as_deref() == Some(name)
        || (arg.name.is_none()
            && signature
                .params
                .get(index)
                .is_some_and(|param| param.name == name))
}
