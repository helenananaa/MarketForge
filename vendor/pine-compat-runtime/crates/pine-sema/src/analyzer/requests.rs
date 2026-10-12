use crate::prelude::*;
mod modern;

const REQUEST_SECURITY_UNSUPPORTED_REASON: &str = "same-context and provider-backed scalar expressions, immutable scalar dependencies, selected scalar-array results, tuples and admitted UDF-local state are supported with explicit gaps/lookahead policies and named calc_bars_count; provider timeframes may be lower or equal, while higher fixed timeframes must be integer multiples of the chart timeframe and calendar months use calendar boundaries; nested requests, recursive UDFs, external mutable or series-persistent captures, reference graph captures and drawing/output side effects remain unsupported";
const LEGACY_SECURITY_UNSUPPORTED_REASON: &str = "legacy security supports same-context or host-provided lower/equal/higher-timeframe requests whose expression is in the request.security scalar/tuple subset, including immutable top-level scalar aliases, const/input/simple captures, pure scalar UDF calls, and direct UDF-local immutable dependency graphs whose nested legacy requests use the same selector and merge policy; higher fixed timeframes must be integer multiples of the chart timeframe and calendar months use calendar boundaries; control-flow-local requests, different-selector nested requests, mutable or persistent UDF state, recursive UDFs, mutable captures, and side effects remain unsupported";
const REQUEST_SECURITY_LOWER_TF_UNSUPPORTED_REASON: &str = "host-provided lower-or-equal-timeframe scalar intrabar arrays and tuples of scalar intrabar arrays support three positional arguments and optional named nonnegative calc_bars_count; other optional policies, collection expressions, mutable captures, and side effects remain unsupported";

#[derive(Debug, Clone)]
struct LegacyProviderRequestContext {
    symbol: Expr,
    timeframe: Expr,
    internal_callee: &'static str,
}

impl Analyzer {
    pub(crate) fn analyze_request_call(
        &mut self,
        name: &str,
        span: Span,
        args: &[CallArg],
    ) -> Option<PineType> {
        if !self.dynamic_requests
            && self.block_depth > 0
            && (self.legacy.dialect() >= crate::PineDialect::V6 || self.function_depth == 0)
        {
            self.unsupported(
                name,
                "request calls in local scopes require dynamic_requests=true",
                span,
            );
            return None;
        }
        match name {
            "request.security" => self.analyze_request_security(span, args),
            "request.security_lower_tf" => self.analyze_request_security_lower_tf(span, args),
            _ => {
                self.unsupported(
                    name,
                    "this request function is outside the supported request.security subset",
                    span,
                );
                None
            }
        }
    }

    fn analyze_request_security(&mut self, span: Span, args: &[CallArg]) -> Option<PineType> {
        self.compatibility.supported.push(FeatureUse {
            feature: "request.security".to_owned(),
            span,
        });

        let arg_types: Vec<_> = args
            .iter()
            .map(|arg| self.analyze_expr(&arg.value))
            .collect();
        let mut unsupported = false;
        if !(3..=6).contains(&args.len()) {
            unsupported = true;
        }
        if args.iter().take(3).any(|arg| arg.name.is_some()) {
            unsupported = true;
            self.diagnostics.push(Diagnostic::error(
                "E_CALL_ARG_NAME",
                "`request.security` currently requires symbol, timeframe, and expression as positional arguments",
                span,
            ));
        }
        if !self.validate_request_security_merge_args(args) {
            unsupported = true;
        }
        for (arg, ty) in args.iter().zip(&arg_types) {
            if arg.name.as_deref() == Some("calc_bars_count")
                && !ty.is_some_and(|ty| {
                    ty.kind == ValueKind::Int && qualifier_at_most(ty.qualifier, Qualifier::Simple)
                })
            {
                unsupported = true;
            }
        }

        self.analyze_request_security_core(
            span,
            args,
            &arg_types,
            unsupported,
            None,
            REQUEST_SECURITY_UNSUPPORTED_REASON,
        )
    }

