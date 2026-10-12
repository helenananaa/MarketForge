use super::{ModuleInfo, exported_scalar_tree_user_type};
use crate::analyzer::context::FunctionParamInfo;
use crate::types::array_kind_from_element_type_name;
use pine_ir::{PineType, Qualifier, ValueKind};
use pine_syntax::{Diagnostic, FunctionParam, Span};

pub(super) fn same_parameter_signature(
    left: &[Option<FunctionParamInfo>],
    right: &[Option<FunctionParamInfo>],
) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|(a, b)| match (a, b) {
            (Some(a), Some(b)) => {
                a.pine_type == b.pine_type && a.user_type_name == b.user_type_name
            }
            (None, None) => true,
            _ => false,
        })
}

pub(super) fn imported_function_overloads(
    function: &super::FunctionInfo,
    alias: &str,
    name: &str,
    module: &ModuleInfo,
    source_context_id: super::SourceContextId,
    context: &super::RewriteContext,
) -> Vec<super::FunctionInfo> {
    function
        .overloads
        .iter()
        .map(|overload| {
            let mut imported = overload.clone();
            imported.source_context_id = source_context_id;
            imported.display_name = format!("{alias}.{name}");
            imported.param_types =
                imported_function_param_types(alias, module, &overload.param_types);
            imported.body = super::rewrite_function_body(&overload.body, &overload.params, context);
            imported
        })
        .collect()
}

pub(super) fn module_function_param_types(
    module: &ModuleInfo,
    params: &[FunctionParam],
    alias: Option<&str>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<Option<FunctionParamInfo>> {
    params
        .iter()
        .map(|param| {
            let Some(type_name) = &param.type_name else {
                return None;
            };
            module_function_param_type(module, type_name, alias, param.span, diagnostics)
        })
        .collect()
}

fn module_function_param_type(
    module: &ModuleInfo,
    type_name: &str,
    alias: Option<&str>,
    span: Span,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<FunctionParamInfo> {
    let explicit_series = type_name.starts_with("series ");
    let explicit_simple = type_name.starts_with("simple ");
    let type_name = type_name
        .strip_prefix("series ")
        .or_else(|| type_name.strip_prefix("simple "))
        .unwrap_or(type_name);
    if explicit_simple && !matches!(type_name, "int" | "float" | "bool" | "string" | "color") {
        diagnostics.push(Diagnostic::error(
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
            } else if exported_scalar_tree_user_type(module, element_type) {
                let type_name = alias
                    .map(|alias| format!("{alias}.{element_type}"))
                    .unwrap_or_else(|| element_type.to_owned());
                (
                    PineType::new(Qualifier::Series, ValueKind::UserTypeArray),
                    Some(type_name),
                )
            } else {
                diagnostics.push(Diagnostic::error(
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
        _ if module.user_types.contains_key(type_name) => {
            let type_name = alias
                .map(|alias| format!("{alias}.{type_name}"))
                .unwrap_or_else(|| type_name.to_owned());
            (
                PineType::new(Qualifier::Series, ValueKind::UserType),
                Some(type_name),
            )
        }
        _ => {
            diagnostics.push(Diagnostic::error(
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

pub(super) fn imported_function_param_types(
    alias: &str,
    module: &ModuleInfo,
    params: &[Option<FunctionParamInfo>],
) -> Vec<Option<FunctionParamInfo>> {
    params
        .iter()
        .map(|param| {
            let mut param = param.clone()?;
            if let Some(type_name) = &param.user_type_name
                && module.user_types.contains_key(type_name)
            {
                param.user_type_name = Some(format!("{alias}.{type_name}"));
            }
            Some(param)
        })
        .collect()
}
