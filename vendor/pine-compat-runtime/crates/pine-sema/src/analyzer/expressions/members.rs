use crate::analyzer::calls::{array_method_builtin_name, drawing_method_builtin_name};
use crate::analyzer::chart_points::{chart_point_field_index, chart_point_field_type};
use crate::prelude::*;

impl Analyzer {
    pub(crate) fn reject_direct_udt_field_history(&mut self, value: &Expr) -> bool {
        if self.legacy.dialect() < crate::PineDialect::V6 {
            return false;
        }
        let is_udt_field = match &value.without_groups().kind {
            ExprKind::Member { receiver, .. } => self
                .type_of_expr_with_params(receiver, &HashMap::new())
                .is_some_and(|ty| ty.kind == ValueKind::UserType),
            ExprKind::QualifiedName(parts) if parts.len() >= 2 => self
                .bound_symbol(&parts[0], value.span)
                .or_else(|| self.scope.resolve(&parts[0]))
                .is_some_and(|symbol| {
                    if symbol.pine_type.kind != ValueKind::UserType {
                        return false;
                    }
                    if parts.len() == 2 {
                        return true;
                    }
                    self.symbol_user_types
                        .get(&symbol.id)
                        .and_then(|identity| {
                            self.user_type_field_path(
                                identity,
                                symbol.pine_type.qualifier,
                                &parts[1..parts.len() - 1],
                            )
                        })
                        .is_some_and(|(ty, _, _)| ty.kind == ValueKind::UserType)
                }),
            _ => false,
        };
        if is_udt_field {
            self.diagnostics.push(Diagnostic::error("E_UDT_FIELD_HISTORY","Pine v6 does not allow history directly on a UDT field; reference the object's history or assign the field to a variable first",value.span));
        }
        is_udt_field
    }

    pub(crate) fn argument_has_side_effect(&self, expr: &Expr) -> bool {
        let mut effect = false;
        crate::modules::visit_expression(expr, &mut |node| {
            let ExprKind::Call { callee, .. } = &node.kind else {
                return;
            };
            let normalized = self.qualified_member_callee(callee);
            let callee = normalized.as_ref().unwrap_or(callee);
            let name = if let ExprKind::Member { receiver, name } = &callee.kind {
                self.type_of_expr_with_params(receiver, &HashMap::new())
                    .and_then(|ty| Self::member_builtin_name(ty.kind, name))
            } else {
                expr_name(callee)
            };
            effect |= name.as_deref().is_some_and(|name| {
                is_output_or_declaration_builtin(name)
                    || is_array_mutation_builtin(name)
                    || is_array_mutation_method_call_name(name)
                    || is_map_mutation_builtin(name)
                    || is_map_mutation_method_call_name(name)
            });
        });
        effect
    }

    pub(crate) fn argument_has_disallowed_udf_side_effect(&self, expr: &Expr) -> bool {
        let mut effect = false;
        crate::modules::visit_expression(expr, &mut |node| {
            let ExprKind::Call { callee, .. } = &node.kind else {
                return;
            };
            let normalized = self.qualified_member_callee(callee);
            let callee = normalized.as_ref().unwrap_or(callee);
            let name = if let ExprKind::Member { receiver, name } = &callee.kind {
                self.type_of_expr_with_params(receiver, &HashMap::new())
                    .and_then(|ty| Self::member_builtin_name(ty.kind, name))
            } else {
                expr_name(callee)
            };
            effect |= name.as_deref().is_some_and(|name| {
                (is_output_or_declaration_builtin(name)
                    || is_array_mutation_builtin(name)
                    || is_array_mutation_method_call_name(name)
                    || is_map_mutation_builtin(name)
                    || is_map_mutation_method_call_name(name))
                    && !(name.ends_with(".new")
                        && self.allows_udf_output_or_declaration_side_effect(name))
            });
        });
        effect
    }

    pub(crate) fn qualified_member_callee(&self, callee: &Expr) -> Option<Expr> {
        if !matches!(&callee.kind,ExprKind::QualifiedName(parts) if parts.len()>=3) {
            return None;
        }
        self.qualified_member_expression(callee)
    }

    pub(crate) fn qualified_member_expression(&self, callee: &Expr) -> Option<Expr> {
        let ExprKind::QualifiedName(parts) = &callee.kind else {
            return None;
        };
        if parts.len() < 2 {
            return None;
        }
        let source_id = self
            .source_context_origins
            .get(&self.current_source_context_id())?
            .0;
        let text = self
            .source_texts
            .get(&source_id)?
            .text()
            .get(callee.span.start..callee.span.end)?;
        let fragment = pine_syntax::SourceFile::new("qualified member", text);
        let spans = pine_syntax::lex(&fragment)
            .tokens
            .into_iter()
            .filter_map(|token| {
                matches!(token.kind, pine_syntax::TokenKind::Identifier(_)).then_some(Span {
                    start: callee.span.start + token.span.start,
                    end: callee.span.start + token.span.end,
                })
            })
            .collect::<Vec<_>>();
        if spans.len() != parts.len() {
            return None;
        }
        let first_span = spans[0];
        let symbol = self
            .bound_symbol(&parts[0], first_span)
            .or_else(|| self.scope.resolve(&parts[0]))?;
        if !matches!(
            symbol.pine_type.kind,
            ValueKind::UserType | ValueKind::ChartPoint
        ) {
            return None;
        }
        let mut receiver = Expr {
            kind: ExprKind::Identifier(parts[0].clone()),
            span: first_span,
        };
        for (index, name) in parts.iter().enumerate().skip(1) {
            let end = spans[index].end;
            let span = Span {
                start: callee.span.start,
                end,
            };
            receiver = Expr {
                kind: ExprKind::Member {
                    receiver: Box::new(receiver),
                    name: name.clone(),
                },
                span,
            };
        }
        Some(receiver)
    }