    fn analyze_request_security_lower_tf(
        &mut self,
        span: Span,
        args: &[CallArg],
    ) -> Option<PineType> {
        let arg_types: Vec<_> = args
            .iter()
            .map(|arg| self.analyze_expr(&arg.value))
            .collect();
        let signature = pine_builtins::get_phase_1_builtin("request.security_lower_tf")
            .expect("request.security_lower_tf signature must exist");
        self.validate_call_args(signature, args, &arg_types);
        let expression_type = arg_types.get(2).copied().flatten();
        let symbol_ok = args.first().is_some_and(|arg| {
            self.request_symbol_is_chart(&arg.value, false)
                || arg_types.first().copied().flatten().is_some_and(|ty| {
                    ty.kind == ValueKind::String
                        && (self.dynamic_requests
                            || qualifier_at_most(ty.qualifier, Qualifier::Simple))
                })
        });
        let timeframe_ok = args.get(1).is_some_and(|arg| {
            self.request_timeframe_is_chart(&arg.value, false)
                || arg_types.get(1).copied().flatten().is_some_and(|ty| {
                    ty.kind == ValueKind::String
                        && (self.dynamic_requests
                            || qualifier_at_most(ty.qualifier, Qualifier::Simple))
                })
        });
        let tuple_array_kinds = (expression_type.is_some_and(|ty| ty.kind == ValueKind::Tuple))
            .then(|| self.tuple_element_types(&args[2].value))
            .flatten()
            .filter(|types| {
                !types.is_empty()
                    && types
                        .iter()
                        .all(|ty| lower_tf_scalar_array_kind(ty.kind).is_some())
            });
        let array_kind = expression_type.and_then(|ty| lower_tf_scalar_array_kind(ty.kind));
        let result_type = if tuple_array_kinds.is_some() {
            Some(PineType::new(Qualifier::Series, ValueKind::Tuple))
        } else {
            array_kind.map(|kind| PineType::new(Qualifier::Series, kind))
        };
        let admitted_args = (args.len() == 3
            || (args.len() == 4
                && args[3].name.as_deref() == Some("calc_bars_count")
                && arg_types.get(3).copied().flatten().is_some_and(|ty| {
                    ty.kind == ValueKind::Int && qualifier_at_most(ty.qualifier, Qualifier::Simple)
                })))
            && args.iter().take(3).all(|arg| arg.name.is_none());
        if !admitted_args
            || !symbol_ok
            || !timeframe_ok
            || !args
                .get(2)
                .is_some_and(|arg| self.modern_request_expression_supported(&arg.value))
            || result_type.is_none()
        {
            self.unsupported(
                "request.security_lower_tf",
                REQUEST_SECURITY_LOWER_TF_UNSUPPORTED_REASON,
                span,
            );
            return result_type;
        }
        self.compatibility.supported.push(FeatureUse {
            feature: "request.security_lower_tf".to_owned(),
            span,
        });
        result_type
    }

    pub(crate) fn analyze_bound_legacy_security(
        &mut self,
        span: Span,
        bound: &crate::legacy::BoundLegacySecurity,
    ) -> Option<PineType> {
        self.compatibility.supported.push(FeatureUse {
            feature: "security".to_owned(),
            span,
        });
        let [symbol, timeframe, ..] = bound.canonical_args.as_slice() else {
            return None;
        };
        let request_context = LegacyProviderRequestContext {
            symbol: symbol.value.clone(),
            timeframe: timeframe.value.clone(),
            internal_callee: bound.internal_callee,
        };
        self.analyze_request_security_core(
            span,
            &bound.canonical_args,
            &bound.canonical_arg_types,
            false,
            Some(&request_context),
            LEGACY_SECURITY_UNSUPPORTED_REASON,
        )
    }

    fn analyze_request_security_core(
        &mut self,
        span: Span,
        args: &[CallArg],
        arg_types: &[Option<PineType>],
        mut unsupported: bool,
        legacy_request_context: Option<&LegacyProviderRequestContext>,
        unsupported_reason: &str,
    ) -> Option<PineType> {
        let legacy = legacy_request_context.is_some();
        let signature = pine_builtins::get_phase_1_builtin("request.security")
            .expect("request.security signature must exist");
        self.validate_call_args(signature, args, arg_types);

        let same_context_symbol = args
            .first()
            .is_some_and(|arg| self.request_symbol_is_chart(&arg.value, legacy));
        let literal_provider_symbol = args.first().is_some_and(|arg| {
            matches!(&arg.value.kind, ExprKind::Literal(Literal::String(value)) if !value.trim().is_empty())
        });
        let provider_symbol = literal_provider_symbol
            || (arg_types
                .first()
                .copied()
                .flatten()
                .is_some_and(|pine_type| {
                    pine_type.kind == ValueKind::String
                        && (self.dynamic_requests
                            || qualifier_at_most(pine_type.qualifier, Qualifier::Simple))
                }));
        if !same_context_symbol && !provider_symbol {
            unsupported = true;
        }
        let same_chart_timeframe = args
            .get(1)
            .is_some_and(|arg| self.request_timeframe_is_chart(&arg.value, legacy));
        let literal_timeframe = args.get(1).is_some_and(|arg| {
            matches!(&arg.value.kind, ExprKind::Literal(Literal::String(value)) if !value.trim().is_empty())
        });
        let provider_timeframe = literal_timeframe
            || (arg_types
                .get(1)
                .copied()
                .flatten()
                .is_some_and(|pine_type| {
                    pine_type.kind == ValueKind::String
                        && (self.dynamic_requests
                            || qualifier_at_most(pine_type.qualifier, Qualifier::Simple))
                }));
        if !same_chart_timeframe && !provider_timeframe {
            unsupported = true;
        }

        let expression_type = arg_types.get(2).copied().flatten();
        let same_context_request = same_context_symbol && same_chart_timeframe;
        if expression_type.is_none_or(|pine_type| {
            if same_context_request {
                !is_request_same_context_type(pine_type)
            } else {
                !is_request_provider_type(pine_type)
            }
        }) {
            unsupported = true;
        }
        let supported_expression = args.get(2).is_some_and(|arg| {
            if !legacy {
                return self.modern_request_expression_supported(&arg.value);
            }
            if same_context_request {
                if legacy {
                    self.request_expression_is_same_context_value(&arg.value)
                } else {
                    self.request_expression_is_modern_same_context_value(&arg.value)
                }
            } else if expression_type.is_some_and(|pine_type| pine_type.kind == ValueKind::Tuple) {
                if legacy {
                    legacy_request_context.is_some_and(|request_context| {
                        self.request_expression_is_legacy_provider_tuple_value(
                            &arg.value,
                            request_context,
                        )
                    })
                } else {
                    self.request_expression_is_provider_tuple_value(&arg.value)
                }
            } else if provider_symbol || provider_timeframe {
                if legacy {
                    legacy_request_context.is_some_and(|request_context| {
                        self.request_expression_is_legacy_provider_scalar(
                            &arg.value,
                            request_context,
                        )
                    })
                } else {
                    self.request_expression_is_provider_value(&arg.value)
                }
            } else {
                false
            }
        });
        if !supported_expression {
            unsupported = true;
        }
        if unsupported {
            self.unsupported(
                if legacy {
                    "security"
                } else {
                    "request.security"
                },
                unsupported_reason,
                span,
            );
            return expression_type.map(series_request_type);
        }

        expression_type.map(series_request_type)
    }

