use pine_ir::{HirExpr, HirExprKind, HirHistoryOffset, HirStmt, HirStmtKind};

pub(crate) fn statements(stmts: &[HirStmt], visitor: &mut impl FnMut(&HirExpr)) {
    statements_and_bindings(stmts, visitor, &mut |_| {});
}

pub(crate) fn statements_and_bindings<'a>(
    stmts: &'a [HirStmt],
    visitor: &mut impl FnMut(&'a HirExpr),
    statement_visitor: &mut impl FnMut(&'a HirStmt),
) {
    for statement in stmts {
        statement_visitor(statement);
        match &statement.kind {
            HirStmtKind::Expr(expr)
            | HirStmtKind::Decl { value: expr, .. }
            | HirStmtKind::Reassign { value: expr, .. }
            | HirStmtKind::FieldReassign { value: expr, .. }
            | HirStmtKind::TupleDecl { value: expr, .. } => {
                expression(expr, visitor, statement_visitor);
            }
            HirStmtKind::ArrayFieldReassign {
                array,
                index,
                value,
                ..
            } => {
                expression(array, visitor, statement_visitor);
                expression(index, visitor, statement_visitor);
                expression(value, visitor, statement_visitor);
            }
            HirStmtKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                expression(condition, visitor, statement_visitor);
                statements_and_bindings(then_branch, visitor, statement_visitor);
                statements_and_bindings(else_branch, visitor, statement_visitor);
            }
            HirStmtKind::Switch { selector, arms } => {
                if let Some(selector) = selector {
                    expression(selector, visitor, statement_visitor);
                }
                for arm in arms {
                    if let Some(condition) = &arm.condition {
                        expression(condition, visitor, statement_visitor);
                    }
                    statements_and_bindings(&arm.body, visitor, statement_visitor);
                }
            }
            HirStmtKind::For {
                from,
                to,
                step,
                body,
                ..
            } => {
                expression(from, visitor, statement_visitor);
                expression(to, visitor, statement_visitor);
                if let Some(step) = step {
                    expression(step, visitor, statement_visitor);
                }
                statements_and_bindings(body, visitor, statement_visitor);
            }
            HirStmtKind::ForIn { iterable, body, .. } => {
                expression(iterable, visitor, statement_visitor);
                statements_and_bindings(body, visitor, statement_visitor);
            }
            HirStmtKind::While { condition, body } => {
                expression(condition, visitor, statement_visitor);
                statements_and_bindings(body, visitor, statement_visitor);
            }
            HirStmtKind::Break | HirStmtKind::Continue => {}
        }
    }
}

fn expression<'a>(
    expr: &'a HirExpr,
    visitor: &mut impl FnMut(&'a HirExpr),
    statement_visitor: &mut impl FnMut(&'a HirStmt),
) {
    visitor(expr);
    match &expr.kind {
        HirExprKind::Call { args, .. } => {
            for arg in args {
                expression(&arg.value, visitor, statement_visitor);
            }
        }
        HirExprKind::Unary { expr, .. } | HirExprKind::FieldAccess { value: expr, .. } => {
            expression(expr, visitor, statement_visitor)
        }
        HirExprKind::History { expr, offset } => {
            expression(expr, visitor, statement_visitor);
            if let HirHistoryOffset::Dynamic(offset) = offset {
                expression(offset, visitor, statement_visitor);
            }
        }
        HirExprKind::Binary { left, right, .. } => {
            expression(left, visitor, statement_visitor);
            expression(right, visitor, statement_visitor);
        }
        HirExprKind::Ternary {
            condition,
            then_expr,
            else_expr,
        } => {
            expression(condition, visitor, statement_visitor);
            expression(then_expr, visitor, statement_visitor);
            expression(else_expr, visitor, statement_visitor);
        }
        HirExprKind::Switch { selector, arms } => {
            if let Some(selector) = selector {
                expression(selector, visitor, statement_visitor);
            }
            for arm in arms {
                if let Some(condition) = &arm.condition {
                    expression(condition, visitor, statement_visitor);
                }
                expression(&arm.result, visitor, statement_visitor);
            }
        }
        HirExprKind::For {
            from,
            to,
            step,
            statements: body,
            result,
            ..
        } => {
            expression(from, visitor, statement_visitor);
            expression(to, visitor, statement_visitor);
            if let Some(step) = step {
                expression(step, visitor, statement_visitor);
            }
            statements_and_bindings(body, visitor, statement_visitor);
            expression(result, visitor, statement_visitor);
        }
        HirExprKind::ForIn {
            iterable,
            statements: body,
            result,
            ..
        } => {
            expression(iterable, visitor, statement_visitor);
            statements_and_bindings(body, visitor, statement_visitor);
            expression(result, visitor, statement_visitor);
        }
        HirExprKind::While {
            condition,
            statements: body,
            result,
        } => {
            expression(condition, visitor, statement_visitor);
            statements_and_bindings(body, visitor, statement_visitor);
            expression(result, visitor, statement_visitor);
        }
        HirExprKind::Tuple(values)
        | HirExprKind::UserTypeConstruct { fields: values, .. }
        | HirExprKind::UserTypeArrayConstruct {
            elements: values, ..
        } => {
            for value in values {
                expression(value, visitor, statement_visitor);
            }
        }
        HirExprKind::Block {
            statements: body,
            result,
        } => {
            statements_and_bindings(body, visitor, statement_visitor);
            expression(result, visitor, statement_visitor);
        }
        HirExprKind::Literal(_) | HirExprKind::Symbol(_) | HirExprKind::Builtin(_) => {}
    }
}
