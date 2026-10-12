use std::borrow::Cow;

use crate::prelude::*;
use crate::symbols::initial_symbol;

use super::resolve_udf_arg_indices_with_defaults;

impl FunctionInfo {
    /// Append omitted defaults as named arguments. Explicit arguments keep
    /// their source order, so their evaluation order is unchanged.
    pub(crate) fn complete_args<'a>(
        &self,
        args: &'a [CallArg],
        call_span: Span,
    ) -> Result<Cow<'a, [CallArg]>, UdfArgError> {
        if args.len() >= self.params.len() || self.default_values.iter().all(Option::is_none) {
            return Ok(Cow::Borrowed(args));
        }
        let indices =
            resolve_udf_arg_indices_with_defaults(&self.params, args, &self.default_values)?;
        let mut used = vec![false; self.params.len()];
        for index in indices {
            used[index] = true;
        }
        let mut completed = args.to_vec();
        for (index, default) in self.default_values.iter().enumerate() {
            if used[index] {
                continue;
            }
            if let Some(value) = default {
                let mut value = value.clone();
                // Missing source tokens have a zero-width caller location.
                // Bind a builtin-name default in that caller's lexical scope,
                // independently of the same default used at another callsite.
                value.span = Span {
                    start: call_span.start,
                    end: call_span.start,
                };
                completed.push(CallArg {
                    name: Some(self.params[index].clone()),
                    span: value.span,
                    value,
                });
            }
        }
        Ok(Cow::Owned(completed))
    }
}

pub(crate) fn function_default_values(
    params: &[FunctionParam],
    version: u16,
    shadowed: &std::collections::HashSet<String>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<Option<Expr>> {
    params.iter().map(|param| {
        let value = param.default_value.as_ref()?;
        let Some((normalized, value_type)) = normalize_default(value, shadowed) else {
            diagnostics.push(Diagnostic::error(
                "E_FUNCTION_DEFAULT",
                "function defaults must be scalar literals, signed numeric literals, or supported built-in variables; calculations, user variables and calls are not supported",
                value.span,
            ));
            return None;
        };
        let explicit_simple = param.type_name.as_deref().is_some_and(|name| name.starts_with("simple "));
        let type_name = param.type_name.as_deref().map(|name| name.strip_prefix("series ").or_else(|| name.strip_prefix("simple ")).unwrap_or(name));
        let declared_kind = match type_name {
            Some("int") => Some(ValueKind::Int),
            Some("float") => Some(ValueKind::Float),
            Some("bool") => Some(ValueKind::Bool),
            Some("string") => Some(ValueKind::String),
            Some("color") => Some(ValueKind::Color),
            None => None,
            _ => {
                diagnostics.push(Diagnostic::error("E_FUNCTION_DEFAULT_TYPE", "default values for reference-type parameters are not supported", param.span));
                return None;
            }
        };
        if value_type.kind == ValueKind::Na && (declared_kind.is_none() || (version >= 6 && declared_kind == Some(ValueKind::Bool))) {
            diagnostics.push(Diagnostic::error("E_FUNCTION_DEFAULT_TYPE", "na defaults require an explicit scalar type; v6 bool defaults cannot be na", param.span));
            return None;
        }
        let declared_qualifier = if explicit_simple {
            Qualifier::Simple
        } else {
            Qualifier::Series
        };
        if let Some(kind) = declared_kind
            && !can_assign(PineType::new(declared_qualifier, kind), value_type) {
                diagnostics.push(Diagnostic::error("E_FUNCTION_DEFAULT_TYPE", format!("default value cannot be assigned to parameter `{}` of type `{}`", param.name, type_name.unwrap()), value.span));
                return None;
        }
        Some(normalized)
    }).collect()
}

fn normalize_default(
    value: &Expr,
    shadowed: &std::collections::HashSet<String>,
) -> Option<(Expr, PineType)> {
    let kind = match &value.kind {
        ExprKind::Literal(literal) => {
            return Some((value.clone(), literal_type(literal)));
        }
        ExprKind::Unary {
            op: UnaryOp::Plus | UnaryOp::Minus,
            expr,
        } => {
            let negative = matches!(
                value.kind,
                ExprKind::Unary {
                    op: UnaryOp::Minus,
                    ..
                }
            );
            let literal = match &expr.kind {
                ExprKind::Literal(Literal::Int(n)) => {
                    Literal::Int(if negative { n.checked_neg()? } else { *n })
                }
                ExprKind::Literal(Literal::Float(n)) if n.is_finite() => {
                    Literal::Float(if negative { -*n } else { *n })
                }
                _ => return None,
            };
            let pine_type = literal_type(&literal);
            return Some((
                Expr {
                    kind: ExprKind::Literal(literal),
                    span: value.span,
                },
                pine_type,
            ));
        }
        ExprKind::Identifier(name) => {
            if shadowed.contains(name) {
                return None;
            }
            let symbol = initial_symbol(name)?;
            return Some((value.clone(), symbol.pine_type));
        }
        ExprKind::QualifiedName(parts) => {
            if parts.first().is_some_and(|name| shadowed.contains(name)) {
                return None;
            }
            let name = parts.join(".");
            if let Some(pine_type) = pine_builtins::builtin_series_value_type(&name)
                && pine_type.qualifier != Qualifier::Const
                && (pine_builtins::named_float_constant(&name).is_some()
                    || pine_builtins::named_int_constant(&name).is_some())
            {
                return Some((value.clone(), pine_type));
            }
            if let Some(value) = pine_builtins::named_color(&name) {
                Literal::ColorHex(format!("#{value:06x}"))
            } else if let Some(value) = pine_builtins::named_int_constant(&name) {
                Literal::Int(value)
            } else if let Some(value) = pine_builtins::named_float_constant(&name) {
                Literal::Float(value)
            } else {
                Literal::String(pine_builtins::named_string_constant(&name)?.to_owned())
            }
        }
        _ => return None,
    };
    let pine_type = literal_type(&kind);
    Some((
        Expr {
            kind: ExprKind::Literal(kind),
            span: value.span,
        },
        pine_type,
    ))
}

pub(crate) fn record_default_shadowing(
    statement: &Stmt,
    names: &mut std::collections::HashSet<String>,
) {
    match &statement.kind {
        StmtKind::Decl { name, .. }
        | StmtKind::Reassign { name, .. }
        | StmtKind::Function { name, .. } => {
            names.insert(name.clone());
        }
        StmtKind::TupleDecl {
            names: bindings, ..
        } => {
            names.extend(bindings.iter().cloned());
        }
        StmtKind::Import(import) => {
            if let Some(alias) = &import.alias {
                names.insert(alias.name.clone());
            }
        }
        StmtKind::UserType(decl) => {
            names.insert(decl.name.clone());
        }
        StmtKind::Export(export) => match &export.item {
            pine_syntax::ExportItem::Const { name, .. }
            | pine_syntax::ExportItem::Function { name, .. } => {
                names.insert(name.clone());
            }
            pine_syntax::ExportItem::UserType { decl, .. } => {
                names.insert(decl.name.clone());
            }
            _ => {}
        },
        _ => {}
    }
}
