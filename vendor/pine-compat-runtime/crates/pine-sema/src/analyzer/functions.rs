use std::collections::HashSet;

use crate::analyzer::user_types::{
    UserTypeArrayElementInference, classify_user_type_array_element_names,
};
use crate::prelude::*;
use crate::source_graph::{SourceContextId, SourceId};

mod defaults;
mod local_arrays;
mod overloads;
pub(crate) use defaults::{function_default_values, record_default_shadowing};
pub(crate) use local_arrays::local_array_mutation_spans;

pub(crate) fn function_param_names(params: &[FunctionParam]) -> Vec<String> {
    params.iter().map(|param| param.name.clone()).collect()
}

pub(crate) fn resolve_udf_arg_indices(
    params: &[String],
    args: &[CallArg],
) -> Result<Vec<usize>, UdfArgError> {
    resolve_udf_arg_indices_with_defaults(params, args, &[])
}

fn resolve_udf_arg_indices_with_defaults(
    params: &[String],
    args: &[CallArg],
    defaults: &[Option<Expr>],
) -> Result<Vec<usize>, UdfArgError> {
    let mut used = vec![false; params.len()];
    let mut indices = Vec::with_capacity(args.len());
    let mut next_positional = 0;
    let mut saw_named = false;

    for arg in args {
        if let Some(name) = &arg.name {
            saw_named = true;
            let Some(param_index) = params.iter().position(|param| param == name) else {
                return Err(UdfArgError::UnknownName {
                    name: name.clone(),
                    span: arg.span,
                });
            };
            if used[param_index] {
                return Err(UdfArgError::Duplicate {
                    name: name.clone(),
                    span: arg.span,
                });
            }
            used[param_index] = true;
            indices.push(param_index);
        } else {
            if saw_named {
                return Err(UdfArgError::PositionalAfterNamed { span: arg.span });
            }
            while next_positional < used.len() && used[next_positional] {
                next_positional += 1;
            }
            if next_positional >= params.len() {
                return Err(UdfArgError::TooMany { span: arg.span });
            }
            used[next_positional] = true;
            indices.push(next_positional);
            next_positional += 1;
        }
    }

    if let Some(missing_index) = used
        .iter()
        .enumerate()
        .position(|(index, used)| !*used && defaults.get(index).is_none_or(Option::is_none))
    {
        return Err(UdfArgError::Missing {
            param: params[missing_index].clone(),
        });
    }

    Ok(indices)
}
pub(crate) fn contains_output_or_declaration_call(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Call { callee, args } => {
            call_has_side_effect(callee)
                || contains_output_or_declaration_call(callee)
                || args
                    .iter()
                    .any(|arg| contains_output_or_declaration_call(&arg.value))
        }
        ExprKind::Unary { expr, .. }
        | ExprKind::History { expr, .. }
        | ExprKind::Group(expr)
        | ExprKind::Member { receiver: expr, .. } => contains_output_or_declaration_call(expr),
        ExprKind::Binary { left, right, .. } => {
            contains_output_or_declaration_call(left) || contains_output_or_declaration_call(right)
        }
        ExprKind::Ternary {
            condition,
            then_expr,
            else_expr,
        } => {
            contains_output_or_declaration_call(condition)
                || contains_output_or_declaration_call(then_expr)
                || contains_output_or_declaration_call(else_expr)
        }
        ExprKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            contains_output_or_declaration_call(condition)
                || then_branch
                    .iter()
                    .any(statement_contains_output_or_declaration_call)
                || else_branch
                    .iter()
                    .any(statement_contains_output_or_declaration_call)
        }
        ExprKind::Switch { selector, arms } => {
            selector
                .as_deref()
                .is_some_and(contains_output_or_declaration_call)
                || arms.iter().any(|arm| {
                    arm.condition
                        .as_ref()
                        .is_some_and(contains_output_or_declaration_call)
                        || switch_arm_result_contains_output_or_declaration_call(&arm.result)
                })
        }
        ExprKind::For {
            from,
            to,
            step,
            body,
            ..
        } => {
            contains_output_or_declaration_call(from)
                || contains_output_or_declaration_call(to)
                || step
                    .as_deref()
                    .is_some_and(contains_output_or_declaration_call)
                || body
                    .iter()
                    .any(statement_contains_output_or_declaration_call)
        }
        ExprKind::While { condition, body } => {
            contains_output_or_declaration_call(condition)
                || body
                    .iter()
                    .any(statement_contains_output_or_declaration_call)
        }
        ExprKind::ForIn { iterable, body, .. } => {
            contains_output_or_declaration_call(iterable)
                || body
                    .iter()
                    .any(statement_contains_output_or_declaration_call)
        }
        ExprKind::Tuple(items) => items.iter().any(contains_output_or_declaration_call),
        ExprKind::Literal(_) | ExprKind::Identifier(_) | ExprKind::QualifiedName(_) => false,
    }
}

