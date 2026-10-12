use super::*;

impl Analyzer {
    pub(super) fn finish_legacy_expr_coercion(
        &mut self,
        expr: &Expr,
        mut lowered: HirExpr,
    ) -> Option<HirExpr> {
        let key = self.expr_key(expr.span);
        if self.legacy_bool_to_float_exprs.contains(&key) {
            if !self.record_lowering_node(expr.span) {
                return None;
            }
            let pine_type = PineType::new(lowered.pine_type.qualifier, ValueKind::Float);
            lowered = HirExpr {
                series_id: self.series_id_for_type(pine_type),
                pine_type,
                kind: HirExprKind::Call {
                    callee: "float".to_owned(),
                    call_site_id: self.alloc_call_site(),
                    args: vec![HirCallArg {
                        name: None,
                        value: lowered,
                    }],
                },
            };
        }
        if self.legacy_numeric_to_bool_exprs.contains(&key) {
            if !self.record_lowering_node(expr.span) {
                return None;
            }
            let pine_type = PineType::new(lowered.pine_type.qualifier, ValueKind::Bool);
            lowered = HirExpr {
                series_id: self.series_id_for_type(pine_type),
                pine_type,
                kind: HirExprKind::Call {
                    callee: "bool".to_owned(),
                    call_site_id: self.alloc_call_site(),
                    args: vec![HirCallArg {
                        name: None,
                        value: lowered,
                    }],
                },
            };
        }
        if self.legacy_integer_division_exprs.contains(&key) {
            if !self.record_lowering_node(expr.span) {
                return None;
            }
            let pine_type = PineType::new(lowered.pine_type.qualifier, ValueKind::Int);
            lowered = HirExpr {
                series_id: self.series_id_for_type(pine_type),
                pine_type,
                kind: HirExprKind::Call {
                    callee: "int".to_owned(),
                    call_site_id: self.alloc_call_site(),
                    args: vec![HirCallArg {
                        name: None,
                        value: lowered,
                    }],
                },
            };
        }
        if self.lowering_inline_depth > 0
            && let Some(series_id) = lowered.series_id
        {
            self.execution_scoped_series_ids.insert(series_id);
        }
        Some(lowered)
    }
}