    pub(crate) fn analyze_member(
        &mut self,
        expr: &Expr,
        receiver: &Expr,
        name: &str,
    ) -> Option<PineType> {
        let key = self.expr_key(expr.span);
        self.expr_types.remove(&key);
        self.expr_user_types.remove(&key);
        self.expr_user_type_identities.remove(&key);
        let receiver_type = self.analyze_expr(receiver)?;
        let (pine_type, type_name, _index) = match receiver_type.kind {
            ValueKind::ChartPoint => {
                let Some(index) = chart_point_field_index(name) else {
                    self.diagnostics.push(Diagnostic::error(
                        "E_CHART_POINT_UNKNOWN_FIELD",
                        format!("unknown chart.point field `{name}`"),
                        expr.span,
                    ));
                    return None;
                };
                (chart_point_field_type(receiver_type, name)?, None, index)
            }
            ValueKind::UserType => {
                let Some(type_name) = self.user_type_name_of_expr(receiver) else {
                    self.diagnostics.push(Diagnostic::error(
                        "E_UDT_UNKNOWN_FIELD",
                        format!("cannot resolve receiver type for field `{name}`"),
                        expr.span,
                    ));
                    return None;
                };
                let names = [name.to_owned()];
                let (ty, nested, fields) = if self.imported_user_types.contains_key(&type_name) {
                    self.resolve_imported_user_type_field_path(
                        &type_name,
                        receiver_type.qualifier,
                        &names,
                        expr.span,
                    )?
                } else {
                    self.resolve_user_type_field_path(
                        &type_name,
                        receiver_type.qualifier,
                        &names,
                        expr.span,
                    )?
                };
                (ty, nested, fields.first()?.index)
            }
            _ => {
                self.diagnostics.push(Diagnostic::error(
                    "E_UDT_UNKNOWN_FIELD",
                    format!(
                        "cannot read field `{name}` from {}",
                        pine_type_name(receiver_type)
                    ),
                    expr.span,
                ));
                return None;
            }
        };
        self.expr_types.insert(key, pine_type);
        if let Some(name) = type_name {
            self.mark_expr_user_type(expr.span, name);
        }
        Some(pine_type)
    }

    pub(crate) fn analyze_member_call(
        &mut self,
        callee: &Expr,
        receiver: &Expr,
        name: &str,
        args: &[CallArg],
        span: Span,
    ) -> Option<PineType> {
        self.expr_types.remove(&self.expr_key(span));
        let receiver_type = self.analyze_expr(receiver)?;
        let canonical = Self::member_builtin_name(receiver_type.kind, name);
        let Some(canonical) = canonical else {
            self.diagnostics.push(Diagnostic::error(
                "E_METHOD_RECEIVER_TYPE",
                format!(
                    "method `{name}` is not supported for {}",
                    pine_type_name(receiver_type)
                ),
                callee.span,
            ));
            return None;
        };
        if let Some(min) = crate::PineDialect::qualified_builtin_min_version(&canonical, true)
            && self.legacy.dialect().version() < min
        {
            self.reject_unavailable_legacy_builtin(&canonical, min, callee.span);
            return None;
        }
        let Some(signature) = pine_builtins::get_phase_1_builtin(&canonical) else {
            self.unsupported(&canonical, "member method is not executable", callee.span);
            return None;
        };
        let mut call_args = vec![CallArg {
            name: None,
            value: receiver.clone(),
            span: receiver.span,
        }];
        call_args.extend_from_slice(args);
        let mut types = vec![Some(receiver_type)];
        types.extend(args.iter().map(|arg| self.analyze_expr(&arg.value)));
        let result = self.analyze_registered_builtin(
            &canonical,
            signature,
            callee.span,
            span,
            &call_args,
            &types,
        );
        if let Some(ty) = result {
            self.expr_types.insert(self.expr_key(span), ty);
        }
        result
    }

    pub(crate) fn member_builtin_name(kind: ValueKind, name: &str) -> Option<String> {
        drawing_method_builtin_name(kind, name)
            .or_else(|| matrix_method_builtin_name(kind, name).map(str::to_owned))
            .or_else(|| {
                kind.array_element_kind()
                    .and_then(|_| array_method_builtin_name(name))
                    .map(str::to_owned)
            })
    }
}