pub(crate) fn call_has_side_effect(callee: &Expr) -> bool {
    matches!(&callee.kind, ExprKind::Member { name, .. } if member_method_may_have_side_effect(name))
        || expr_name(callee).as_deref().is_some_and(|name| {
            is_output_or_declaration_builtin(name)
                || is_array_mutation_builtin(name)
                || is_array_mutation_method_call_name(name)
                || is_map_mutation_builtin(name)
                || is_map_mutation_method_call_name(name)
        })
}

fn member_method_may_have_side_effect(method: &str) -> bool {
    pine_builtins::PHASE_1_BUILTINS.iter().any(|signature| {
        signature
            .name
            .rsplit_once('.')
            .is_some_and(|(_, name)| name == method)
            && (is_output_or_declaration_builtin(signature.name)
                || is_array_mutation_builtin(signature.name)
                || is_map_mutation_builtin(signature.name))
    })
}

fn switch_arm_result_contains_output_or_declaration_call(result: &SwitchArmResult) -> bool {
    match result {
        SwitchArmResult::Expr(expr) => contains_output_or_declaration_call(expr),
        SwitchArmResult::Block(statements) => statements
            .iter()
            .any(statement_contains_output_or_declaration_call),
    }
}

fn function_branch_has_return(branch: &[Stmt]) -> bool {
    branch.last().is_some_and(function_statement_has_return)
}

fn function_statement_has_return(statement: &Stmt) -> bool {
    match &statement.kind {
        StmtKind::Expr(_)
        | StmtKind::Decl { .. }
        | StmtKind::TupleDecl { .. }
        | StmtKind::Reassign { .. }
        | StmtKind::FieldReassign { .. }
        | StmtKind::For { .. }
        | StmtKind::ForIn { .. }
        | StmtKind::While { .. } => true,
        StmtKind::If {
            then_branch,
            else_branch,
            ..
        } => {
            function_branch_has_return(then_branch)
                && (else_branch.is_empty() || function_branch_has_return(else_branch))
        }
        _ => false,
    }
}

pub(crate) fn field_reassign_result_expr(
    receiver: &str,
    path: &[String],
    field: &str,
    span: Span,
) -> Expr {
    let mut result = Expr {
        kind: ExprKind::Identifier(receiver.to_owned()),
        span,
    };
    for name in path
        .iter()
        .map(String::as_str)
        .chain(std::iter::once(field))
    {
        result = Expr {
            kind: ExprKind::Member {
                receiver: Box::new(result),
                name: name.to_owned(),
            },
            span,
        };
    }
    result
}

pub(crate) fn statement_contains_output_or_declaration_call(statement: &Stmt) -> bool {
    match &statement.kind {
        StmtKind::Expr(expr) => contains_output_or_declaration_call(expr),
        StmtKind::Decl { value, .. }
        | StmtKind::Reassign { value, .. }
        | StmtKind::FieldReassign { value, .. }
        | StmtKind::TupleDecl { value, .. } => contains_output_or_declaration_call(value),
        StmtKind::ArrayFieldReassign {
            array,
            index,
            value,
            ..
        } => {
            contains_output_or_declaration_call(array)
                || contains_output_or_declaration_call(index)
                || contains_output_or_declaration_call(value)
        }
        StmtKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            contains_output_or_declaration_call(condition)
                || then_branch
                    .iter()
                    .any(statement_contains_output_or_declaration_call)
                || else_branch
                    .iter()
                    .any(statement_contains_output_or_declaration_call)
        }
        StmtKind::For {
            from,
            to,
            step,
            body,
            ..
        } => {
            contains_output_or_declaration_call(from)
                || contains_output_or_declaration_call(to)
                || step
                    .as_ref()
                    .is_some_and(contains_output_or_declaration_call)
                || body
                    .iter()
                    .any(statement_contains_output_or_declaration_call)
        }
        StmtKind::ForIn { iterable, body, .. } => {
            contains_output_or_declaration_call(iterable)
                || body
                    .iter()
                    .any(statement_contains_output_or_declaration_call)
        }
        StmtKind::While { condition, body } => {
            contains_output_or_declaration_call(condition)
                || body
                    .iter()
                    .any(statement_contains_output_or_declaration_call)
        }
        StmtKind::Break | StmtKind::Continue | StmtKind::Function { .. } => false,
        StmtKind::Import(_)
        | StmtKind::Library(_)
        | StmtKind::Export(_)
        | StmtKind::UserType(_)
        | StmtKind::Method(_) => false,
        StmtKind::Unsupported { .. } => false,
    }
}
pub(crate) fn has_duplicate_param(params: &[String]) -> bool {
    for (index, param) in params.iter().enumerate() {
        if params[index + 1..].iter().any(|other| other == param) {
            return true;
        }
    }
    false
}