    fn request_symbol_is_chart(&self, expr: &Expr, legacy: bool) -> bool {
        expr_name(expr)
            .as_deref()
            .is_some_and(|name| name == "syminfo.tickerid" || (legacy && name == "tickerid"))
    }

    fn request_timeframe_is_chart(&self, expr: &Expr, legacy: bool) -> bool {
        expr_name(expr)
            .as_deref()
            .is_some_and(|name| name == "timeframe.period" || (legacy && name == "period"))
    }

    fn validate_request_security_merge_args(&mut self, args: &[CallArg]) -> bool {
        let mut supported = true;
        let signature = pine_builtins::get_phase_1_builtin("request.security")
            .expect("request.security signature must exist");
        self.validate_label_string_arg(
            signature,
            args,
            3,
            "gaps",
            &["barmerge.gaps_off", "barmerge.gaps_on"],
        );
        self.validate_label_string_arg(
            signature,
            args,
            4,
            "lookahead",
            &["barmerge.lookahead_off", "barmerge.lookahead_on"],
        );

        for (index, arg) in args.iter().enumerate().skip(3) {
            let allowed_name = match arg.name.as_deref() {
                Some("gaps") => "gaps",
                Some("lookahead") => "lookahead",
                Some("calc_bars_count") => continue,
                Some(_) => {
                    supported = false;
                    continue;
                }
                None => match index {
                    3 => "gaps",
                    4 => "lookahead",
                    _ => {
                        supported = false;
                        continue;
                    }
                },
            };
            let Some(value) = self.known_const_string_value(&arg.value) else {
                continue;
            };
            let allowed_value = if allowed_name == "gaps" {
                "barmerge.gaps_off"
            } else {
                "barmerge.lookahead_off"
            };
            if value != allowed_value && value != allowed_value.replace("_off", "_on") {
                supported = false;
            }
        }
        supported
    }

    fn request_expression_is_same_context_value(&self, expr: &Expr) -> bool {
        match &expr.kind {
            ExprKind::Tuple(items) => items
                .iter()
                .all(|item| self.request_expression_is_same_context_value(item)),
            _ => self.request_expression_is_pure_scalar(expr),
        }
    }

