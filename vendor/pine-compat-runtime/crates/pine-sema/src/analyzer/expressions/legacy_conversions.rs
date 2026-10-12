use super::*;

impl Analyzer {
    pub(crate) fn v4_v5_series_output_offset_arg(
        &mut self,
        call_name: &str,
        param_name: &str,
        expr: &Expr,
        actual_type: PineType,
        accepts: Accepts,
    ) -> bool {
        if !matches!(self.legacy.dialect(), PineDialect::V4 | PineDialect::V5)
            || param_name != "offset"
            || accepts != Accepts::SimpleIntCompatible
            || actual_type != PineType::new(Qualifier::Series, ValueKind::Int)
            || !matches!(
                call_name,
                "plot" | "plotchar" | "plotshape" | "plotarrow" | "bgcolor" | "barcolor"
            )
        {
            return false;
        }

        if self
            .v4_v5_series_output_offset_exprs
            .insert(self.expr_key(expr.span))
        {
            let version = self.legacy.dialect().version();
            self.compatibility
                .legacy_emulations
                .push(crate::compatibility::LegacyEmulation {
                    feature: format!("v{version}.series_output_offset"),
                    behavior: format!(
                        "Pine v{version} output offsets accept a series int and apply its final evaluated value to the complete rendered output"
                    ),
                    span: expr.span,
                });
        }
        true
    }

    pub(crate) fn legacy_numeric_bool_arg(
        &mut self,
        expr: &Expr,
        actual_type: PineType,
        accepts: Accepts,
    ) -> bool {
        if self.legacy.dialect() == PineDialect::V6
            || !matches!(
                accepts,
                Accepts::BoolCompatible | Accepts::SimpleBoolCompatible
            )
            || !matches!(
                actual_type.kind,
                ValueKind::Int | ValueKind::Float | ValueKind::Na
            )
        {
            return false;
        }

        let coerced_type = PineType::new(actual_type.qualifier, ValueKind::Bool);
        if !accepts_type(accepts, coerced_type) {
            return false;
        }
        let key = self.expr_key(expr.span);
        if !self.legacy_numeric_to_bool_exprs.contains(&key) {
            self.record_numeric_to_bool_coercion(expr.span);
        }
        true
    }

    pub(super) fn uses_versioned_integer_division(
        &self,
        op: BinaryOp,
        left_type: PineType,
        right_type: PineType,
    ) -> bool {
        op == BinaryOp::Div
            && left_type.kind == ValueKind::Int
            && right_type.kind == ValueKind::Int
            && (self.legacy.dialect() <= PineDialect::V3
                || (matches!(self.legacy.dialect(), PineDialect::V4 | PineDialect::V5)
                    && left_type.qualifier == Qualifier::Const
                    && right_type.qualifier == Qualifier::Const))
    }

    pub(crate) fn extreme_division_length_arg(
        &mut self,
        call_name: &str,
        param_name: &str,
        expr: &Expr,
        actual_type: PineType,
    ) -> bool {
        let legacy_ema = self.legacy.dialect() == PineDialect::V4 && call_name == "ta.ema";
        let extreme = matches!(self.legacy.dialect(), PineDialect::V5 | PineDialect::V6)
            && matches!(call_name, "ta.lowest" | "ta.highest");
        if !(legacy_ema || extreme)
            || param_name != "length"
            || actual_type.kind != ValueKind::Float
        {
            return false;
        }
        let ExprKind::Binary {
            op: BinaryOp::Div,
            left,
            right,
        } = &expr.kind
        else {
            return false;
        };
        if ![left, right].iter().all(|operand| {
            self.analyze_expr(operand)
                .is_some_and(|ty| ty.kind == ValueKind::Int)
        }) {
            return false;
        }
        if self
            .legacy_integer_division_exprs
            .insert(self.expr_key(expr.span))
        {
            self.compatibility
                .legacy_emulations
                .push(crate::compatibility::LegacyEmulation {
                    feature: format!("v{}.{}division_length", self.legacy.dialect().version(), if legacy_ema { "ema_" } else { "extreme_" }),
                    behavior: format!("Pine v{} {call_name} length context truncates an integer-operand quotient; the ordinary quotient retains its fractional value", self.legacy.dialect().version()),
                    span: expr.span,
                });
        }
        true
    }

    fn record_versioned_integer_division(&mut self, span: Span, qualifier: Qualifier) -> PineType {
        if self
            .legacy_integer_division_exprs
            .insert(self.expr_key(span))
        {
            let version = self.legacy.dialect().version();
            let behavior = if matches!(self.legacy.dialect(), PineDialect::V4 | PineDialect::V5) {
                format!(
                    "Pine v{version} division of two const int values produces an int and discards the fractional remainder"
                )
            } else {
                format!(
                    "Pine v{version} division of two int values produces an int and discards the fractional remainder"
                )
            };
            self.compatibility
                .legacy_emulations
                .push(crate::compatibility::LegacyEmulation {
                    feature: format!("v{version}.integer_division"),
                    behavior,
                    span,
                });
        }
        PineType::new(qualifier, ValueKind::Int)
    }