impl Analyzer {
    pub(crate) fn function_param_type(
        &mut self,
        type_name: &str,
        span: Span,
    ) -> Option<FunctionParamInfo> {
        let explicit_series = type_name.starts_with("series ");
        let explicit_simple = type_name.starts_with("simple ");
        let type_name = type_name
            .strip_prefix("series ")
            .or_else(|| type_name.strip_prefix("simple "))
            .unwrap_or(type_name);
        if explicit_simple && !matches!(type_name, "int" | "float" | "bool" | "string" | "color") {
            self.diagnostics.push(Diagnostic::error(
                "E_FUNCTION_PARAM_TYPE",
                format!("function parameter type `{type_name}` is not supported"),
                span,
            ));
            return None;
        }
        let qualifier = if explicit_simple {
            Qualifier::Simple
        } else {
            Qualifier::Series
        };
        let (pine_type, user_type_name) = match type_name {
            _ if type_name.starts_with("array<") && type_name.ends_with('>') => {
                let element_type = &type_name["array<".len()..type_name.len() - 1];
                if let Some(kind) = array_kind_from_element_type_name(element_type) {
                    (PineType::new(Qualifier::Series, kind), None)
                } else if matches!(
                    classify_user_type_array_element_names(
                        &self.user_types,
                        &[element_type.to_owned()]
                    ),
                    Some(UserTypeArrayElementInference::SameLocal(_))
                ) || self
                    .imported_user_types
                    .get(element_type)
                    .is_some_and(|_| self.imported_user_type_array_is_supported(element_type))
                {
                    (
                        PineType::new(Qualifier::Series, ValueKind::UserTypeArray),
                        Some(element_type.to_owned()),
                    )
                } else {
                    self.diagnostics.push(Diagnostic::error(
                        "E_FUNCTION_PARAM_TYPE",
                        format!("function parameter type `{type_name}` is not supported"),
                        span,
                    ));
                    return None;
                }
            }
            "int" => (PineType::new(qualifier, ValueKind::Int), None),
            "float" => (PineType::new(qualifier, ValueKind::Float), None),
            "bool" => (PineType::new(qualifier, ValueKind::Bool), None),
            "string" => (PineType::new(qualifier, ValueKind::String), None),
            "color" => (PineType::new(qualifier, ValueKind::Color), None),
            "label" => (PineType::new(Qualifier::Series, ValueKind::Label), None),
            "line" => (PineType::new(Qualifier::Series, ValueKind::Line), None),
            "linefill" => (PineType::new(Qualifier::Series, ValueKind::LineFill), None),
            "polyline" => (PineType::new(Qualifier::Series, ValueKind::Polyline), None),
            "box" => (PineType::new(Qualifier::Series, ValueKind::Box), None),
            "table" => (PineType::new(Qualifier::Series, ValueKind::Table), None),
            "chart.point" => (
                PineType::new(Qualifier::Series, ValueKind::ChartPoint),
                None,
            ),
            _ if self.user_types.contains_key(type_name) => (
                PineType::new(Qualifier::Series, ValueKind::UserType),
                Some(type_name.to_owned()),
            ),
            _ if self.imported_user_types.contains_key(type_name) => (
                PineType::new(Qualifier::Series, ValueKind::UserType),
                Some(type_name.to_owned()),
            ),
            _ => {
                self.diagnostics.push(Diagnostic::error(
                    "E_FUNCTION_PARAM_TYPE",
                    format!("function parameter type `{type_name}` is not supported"),
                    span,
                ));
                return None;
            }
        };
        Some(FunctionParamInfo {
            pine_type,
            explicit_series,
            explicit_simple,
            user_type_name,
            span,
        })
    }

    pub(crate) fn register_functions(&mut self, program: &Program) {
        let mut default_shadowed_names = HashSet::new();
        for statement in &program.statements {
            let StmtKind::Function { name, params, body } = &statement.kind else {
                record_default_shadowing(statement, &mut default_shadowed_names);
                continue;
            };
            if self.functions.contains_key(name) {
                self.diagnostics.push(Diagnostic::error(
                    "E_FUNCTION_DUPLICATE",
                    format!("function `{name}` is already defined"),
                    statement.span,
                ));
                continue;
            }
            if pine_builtins::is_phase_1_builtin(name)
                || INITIAL_SYMBOLS
                    .iter()
                    .any(|(symbol_name, _)| symbol_name == name)
            {
                self.diagnostics.push(Diagnostic::error(
                    "E_FUNCTION_NAME",
                    format!("function `{name}` conflicts with an existing symbol"),
                    statement.span,
                ));
                continue;
            }
            let param_names = function_param_names(params);
            if has_duplicate_param(&param_names) {
                self.diagnostics.push(Diagnostic::error(
                    "E_FUNCTION_PARAM",
                    format!("function `{name}` has duplicate parameter names"),
                    statement.span,
                ));
                continue;
            }
            let mut param_types = Vec::with_capacity(params.len());
            let mut valid = true;
            for param in params {
                let Some(type_name) = &param.type_name else {
                    param_types.push(None);
                    continue;
                };
                let Some(param_type) = self.function_param_type(type_name, param.span) else {
                    valid = false;
                    param_types.push(None);
                    continue;
                };
                param_types.push(Some(param_type));
            }
            if !valid {
                continue;
            }
            let default_values = function_default_values(
                params,
                program.version.map_or(1, |version| version.version),
                &default_shadowed_names,
                &mut self.diagnostics,
            );
            self.functions.insert(
                name.clone(),
                FunctionInfo {
                    overloads: Vec::new(),
                    display_name: name.clone(),
                    source_id: SourceId::root(),
                    source_context_id: SourceContextId::root(),
                    params: param_names,
                    param_types,
                    default_values,
                    body: body.clone(),
                    span: statement.span,
                },
            );
        }
    }