    fn request_expression_is_pure_scalar(&self, expr: &Expr) -> bool {
        match &expr.kind {
            ExprKind::Member { .. } => false,
            ExprKind::Literal(_) | ExprKind::Identifier(_) => true,
            ExprKind::QualifiedName(_) => expr_name(expr)
                .as_deref()
                .is_none_or(|name| !is_strategy_state_variable(name)),
            ExprKind::Unary { expr, .. } | ExprKind::Group(expr) => {
                self.request_expression_is_pure_scalar(expr)
            }
            ExprKind::Binary { left, right, .. } => {
                self.request_expression_is_pure_scalar(left)
                    && self.request_expression_is_pure_scalar(right)
            }
            ExprKind::Ternary {
                condition,
                then_expr,
                else_expr,
            } => {
                self.request_expression_is_pure_scalar(condition)
                    && self.request_expression_is_pure_scalar(then_expr)
                    && self.request_expression_is_pure_scalar(else_expr)
            }
            ExprKind::History { expr, offset } => {
                self.request_expression_is_pure_scalar(expr)
                    && self.request_expression_is_pure_scalar(offset)
            }
            ExprKind::Call { callee, args } => {
                let Some(name) = self.request_expression_call_name(callee) else {
                    return false;
                };
                (request_scalar_call_is_supported(&name) || request_tuple_call_is_supported(&name))
                    && args.iter().all(|arg| {
                        request_call_argument_is_admitted(&name, arg)
                            && self.request_expression_is_same_context_value(&arg.value)
                    })
            }
            ExprKind::Switch { selector, arms } => {
                selector
                    .as_deref()
                    .is_none_or(|selector| self.request_expression_is_same_context_value(selector))
                    && arms.iter().all(|arm| {
                        arm.condition.as_ref().is_none_or(|condition| {
                            self.request_expression_is_same_context_value(condition)
                        }) && match &arm.result {
                            SwitchArmResult::Expr(result) => {
                                self.request_expression_is_same_context_value(result)
                            }
                            SwitchArmResult::Block(_) => false,
                        }
                    })
            }
            ExprKind::Tuple(_)
            | ExprKind::If { .. }
            | ExprKind::For { .. }
            | ExprKind::ForIn { .. }
            | ExprKind::While { .. } => false,
        }
    }

    fn request_expression_is_provider_tuple_value(&self, expr: &Expr) -> bool {
        match &expr.kind {
            ExprKind::Tuple(items) => items
                .iter()
                .all(|item| self.request_expression_is_provider_value(item)),
            ExprKind::Call { callee, args } => {
                let Some(name) = self.request_expression_call_name(callee) else {
                    return false;
                };
                request_provider_tuple_call_is_supported(&name)
                    && args.iter().all(|arg| {
                        arg.name.is_none() && self.request_expression_is_provider_scalar(&arg.value)
                    })
            }
            _ => false,
        }
    }

    fn request_expression_is_legacy_provider_tuple_value(
        &self,
        expr: &Expr,
        request_context: &LegacyProviderRequestContext,
    ) -> bool {
        match &expr.kind {
            ExprKind::Tuple(items) => items.iter().all(|item| {
                self.request_expression_is_legacy_provider_scalar(item, request_context)
            }),
            ExprKind::Call { callee, args } => {
                let Some(name) = self.request_expression_call_name(callee) else {
                    return false;
                };
                request_provider_tuple_call_is_supported(&name)
                    && args.iter().all(|arg| {
                        arg.name.is_none()
                            && self.request_expression_is_legacy_provider_scalar(
                                &arg.value,
                                request_context,
                            )
                    })
            }
            _ => false,
        }
    }

    fn request_expression_is_legacy_provider_scalar(
        &self,
        expr: &Expr,
        request_context: &LegacyProviderRequestContext,
    ) -> bool {
        self.request_expression_is_legacy_provider_scalar_inner(
            expr,
            &mut std::collections::HashSet::new(),
            &std::collections::HashSet::new(),
            &mut Vec::new(),
            request_context,
            false,
        )
    }

