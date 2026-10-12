use crate::analyzer::chart_points::{chart_point_field_index, chart_point_field_type};
use crate::prelude::*;

impl Analyzer {
    pub(super) fn lower_member_access(
        &mut self,
        expr: &Expr,
        receiver: &Expr,
        name: &str,
        param_exprs: &HashMap<String, HirExpr>,
        param_types: &HashMap<String, PineType>,
    ) -> Option<HirExpr> {
        let value = self.lower_expr_with_params(receiver, param_exprs, param_types)?;
        let (pine_type, index) = if value.pine_type.kind == ValueKind::ChartPoint {
            (
                chart_point_field_type(value.pine_type, name)?,
                chart_point_field_index(name)?,
            )
        } else {
            let identity = self.user_type_name_of_expr_with_params(receiver, param_exprs)?;
            let (ty, _, fields) = self.user_type_field_path(
                &identity,
                value.pine_type.qualifier,
                &[name.to_owned()],
            )?;
            (ty, fields.first()?.index)
        };
        let series_id = self.lower_expr_series_id(expr, pine_type);
        self.finish_legacy_expr_coercion(
            expr,
            HirExpr {
                pine_type,
                series_id,
                kind: HirExprKind::FieldAccess {
                    value: Box::new(value),
                    index,
                },
            },
        )
    }

    pub(super) fn lower_member_call(
        &mut self,
        expr: &Expr,
        receiver: &Expr,
        method: &str,
        args: &[CallArg],
        param_exprs: &HashMap<String, HirExpr>,
        param_types: &HashMap<String, PineType>,
    ) -> Option<HirExpr> {
        // Lower the receiver exactly once, then let ordinary argument binding
        // consume the already lowered value through an unspellable placeholder.
        let value = self.lower_expr_with_params(receiver, param_exprs, param_types)?;
        let name = Self::member_builtin_name(value.pine_type.kind, method)?;
        let placeholder = "\0member_receiver".to_owned();
        let mut exprs = param_exprs.clone();
        let mut types = param_types.clone();
        types.insert(placeholder.clone(), value.pine_type);
        exprs.insert(placeholder.clone(), value);
        let mut call_args = vec![CallArg {
            name: None,
            span: receiver.span,
            value: Expr {
                kind: ExprKind::Identifier(placeholder),
                span: receiver.span,
            },
        }];
        call_args.extend_from_slice(args);
        let lowered = self.lower_builtin_call_args(&name, &call_args, &exprs, &types)?;
        let arg_types = lowered
            .iter()
            .map(|arg| Some(arg.value.pine_type))
            .collect::<Vec<_>>();
        let signature = pine_builtins::get_phase_1_builtin(&name)?;
        // `lowered` is already in signature-slot order, including omitted slots.
        let pine_type = self.return_type(signature, &arg_types)?;
        let series_id = self.lower_expr_series_id(expr, pine_type);
        let call_site_id = self.alloc_call_site_at(expr.span);
        self.finish_legacy_expr_coercion(
            expr,
            HirExpr {
                pine_type,
                series_id,
                kind: HirExprKind::Call {
                    callee: name,
                    call_site_id,
                    args: lowered,
                },
            },
        )
    }
}