    pub(crate) fn analyze_udf_call(
        &mut self,
        name: &str,
        span: Span,
        call_span: Span,
        args: &[CallArg],
        arg_types: &[Option<PineType>],
    ) -> Option<PineType> {
        let group = self.functions.get(name)?;
        let is_overloaded = !group.overloads.is_empty();
        let function = match group.select_overload(args, arg_types) {
            Some(function) => function.clone(),
            None => {
                self.diagnostics.push(Diagnostic::error(
                    "E_FUNCTION_ARG_TYPE",
                    format!(
                        "no supported overload of `{}` matches these arguments",
                        group.display_name
                    ),
                    span,
                ));
                return None;
            }
        };
        if self.function_stack.iter().any(|active| active == name) {
            self.diagnostics.push(Diagnostic::error(
                "E_RECURSIVE_FUNCTION",
                format!(
                    "recursive function `{}` is not supported",
                    function.display_name
                ),
                span,
            ));
            return None;
        }
        if self.function_stack.len() >= MAX_FUNCTION_CALL_DEPTH {
            self.diagnostics.push(Diagnostic::error(
                "E_FUNCTION_CALL_DEPTH",
                "user-defined function call chain is too deep",
                span,
            ));
            return None;
        }
        for arg in args {
            // A direct input at global callsite is evaluated in the caller
            // before inlining, just like an input assigned to a global variable.
            // Keep effects inside its arguments and inside function bodies gated.
            let direct_global_input = self.legacy.dialect().version() >= 5
                && self.function_depth == 0
                && self.block_depth == 0
                && matches!(&arg.value.kind, ExprKind::Call { callee, args }
                    if expr_name(callee).is_some_and(|name| name.starts_with("input."))
                        && args.iter().all(|arg| !self.argument_has_side_effect(&arg.value)));
            if !direct_global_input && self.argument_has_disallowed_udf_side_effect(&arg.value) {
                self.unsupported(
                    "function_side_effect",
                    "side-effecting calls cannot be passed as user-defined function arguments",
                    arg.span,
                );
            }
        }
        let explicit_count = args.len();
        let completed_args = match function.complete_args(args, call_span) {
            Ok(args) => args,
            Err(error) => {
                self.report_udf_arg_error(
                    &function.display_name,
                    span,
                    function.params.len(),
                    args.len(),
                    error,
                );
                return None;
            }
        };
        let args = completed_args.as_ref();
        let arg_indices = match resolve_udf_arg_indices(&function.params, args) {
            Ok(arg_indices) => arg_indices,
            Err(error) => {
                self.report_udf_arg_error(
                    &function.display_name,
                    span,
                    function.params.len(),
                    args.len(),
                    error,
                );
                return None;
            }
        };

        self.compatibility.supported.push(FeatureUse {
            feature: "function".to_owned(),
            span: function.span,
        });
        let mut resolved_arg_types = vec![None; function.params.len()];
        let mut resolved_arg_user_types = vec![None; function.params.len()];
        let mut resolved_arg_user_type_arrays = vec![None; function.params.len()];
        let mut resolved_arg_map_infos = vec![None; function.params.len()];
        let mut resolved_arg_const_switch_keys = vec![None; function.params.len()];
        for (arg_index, param_index) in arg_indices.iter().copied().enumerate() {
            if arg_index >= explicit_count {
                let value = &args[arg_index].value;
                resolved_arg_types[param_index] = self.analyze_expr(value);
                if resolved_arg_types[param_index].is_some_and(|t| {
                    !matches!(
                        t.kind,
                        ValueKind::Int
                            | ValueKind::Float
                            | ValueKind::Bool
                            | ValueKind::String
                            | ValueKind::Color
                            | ValueKind::Na
                    )
                }) {
                    self.diagnostics.push(Diagnostic::error(
                        "E_FUNCTION_DEFAULT_TYPE",
                        "defaults that resolve to reference values in the caller are not supported",
                        call_span,
                    ));
                }
                if matches!(value.kind, ExprKind::Literal(_)) {
                    resolved_arg_const_switch_keys[param_index] =
                        self.known_const_switch_key(value);
                }
                continue;
            }
            let resolved_arg_type = arg_types.get(arg_index).copied().flatten();
            resolved_arg_types[param_index] = resolved_arg_type;
            resolved_arg_user_types[param_index] = args
                .get(arg_index)
                .and_then(|arg| self.user_type_name_of_expr(&arg.value));
            resolved_arg_user_type_arrays[param_index] = args
                .get(arg_index)
                .and_then(|arg| self.user_type_array_name_of_expr(&arg.value));
            resolved_arg_map_infos[param_index] = args
                .get(arg_index)
                .and_then(|arg| self.map_type_of_expr(&arg.value));
            resolved_arg_const_switch_keys[param_index] = args
                .get(arg_index)
                .and_then(|arg| self.known_const_switch_key(&arg.value));
        }
        self.scope.push_scope();
        let mut param_symbols = std::collections::HashSet::new();
        let mut param_const_switch_keys = std::collections::HashMap::new();
        for (
            (param, expected_type),
            (
                (((arg_type, arg_user_type), arg_user_type_array), arg_map_info),
                arg_const_switch_key,
            ),
        ) in function.params.iter().zip(function.param_types.iter()).zip(
            resolved_arg_types
                .into_iter()
                .zip(resolved_arg_user_types)
                .zip(resolved_arg_user_type_arrays)
                .zip(resolved_arg_map_infos)
                .zip(resolved_arg_const_switch_keys),
        ) {
            let arg_type = arg_type.unwrap_or(UNKNOWN);
            let bound_type = expected_type
                .as_ref()
                .map_or(arg_type, |expected| expected.bound_type(arg_type));
            let symbol = self.define_local_symbol(param, bound_type, None, false);
            param_symbols.insert(symbol.id);
            if let Some(key) = arg_const_switch_key.as_ref() {
                self.record_symbol_const_switch_key(symbol, key);
            }
            if let Some(expected_type) = expected_type {
                if !can_assign(expected_type.pine_type, arg_type) {
                    self.push_diagnostic_in_context(
                        Diagnostic::error(
                            "E_FUNCTION_ARG_TYPE",
                            format!(
                                "cannot pass {} to function parameter `{}` of type {}",
                                pine_type_name(arg_type),
                                param,
                                pine_type_name(expected_type.pine_type)
                            ),
                            expected_type.span,
                        ),
                        function.source_context_id,
                    );
                }
                if expected_type.pine_type.kind == ValueKind::UserTypeArray {
                    if let Some(expected_type_name) = &expected_type.user_type_name {
                        if arg_user_type_array.as_deref() == Some(expected_type_name.as_str()) {
                            self.mark_symbol_user_type_array(symbol, expected_type_name.clone());
                        } else if arg_type.kind == ValueKind::UserTypeArray {
                            self.push_diagnostic_in_context(Diagnostic::error(
                                "E_FUNCTION_ARG_TYPE",
                                format!(
                                    "cannot pass a different user-defined type array to function parameter `{param}`",
                                ),
                                expected_type.span,
                            ), function.source_context_id);
                        }
                    }
                } else {
                    if let Some(expected_type_name) = &expected_type.user_type_name
                        && arg_user_type.as_deref() == Some(expected_type_name.as_str())
                    {
                        self.mark_symbol_user_type(symbol, expected_type_name.clone());
                    }
                }
            }
            if let Some(type_name) = arg_user_type {
                self.mark_symbol_user_type(symbol, type_name);
            }
            if let Some(type_name) = arg_user_type_array {
                self.mark_symbol_user_type_array(symbol, type_name);
            }
            if let Some(info) = arg_map_info {
                self.mark_symbol_map(symbol, info);
            }
            if let Some(key) = arg_const_switch_key {
                param_const_switch_keys.insert(param.clone(), key);
            }
        }
        self.function_stack.push(name.to_owned());
        self.function_param_symbols.push(param_symbols);
        self.function_param_const_switch_keys
            .push(param_const_switch_keys);
        self.function_context_is_method.push(false);
        self.function_tuple_identity_slots.push(HashSet::new());
        self.function_depth += 1;
        let (return_type, body_user_type, body_user_type_array, body_map, unresolved_tuple_slots) =
            self.with_source_context(function.source_context_id, |analyzer| {
                let diagnostic_start = analyzer.diagnostics.len();
                let return_type = analyzer.analyze_function_body(&function.body, function.span);
                let body_user_type = return_type
                    .is_some_and(|pine_type| pine_type.kind == ValueKind::UserType)
                    .then(|| analyzer.user_type_name_of_function_body(&function.body))
                    .flatten();
                let body_user_type_array = return_type
                    .is_some_and(|pine_type| pine_type.kind == ValueKind::UserTypeArray)
                    .then(|| analyzer.user_type_array_name_of_function_body(&function.body))
                    .flatten();
                let body_map = return_type
                    .is_some_and(|pine_type| pine_type.kind == ValueKind::Map)
                    .then(|| analyzer.map_type_of_function_body(&function.body))
                    .flatten();
                let has_new_errors = analyzer.diagnostics[diagnostic_start..]
                    .iter()
                    .any(|diagnostic| diagnostic.severity == Severity::Error);
                let unresolved_tuple_slots = if !has_new_errors
                    && return_type.is_some_and(|pine_type| pine_type.kind == ValueKind::Tuple)
                {
                    analyzer.unresolved_function_body_user_type_array_tuple_slots(&function.body)
                } else {
                    Vec::new()
                };
                (
                    return_type,
                    body_user_type,
                    body_user_type_array,
                    body_map,
                    unresolved_tuple_slots,
                )
            });
        self.function_depth -= 1;
        self.function_context_is_method.pop();
        self.function_param_const_switch_keys.pop();
        self.function_param_symbols.pop();
        self.function_stack.pop();
        self.scope.pop_scope();

        let mut tuple_identity_slots = self
            .function_tuple_identity_slots
            .pop()
            .expect("tuple identity call scope should exist");
        tuple_identity_slots.extend(unresolved_tuple_slots);
        if let Some(parent_slots) = self.function_tuple_identity_slots.last_mut() {
            parent_slots.extend(tuple_identity_slots);
        } else {
            let mut tuple_identity_slots: Vec<_> = tuple_identity_slots.into_iter().collect();
            tuple_identity_slots.sort_unstable();
            for index in tuple_identity_slots {
                self.diagnostics.push(Diagnostic::error(
                    "E_TUPLE_UDT_ARRAY_IDENTITY",
                    format!(
                        "tuple element {} user-defined type array must resolve to one element identity",
                        index + 1
                    ),
                    call_span,
                ));
            }
        }

        if return_type.is_some_and(|pine_type| pine_type.kind == ValueKind::UserType) {
            let returned_type_name = body_user_type.or_else(|| {
                let FunctionBody::Expr(expr) = &function.body else {
                    return None;
                };
                let ExprKind::Identifier(returned_param) = &expr.kind else {
                    return None;
                };
                let param_index = function
                    .params
                    .iter()
                    .position(|param| param == returned_param)?;
                let arg_index = arg_indices
                    .iter()
                    .position(|mapped_param_index| *mapped_param_index == param_index)?;
                self.user_type_name_of_expr(&args[arg_index].value)
            });
            if let Some(type_name) = returned_type_name {
                self.mark_expr_user_type(call_span, type_name.clone());
                self.mark_expr_user_type(span, type_name);
            }
        }
        if return_type.is_some_and(|pine_type| pine_type.kind == ValueKind::UserTypeArray)
            && let Some(type_name) = body_user_type_array
        {
            self.mark_expr_user_type_array(call_span, type_name.clone());
            self.mark_expr_user_type_array(span, type_name);
        }
        if return_type.is_some_and(|pine_type| pine_type.kind == ValueKind::Map)
            && let Some(info) = body_map
        {
            self.mark_expr_map(call_span, info);
            self.mark_expr_map(span, info);
        }

        if is_overloaded
            && return_type.is_some_and(|value| {
                !matches!(
                    value.kind,
                    ValueKind::Int
                        | ValueKind::Float
                        | ValueKind::Bool
                        | ValueKind::String
                        | ValueKind::Color
                        | ValueKind::Na
                )
            })
        {
            self.unsupported(
                "function_overload_result",
                "imported overloads currently require scalar results",
                call_span,
            );
            return None;
        }
        return_type
    }