    fn request_expression_is_legacy_provider_scalar_inner(
        &self,
        expr: &Expr,
        visiting: &mut std::collections::HashSet<SymbolId>,
        local_names: &std::collections::HashSet<String>,
        udf_stack: &mut Vec<String>,
        request_context: &LegacyProviderRequestContext,
        allow_nested_legacy_security: bool,
    ) -> bool {
        match &expr.kind {
            ExprKind::Member { .. } => false,
            ExprKind::Literal(_) => true,
            ExprKind::Identifier(name) => {
                if local_names.contains(name) {
                    return true;
                }
                if is_request_provider_scalar_name(name) {
                    return true;
                }
                let Some(symbol) = self
                    .bindings
                    .get(&self.binding_key(name, expr.span))
                    .copied()
                else {
                    return false;
                };
                let global = self.scope.symbol_is_global(symbol.id);
                let active_function_parameter = self
                    .function_param_symbols
                    .last()
                    .is_some_and(|params| params.contains(&symbol.id));
                let active_function_local =
                    !global && !self.function_stack.is_empty() && self.block_depth == 0;
                if (!global && !active_function_local)
                    || symbol.persistence != PersistenceKind::None
                    || self.request_reassigned_names.contains(name)
                    || !is_request_scalar_type(symbol.pine_type)
                {
                    return false;
                }
                if active_function_parameter {
                    return true;
                }
                if qualifier_at_most(symbol.pine_type.qualifier, Qualifier::Simple) {
                    return true;
                }
                if !visiting.insert(symbol.id) {
                    return false;
                }
                let supported = self
                    .with_symbol_initializer(symbol.id, |analyzer, initializer| {
                        Some(analyzer.request_expression_is_legacy_provider_scalar_inner(
                            initializer,
                            visiting,
                            &std::collections::HashSet::new(),
                            udf_stack,
                            request_context,
                            // Immutable global aliases are evaluated again in
                            // the requested context. A nested legacy request
                            // may be inlined there when its selector and merge
                            // policy match the enclosing request.
                            active_function_local || global,
                        ))
                    })
                    .unwrap_or(false);
                visiting.remove(&symbol.id);
                supported
            }
            ExprKind::QualifiedName(_) => expr_name(expr)
                .as_deref()
                .is_some_and(is_request_provider_scalar_name),
            ExprKind::Unary { expr, .. } | ExprKind::Group(expr) => self
                .request_expression_is_legacy_provider_scalar_inner(
                    expr,
                    visiting,
                    local_names,
                    udf_stack,
                    request_context,
                    allow_nested_legacy_security,
                ),
            ExprKind::Binary { left, right, .. } => {
                self.request_expression_is_legacy_provider_scalar_inner(
                    left,
                    visiting,
                    local_names,
                    udf_stack,
                    request_context,
                    allow_nested_legacy_security,
                ) && self.request_expression_is_legacy_provider_scalar_inner(
                    right,
                    visiting,
                    local_names,
                    udf_stack,
                    request_context,
                    allow_nested_legacy_security,
                )
            }
            ExprKind::Ternary {
                condition,
                then_expr,
                else_expr,
            } => {
                self.request_expression_is_legacy_provider_scalar_inner(
                    condition,
                    visiting,
                    local_names,
                    udf_stack,
                    request_context,
                    allow_nested_legacy_security,
                ) && self.request_expression_is_legacy_provider_scalar_inner(
                    then_expr,
                    visiting,
                    local_names,
                    udf_stack,
                    request_context,
                    allow_nested_legacy_security,
                ) && self.request_expression_is_legacy_provider_scalar_inner(
                    else_expr,
                    visiting,
                    local_names,
                    udf_stack,
                    request_context,
                    allow_nested_legacy_security,
                )
            }
            ExprKind::History { expr, offset } => {
                if crate::types::const_int_value(offset).is_some_and(|value| value > 0)
                    && let ExprKind::Identifier(name) = &expr.kind
                    && let Some(symbol) = self.bindings.get(&self.binding_key(name, expr.span))
                    && visiting.contains(&symbol.id)
                {
                    return self.request_expression_is_legacy_provider_scalar_inner(
                        offset,
                        visiting,
                        local_names,
                        udf_stack,
                        request_context,
                        allow_nested_legacy_security,
                    );
                }
                self.request_expression_is_legacy_provider_scalar_inner(
                    expr,
                    visiting,
                    local_names,
                    udf_stack,
                    request_context,
                    allow_nested_legacy_security,
                ) && self.request_expression_is_legacy_provider_scalar_inner(
                    offset,
                    visiting,
                    local_names,
                    udf_stack,
                    request_context,
                    allow_nested_legacy_security,
                )
            }
            ExprKind::Call { callee, args } => {
                let Some(source_name) = expr_name(callee) else {
                    return false;
                };
                if matches!(callee.kind, ExprKind::Identifier(_))
                    && self.functions.contains_key(&source_name)
                {
                    return self.request_legacy_provider_udf_call_is_supported(
                        &source_name,
                        args,
                        visiting,
                        local_names,
                        udf_stack,
                        request_context,
                    );
                }
                let Some(name) = self.request_expression_call_name(callee) else {
                    return false;
                };
                if allow_nested_legacy_security && name.starts_with("$legacy.security.") {
                    return self.request_nested_legacy_security_is_supported(
                        &name,
                        args,
                        visiting,
                        local_names,
                        udf_stack,
                        request_context,
                    );
                }
                if name == "input.source" {
                    return args.first().is_some_and(|arg| {
                        self.request_expression_is_legacy_provider_scalar_inner(
                            &arg.value,
                            visiting,
                            local_names,
                            udf_stack,
                            request_context,
                            allow_nested_legacy_security,
                        )
                    });
                }
                legacy_request_scalar_call_is_supported(&name)
                    && args.iter().all(|arg| {
                        request_call_argument_is_admitted(&name, arg)
                            && self.request_expression_is_legacy_provider_scalar_inner(
                                &arg.value,
                                visiting,
                                local_names,
                                udf_stack,
                                request_context,
                                allow_nested_legacy_security,
                            )
                    })
            }
            ExprKind::Switch { selector, arms } => {
                selector.as_deref().is_none_or(|selector| {
                    self.request_expression_is_legacy_provider_scalar_inner(
                        selector,
                        visiting,
                        local_names,
                        udf_stack,
                        request_context,
                        allow_nested_legacy_security,
                    )
                }) && arms.iter().all(|arm| {
                    arm.condition.as_ref().is_none_or(|condition| {
                        self.request_expression_is_legacy_provider_scalar_inner(
                            condition,
                            visiting,
                            local_names,
                            udf_stack,
                            request_context,
                            allow_nested_legacy_security,
                        )
                    }) && match &arm.result {
                        SwitchArmResult::Expr(result) => self
                            .request_expression_is_legacy_provider_scalar_inner(
                                result,
                                visiting,
                                local_names,
                                udf_stack,
                                request_context,
                                allow_nested_legacy_security,
                            ),
                        SwitchArmResult::Block(_) => false,
                    }
                })
            }
            ExprKind::Tuple(_)
            | ExprKind::If { .. }
            | ExprKind::For { .. }
            | ExprKind::ForIn { .. }
            | ExprKind::While { .. } => false,
        }
    }

