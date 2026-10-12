use pine_ir::{HirCallArg, HirExpr};

use crate::runtime::call_context::RuntimeCallContext;
use crate::*;

#[derive(Clone, Copy)]
pub(crate) struct RuntimeArgs<'args> {
    raw: &'args [HirCallArg],
    positional: bool,
}

impl<'args> RuntimeArgs<'args> {
    pub(crate) fn new(raw: &'args [HirCallArg], positional: bool) -> Self {
        Self { raw, positional }
    }

    pub(crate) fn raw(self) -> &'args [HirCallArg] {
        self.raw
    }

    pub(crate) fn exprs(self) -> impl Iterator<Item = &'args HirExpr> {
        self.raw.iter().map(|arg| &arg.value)
    }

    pub(crate) fn value(
        self,
        context: &mut RuntimeCallContext<'_, '_>,
        index: usize,
        name: &str,
    ) -> Result<PineValue, RuntimeError> {
        match self.expr(index, name) {
            Some(expr) => context.eval_expr(expr),
            None => Ok(PineValue::Na),
        }
    }

    pub(crate) fn optional_value(
        self,
        context: &mut RuntimeCallContext<'_, '_>,
        index: usize,
        name: &str,
    ) -> Result<Option<PineValue>, RuntimeError> {
        self.expr(index, name)
            .map(|expr| context.eval_expr(expr))
            .transpose()
    }

    pub(crate) fn expr(self, index: usize, name: &str) -> Option<&'args HirExpr> {
        if self.positional {
            positional_arg(self.raw, index).map(|arg| &arg.value)
        } else {
            call_arg_expr(self.raw, index, name)
        }
    }
}

pub(crate) fn output_id(value: PineValue) -> Option<u32> {
    match value {
        PineValue::Plot(id) | PineValue::HLine(id) => Some(id),
        _ => None,
    }
}

pub(crate) fn call_arg_expr<'a>(
    args: &'a [HirCallArg],
    index: usize,
    name: &str,
) -> Option<&'a HirExpr> {
    args.iter()
        .find(|arg| arg.name.as_deref() == Some(name))
        .or_else(|| positional_arg(args, index).filter(|arg| arg.name.is_none()))
        .map(|arg| &arg.value)
}

pub(crate) fn positional_arg(args: &[HirCallArg], index: usize) -> Option<&HirCallArg> {
    args.get(index)
        .filter(|arg| arg.name.as_deref() != Some(pine_ir::OMITTED_BUILTIN_ARG))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argument(name: Option<&str>, value: i64) -> HirCallArg {
        HirCallArg {
            name: name.map(str::to_owned),
            value: HirExpr {
                kind: pine_ir::HirExprKind::Literal(pine_ir::HirLiteral::Int(value)),
                pine_type: pine_ir::PineType::new(
                    pine_ir::Qualifier::Const,
                    pine_ir::ValueKind::Int,
                ),
                series_id: None,
            },
        }
    }

    #[test]
    fn named_layout_keeps_first_named_override_and_absent_optional_arguments() {
        let raw = [
            argument(None, 1),
            argument(Some("base"), 3),
            argument(Some("base"), 5),
        ];
        let args = RuntimeArgs::new(&raw, false);
        assert!(std::ptr::eq(args.expr(0, "base").unwrap(), &raw[1].value));
        assert!(args.expr(1, "exponent").is_none());
        assert!(args.expr(usize::MAX, "missing").is_none());
    }

    #[test]
    fn prepared_positions_do_not_bind_or_evaluate_omitted_slots() {
        let raw = [
            argument(Some(pine_ir::OMITTED_BUILTIN_ARG), 99),
            argument(None, 2),
        ];
        let args = RuntimeArgs::new(&raw, true);
        assert!(args.expr(0, "base").is_none());
        assert!(std::ptr::eq(
            args.expr(1, "exponent").unwrap(),
            &raw[1].value
        ));
        assert!(args.expr(2, "precision").is_none());
    }
}