    pub(crate) fn analyze_function_body(
        &mut self,
        body: &FunctionBody,
        span: Span,
    ) -> Option<PineType> {
        match body {
            FunctionBody::Expr(expr) => self.analyze_expr(expr),
            FunctionBody::Block(statements) => {
                let Some((last, prefix)) = statements.split_last() else {
                    self.diagnostics.push(Diagnostic::error(
                        "E_FUNCTION_RETURN",
                        "user-defined function block must end with an expression",
                        span,
                    ));
                    return None;
                };
                for statement in prefix {
                    if self.legacy.dialect().version() <= 2 && self.function_depth > 0 {
                        self.predeclare_legacy_function_self_history(statement);
                    }
                    self.analyze_stmt(statement);
                }
                if self.legacy.dialect().version() <= 2 && self.function_depth > 0 {
                    self.predeclare_legacy_function_self_history(last);
                }
                match &last.kind {
                    StmtKind::Expr(expr) => self.analyze_expr(expr),
                    StmtKind::TupleDecl { value, .. } => {
                        self.analyze_stmt(last);
                        self.type_of_expr_with_params(value, &HashMap::new())
                    }
                    StmtKind::Decl { name, .. } | StmtKind::Reassign { name, .. } => {
                        self.analyze_function_symbol_statement_return(last, name)
                    }
                    StmtKind::FieldReassign {
                        receiver,
                        path,
                        field,
                        ..
                    } => {
                        self.analyze_stmt(last);
                        self.analyze_expr(&field_reassign_result_expr(
                            receiver, path, field, last.span,
                        ))
                    }
                    StmtKind::If {
                        condition,
                        then_branch,
                        else_branch,
                    } if function_branch_has_return(then_branch)
                        && (else_branch.is_empty() || function_branch_has_return(else_branch)) =>
                    {
                        self.analyze_function_if_return(
                            condition,
                            then_branch,
                            else_branch,
                            last.span,
                        )
                    }
                    StmtKind::For {
                        counter,
                        from,
                        to,
                        step,
                        body,
                    } => self.analyze_function_for_return(
                        counter,
                        from,
                        to,
                        step.as_ref(),
                        body,
                        last.span,
                    ),
                    StmtKind::ForIn {
                        index,
                        value,
                        iterable,
                        body,
                    } => self.analyze_function_for_in_return(
                        index.as_deref(),
                        value,
                        iterable,
                        body,
                        last.span,
                    ),
                    StmtKind::While { condition, body } => {
                        self.analyze_function_while_return(condition, body, last.span)
                    }
                    _ => {
                        self.analyze_stmt(last);
                        self.diagnostics.push(Diagnostic::error(
                            "E_FUNCTION_RETURN",
                            "user-defined function block must end with an expression",
                            last.span,
                        ));
                        None
                    }
                }
            }
        }
    }

