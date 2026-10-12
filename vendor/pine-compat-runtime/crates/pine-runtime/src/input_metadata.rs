use pine_ir::{HirCallArg, HirExpr, HirExprKind, HirProgram, Qualifier, ValueKind};

use crate::PineValue;

mod bindings;
mod constant;

#[derive(Debug, Clone, PartialEq)]
pub struct InputCall {
    pub call_site_id: u32,
    pub name: String,
    /// The analyzer-resolved value kind returned by this input call.
    ///
    /// This is especially important for the generic legacy `input()` form:
    /// hosts must parse overrides according to the resolved `defval` type
    /// instead of guessing from a textual override.
    pub value_kind: ValueKind,
    /// Whether this call selects a chart source, including generic `input(close)`.
    pub is_source: bool,
    pub title: Option<String>,
    pub default_value: Option<PineValue>,
    pub min_value: Option<PineValue>,
    pub max_value: Option<PineValue>,
    pub step: Option<PineValue>,
    pub options: Vec<PineValue>,
}

#[must_use]
pub fn input_calls(program: &HirProgram) -> Vec<InputCall> {
    let minimal = constant::minimal_program(program.language_version);
    let mut evaluator = constant::ConstEvaluator::new(program, &minimal);
    let mut calls = Vec::new();
    crate::runtime::hir_walk::statements(&program.statements, &mut |expr| {
        let HirExprKind::Call {
            callee,
            call_site_id,
            args,
        } = &expr.kind
        else {
            return;
        };
        if !is_input_call(callee) {
            return;
        }
        let is_source = callee == "input.source"
            || (callee == "input"
                && expr.pine_type.kind == ValueKind::Float
                && expr.pine_type.qualifier == Qualifier::Series);
        let numeric = matches!(
            callee.as_str(),
            "input.int" | "input.float" | "input.price" | "input.time"
        );
        calls.push(InputCall {
            call_site_id: call_site_id.0,
            name: callee.clone(),
            value_kind: expr.pine_type.kind,
            is_source,
            title: input_arg(args, 1, "title")
                .and_then(|expr| evaluator.value(expr))
                .and_then(|value| {
                    if let PineValue::String(value) = value {
                        Some(value)
                    } else {
                        None
                    }
                }),
            default_value: input_arg(args, 0, "defval").and_then(|expr| {
                if is_source && let HirExprKind::Builtin(name) = &expr.kind {
                    return Some(PineValue::String(name.clone()));
                }
                evaluator.metadata_value(expr)
            }),
            min_value: numeric
                .then(|| input_arg_value(args, 2, "minval", &mut evaluator))
                .flatten(),
            max_value: numeric
                .then(|| input_arg_value(args, 3, "maxval", &mut evaluator))
                .flatten(),
            step: numeric
                .then(|| input_arg_value(args, 4, "step", &mut evaluator))
                .flatten(),
            options: input_options(callee, args, &mut evaluator),
        });
    });
    calls
}

fn is_input_call(name: &str) -> bool {
    name == "input" || name.starts_with("input.")
}

fn input_arg<'a>(args: &'a [HirCallArg], index: usize, name: &str) -> Option<&'a HirExpr> {
    args.iter()
        .find(|arg| arg.name.as_deref() == Some(name))
        .or_else(|| args.get(index).filter(|arg| arg.name.is_none()))
        .map(|arg| &arg.value)
}

fn input_arg_value(
    args: &[HirCallArg],
    index: usize,
    name: &str,
    evaluator: &mut constant::ConstEvaluator<'_>,
) -> Option<PineValue> {
    input_arg(args, index, name).and_then(|expr| evaluator.metadata_value(expr))
}

fn input_options(
    name: &str,
    args: &[HirCallArg],
    evaluator: &mut constant::ConstEvaluator<'_>,
) -> Vec<PineValue> {
    let index = if matches!(
        name,
        "input"
            | "input.int"
            | "input.float"
            | "input.price"
            | "input.time"
            | "input.string"
            | "input.symbol"
            | "input.timeframe"
            | "input.session"
    ) {
        if matches!(
            name,
            "input.int" | "input.float" | "input.price" | "input.time"
        ) {
            5
        } else {
            2
        }
    } else {
        return Vec::new();
    };
    let Some(expr) = input_arg(args, index, "options") else {
        return Vec::new();
    };
    match &expr.kind {
        HirExprKind::Tuple(values) => values
            .iter()
            .map(|value| evaluator.metadata_value(value))
            .collect::<Option<Vec<_>>>()
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}
