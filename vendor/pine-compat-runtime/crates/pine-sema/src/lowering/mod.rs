use crate::prelude::*;

mod binary;
mod blocks;
mod budget;
mod builtin_calls;
mod call_result_methods;
mod function_returns;
mod inline_calls;
mod legacy;
mod legacy_conversions;
mod literals;
mod members;
mod program;
mod pure_series;
mod reassignments;
mod tuple_returns;
mod user_type_copy;
pub(crate) mod user_types;

pub(crate) use blocks::prepend_block_statements;
pub(crate) use literals::lower_literal;

fn builtin_qualified_series_key(parts: &[String]) -> Option<String> {
    if parts.len() < 2 {
        return None;
    }
    let name = parts.join(".");
    let is_named_const = pine_builtins::named_color(&name).is_some()
        || pine_builtins::named_float_constant(&name).is_some()
        || pine_builtins::named_int_constant(&name).is_some()
        || pine_builtins::named_string_constant(&name).is_some();
    let is_stable_builtin_value =
        pine_builtins::builtin_series_value_type(&name).is_some_and(|pine_type| {
            pine_type.qualifier != Qualifier::Series
                || name.starts_with("barstate.")
                || name.starts_with("session.")
        });
    (is_named_const || is_stable_builtin_value).then(|| format!("builtin:{name}"))
}

fn pure_math_call_name(name: &str) -> bool {
    matches!(
        name,
        "math.abs"
            | "math.max"
            | "math.min"
            | "math.avg"
            | "math.floor"
            | "math.ceil"
            | "math.trunc"
            | "math.sqrt"
            | "math.cbrt"
            | "math.log"
            | "math.log10"
            | "math.exp"
            | "math.acos"
            | "math.asin"
            | "math.atan"
            | "math.sign"
            | "math.todegrees"
            | "math.toradians"
            | "math.sin"
            | "math.cos"
            | "math.tan"
            | "math.pow"
            | "math.hypot"
            | "math.round"
            | "math.round_to_mintick"
    )
}

fn pure_math_variadic_call_name(name: &str) -> bool {
    matches!(name, "math.max" | "math.min" | "math.avg")
}

fn pure_fixed_builtin_call_name(name: &str) -> bool {
    matches!(
        name,
        "nz" | "fixnan"
            | "str.tonumber"
            | "str.length"
            | "str.pos"
            | "color.r"
            | "color.g"
            | "color.b"
            | "color.t"
    )
}

pub(crate) fn lower_unary_op(op: UnaryOp) -> HirUnaryOp {
    match op {
        UnaryOp::Plus => HirUnaryOp::Plus,
        UnaryOp::Minus => HirUnaryOp::Minus,
        UnaryOp::Not => HirUnaryOp::Not,
    }
}
pub(crate) fn lower_binary_op(op: BinaryOp) -> HirBinaryOp {
    match op {
        BinaryOp::Add => HirBinaryOp::Add,
        BinaryOp::Sub => HirBinaryOp::Sub,
        BinaryOp::Mul => HirBinaryOp::Mul,
        BinaryOp::Div => HirBinaryOp::Div,
        BinaryOp::Mod => HirBinaryOp::Mod,
        BinaryOp::Eq => HirBinaryOp::Eq,
        BinaryOp::NotEq => HirBinaryOp::NotEq,
        BinaryOp::Gt => HirBinaryOp::Gt,
        BinaryOp::Gte => HirBinaryOp::Gte,
        BinaryOp::Lt => HirBinaryOp::Lt,
        BinaryOp::Lte => HirBinaryOp::Lte,
        BinaryOp::And => HirBinaryOp::And,
        BinaryOp::Or => HirBinaryOp::Or,
    }
}
impl Analyzer {
    pub(crate) fn lower_symbols(&self) -> Vec<HirSymbol> {
        self.scope.lower_symbols()
    }

    fn lower_alias_decl_symbol_series_id(
        &mut self,
        name: &str,
        symbol: SymbolInfo,
        source_expr: &Expr,
        lowered_value: &HirExpr,
    ) -> SymbolInfo {
        if !self.lower_symbol_overrides.is_empty()
            || symbol.persistence != PersistenceKind::None
            || self.legacy_v2_predeclared_symbols.contains(&symbol.id)
            || self.lower_reassigned_symbols.contains(&symbol.id)
            || lowered_value.pine_type.qualifier != Qualifier::Series
            || is_collection_kind(lowered_value.pine_type.kind)
            || lowered_value.pine_type.kind == ValueKind::Tuple
            || self.pure_expr_series_key(source_expr).is_none()
        {
            return symbol;
        }
        let Some(series_id) = lowered_value.series_id else {
            return symbol;
        };
        if symbol.series_id == Some(series_id) {
            return symbol;
        }

        let updated = SymbolInfo {
            series_id: Some(series_id),
            ..symbol
        };
        self.scope.update(name, updated);
        for binding in self.bindings.values_mut() {
            if binding.id == symbol.id {
                *binding = updated;
            }
        }
        updated
    }

