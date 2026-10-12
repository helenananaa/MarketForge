use std::collections::HashMap;

use super::resolve_udf_arg_indices_with_defaults;
use crate::prelude::*;

impl FunctionInfo {
    /// Prefer candidates that dominate by conversion/qualifier cost, then keep
    /// declaration order among incomparable candidates (native control evidence).
    pub(crate) fn select_overload(
        &self,
        args: &[CallArg],
        types: &[Option<PineType>],
    ) -> Option<&FunctionInfo> {
        if self.overloads.is_empty() {
            return Some(self);
        }
        let candidates: Vec<_> = std::iter::once(self)
            .chain(self.overloads.iter())
            .filter_map(|candidate| {
                let indices = resolve_udf_arg_indices_with_defaults(
                    &candidate.params,
                    args,
                    &candidate.default_values,
                )
                .ok()?;
                let mut costs = Vec::new();
                for (argument, index) in indices.into_iter().enumerate() {
                    let actual = types.get(argument).copied().flatten()?;
                    let expected = candidate.param_types.get(index)?.as_ref()?;
                    // Reference overloads require identity-aware dispatch. Do not
                    // pick a same-kind but different UDT using scalar ranking.
                    if expected.user_type_name.is_some() {
                        return None;
                    }
                    if !matches!(
                        expected.pine_type.kind,
                        ValueKind::Int
                            | ValueKind::Float
                            | ValueKind::Bool
                            | ValueKind::String
                            | ValueKind::Color
                    ) {
                        return None;
                    }
                    let conversion = if actual.kind == expected.pine_type.kind {
                        0
                    } else if actual.kind == ValueKind::Int
                        && expected.pine_type.kind == ValueKind::Float
                    {
                        1
                    } else {
                        return None;
                    };
                    let qualifier = match expected.pine_type.qualifier {
                        Qualifier::Const => 0,
                        Qualifier::Input => 1,
                        Qualifier::Simple => 2,
                        Qualifier::Series => 3,
                    };
                    let actual_qualifier = match actual.qualifier {
                        Qualifier::Const => 0,
                        Qualifier::Input => 1,
                        Qualifier::Simple => 2,
                        Qualifier::Series => 3,
                    };
                    if actual_qualifier > qualifier {
                        return None;
                    }
                    costs.push((conversion, qualifier));
                }
                Some((candidate, costs))
            })
            .collect();
        let (winner, _) = candidates.iter().find(|(_, costs)| {
            !candidates.iter().any(|(_, other)| {
                other.iter().zip(costs).all(|(a, b)| a <= b)
                    && other.iter().zip(costs).any(|(a, b)| a < b)
            })
        })?;
        Some(*winner)
    }
}

impl Analyzer {
    pub(crate) fn function_for_call_with_params(
        &self,
        name: &str,
        args: &[CallArg],
        param_types: &HashMap<String, PineType>,
    ) -> Option<&FunctionInfo> {
        let group = self.functions.get(name)?;
        if group.overloads.is_empty() {
            return Some(group);
        }
        let types: Vec<_> = args
            .iter()
            .map(|arg| self.type_of_expr_with_params(&arg.value, param_types))
            .collect();
        group.select_overload(args, &types)
    }
}