    pub(super) fn infer_unary_with_legacy(
        &mut self,
        op: UnaryOp,
        expr_type: PineType,
        operand_span: Span,
        expression_span: Span,
    ) -> Option<PineType> {
        let dialect = self.legacy.dialect();
        if matches!(dialect, PineDialect::V1 | PineDialect::V2)
            && matches!(op, UnaryOp::Plus | UnaryOp::Minus)
            && expr_type.kind == ValueKind::Bool
        {
            self.record_bool_to_float_coercion(operand_span);
            let result = PineType::new(expr_type.qualifier, ValueKind::Float);
            self.expr_types
                .insert(self.expr_key(expression_span), result);
            return Some(result);
        }
        if dialect != PineDialect::V6
            && op == UnaryOp::Not
            && matches!(
                expr_type.kind,
                ValueKind::Int | ValueKind::Float | ValueKind::Na
            )
        {
            self.record_numeric_to_bool_coercion(operand_span);
            let result = PineType::new(expr_type.qualifier, ValueKind::Bool);
            self.expr_types
                .insert(self.expr_key(expression_span), result);
            return Some(result);
        }
        self.infer_unary(op, expr_type, operand_span)
    }

    pub(super) fn infer_binary_with_legacy(
        &mut self,
        op: BinaryOp,
        left_type: PineType,
        right_type: PineType,
        left_span: Span,
        right_span: Span,
        span: Span,
    ) -> Option<PineType> {
        let dialect = self.legacy.dialect();
        if self.uses_versioned_integer_division(op, left_type, right_type) {
            return Some(self.record_versioned_integer_division(
                span,
                strongest_qualifier(left_type.qualifier, right_type.qualifier),
            ));
        }
        let arithmetic = matches!(
            op,
            BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod
        );
        let comparison = matches!(
            op,
            BinaryOp::Eq
                | BinaryOp::NotEq
                | BinaryOp::Gt
                | BinaryOp::Gte
                | BinaryOp::Lt
                | BinaryOp::Lte
        );
        let legacy_numeric_kind = |kind| {
            matches!(
                kind,
                ValueKind::Int | ValueKind::Float | ValueKind::Bool | ValueKind::Na
            )
        };
        if matches!(dialect, PineDialect::V1 | PineDialect::V2)
            && arithmetic
            && (left_type.kind == ValueKind::Bool || right_type.kind == ValueKind::Bool)
            && legacy_numeric_kind(left_type.kind)
            && legacy_numeric_kind(right_type.kind)
        {
            if left_type.kind == ValueKind::Bool {
                self.record_bool_to_float_coercion(left_span);
            }
            if right_type.kind == ValueKind::Bool {
                self.record_bool_to_float_coercion(right_span);
            }
            let result = PineType::new(
                strongest_qualifier(left_type.qualifier, right_type.qualifier),
                ValueKind::Float,
            );
            self.expr_types.insert(self.expr_key(span), result);
            return Some(result);
        }
        if matches!(dialect, PineDialect::V1 | PineDialect::V2)
            && comparison
            && ((left_type.kind == ValueKind::Bool && is_numeric(right_type.kind))
                || (right_type.kind == ValueKind::Bool && is_numeric(left_type.kind)))
        {
            if left_type.kind == ValueKind::Bool {
                self.record_bool_to_float_coercion(left_span);
            }
            if right_type.kind == ValueKind::Bool {
                self.record_bool_to_float_coercion(right_span);
            }
            let result = PineType::new(
                strongest_qualifier(left_type.qualifier, right_type.qualifier),
                ValueKind::Bool,
            );
            self.expr_types.insert(self.expr_key(span), result);
            return Some(result);
        }
        if dialect != PineDialect::V6
            && matches!(op, BinaryOp::And | BinaryOp::Or)
            && matches!(
                left_type.kind,
                ValueKind::Bool | ValueKind::Int | ValueKind::Float | ValueKind::Na
            )
            && matches!(
                right_type.kind,
                ValueKind::Bool | ValueKind::Int | ValueKind::Float | ValueKind::Na
            )
        {
            if left_type.kind != ValueKind::Bool {
                self.record_numeric_to_bool_coercion(left_span);
            }
            if right_type.kind != ValueKind::Bool {
                self.record_numeric_to_bool_coercion(right_span);
            }
            return Some(PineType::new(
                strongest_qualifier(left_type.qualifier, right_type.qualifier),
                ValueKind::Bool,
            ));
        }
        self.infer_binary(op, left_type, right_type, span)
    }

    pub(super) fn record_bool_to_float_coercion(&mut self, span: Span) {
        self.legacy_bool_to_float_exprs.insert(self.expr_key(span));
        let version = self.legacy.dialect().version();
        self.compatibility
            .legacy_emulations
            .push(crate::compatibility::LegacyEmulation {
                feature: format!("v{version}.bool_arithmetic"),
                behavior: format!(
                    "Pine v{version} boolean arithmetic converts true to 1.0, false to 0.0, and unavailable boolean values to canonical na before applying the numeric operator"
                ),
                span,
            });
    }

    pub(crate) fn record_numeric_to_bool_coercion(&mut self, span: Span) {
        self.legacy_numeric_to_bool_exprs
            .insert(self.expr_key(span));
        let version = self.legacy.dialect().version();
        self.compatibility
            .legacy_emulations
            .push(crate::compatibility::LegacyEmulation {
                feature: format!("v{version}.numeric_to_bool"),
                behavior: format!(
                    "Pine v{version} condition conversion treats zero and na as false and every other finite numeric value as true"
                ),
                span,
            });
    }
}