    pub(crate) fn bind_symbol(&mut self, name: &str, span: Span, symbol: SymbolInfo) {
        let key = self.binding_key(name, span);
        self.bindings.insert(key, symbol);
    }

    pub(crate) fn bound_symbol(&self, name: &str, span: Span) -> Option<SymbolInfo> {
        let symbol = self.bindings.get(&self.binding_key(name, span)).copied()?;
        self.lower_symbol_overrides
            .iter()
            .rev()
            .find_map(|overrides| overrides.get(&symbol.id).copied())
            .or(Some(symbol))
    }

    pub(crate) fn has_lower_symbol_override(&self, symbol_id: SymbolId) -> bool {
        self.lower_symbol_overrides
            .iter()
            .rev()
            .any(|overrides| overrides.contains_key(&symbol_id))
    }

    pub(crate) fn lower_decl_symbol(&mut self, name: &str, span: Span) -> Option<SymbolInfo> {
        let symbol = self.bindings.get(&self.binding_key(name, span)).copied()?;
        if self.lower_symbol_overrides.is_empty() || self.scope.contains_lower_symbol(symbol.id) {
            return Some(symbol);
        }
        if let Some(existing) = self
            .lower_symbol_overrides
            .iter()
            .rev()
            .find_map(|overrides| overrides.get(&symbol.id).copied())
        {
            return Some(existing);
        }
        let fresh = self.fresh_lower_symbol(name, symbol);
        self.lower_symbol_overrides
            .last_mut()
            .expect("override scope is active")
            .insert(symbol.id, fresh);
        Some(fresh)
    }

    pub(crate) fn lower_stmt(&mut self, statement: &pine_syntax::Stmt) -> Option<HirStmt> {
        self.lower_stmt_with_params(statement, &HashMap::new(), &HashMap::new())
    }