    fn request_legacy_provider_udf_call_is_supported(
        &self,
        name: &str,
        args: &[CallArg],
        visiting: &mut std::collections::HashSet<SymbolId>,
        local_names: &std::collections::HashSet<String>,
        udf_stack: &mut Vec<String>,
        request_context: &LegacyProviderRequestContext,
    ) -> bool {
        if udf_stack.iter().any(|active| active == name) {
            return false;
        }
        let Some(function) = self.functions.get(name).cloned() else {
            return false;
        };
        if resolve_udf_arg_indices(&function.params, args).is_err()
            || !args.iter().all(|arg| {
                self.request_expression_is_legacy_provider_scalar_inner(
                    &arg.value,
                    visiting,
                    local_names,
                    udf_stack,
                    request_context,
                    false,
                )
            })
        {
            return false;
        }

        let mut function_locals: std::collections::HashSet<_> =
            function.params.iter().cloned().collect();
        udf_stack.push(name.to_owned());
        let supported = self.with_source_context_ref(function.source_context_id, |analyzer| {
            match &function.body {
                FunctionBody::Expr(expr) => analyzer
                    .request_expression_is_legacy_provider_scalar_inner(
                        expr,
                        visiting,
                        &function_locals,
                        udf_stack,
                        request_context,
                        false,
                    ),
                FunctionBody::Block(statements) => {
                    let Some((last, prefix)) = statements.split_last() else {
                        return false;
                    };
                    for statement in prefix {
                        let StmtKind::Decl {
                            mode, name, value, ..
                        } = &statement.kind
                        else {
                            return false;
                        };
                        if !matches!(
                            mode,
                            pine_syntax::DeclMode::Normal | pine_syntax::DeclMode::Const
                        ) || function_locals.contains(name)
                            || !analyzer.request_expression_is_legacy_provider_scalar_inner(
                                value,
                                visiting,
                                &function_locals,
                                udf_stack,
                                request_context,
                                false,
                            )
                        {
                            return false;
                        }
                        function_locals.insert(name.clone());
                    }
                    let StmtKind::Expr(result) = &last.kind else {
                        return false;
                    };
                    analyzer.request_expression_is_legacy_provider_scalar_inner(
                        result,
                        visiting,
                        &function_locals,
                        udf_stack,
                        request_context,
                        false,
                    )
                }
            }
        });
        udf_stack.pop();
        supported
    }

    fn request_nested_legacy_security_is_supported(
        &self,
        internal_callee: &str,
        args: &[CallArg],
        visiting: &mut std::collections::HashSet<SymbolId>,
        local_names: &std::collections::HashSet<String>,
        udf_stack: &mut Vec<String>,
        request_context: &LegacyProviderRequestContext,
    ) -> bool {
        let [symbol, timeframe, expression] = args else {
            return false;
        };
        if args.iter().any(|arg| arg.name.is_some())
            || internal_callee != request_context.internal_callee
            || !self.request_selector_exprs_match(&symbol.value, &request_context.symbol)
            || !self.request_selector_exprs_match(&timeframe.value, &request_context.timeframe)
        {
            return false;
        }
        self.request_expression_is_legacy_provider_scalar_inner(
            &expression.value,
            visiting,
            local_names,
            udf_stack,
            request_context,
            true,
        )
    }

    fn request_selector_exprs_match(&self, left: &Expr, right: &Expr) -> bool {
        if let (Some(left), Some(right)) = (
            self.known_const_string_value(left),
            self.known_const_string_value(right),
        ) {
            return left == right;
        }
        match (&left.kind, &right.kind) {
            (ExprKind::Identifier(left_name), ExprKind::Identifier(right_name)) => {
                let left = self
                    .bindings
                    .get(&self.binding_key(left_name, left.span))
                    .map(|symbol| symbol.id);
                let right = self
                    .bindings
                    .get(&self.binding_key(right_name, right.span))
                    .map(|symbol| symbol.id);
                matches!((left, right), (Some(left), Some(right)) if left == right)
            }
            (ExprKind::QualifiedName(_), ExprKind::QualifiedName(_)) => {
                expr_name(left) == expr_name(right)
            }
            _ => false,
        }
    }

