use crate::prelude::*;

pub(crate) fn field_default_type(value: &Expr) -> Option<PineType> {
    use pine_syntax::{Literal, UnaryOp};
    let kind = match &value.without_groups().kind {
        ExprKind::Literal(literal) => match literal {
            Literal::Int(_) => ValueKind::Int,
            Literal::Float(_) => ValueKind::Float,
            Literal::Bool(_) => ValueKind::Bool,
            Literal::String(_) => ValueKind::String,
            Literal::ColorHex(_) => ValueKind::Color,
        },
        ExprKind::Unary {
            op: UnaryOp::Plus | UnaryOp::Minus,
            expr,
        } if matches!(
            expr.kind,
            ExprKind::Literal(Literal::Int(_) | Literal::Float(_))
        ) =>
        {
            return field_default_type(expr);
        }
        ExprKind::Identifier(_) | ExprKind::QualifiedName(_) => {
            let name = crate::analyzer::calls::expr_name(value)?;
            if name == "na" {
                ValueKind::Na
            } else if let Some(symbol) = crate::symbols::initial_symbol(&name) {
                return Some(symbol.pine_type);
            } else if let Some(ty) = pine_builtins::builtin_series_value_type(&name) {
                return Some(ty);
            } else if pine_builtins::named_int_constant(&name).is_some() {
                ValueKind::Int
            } else if pine_builtins::named_float_constant(&name).is_some() {
                ValueKind::Float
            } else if pine_builtins::named_string_constant(&name).is_some() {
                ValueKind::String
            } else if pine_builtins::named_color(&name).is_some() {
                ValueKind::Color
            } else {
                return None;
            }
        }
        _ => return None,
    };
    Some(PineType::new(Qualifier::Const, kind))
}

impl Analyzer {
    pub(crate) fn analyze_udt_default(&mut self, value: &Expr) -> Option<PineType> {
        if let ExprKind::Identifier(name) = &value.kind
            && let Some(symbol) = crate::symbols::initial_symbol(name)
        {
            self.bind_symbol(name, value.span, symbol);
            return Some(symbol.pine_type);
        }
        self.analyze_expr(value)
    }
    pub(crate) fn udt_default_argument(
        &self,
        type_name: &str,
        default: Option<&Expr>,
        span: Span,
    ) -> Expr {
        default.cloned().unwrap_or_else(|| Expr {
            span,
            kind: if type_name == "bool" && self.legacy.dialect() >= crate::PineDialect::V6 {
                ExprKind::Literal(pine_syntax::Literal::Bool(false))
            } else {
                ExprKind::Identifier("na".to_owned())
            },
        })
    }
}
