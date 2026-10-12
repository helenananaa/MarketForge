use pine_syntax::{Expr, ExprKind, Stmt, StmtKind, SwitchArmResult};

// Analysis-only projection. Keep original spans so bindings and library source
// contexts continue to resolve; never lower or execute this synthetic tree.
pub(super) fn project(expr: &Expr, index: usize, depth: u32) -> Option<Expr> {
    if depth > super::MAX_STRING_VALUE_DOMAIN_DEPTH {
        return None;
    }
    let mut result = expr.clone();
    match &mut result.kind {
        ExprKind::Tuple(items) => return items.get(index).cloned(),
        ExprKind::Group(value) => return project(value, index, depth + 1),
        ExprKind::Ternary {
            then_expr,
            else_expr,
            ..
        } => {
            **then_expr = project(then_expr, index, depth + 1)?;
            **else_expr = project(else_expr, index, depth + 1)?;
        }
        ExprKind::If {
            then_branch,
            else_branch,
            ..
        } => {
            project_branch(then_branch, index, depth + 1)?;
            project_branch(else_branch, index, depth + 1)?;
        }
        ExprKind::Switch { arms, .. } => {
            for arm in arms {
                match &mut arm.result {
                    SwitchArmResult::Expr(value) => *value = project(value, index, depth + 1)?,
                    SwitchArmResult::Block(body) => project_branch(body, index, depth + 1)?,
                }
            }
        }
        _ => return None,
    }
    Some(result)
}

fn project_branch(body: &mut [Stmt], index: usize, depth: u32) -> Option<()> {
    let StmtKind::Expr(value) = &mut body.last_mut()?.kind else {
        return None;
    };
    *value = project(value, index, depth)?;
    Some(())
}