    fn predeclare_legacy_function_self_history(&mut self, statement: &Stmt) {
        let StmtKind::Decl {
            mode: pine_syntax::DeclMode::Normal,
            declared_type: None,
            name,
            value,
        } = &statement.kind
        else {
            return;
        };
        let mut self_history_span = None;
        crate::modules::visit_expression(value, &mut |expr| {
            if let ExprKind::History { expr: target, .. } = &expr.kind
                && matches!(&target.kind, ExprKind::Identifier(target_name) if target_name == name)
            {
                self_history_span = Some(expr.span);
            }
        });
        let Some(history_span) = self_history_span else {
            return;
        };
        // Prior locals have their actual types by this point. Seed only the
        // current recurrence before analyzing its own initializer.
        let seed = PineType::new(Qualifier::Series, ValueKind::Float);
        let inferred = std::collections::HashMap::from([(name.clone(), seed)]);
        if !self
            .legacy_graph_type_of_expr(value, &inferred)
            .is_some_and(|pine_type| pine_type.kind == ValueKind::Float)
        {
            return;
        }
        let symbol = self.define_local_symbol(name, seed, None, false);
        self.legacy_v2_predeclared_symbols.insert(symbol.id);
        self.compatibility
            .legacy_emulations
            .push(crate::compatibility::LegacyEmulation {
                feature: format!("v{}.self_reference", self.legacy.dialect().version()),
                behavior: "legacy function-local self-history uses one series identity across bars"
                    .to_owned(),
                span: history_span,
            });
    }

