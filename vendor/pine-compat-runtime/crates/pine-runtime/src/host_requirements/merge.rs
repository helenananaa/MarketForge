use pine_ir::{HirCallArg, HirExpr, HirExprKind, HirLiteral, HirProgram, Qualifier, SymbolId};
use std::collections::HashMap;

pub(super) fn option(
    program: &HirProgram,
    initializers: &HashMap<SymbolId, &HirExpr>,
    args: &[HirCallArg],
    index: usize,
    name: &str,
    on: &'static str,
    off: &'static str,
) -> &'static str {
    let Some(expr) = crate::builtins::args::call_arg_expr(args, index, name) else {
        return off;
    };
    match constant_string(program, initializers, expr, 0).as_deref() {
        Some("barmerge.gaps_on" | "barmerge.lookahead_on") => on,
        Some("barmerge.gaps_off" | "barmerge.lookahead_off") => off,
        _ => "runtimeExpression",
    }
}

// Inventory must not execute Pine or guess an unresolved option is the default.
fn constant_string(
    program: &HirProgram,
    initializers: &HashMap<SymbolId, &HirExpr>,
    expr: &HirExpr,
    depth: usize,
) -> Option<String> {
    if depth >= 64 {
        return None;
    }
    match &expr.kind {
        HirExprKind::Literal(HirLiteral::String(value)) => Some(value.clone()),
        HirExprKind::Builtin(name) if name.starts_with("barmerge.") => Some(name.clone()),
        HirExprKind::Symbol(id) => {
            let symbol = program.symbols.iter().find(|symbol| symbol.id == *id)?;
            if symbol.pine_type.qualifier != Qualifier::Const {
                return None;
            }
            constant_string(program, initializers, initializers.get(id)?, depth + 1)
        }
        _ => None,
    }
}
