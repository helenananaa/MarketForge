//! Conservative retention specialization for immutable integer host inputs.
use pine_ir::{HirExprKind, HirLiteral, HirProgram, HirStmtKind};

/// Infer history with concrete integer input values while preserving the
/// executable HIR. Inputs must be direct top-level `input.int` declarations;
/// unsupported shapes return None rather than guessing a retention bound.
/// The caller must bind these exact values for the entire runtime lifetime.
pub fn specialize_integer_input_history(
    program: &HirProgram,
    values: &[(u32, i64)],
) -> Option<HirProgram> {
    let mut analysis = program.clone();
    let mut remaining = std::collections::HashMap::new();
    for &(id, value) in values {
        if remaining.insert(id, value).is_some() {
            return None;
        }
    }
    for statement in &mut analysis.statements {
        let HirStmtKind::Decl { value, .. } = &mut statement.kind else {
            continue;
        };
        let HirExprKind::Call {
            callee,
            call_site_id,
            ..
        } = &value.kind
        else {
            continue;
        };
        if callee == "input.int" {
            let literal = remaining.remove(&call_site_id.0)?;
            value.kind = HirExprKind::Literal(HirLiteral::Int(literal));
            value.pine_type.qualifier = pine_ir::Qualifier::Const;
        }
    }
    if !remaining.is_empty() {
        return None;
    }
    let inferred =
        crate::history::infer_history_requirements(&analysis.statements, &analysis.symbols);
    let mut result = program.clone();
    result.history = inferred.program;
    result.series_history = inferred.series;
    Some(result)
}