    pub(crate) fn lower_stmt_with_params(
        &mut self,
        statement: &pine_syntax::Stmt,
        param_exprs: &HashMap<String, HirExpr>,
        param_types: &HashMap<String, PineType>,
    ) -> Option<HirStmt> {
        if !self.record_lowering_node(statement.span) {
            return None;
        }

        let kind = match &statement.kind {
            StmtKind::Expr(expr) => match &expr.kind {
                ExprKind::Switch { selector, arms } => HirStmtKind::Switch {
                    selector: match selector {
                        Some(selector) => {
                            Some(self.lower_expr_with_params(selector, param_exprs, param_types)?)
                        }
                        None => None,
                    },
                    arms: arms
                        .iter()
                        .map(|arm| {
                            Some(HirSwitchStmtArm {
                                condition: match &arm.condition {
                                    Some(condition) => Some(self.lower_expr_with_params(
                                        condition,
                                        param_exprs,
                                        param_types,
                                    )?),
                                    None => None,
                                },
                                body: self.lower_switch_stmt_arm_body_with_params(
                                    &arm.result,
                                    param_exprs,
                                    param_types,
                                )?,
                            })
                        })
                        .collect::<Option<_>>()?,
                },
                _ => HirStmtKind::Expr(self.lower_expr_with_params(
                    expr,
                    param_exprs,
                    param_types,
                )?),
            },
            StmtKind::If {
                condition,
                then_branch,
                else_branch,
            } => HirStmtKind::If {
                condition: self.lower_expr_with_params(condition, param_exprs, param_types)?,
                then_branch: then_branch
                    .iter()
                    .map(|statement| {
                        self.lower_stmt_with_params(statement, param_exprs, param_types)
                    })
                    .collect::<Option<_>>()?,
                else_branch: else_branch
                    .iter()
                    .map(|statement| {
                        self.lower_stmt_with_params(statement, param_exprs, param_types)
                    })
                    .collect::<Option<_>>()?,
            },
            StmtKind::For {
                counter,
                from,
                to,
                step,
                body,
            } => HirStmtKind::For {
                counter: self.lower_decl_symbol(counter, statement.span)?.id,
                from: self.lower_expr_with_params(from, param_exprs, param_types)?,
                to: self.lower_expr_with_params(to, param_exprs, param_types)?,
                step: match step {
                    Some(step) => {
                        Some(self.lower_expr_with_params(step, param_exprs, param_types)?)
                    }
                    None => None,
                },
                body: body
                    .iter()
                    .map(|statement| {
                        self.lower_stmt_with_params(statement, param_exprs, param_types)
                    })
                    .collect::<Option<_>>()?,
            },
            StmtKind::ForIn {
                index,
                value,
                iterable,
                body,
            } => {
                let value_symbol = self.lower_decl_symbol(value, statement.span)?;
                if let Some(type_name) =
                    self.user_type_array_name_of_expr_with_params(iterable, param_exprs)
                {
                    self.mark_symbol_id_user_type(value_symbol.id, type_name);
                }
                HirStmtKind::ForIn {
                    index: match index {
                        Some(index) => Some(self.lower_decl_symbol(index, statement.span)?.id),
                        None => None,
                    },
                    value: value_symbol.id,
                    iterable: self.lower_expr_with_params(iterable, param_exprs, param_types)?,
                    body: body
                        .iter()
                        .map(|statement| {
                            self.lower_stmt_with_params(statement, param_exprs, param_types)
                        })
                        .collect::<Option<_>>()?,
                }
            }
            StmtKind::While { condition, body } => HirStmtKind::While {
                condition: self.lower_expr_with_params(condition, param_exprs, param_types)?,
                body: body
                    .iter()
                    .map(|statement| {
                        self.lower_stmt_with_params(statement, param_exprs, param_types)
                    })
                    .collect::<Option<_>>()?,
            },
            StmtKind::Break => HirStmtKind::Break,
            StmtKind::Continue => HirStmtKind::Continue,
            StmtKind::Decl { name, value, .. } => {
                let symbol = self.lower_decl_symbol(name, statement.span)?;
                if symbol.pine_type.kind == ValueKind::Tuple {
                    if let Some(results) =
                        self.tuple_user_type_array_results_with_params(value, param_exprs)
                    {
                        self.symbol_tuple_user_type_arrays
                            .insert(symbol.id, results);
                    } else {
                        self.symbol_tuple_user_type_arrays.remove(&symbol.id);
                    }
                } else {
                    self.symbol_tuple_user_type_arrays.remove(&symbol.id);
                }
                if let Some(type_name) = self.user_type_name_of_expr_with_params(value, param_exprs)
                {
                    self.mark_symbol_id_user_type(symbol.id, type_name);
                }
                if let Some(type_name) =
                    self.user_type_array_name_of_expr_with_params(value, param_exprs)
                {
                    self.mark_symbol_user_type_array(symbol, type_name);
                }
                let lowered_value = self.lower_expr_with_params(value, param_exprs, param_types)?;
                let symbol =
                    self.lower_alias_decl_symbol_series_id(name, symbol, value, &lowered_value);
                HirStmtKind::Decl {
                    symbol: symbol.id,
                    value: lowered_value,
                }
            }
            StmtKind::Reassign { name, value } => HirStmtKind::Reassign {
                symbol: self.bound_symbol(name, statement.span)?.id,
                value: self.lower_expr_with_params(value, param_exprs, param_types)?,
            },
            StmtKind::FieldReassign {
                receiver,
                path,
                field,
                value,
            } => {
                let mut parts = vec![receiver.clone()];
                parts.extend(path.clone());
                parts.push(field.clone());
                let access = self
                    .chart_point_field_access_for_lowering(&parts, statement.span)
                    .map(|access| (access.receiver, access.index, Vec::new()))
                    .or_else(|| {
                        self.user_type_field_access_for_lowering(&parts, statement.span)
                            .and_then(|access| {
                                access.fields.last().map(|field| {
                                    (
                                        access.receiver,
                                        field.index,
                                        access.fields[..access.fields.len() - 1]
                                            .iter()
                                            .map(|field| field.index)
                                            .collect(),
                                    )
                                })
                            })
                    })?;
                HirStmtKind::FieldReassign {
                    path: access.2,
                    symbol: match param_exprs.get(&access.0).map(|expr| &expr.kind) {
                        Some(HirExprKind::Symbol(symbol)) => *symbol,
                        _ => self.bound_symbol(&access.0, statement.span)?.id,
                    },
                    field_index: access.1,
                    value: self.lower_expr_with_params(value, param_exprs, param_types)?,
                }
            }
            StmtKind::ArrayFieldReassign {
                array,
                index,
                field,
                value,
            } => {
                let type_name = self.user_type_array_name_of_expr(array)?;
                let field_index = self
                    .user_types
                    .get(&type_name)?
                    .fields
                    .iter()
                    .position(|candidate| candidate.name == *field)?;
                HirStmtKind::ArrayFieldReassign {
                    array: self.lower_expr_with_params(array, param_exprs, param_types)?,
                    index: self.lower_expr_with_params(index, param_exprs, param_types)?,
                    field_index,
                    value: self.lower_expr_with_params(value, param_exprs, param_types)?,
                }
            }
            StmtKind::TupleDecl { names, value } => {
                let user_type_array_results =
                    self.tuple_user_type_array_results_with_params(value, param_exprs);
                let symbols = names
                    .iter()
                    .map(|name| self.lower_decl_symbol(name, statement.span))
                    .collect::<Option<Vec<_>>>()?;
                for (index, symbol) in symbols.iter().copied().enumerate() {
                    self.symbol_tuple_user_type_arrays.remove(&symbol.id);
                    if symbol.pine_type.kind != ValueKind::UserTypeArray {
                        continue;
                    }
                    let Some(UserTypeArrayIdentityResult::Known(type_name)) =
                        user_type_array_results
                            .as_ref()
                            .and_then(|results| results.get(index))
                    else {
                        return None;
                    };
                    self.mark_symbol_user_type_array(symbol, type_name.clone());
                }
                HirStmtKind::TupleDecl {
                    symbols: symbols.into_iter().map(|symbol| symbol.id).collect(),
                    value: self.lower_expr_with_params(value, param_exprs, param_types)?,
                }
            }
            StmtKind::Function { .. }
            | StmtKind::Import(_)
            | StmtKind::Library(_)
            | StmtKind::Export(_)
            | StmtKind::UserType(_)
            | StmtKind::Method(_) => return None,
            StmtKind::Unsupported { .. } => return None,
        };

        Some(HirStmt { kind })
    }