    fn request_expression_is_provider_scalar(&self, expr: &Expr) -> bool {
        match &expr.kind {
            ExprKind::Member { .. } => false,
            ExprKind::Literal(_) => true,
            ExprKind::Identifier(_) | ExprKind::QualifiedName(_) => expr_name(expr)
                .as_deref()
                .is_some_and(is_request_provider_scalar_name),
            ExprKind::Unary { expr, .. } | ExprKind::Group(expr) => {
                self.request_expression_is_provider_scalar(expr)
            }
            ExprKind::Binary { left, right, .. } => {
                self.request_expression_is_provider_scalar(left)
                    && self.request_expression_is_provider_scalar(right)
            }
            ExprKind::Ternary {
                condition,
                then_expr,
                else_expr,
            } => {
                self.request_expression_is_provider_scalar(condition)
                    && self.request_expression_is_provider_scalar(then_expr)
                    && self.request_expression_is_provider_scalar(else_expr)
            }
            ExprKind::History { expr, offset } => {
                self.request_expression_is_provider_scalar(expr)
                    && self.request_expression_is_provider_scalar(offset)
            }
            ExprKind::Call { callee, args } => {
                let Some(name) = self.request_expression_call_name(callee) else {
                    return false;
                };
                request_scalar_call_is_supported(&name)
                    && args.iter().all(|arg| {
                        request_call_argument_is_admitted(&name, arg)
                            && self.request_expression_is_provider_scalar(&arg.value)
                    })
            }
            ExprKind::Switch { selector, arms } => {
                selector
                    .as_deref()
                    .is_none_or(|selector| self.request_expression_is_provider_scalar(selector))
                    && arms.iter().all(|arm| {
                        arm.condition.as_ref().is_none_or(|condition| {
                            self.request_expression_is_provider_scalar(condition)
                        }) && match &arm.result {
                            SwitchArmResult::Expr(result) => {
                                self.request_expression_is_provider_scalar(result)
                            }
                            SwitchArmResult::Block(_) => false,
                        }
                    })
            }
            ExprKind::Tuple(_)
            | ExprKind::If { .. }
            | ExprKind::For { .. }
            | ExprKind::ForIn { .. }
            | ExprKind::While { .. } => false,
        }
    }

    fn request_expression_call_name(&self, callee: &Expr) -> Option<String> {
        let name = expr_name(callee)?;
        Some(
            self.legacy
                .canonical_call_name(self.current_source_context_id(), callee.span)
                .map_or(name, str::to_owned),
        )
    }

    fn request_expression_is_provider_value(&self, expr: &Expr) -> bool {
        self.request_expression_is_provider_scalar(expr)
            || self.request_expression_is_provider_array(expr)
    }

    fn request_expression_is_modern_same_context_value(&self, expr: &Expr) -> bool {
        match &expr.kind {
            ExprKind::Tuple(items) => items
                .iter()
                .all(|item| self.request_expression_is_modern_same_context_value(item)),
            _ => {
                self.request_expression_is_provider_array(expr)
                    || self.request_expression_is_same_context_value(expr)
            }
        }
    }

    fn request_expression_is_provider_array(&self, expr: &Expr) -> bool {
        if !self
            .type_of_expr_with_params(expr, &HashMap::new())
            .is_some_and(|ty| crate::types::is_scalar_array_kind(ty.kind))
        {
            return false;
        }
        let ExprKind::Call { callee, args } = &expr.without_groups().kind else {
            return false;
        };
        let Some(name) = self.request_expression_call_name(callee) else {
            return false;
        };
        matches!(
            name.as_str(),
            "array.from"
                | "array.new_float"
                | "array.new_int"
                | "array.new_bool"
                | "array.new_string"
                | "array.new_color"
                | "array.new<float>"
                | "array.new<int>"
                | "array.new<bool>"
                | "array.new<string>"
                | "array.new<color>"
                | "ta.pivot_point_levels"
        ) && args
            .iter()
            .all(|arg| self.request_expression_is_provider_scalar(&arg.value))
    }
}

fn series_request_type(pine_type: PineType) -> PineType {
    PineType::new(Qualifier::Series, pine_type.kind)
}

fn is_request_scalar_type(pine_type: PineType) -> bool {
    matches!(
        pine_type.kind,
        ValueKind::Int
            | ValueKind::Float
            | ValueKind::Bool
            | ValueKind::String
            | ValueKind::Color
            | ValueKind::Na
    )
}

fn lower_tf_scalar_array_kind(kind: ValueKind) -> Option<ValueKind> {
    matches!(
        kind,
        ValueKind::Int | ValueKind::Float | ValueKind::Bool | ValueKind::String | ValueKind::Color
    )
    .then(|| kind.array_kind_from_element_kind())
    .flatten()
}