    fn analyze_function_symbol_statement_return(
        &mut self,
        statement: &Stmt,
        name: &str,
    ) -> Option<PineType> {
        self.analyze_stmt(statement);
        self.scope.resolve(name).map(|symbol| symbol.pine_type)
    }

    fn analyze_function_if_return(
        &mut self,
        condition: &Expr,
        then_branch: &[Stmt],
        else_branch: &[Stmt],
        span: Span,
    ) -> Option<PineType> {
        let condition_type = self.analyze_expr(condition)?;
        self.expect_bool(condition_type, condition.span);
        self.compatibility.supported.push(FeatureUse {
            feature: "if".to_owned(),
            span,
        });

        let condition_qualifier = condition_type.qualifier;
        let condition_value = self.known_const_bool_value(condition);
        self.block_depth += 1;
        self.assignment_qualifier_context.push(condition_qualifier);
        let (then_type, else_type) = match condition_value {
            Some(true) => {
                let then_type = self.analyze_function_branch_return(then_branch);
                let else_type = if else_branch.is_empty() {
                    Some(PineType::new(Qualifier::Const, ValueKind::Na))
                } else {
                    self.analyze_without_symbol_effects(|analyzer| {
                        analyzer.analyze_function_branch_return(else_branch)
                    })
                };
                (then_type, else_type)
            }
            Some(false) => {
                let then_type = self.analyze_without_symbol_effects(|analyzer| {
                    analyzer.analyze_function_branch_return(then_branch)
                });
                let else_type = if else_branch.is_empty() {
                    Some(PineType::new(Qualifier::Const, ValueKind::Na))
                } else {
                    self.analyze_function_branch_return(else_branch)
                };
                (then_type, else_type)
            }
            None => {
                let then_type = self.analyze_function_branch_return(then_branch);
                let else_type = if else_branch.is_empty() {
                    Some(PineType::new(Qualifier::Const, ValueKind::Na))
                } else {
                    self.analyze_function_branch_return(else_branch)
                };
                (then_type, else_type)
            }
        };
        self.assignment_qualifier_context.pop();
        self.block_depth -= 1;

        let then_type = then_type?;
        let else_type = else_type?;
        let pine_type =
            self.merge_branch_types(condition_type, then_type, else_type, condition_value, span)?;
        if pine_type.kind == ValueKind::UserType
            && self
                .user_type_name_of_if_branches(then_branch, else_branch)
                .is_none()
        {
            self.diagnostics.push(Diagnostic::error(
                "E_BRANCH_TYPE",
                "if user-defined type branches must resolve to the same local UDT",
                span,
            ));
            return None;
        }
        if pine_type.kind == ValueKind::Map && !self.mark_if_map(span, then_branch, else_branch) {
            self.diagnostics.push(Diagnostic::error(
                "E_BRANCH_TYPE",
                "if map branches must resolve to the same map template",
                span,
            ));
            return None;
        }
        if pine_type.kind == ValueKind::UserTypeArray
            && !self.mark_if_user_type_array(span, then_branch, else_branch)
        {
            self.diagnostics.push(Diagnostic::error(
                "E_BRANCH_TYPE",
                "if UDT array branches must resolve to the same element identity",
                span,
            ));
            return None;
        }
        Some(pine_type)
    }

