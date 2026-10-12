use super::*;

fn sort_field_index_expr(index: usize) -> HirExpr {
    HirExpr {
        kind: HirExprKind::Literal(HirLiteral::Int(index as i64)),
        pine_type: PineType::new(Qualifier::Const, ValueKind::Int),
        series_id: None,
    }
}

fn ascending_sort_order_expr() -> HirExpr {
    HirExpr {
        kind: HirExprKind::Literal(HirLiteral::String("order.ascending".to_owned())),
        pine_type: PineType::new(Qualifier::Const, ValueKind::String),
        series_id: None,
    }
}

fn omitted_builtin_arg() -> HirCallArg {
    HirCallArg {
        name: Some(pine_ir::OMITTED_BUILTIN_ARG.to_owned()),
        value: HirExpr {
            kind: HirExprKind::Builtin("na".to_owned()),
            pine_type: PineType::new(Qualifier::Const, ValueKind::Na),
            series_id: None,
        },
    }
}

fn signature_accepts_lowered_args(signature: &BuiltinSignature, args: &[HirCallArg]) -> bool {
    let mut positional_index = 0;
    args.iter().all(|arg| {
        if arg.name.as_deref() == Some(pine_ir::LEGACY_TRANSPARENCY_ARG) {
            return true;
        }
        let parameter_index = match arg.name.as_deref() {
            Some(name) => signature
                .params
                .iter()
                .position(|parameter| parameter.name == name),
            None => {
                let index = positional_index;
                positional_index += 1;
                (index < signature.params.len())
                    .then_some(index)
                    .or_else(|| signature.variadic.then_some(signature.params.len() - 1))
            }
        };
        parameter_index.is_some_and(|index| {
            let accepts = signature.params[index].accepts;
            let matrix_type = |slot: usize| {
                signature
                    .params
                    .get(slot)
                    .and_then(|param| {
                        args.iter()
                            .find(|arg| arg.name.as_deref() == Some(param.name))
                            .or_else(|| args.get(slot).filter(|arg| arg.name.is_none()))
                    })
                    .map(|arg| arg.value.pine_type)
            };
            match accepts {
                pine_builtins::Accepts::MatrixElementCompatible(slot) => matrix_type(slot)
                    .and_then(|ty| {
                        crate::types::accepts_matrix_element_arg(ty, arg.value.pine_type)
                    })
                    .unwrap_or(false),
                pine_builtins::Accepts::MatrixElementArray(slot) => matrix_type(slot)
                    .and_then(|ty| {
                        crate::types::accepts_matrix_element_array_arg(ty, arg.value.pine_type)
                    })
                    .unwrap_or(false),
                _ => crate::types::accepts_type(accepts, arg.value.pine_type),
            }
        })
    })
}

impl Analyzer {
    pub(super) fn lower_builtin_call_args(
        &mut self,
        builtin_name: &str,
        args: &[CallArg],
        param_exprs: &HashMap<String, HirExpr>,
        param_types: &HashMap<String, PineType>,
    ) -> Option<Vec<HirCallArg>> {
        let sort_field_index = matches!(builtin_name, "array.sort" | "array.sort_indices")
            .then(|| self.user_type_array_sort_field_index(args))
            .flatten();
        if let Some(sort_field_index) = sort_field_index {
            let (_, id) = crate::analyzer::user_type_array_sort::user_type_array_sort_arg(args, 0)?;
            let id = self.lower_expr_with_params(&id.value, param_exprs, param_types)?;
            let order = crate::analyzer::user_type_array_sort::user_type_array_sort_arg(args, 1)
                .and_then(|(_, order)| {
                    self.lower_expr_with_params(&order.value, param_exprs, param_types)
                })
                .unwrap_or_else(ascending_sort_order_expr);
            return Some(vec![
                HirCallArg {
                    name: None,
                    value: id,
                },
                HirCallArg {
                    name: None,
                    value: order,
                },
                HirCallArg {
                    name: None,
                    value: sort_field_index_expr(sort_field_index),
                },
            ]);
        }

        let mut lowered_args: Vec<_> = args
            .iter()
            .map(|arg| {
                Some(HirCallArg {
                    name: arg.name.clone(),
                    value: self.lower_expr_with_params(&arg.value, param_exprs, param_types)?,
                })
            })
            .collect::<Option<_>>()?;

        if builtin_name == "strategy.close"
            && self.legacy.dialect() <= crate::PineDialect::V4
            && lowered_args.get(1).is_some_and(|arg| arg.name.is_none())
        {
            lowered_args[1].name = Some("when".to_owned());
        }

        if !lowered_args.iter().any(|arg| arg.name.is_some())
            || matches!(builtin_name, "array.min" | "array.max")
        {
            return Some(lowered_args);
        }

        if builtin_name == "timestamp"
            && lowered_args.len() == 1
            && lowered_args[0].name.as_deref() == Some("dateString")
        {
            let mut arg = lowered_args.into_iter().next()?;
            arg.name = None;
            return Some(vec![arg]);
        }

        let Some(signature) = pine_builtins::PHASE_1_BUILTINS
            .iter()
            .filter(|signature| signature.name == builtin_name)
            .find(|signature| signature_accepts_lowered_args(signature, &lowered_args))
        else {
            // Synthetic legacy calls have already had their arguments rewritten to
            // canonical names, but they are intentionally absent from the public
            // builtin registry.
            return Some(lowered_args);
        };
        let mut slots: Vec<Option<HirCallArg>> = vec![None; signature.params.len()];
        let point_overload_discriminator = match (builtin_name, signature.params.first()) {
            ("label.new", Some(parameter)) if parameter.name == "point" => Some("point"),
            ("line.new", Some(parameter)) if parameter.name == "first_point" => Some("first_point"),
            ("box.new", Some(parameter)) if parameter.name == "top_left" => Some("top_left"),
            _ => None,
        };
        let mut internal_args = Vec::new();
        let mut positional_index = 0;
        for mut arg in lowered_args {
            if arg.name.as_deref() == Some(pine_ir::LEGACY_TRANSPARENCY_ARG) {
                internal_args.push(arg);
                continue;
            }
            let parameter_index = match arg.name.as_deref() {
                Some(name) => signature
                    .params
                    .iter()
                    .position(|parameter| parameter.name == name)?,
                None => {
                    let index = positional_index;
                    positional_index += 1;
                    index
                }
            };
            if parameter_index >= slots.len() {
                if !signature.variadic {
                    return None;
                }
                slots.resize(parameter_index + 1, None);
            }
            arg.name = None;
            slots[parameter_index] = Some(arg);
        }

        let mut normalized = slots
            .iter()
            .rposition(Option::is_some)
            .map(|last_bound| {
                slots
                    .into_iter()
                    .take(last_bound + 1)
                    .map(|arg| arg.unwrap_or_else(omitted_builtin_arg))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if let (Some(name), Some(first)) = (point_overload_discriminator, normalized.first_mut()) {
            // Drawing point overloads need an explicit discriminator when the
            // point expression itself is `na`, whose HIR kind carries no object type.
            first.name = Some(name.to_owned());
        }
        normalized.extend(internal_args);
        Some(normalized)
    }
}
