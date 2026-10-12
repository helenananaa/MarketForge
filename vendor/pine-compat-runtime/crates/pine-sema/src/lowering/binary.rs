use super::*;

impl Analyzer {
    pub(super) fn lower_binary_expr_with_params(
        &mut self,
        root: &Expr,
        param_exprs: &HashMap<String, HirExpr>,
        param_types: &HashMap<String, PineType>,
    ) -> Option<HirExpr> {
        enum Work<'expr> {
            Visit(&'expr Expr),
            Build {
                expr: &'expr Expr,
                op: BinaryOp,
                pine_type: PineType,
                series_id: Option<pine_ir::SeriesId>,
            },
        }

        let mut work = vec![Work::Visit(root)];
        let mut values = Vec::new();
        while let Some(item) = work.pop() {
            match item {
                Work::Visit(expr) => {
                    let ExprKind::Binary { op, left, right } = &expr.kind else {
                        values.push(self.lower_expr_with_params(expr, param_exprs, param_types)?);
                        continue;
                    };
                    if !self.record_lowering_node(expr.span) {
                        return None;
                    }
                    let pine_type = self.type_of_expr_with_params(expr, param_types)?;
                    let series_id = self.lower_expr_series_id(expr, pine_type);
                    if self.legacy.dialect().version() < 5
                        && pine_type.qualifier == Qualifier::Const
                        && matches!(
                            op,
                            BinaryOp::Eq
                                | BinaryOp::NotEq
                                | BinaryOp::Lt
                                | BinaryOp::Lte
                                | BinaryOp::Gt
                                | BinaryOp::Gte
                        )
                        && let Some(value) = self.known_const_bool_value(expr)
                    {
                        values.push(HirExpr {
                            pine_type,
                            series_id,
                            kind: HirExprKind::Literal(HirLiteral::Bool(value)),
                        });
                        continue;
                    }
                    work.push(Work::Build {
                        expr,
                        op: *op,
                        pine_type,
                        series_id,
                    });
                    work.push(Work::Visit(right));
                    work.push(Work::Visit(left));
                }
                Work::Build {
                    expr,
                    op,
                    pine_type,
                    series_id,
                } => {
                    let right = values.pop()?;
                    let left = values.pop()?;
                    let lowered = HirExpr {
                        pine_type,
                        series_id,
                        kind: HirExprKind::Binary {
                            op: if pine_type.kind == ValueKind::PlotDisplay {
                                match op {
                                    BinaryOp::Add => pine_ir::HirBinaryOp::DisplayUnion,
                                    BinaryOp::Sub => pine_ir::HirBinaryOp::DisplayDifference,
                                    _ => lower_binary_op(op),
                                }
                            } else {
                                lower_binary_op(op)
                            },
                            left: Box::new(left),
                            right: Box::new(right),
                        },
                    };
                    values.push(self.finish_legacy_expr_coercion(expr, lowered)?);
                }
            }
        }
        debug_assert_eq!(values.len(), 1);
        values.pop()
    }
}