    pub(crate) fn lower_expr_with_params(
        &mut self,
        expr: &Expr,
        param_exprs: &HashMap<String, HirExpr>,
        param_types: &HashMap<String, PineType>,
    ) -> Option<HirExpr> {
        if matches!(expr.kind, ExprKind::Binary { .. }) {
            return self.lower_binary_expr_with_params(expr, param_exprs, param_types);
        }

        if !self.record_lowering_node(expr.span) {
            return None;
        }
        if let ExprKind::Call { callee, args } = &expr.kind
            && let Some((receiver, _, true)) = self.udt_copy_receiver(callee, args, param_types)
        {
            return self.lower_udt_copy(expr, &receiver, param_exprs, param_types);
        }

        if let ExprKind::Group(inner) = &expr.kind {
            let lowered = self.lower_expr_with_params(inner, param_exprs, param_types)?;
            return self.finish_legacy_expr_coercion(expr, lowered);
        }

        if let ExprKind::Identifier(name) = &expr.kind
            && let Some(param_expr) = param_exprs.get(name)
            && self
                .bindings
                .get(&self.binding_key(name, expr.span))
                .is_none_or(|symbol| !self.has_lower_symbol_override(symbol.id))
        {
            return self.finish_legacy_expr_coercion(expr, param_expr.clone());
        }

        if let Some(member) = self.qualified_member_expression(expr)
            && let ExprKind::Member { receiver, name } = &member.kind
        {
            return self.lower_member_access(expr, receiver, name, param_exprs, param_types);
        }
        if let ExprKind::Member { receiver, name } = &expr.kind {
            return self.lower_member_access(expr, receiver, name, param_exprs, param_types);
        }
        if let ExprKind::Call { callee, args } = &expr.kind {
            let normalized = self.qualified_member_callee(callee);
            let callee = normalized.as_ref().unwrap_or(callee);
            if let ExprKind::Member { receiver, name } = &callee.kind {
                return self.lower_member_call(
                    expr,
                    receiver,
                    name,
                    args,
                    param_exprs,
                    param_types,
                );
            }
        }
        let pine_type = self.type_of_expr_with_params(expr, param_types)?;
        let series_id = self.lower_expr_series_id(expr, pine_type);

        let kind = match &expr.kind {
            ExprKind::Member { .. } => {
                unreachable!("members are lowered with call-specific receiver identity")
            }
            ExprKind::Literal(literal) => HirExprKind::Literal(lower_literal(literal)),
            ExprKind::Identifier(name) => {
                if let Some(kind) = self.lower_legacy_value(expr.span) {
                    kind
                } else {
                    HirExprKind::Symbol(
                        self.bound_symbol(name, expr.span)
                            .or_else(|| {
                                // Omitted UDT fields synthesize `na` without a source token.
                                (name == "na")
                                    .then(|| {
                                        self.scope.all_symbols.iter().find_map(|(name, symbol)| {
                                            (name == "na" && symbol.pine_type.kind == ValueKind::Na)
                                                .then_some(*symbol)
                                        })
                                    })
                                    .flatten()
                            })?
                            .id,
                    )
                }
            }
            ExprKind::QualifiedName(parts) => {
                if let Some(field) = self
                    .type_of_bound_chart_point_field_access(parts, expr.span)
                    .or_else(|| self.type_of_chart_point_field_access(parts))
                {
                    let access = self.chart_point_field_access_for_lowering(parts, expr.span)?;
                    let receiver_symbol = self
                        .bound_symbol(&access.receiver, expr.span)
                        .or_else(|| self.scope.resolve(&access.receiver))?;
                    return self.finish_legacy_expr_coercion(
                        expr,
                        HirExpr {
                            pine_type: field,
                            series_id,
                            kind: HirExprKind::FieldAccess {
                                value: Box::new(
                                    param_exprs
                                        .get(&access.receiver)
                                        .cloned()
                                        .unwrap_or(HirExpr {
                                            kind: HirExprKind::Symbol(receiver_symbol.id),
                                            pine_type: receiver_symbol.pine_type,
                                            series_id: receiver_symbol.series_id,
                                        }),
                                ),
                                index: access.index,
                            },
                        },
                    );
                }
                if let Some(field) = self
                    .type_of_bound_user_type_field_access(parts, expr.span)
                    .or_else(|| self.type_of_user_type_field_access(parts))
                {
                    let access = self.user_type_field_access_for_lowering(parts, expr.span)?;
                    let receiver_symbol = self
                        .bound_symbol(&access.receiver, expr.span)
                        .or_else(|| self.scope.resolve(&access.receiver))?;
                    let mut value = param_exprs
                        .get(&access.receiver)
                        .cloned()
                        .unwrap_or(HirExpr {
                            kind: HirExprKind::Symbol(receiver_symbol.id),
                            pine_type: receiver_symbol.pine_type,
                            series_id: receiver_symbol.series_id,
                        });
                    let last_index = access.fields.len().saturating_sub(1);
                    for (index, field_access) in access.fields.iter().enumerate() {
                        value = HirExpr {
                            pine_type: field_access.pine_type,
                            series_id: (index == last_index).then_some(series_id).flatten(),
                            kind: HirExprKind::FieldAccess {
                                value: Box::new(value),
                                index: field_access.index,
                            },
                        };
                    }
                    debug_assert_eq!(value.pine_type, field);
                    return self.finish_legacy_expr_coercion(expr, value);
                }
                self.lower_legacy_value(expr.span)
                    .unwrap_or_else(|| HirExprKind::Builtin(parts.join(".")))
            }
            ExprKind::Unary { op, expr } => HirExprKind::Unary {
                op: lower_unary_op(*op),
                expr: Box::new(self.lower_expr_with_params(expr, param_exprs, param_types)?),
            },
            ExprKind::Binary { .. } => {
                unreachable!("binary expressions use iterative lowering")
            }
            ExprKind::Ternary {
                condition,
                then_expr,
                else_expr,
            } => HirExprKind::Ternary {
                condition: Box::new(self.lower_expr_with_params(
                    condition,
                    param_exprs,
                    param_types,
                )?),
                then_expr: Box::new(self.lower_expr_with_params(
                    then_expr,
                    param_exprs,
                    param_types,
                )?),
                else_expr: Box::new(self.lower_expr_with_params(
                    else_expr,
                    param_exprs,
                    param_types,
                )?),
            },
            ExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => HirExprKind::Ternary {
                condition: Box::new(self.lower_expr_with_params(
                    condition,
                    param_exprs,
                    param_types,
                )?),
                then_expr: Box::new(self.lower_expr_branch_return(
                    then_branch,
                    param_exprs,
                    param_types,
                )?),
                else_expr: Box::new(self.lower_expr_branch_return(
                    else_branch,
                    param_exprs,
                    param_types,
                )?),
            },
            ExprKind::Switch { selector, arms } => HirExprKind::Switch {
                selector: match selector {
                    Some(selector) => Some(Box::new(self.lower_expr_with_params(
                        selector,
                        param_exprs,
                        param_types,
                    )?)),
                    None => None,
                },
                arms: arms
                    .iter()
                    .map(|arm| {
                        Some(HirSwitchArm {
                            condition: match &arm.condition {
                                Some(condition) => Some(self.lower_expr_with_params(
                                    condition,
                                    param_exprs,
                                    param_types,
                                )?),
                                None => None,
                            },
                            result: self.lower_switch_arm_result_with_params(
                                &arm.result,
                                param_exprs,
                                param_types,
                            )?,
                        })
                    })
                    .collect::<Option<_>>()?,
            },
            ExprKind::For {
                counter,
                from,
                to,
                step,
                body,
            } => {
                let (last, prefix) = body.split_last()?;
                HirExprKind::For {
                    counter: self.lower_decl_symbol(counter, expr.span)?.id,
                    from: Box::new(self.lower_expr_with_params(from, param_exprs, param_types)?),
                    to: Box::new(self.lower_expr_with_params(to, param_exprs, param_types)?),
                    step: match step {
                        Some(step) => Some(Box::new(self.lower_expr_with_params(
                            step,
                            param_exprs,
                            param_types,
                        )?)),
                        None => None,
                    },
                    statements: prefix
                        .iter()
                        .map(|statement| {
                            self.lower_stmt_with_params(statement, param_exprs, param_types)
                        })
                        .collect::<Option<_>>()?,
                    result: Box::new(self.lower_loop_tail(last, param_exprs, param_types)?),
                }
            }
            ExprKind::ForIn {
                index,
                value,
                iterable,
                body,
            } => {
                let (last, prefix) = body.split_last()?;
                let value_symbol = self.lower_decl_symbol(value, expr.span)?;
                if let Some(type_name) =
                    self.user_type_array_name_of_expr_with_params(iterable, param_exprs)
                {
                    self.mark_symbol_id_user_type(value_symbol.id, type_name);
                }
                HirExprKind::ForIn {
                    index: match index {
                        Some(index) => Some(self.lower_decl_symbol(index, expr.span)?.id),
                        None => None,
                    },
                    value: value_symbol.id,
                    iterable: Box::new(self.lower_expr_with_params(
                        iterable,
                        param_exprs,
                        param_types,
                    )?),
                    statements: prefix
                        .iter()
                        .map(|statement| {
                            self.lower_stmt_with_params(statement, param_exprs, param_types)
                        })
                        .collect::<Option<_>>()?,
                    result: Box::new(self.lower_loop_tail(last, param_exprs, param_types)?),
                }
            }
            ExprKind::While { condition, body } => {
                let (last, prefix) = body.split_last()?;
                HirExprKind::While {
                    condition: Box::new(self.lower_expr_with_params(
                        condition,
                        param_exprs,
                        param_types,
                    )?),
                    statements: prefix
                        .iter()
                        .map(|statement| {
                            self.lower_stmt_with_params(statement, param_exprs, param_types)
                        })
                        .collect::<Option<_>>()?,
                    result: Box::new(self.lower_loop_tail(last, param_exprs, param_types)?),
                }
            }
            ExprKind::Tuple(items) => HirExprKind::Tuple(
                items
                    .iter()
                    .map(|item| self.lower_expr_with_params(item, param_exprs, param_types))
                    .collect::<Option<_>>()?,
            ),
            ExprKind::Call { callee, args } => {
                let name = expr_name(callee)?;
                if self.udt_matrix_constructor_name(&name).is_some() {
                    let values = self.udt_matrix_constructor_args(args, expr.span)?;
                    let args = values
                        .iter()
                        .map(|value| {
                            Some(HirCallArg {
                                name: None,
                                value: self.lower_expr_with_params(
                                    value,
                                    param_exprs,
                                    param_types,
                                )?,
                            })
                        })
                        .collect::<Option<Vec<_>>>()?;
                    return Some(HirExpr {
                        pine_type: PineType::new(Qualifier::Series, ValueKind::UserTypeMatrix),
                        series_id,
                        kind: HirExprKind::Call {
                            callee: name,
                            call_site_id: self.alloc_call_site_at(expr.span),
                            args,
                        },
                    });
                }
                if let Some(constructor) = self
                    .user_type_constructor_for_lowering(&name, args, param_types)
                    .or_else(|| {
                        self.imported_user_type_constructor_for_lowering(&name, args, param_types)
                    })
                {
                    let fields = constructor
                        .field_args
                        .iter()
                        .zip(&constructor.field_defaults)
                        .map(|(arg, is_default)| {
                            if *is_default {
                                self.lower_expr_with_params(arg, &HashMap::new(), &HashMap::new())
                            } else {
                                self.lower_expr_with_params(arg, param_exprs, param_types)
                            }
                        })
                        .collect::<Option<_>>()?;
                    let lowered = HirExpr {
                        pine_type,
                        series_id,
                        kind: HirExprKind::UserTypeConstruct {
                            identity: HirUserTypeIdentity {
                                source_id: constructor.identity.source_id.get(),
                                type_name: constructor.identity.name,
                            },
                            fields,
                        },
                    };
                    return self.finish_legacy_expr_coercion(expr, lowered);
                }
                if name == "array.from"
                    && pine_type.kind == ValueKind::UserTypeArray
                    && let Some(type_name) =
                        self.user_type_array_name_of_expr_with_params(expr, param_exprs)
                {
                    let elements = args
                        .iter()
                        .map(|arg| {
                            self.lower_expr_with_params(&arg.value, param_exprs, param_types)
                        })
                        .collect::<Option<_>>()?;
                    let lowered = HirExpr {
                        pine_type,
                        series_id,
                        kind: HirExprKind::UserTypeArrayConstruct {
                            type_name,
                            elements,
                        },
                    };
                    return self.finish_legacy_expr_coercion(expr, lowered);
                }
                if let Some(result) = self.lower_postfix_call_result_method(
                    callee,
                    args,
                    pine_type,
                    series_id,
                    param_exprs,
                    param_types,
                ) {
                    return result
                        .and_then(|lowered| self.finish_legacy_expr_coercion(expr, lowered));
                }
                if matches!(callee.kind, ExprKind::QualifiedName(_))
                    && let Some(method_call) = self.lower_alias_qualified_user_method_call(
                        &name,
                        callee.span,
                        args,
                        param_exprs,
                        param_types,
                    )
                {
                    let pure_call_series_id =
                        pure_series::pure_alias_qualified_user_method_call_series_key(
                            self, &name, args,
                        )
                        .and(series_id);
                    let mut method_call = method_call;
                    if let Some(series_id) = pure_call_series_id {
                        method_call.series_id = Some(series_id);
                    }
                    return self.finish_legacy_expr_coercion(expr, method_call);
                }
                if let Some(method_call) = self.lower_local_qualified_user_method_call(
                    &name,
                    callee.span,
                    args,
                    param_exprs,
                    param_types,
                ) {
                    let pure_call_series_id =
                        pure_series::pure_local_qualified_user_method_call_series_key(
                            self, &name, args,
                        )
                        .and(series_id);
                    let mut method_call = method_call;
                    if let Some(series_id) = pure_call_series_id {
                        method_call.series_id = Some(series_id);
                    }
                    return self.finish_legacy_expr_coercion(expr, method_call);
                }
                if let Some((receiver_name, method_name)) = method_call_parts(callee)
                    && self
                        .bound_symbol(receiver_name, callee.span)
                        .or_else(|| self.scope.resolve(receiver_name))
                        .and_then(|symbol| {
                            self.symbol_user_types.get(&symbol.id).cloned().or_else(|| {
                                (symbol.pine_type.kind == ValueKind::Color
                                    && self.methods.contains_key(&(
                                        "color".to_owned(),
                                        method_name.to_owned(),
                                    )))
                                .then(|| "color".to_owned())
                            })
                        })
                        .is_some()
                {
                    let pure_call_series_id = pure_series::pure_user_method_call_series_key(
                        self,
                        receiver_name,
                        method_name,
                        callee.span,
                        args,
                    )
                    .and(series_id);
                    let mut call = self.lower_user_method_call(
                        receiver_name,
                        method_name,
                        callee.span,
                        args,
                        param_exprs,
                        param_types,
                    )?;
                    if let Some(series_id) = pure_call_series_id {
                        call.series_id = Some(series_id);
                    }
                    return self.finish_legacy_expr_coercion(expr, call);
                }
                if self.functions.contains_key(&name) {
                    let pure_call_series_id =
                        pure_series::pure_udf_call_series_key(self, &name, args, expr.span)
                            .and(series_id);
                    let mut call =
                        self.lower_udf_call(&name, expr.span, args, param_exprs, param_types)?;
                    if let Some(series_id) = pure_call_series_id {
                        call.series_id = Some(series_id);
                    }
                    return self.finish_legacy_expr_coercion(expr, call);
                }
                if let Some(lowered) = self.lower_recorded_legacy_call(
                    callee.span,
                    args,
                    pine_type,
                    series_id,
                    param_exprs,
                    param_types,
                ) {
                    return lowered
                        .and_then(|lowered| self.finish_legacy_expr_coercion(expr, lowered));
                }
                let name = self
                    .legacy
                    .canonical_call_name(self.current_source_context_id(), callee.span)
                    .map_or(name, str::to_owned);
                let canonical_args = self.lower_legacy_call_args(callee.span, args);
                let args = canonical_args.as_deref().unwrap_or(args);
                if pine_builtins::get_phase_1_builtin(&name).is_none()
                    && !name.starts_with("map.")
                    && let Some((receiver_name, method_name)) = method_call_parts(callee)
                    && let Some(builtin_name) = param_types
                        .get(receiver_name)
                        .map(|pine_type| pine_type.kind)
                        .or_else(|| {
                            self.bound_symbol(receiver_name, callee.span)
                                .map(|symbol| symbol.pine_type.kind)
                        })
                        .and_then(|receiver_kind| {
                            drawing_method_builtin_name(receiver_kind, method_name)
                                .or_else(|| {
                                    matrix_method_builtin_name(receiver_kind, method_name)
                                        .map(ToOwned::to_owned)
                                })
                                .or_else(|| {
                                    (receiver_kind == ValueKind::Map)
                                        .then(|| map_method_builtin_name(method_name))
                                        .flatten()
                                        .map(ToOwned::to_owned)
                                })
                        })
                        .or_else(|| array_method_builtin_name(method_name).map(ToOwned::to_owned))
                {
                    let receiver_arg = receiver_call_arg(receiver_name, callee.span);
                    let mut method_args = Vec::with_capacity(args.len() + 1);
                    method_args.push(receiver_arg);
                    method_args.extend(args.iter().cloned());
                    let lowered_args = self.lower_builtin_call_args(
                        &builtin_name,
                        &method_args,
                        param_exprs,
                        param_types,
                    )?;
                    let call_site_id = self.alloc_call_site_at(expr.span);
                    return self.finish_legacy_expr_coercion(
                        expr,
                        HirExpr {
                            pine_type,
                            series_id,
                            kind: HirExprKind::Call {
                                callee: builtin_name,
                                call_site_id,
                                args: lowered_args,
                            },
                        },
                    );
                }
                let call_site_id = self.alloc_call_site_at(expr.span);
                let lowered_args =
                    self.lower_builtin_call_args(&name, args, param_exprs, param_types)?;
                if name == "request.security_lower_tf" && pine_type.kind == ValueKind::Tuple {
                    let tuple_types = self.tuple_element_types(&args[2].value)?;
                    self.lower_tf_tuple_types.push((
                        call_site_id,
                        tuple_types.into_iter().map(|ty| ty.kind).collect(),
                    ));
                }
                HirExprKind::Call {
                    callee: name,
                    call_site_id,
                    args: lowered_args,
                }
            }
            ExprKind::History { expr, offset } => {
                let offset = match self
                    .lowering_history_offset_constant(offset, param_exprs)
                    .and_then(|value| u32::try_from(value).ok())
                {
                    Some(offset) => HirHistoryOffset::Constant(offset),
                    None => HirHistoryOffset::Dynamic(Box::new(self.lower_expr_with_params(
                        offset,
                        param_exprs,
                        param_types,
                    )?)),
                };
                let mut lowered_expr =
                    self.lower_expr_with_params(expr, param_exprs, param_types)?;
                if lowered_expr.series_id.is_none() {
                    lowered_expr.series_id = self.lower_expr_series_id(
                        expr,
                        PineType::new(Qualifier::Series, lowered_expr.pine_type.kind),
                    );
                }
                HirExprKind::History {
                    expr: Box::new(lowered_expr),
                    offset,
                }
            }
            ExprKind::Group(_) => {
                unreachable!("grouped expressions are lowered before this match")
            }
        };

        self.finish_legacy_expr_coercion(
            expr,
            HirExpr {
                kind,
                pine_type,
                series_id,
            },
        )
    }

