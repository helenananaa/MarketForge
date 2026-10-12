use super::*;
mod owned_fields;

pub(super) fn function_body_has_side_effect(body: &FunctionBody, declarations: &[Stmt]) -> bool {
    match body {
        FunctionBody::Expr(expr) => {
            let mut has_side_effect = false;
            visit_expr(expr, &mut |expr| {
                if let ExprKind::Call { callee, .. } = &expr.kind
                    && crate::analyzer::functions::call_has_side_effect(callee)
                    && !is_export_table_call(callee)
                {
                    has_side_effect = true;
                }
            });
            has_side_effect
        }
        FunctionBody::Block(statements) => {
            let mut allowed = crate::analyzer::functions::local_array_mutation_spans(body);
            allowed.extend(owned_fields::local_field_mutation_spans(body, declarations));
            let mut has_side_effect = false;
            for statement in statements {
                visit_statement_exprs(statement, &mut |expr| {
                    if let ExprKind::Call { callee, .. } = &expr.kind
                        && crate::analyzer::functions::call_has_side_effect(callee)
                        && !allowed.contains(&callee.span)
                        && !is_export_table_call(callee)
                    {
                        has_side_effect = true;
                    }
                });
            }
            has_side_effect
        }
    }
}

fn is_export_table_call(callee: &Expr) -> bool {
    crate::analyzer::calls::expr_name(callee).is_some_and(|name| name.starts_with("table."))
}

pub(super) fn first_statement_span(program: &Program) -> Option<Span> {
    program.statements.first().map(|statement| statement.span)
}

pub(super) fn visit_statement_exprs(statement: &Stmt, visitor: &mut impl FnMut(&Expr)) {
    match &statement.kind {
        StmtKind::Expr(expr)
        | StmtKind::Decl { value: expr, .. }
        | StmtKind::Reassign { value: expr, .. }
        | StmtKind::FieldReassign { value: expr, .. }
        | StmtKind::TupleDecl { value: expr, .. } => visit_expr(expr, visitor),
        StmtKind::ArrayFieldReassign {
            array,
            index,
            value,
            ..
        } => {
            visit_expr(array, visitor);
            visit_expr(index, visitor);
            visit_expr(value, visitor);
        }
        StmtKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            visit_expr(condition, visitor);
            for statement in then_branch.iter().chain(else_branch) {
                visit_statement_exprs(statement, visitor);
            }
        }
        StmtKind::For {
            from,
            to,
            step,
            body,
            ..
        } => {
            visit_expr(from, visitor);
            visit_expr(to, visitor);
            if let Some(step) = step {
                visit_expr(step, visitor);
            }
            for statement in body {
                visit_statement_exprs(statement, visitor);
            }
        }
        StmtKind::While { condition, body } => {
            visit_expr(condition, visitor);
            for statement in body {
                visit_statement_exprs(statement, visitor);
            }
        }
        StmtKind::ForIn { iterable, body, .. } => {
            visit_expr(iterable, visitor);
            for statement in body {
                visit_statement_exprs(statement, visitor);
            }
        }
        StmtKind::Export(export) => match &export.item {
            ExportItem::Const { value, .. } => visit_expr(value, visitor),
            ExportItem::Function { body, .. } => visit_function_body(body, visitor),
            ExportItem::UserType { .. } => {}
            ExportItem::Unknown { .. } => {}
        },
        StmtKind::Method(method) => visit_function_body(&method.body, visitor),
        StmtKind::Import(_)
        | StmtKind::Library(_)
        | StmtKind::UserType(_)
        | StmtKind::Break
        | StmtKind::Continue
        | StmtKind::Function { .. }
        | StmtKind::Unsupported { .. } => {}
    }
}

fn visit_function_body(body: &FunctionBody, visitor: &mut impl FnMut(&Expr)) {
    match body {
        FunctionBody::Expr(expr) => visit_expr(expr, visitor),
        FunctionBody::Block(statements) => {
            for statement in statements {
                visit_statement_exprs(statement, visitor);
            }
        }
    }
}

pub(super) fn visit_expr(expr: &Expr, visitor: &mut impl FnMut(&Expr)) {
    visitor(expr);
    match &expr.kind {
        ExprKind::Unary { expr, .. }
        | ExprKind::History { expr, .. }
        | ExprKind::Group(expr)
        | ExprKind::Member { receiver: expr, .. } => visit_expr(expr, visitor),
        ExprKind::Binary { left, right, .. } => {
            visit_expr(left, visitor);
            visit_expr(right, visitor);
        }
        ExprKind::Ternary {
            condition,
            then_expr,
            else_expr,
        } => {
            visit_expr(condition, visitor);
            visit_expr(then_expr, visitor);
            visit_expr(else_expr, visitor);
        }
        ExprKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            visit_expr(condition, visitor);
            for statement in then_branch.iter().chain(else_branch) {
                visit_statement_exprs(statement, visitor);
            }
        }
        ExprKind::For {
            from,
            to,
            step,
            body,
            ..
        } => {
            visit_expr(from, visitor);
            visit_expr(to, visitor);
            if let Some(step) = step {
                visit_expr(step, visitor);
            }
            for statement in body {
                visit_statement_exprs(statement, visitor);
            }
        }
        ExprKind::ForIn { iterable, body, .. } => {
            visit_expr(iterable, visitor);
            for statement in body {
                visit_statement_exprs(statement, visitor);
            }
        }
        ExprKind::While { condition, body } => {
            visit_expr(condition, visitor);
            for statement in body {
                visit_statement_exprs(statement, visitor);
            }
        }
        ExprKind::Switch { selector, arms } => {
            if let Some(selector) = selector {
                visit_expr(selector, visitor);
            }
            for arm in arms {
                if let Some(condition) = &arm.condition {
                    visit_expr(condition, visitor);
                }
                match &arm.result {
                    SwitchArmResult::Expr(result) => visit_expr(result, visitor),
                    SwitchArmResult::Block(statements) => {
                        for statement in statements {
                            visit_statement_exprs(statement, visitor);
                        }
                    }
                }
            }
        }
        ExprKind::Tuple(items) => {
            for item in items {
                visit_expr(item, visitor);
            }
        }
        ExprKind::Call { callee, args } => {
            visit_expr(callee, visitor);
            for arg in args {
                visit_expr(&arg.value, visitor);
            }
        }
        ExprKind::Literal(_) | ExprKind::Identifier(_) | ExprKind::QualifiedName(_) => {}
    }
}
