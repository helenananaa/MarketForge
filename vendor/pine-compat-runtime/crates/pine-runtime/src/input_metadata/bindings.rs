//! Discover immutable bindings with the common HIR walker. Any reassignment
//! makes an initializer unavailable; no branch reachability is assumed.
use pine_ir::{HirExpr, HirExprKind, HirProgram, HirStmtKind, SymbolId};
use std::collections::{HashMap, HashSet};

pub(super) fn initializers(program: &HirProgram) -> HashMap<SymbolId, &HirExpr> {
    let mut initializers = HashMap::new();
    let mut reassigned = HashSet::new();
    crate::runtime::hir_walk::statements_and_bindings(
        &program.statements,
        &mut |_| {},
        &mut |statement| match &statement.kind {
            HirStmtKind::Decl { symbol, value } => {
                initializers.insert(*symbol, value);
            }
            HirStmtKind::TupleDecl { symbols, value } => {
                if let HirExprKind::Tuple(values) = &value.kind {
                    initializers.extend(symbols.iter().copied().zip(values));
                }
            }
            HirStmtKind::Reassign { symbol, .. } => {
                reassigned.insert(*symbol);
            }
            _ => {}
        },
    );
    initializers.retain(|symbol, _| !reassigned.contains(symbol));
    initializers
}