fn is_request_same_context_type(pine_type: PineType) -> bool {
    is_request_scalar_type(pine_type)
        || pine_type.kind == ValueKind::Tuple
        || crate::types::is_scalar_array_kind(pine_type.kind)
}

fn is_request_provider_type(pine_type: PineType) -> bool {
    is_request_scalar_type(pine_type)
        || pine_type.kind == ValueKind::Tuple
        || crate::types::is_scalar_array_kind(pine_type.kind)
}

fn request_tuple_call_is_supported(name: &str) -> bool {
    matches!(
        name,
        "ta.macd" | "ta.bb" | "ta.kc" | "ta.supertrend" | "ta.dmi" | "ta.vwap"
    )
}

fn request_provider_tuple_call_is_supported(name: &str) -> bool {
    matches!(
        name,
        "ta.macd" | "ta.bb" | "ta.kc" | "ta.supertrend" | "ta.dmi" | "ta.vwap"
    )
}

fn is_request_provider_scalar_name(name: &str) -> bool {
    matches!(
        name,
        "syminfo.tickerid"
            | "timeframe.period"
            | "open"
            | "high"
            | "low"
            | "close"
            | "hl2"
            | "hlc3"
            | "hlcc4"
            | "ohlc4"
            | "volume"
            | "time"
            | "tr"
            | "bar_index"
            | "barstate.isfirst"
            | "barstate.islast"
            | "barstate.islastconfirmedhistory"
            | "barstate.isnew"
            | "barstate.isconfirmed"
            | "barstate.ishistory"
            | "barstate.isrealtime"
            | "ta.accdist"
            | "ta.iii"
            | "ta.nvi"
            | "ta.obv"
            | "ta.pvi"
            | "ta.pvt"
            | "ta.tr"
            | "ta.vwap"
            | "ta.wvad"
    )
}

fn request_scalar_call_is_supported(name: &str) -> bool {
    matches!(
        name,
        "na" | "nz"
            | "time"
            | "time_close"
            | "timeframe.in_seconds"
            | "math.abs"
            | "math.max"
            | "math.min"
            | "math.avg"
            | "math.floor"
            | "math.ceil"
            | "math.trunc"
            | "math.sqrt"
            | "math.cbrt"
            | "math.log"
            | "math.log10"
            | "math.exp"
            | "math.acos"
            | "math.asin"
            | "math.atan"
            | "math.sign"
            | "math.todegrees"
            | "math.toradians"
            | "math.sin"
            | "math.cos"
            | "math.tan"
            | "math.pow"
            | "math.hypot"
            | "math.round"
            | "math.round_to_mintick"
            | "math.sum"
            | "ta.cum"
            | "ta.sma"
            | "ta.ema"
            | "ta.dema"
            | "ta.tema"
            | "ta.rma"
            | "ta.rsi"
            | "ta.tsi"
            | "ta.cmo"
            | "ta.cci"
            | "ta.cog"
            | "ta.bop"
            | "ta.ao"
            | "ta.max"
            | "ta.min"
            | "ta.mfi"
            | "ta.stoch"
            | "ta.wpr"
            | "ta.sar"
            | "ta.tr"
            | "ta.atr"
            | "ta.highest"
            | "ta.lowest"
            | "ta.highestbars"
            | "ta.lowestbars"
            | "ta.change"
            | "ta.mom"
            | "ta.roc"
            | "ta.range"
            | "ta.dev"
            | "ta.vwap"
            | "ta.bbw"
            | "ta.kcw"
            | "ta.pivothigh"
            | "ta.pivotlow"
            | "ta.correlation"
            | "ta.covariance"
            | "ta.median"
            | "ta.mode"
            | "ta.percentile_linear_interpolation"
            | "ta.percentile_nearest_rank"
            | "ta.percentrank"
            | "ta.stdev"
            | "ta.variance"
            | "ta.wma"
            | "ta.vwma"
            | "ta.swma"
            | "ta.hma"
            | "ta.alma"
            | "ta.linreg"
            | "ta.rising"
            | "ta.falling"
            | "ta.barssince"
            | "ta.valuewhen"
            | "ta.cross"
            | "ta.crossover"
            | "ta.crossunder"
    )
}

fn legacy_request_scalar_call_is_supported(name: &str) -> bool {
    request_scalar_call_is_supported(name) || name == "int"
}

fn request_call_argument_is_admitted(callee: &str, arg: &CallArg) -> bool {
    match arg.name.as_deref() {
        None => true,
        Some(name) if matches!(callee, "time" | "time_close") => matches!(
            name,
            "timeframe" | "session" | "timezone" | "bars_back" | "timeframe_bars_back"
        ),
        Some(_) => false,
    }
}