    /// Analysis-time symbol constants may belong to another call of an inlined
    /// function, including constants propagated through local aliases. Within a
    /// parameterized body fold only syntax-local constants; evaluate all other
    /// offsets with the current call's lowered bindings.
    pub(super) fn lowering_history_offset_constant(
        &self,
        offset: &Expr,
        param_exprs: &HashMap<String, HirExpr>,
    ) -> Option<i64> {
        if param_exprs.is_empty() {
            self.known_history_offset_int_value(offset)
        } else {
            crate::types::const_int_value(offset)
        }
    }

    fn lower_expr_series_id(
        &mut self,
        expr: &Expr,
        pine_type: PineType,
    ) -> Option<pine_ir::SeriesId> {
        if (pine_type.qualifier != Qualifier::Series && !is_collection_kind(pine_type.kind))
            || pine_type.kind == ValueKind::Tuple
        {
            return None;
        }

        if let ExprKind::Identifier(name) = &expr.kind {
            if let Some(canonical_name) = self
                .legacy
                .canonical_value_name(self.current_source_context_id(), expr.span)
            {
                if let Some(symbol) = self.scope.resolve(canonical_name) {
                    if symbol.series_id.is_some() {
                        return symbol.series_id;
                    }
                } else if pine_type.qualifier != Qualifier::Series {
                    return None;
                }
            } else if let Some(series_id) = self
                .bound_symbol(name, expr.span)
                .and_then(|symbol| symbol.series_id)
            {
                return Some(series_id);
            } else if pine_type.qualifier != Qualifier::Series {
                return None;
            }

            if let Some(series_id) = pure_series::pure_expr_series_id(self, expr) {
                return Some(series_id);
            }
            return Some(self.alloc_series());
        }

        if pine_type.qualifier == Qualifier::Series
            && !is_collection_kind(pine_type.kind)
            && let Some(series_id) = pure_series::pure_expr_series_id(self, expr)
        {
            return Some(series_id);
        }

        Some(self.alloc_series())
    }

    fn pure_expr_series_key(&self, expr: &Expr) -> Option<String> {
        pure_series::pure_expr_series_key(self, expr)
    }
}