    fn analyze_function_branch_return(&mut self, branch: &[Stmt]) -> Option<PineType> {
        let (last, prefix) = branch.split_last()?;
        self.scope.push_scope();
        for statement in prefix {
            self.analyze_stmt(statement);
        }
        let pine_type = match &last.kind {
            StmtKind::Expr(expr) => self.analyze_expr(expr),
            StmtKind::TupleDecl { value, .. } => {
                self.analyze_stmt(last);
                self.type_of_expr_with_params(value, &HashMap::new())
            }
            StmtKind::Decl { name, .. } | StmtKind::Reassign { name, .. } => {
                self.analyze_function_symbol_statement_return(last, name)
            }
            StmtKind::FieldReassign {
                receiver,
                path,
                field,
                ..
            } => {
                self.analyze_stmt(last);
                self.analyze_expr(&field_reassign_result_expr(
                    receiver, path, field, last.span,
                ))
            }
            StmtKind::If {
                condition,
                then_branch,
                else_branch,
            } if function_branch_has_return(then_branch)
                && (else_branch.is_empty() || function_branch_has_return(else_branch)) =>
            {
                self.analyze_function_if_return(condition, then_branch, else_branch, last.span)
            }
            StmtKind::For {
                counter,
                from,
                to,
                step,
                body,
            } => {
                self.analyze_function_for_return(counter, from, to, step.as_ref(), body, last.span)
            }
            StmtKind::ForIn {
                index,
                value,
                iterable,
                body,
            } => self.analyze_function_for_in_return(
                index.as_deref(),
                value,
                iterable,
                body,
                last.span,
            ),
            StmtKind::While { condition, body } => {
                self.analyze_function_while_return(condition, body, last.span)
            }
            _ => None,
        };
        self.scope.pop_scope();
        pine_type
    }

    pub(crate) fn report_udf_arg_error(
        &mut self,
        function_name: &str,
        call_span: Span,
        expected: usize,
        got: usize,
        error: UdfArgError,
    ) {
        match error {
            UdfArgError::UnknownName { name, span } => {
                self.diagnostics.push(Diagnostic::error(
                    "E_FUNCTION_ARG_NAME",
                    format!("`{function_name}` has no argument named `{name}`"),
                    span,
                ));
            }
            UdfArgError::Duplicate { name, span } => {
                self.diagnostics.push(Diagnostic::error(
                    "E_FUNCTION_ARG_DUPLICATE",
                    format!("`{function_name}` argument `{name}` is provided more than once"),
                    span,
                ));
            }
            UdfArgError::PositionalAfterNamed { span } => {
                self.diagnostics.push(Diagnostic::error(
                    "E_FUNCTION_ARG_ORDER",
                    "positional arguments cannot follow named arguments in user-defined function calls",
                    span,
                ));
            }
            UdfArgError::TooMany { span } => {
                self.diagnostics.push(Diagnostic::error(
                    "E_FUNCTION_ARITY",
                    format!("`{function_name}` expects {expected} argument(s), got {got}"),
                    span,
                ));
            }
            UdfArgError::Missing { param } => {
                self.diagnostics.push(Diagnostic::error(
                    "E_FUNCTION_ARITY",
                    format!("`{function_name}` is missing argument `{param}`"),
                    call_span,
                ));
            }
        }
    }
}
