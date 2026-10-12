use super::*;
use std::collections::HashMap;

pub(super) fn pure_variadic_named_call_arg_keys(
    analyzer: &Analyzer,
    name: &str,
    args: &[CallArg],
    param_keys: &HashMap<String, String>,
    allow_udf_calls: bool,
    udf_stack: &mut Vec<String>,
) -> Option<Vec<String>> {
    let signature = pine_builtins::get_phase_1_builtin(name)?;
    if !signature.variadic || args.len() > signature.params.len() {
        return None;
    }

    let mut arg_keys = vec![None; signature.params.len()];
    let mut saw_named = false;
    for (index, arg) in args.iter().enumerate() {
        let param_index = if let Some(arg_name) = &arg.name {
            saw_named = true;
            signature
                .params
                .iter()
                .position(|param| param.name == arg_name)?
        } else {
            if saw_named {
                return None;
            }
            index
        };
        if arg_keys.get(param_index)?.is_some() {
            return None;
        }
        arg_keys[param_index] = Some(pure_expr_series_key_with_params(
            analyzer,
            &arg.value,
            param_keys,
            allow_udf_calls,
            udf_stack,
        )?);
    }

    arg_keys.into_iter().collect()
}

pub(super) fn pure_fixed_call_arg_keys(
    analyzer: &Analyzer,
    name: &str,
    args: &[CallArg],
    param_keys: &HashMap<String, String>,
    allow_udf_calls: bool,
    udf_stack: &mut Vec<String>,
) -> Option<Vec<String>> {
    let signature = pine_builtins::get_phase_1_builtin(name)?;
    if signature.variadic || args.len() > signature.params.len() {
        return None;
    }

    let mut arg_keys = vec![None; signature.params.len()];
    let mut saw_named = false;
    for (index, arg) in args.iter().enumerate() {
        let param_index = if let Some(arg_name) = &arg.name {
            saw_named = true;
            signature
                .params
                .iter()
                .position(|param| param.name == arg_name)?
        } else {
            if saw_named {
                return None;
            }
            index
        };
        if arg_keys.get(param_index)?.is_some() {
            return None;
        }
        arg_keys[param_index] = Some(pure_expr_series_key_with_params(
            analyzer,
            &arg.value,
            param_keys,
            allow_udf_calls,
            udf_stack,
        )?);
    }

    signature
        .params
        .iter()
        .zip(arg_keys)
        .map(|(param, arg_key)| {
            if param.optional {
                Some(arg_key.unwrap_or_else(|| "arg:none".to_owned()))
            } else {
                arg_key
            }
        })
        .collect()
}

pub(super) fn udf_call_param_keys(
    analyzer: &Analyzer,
    function: &FunctionInfo,
    args: &[CallArg],
    call_span: Span,
    caller_param_keys: &HashMap<String, String>,
    udf_stack: &mut Vec<String>,
) -> Option<HashMap<String, String>> {
    let explicit_count = args.len();
    let completed_args = function.complete_args(args, call_span).ok()?;
    let args = completed_args.as_ref();
    let arg_indices = resolve_udf_arg_indices(&function.params, args).ok()?;
    let mut param_keys = HashMap::new();
    let mut pending_field_keys = Vec::new();
    for (arg_index, (arg, param_index)) in args.iter().zip(arg_indices).enumerate() {
        let param_name = function.params.get(param_index)?;
        let arg_user_type_name = (arg_index < explicit_count)
            .then(|| analyzer.user_type_name_of_expr(&arg.value))
            .flatten();
        let arg_key = if let Some(type_name) = arg_user_type_name.as_deref() {
            user_type_value_series_key(
                analyzer,
                &arg.value,
                type_name,
                caller_param_keys,
                udf_stack,
            )?
        } else {
            pure_expr_series_key_with_params(
                analyzer,
                &arg.value,
                caller_param_keys,
                true,
                udf_stack,
            )?
        };
        param_keys.insert(param_name.clone(), arg_key);
        if arg_index >= explicit_count {
            continue;
        }
        let caller_field_keys = alias_field_param_keys(param_name, &arg.value, caller_param_keys);
        if !caller_field_keys.is_empty() {
            pending_field_keys.push(caller_field_keys);
        } else if let Some(type_name) = arg_user_type_name.as_deref()
            && let Some(field_keys) = field_param_keys_for_user_type_expr(
                analyzer,
                param_name,
                &arg.value,
                type_name,
                caller_param_keys,
                udf_stack,
            )
        {
            pending_field_keys.push(field_keys);
        }
    }
    if param_keys.len() != function.params.len() {
        return None;
    }
    for field_keys in pending_field_keys {
        param_keys.extend(field_keys);
    }
    Some(param_keys)
}
