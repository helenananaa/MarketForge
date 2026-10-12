use crate::prelude::*;

impl Analyzer {
    pub(crate) fn validate_udt_matrix_operation(
        &mut self,
        signature: &BuiltinSignature,
        args: &[CallArg],
        types: &[Option<PineType>],
    ) {
        if !signature.name.starts_with("matrix.")
            || !types
                .iter()
                .flatten()
                .any(|ty| ty.kind == ValueKind::UserTypeMatrix)
        {
            return;
        }
        let method = signature.name.strip_prefix("matrix.").unwrap();
        if crate::types::matrix_method_builtin_name(ValueKind::UserTypeMatrix, method).is_none() {
            self.diagnostics.push(Diagnostic::error(
                "E_CALL_ARG_TYPE",
                "operation does not accept a UDT matrix",
                args.first().map_or(Span::default(), |arg| arg.span),
            ));
            return;
        }
        let slot = |name: &str| -> Option<&CallArg> {
            let index = signature
                .params
                .iter()
                .position(|param| param.name == name)?;
            args.iter()
                .find(|arg| arg.name.as_deref() == Some(name))
                .or_else(|| args.get(index).filter(|arg| arg.name.is_none()))
        };
        let Some(expected) = slot("id")
            .or_else(|| slot("id1"))
            .and_then(|arg| self.user_type_matrix_name(&arg.value))
        else {
            if !matches!(method, "rows" | "columns" | "elements_count" | "is_square") {
                self.diagnostics.push(Diagnostic::error(
                    "E_UDT_MATRIX_ARG",
                    "matrix operation requires a known UDT element identity",
                    args.first().map_or(Span::default(), |arg| arg.span),
                ));
            }
            return;
        };
        for (name, kind) in [("value", 0), ("array_id", 1), ("id2", 2)] {
            let Some(arg) = slot(name) else {
                continue;
            };
            if self
                .type_of_expr_with_params(&arg.value, &HashMap::new())
                .is_some_and(|ty| ty.kind == ValueKind::Na)
            {
                continue;
            }
            let actual = match kind {
                0 => self.user_type_name_of_expr(&arg.value),
                1 => self.user_type_array_name_of_expr(&arg.value),
                _ => self.user_type_matrix_name(&arg.value),
            };
            if actual
                .as_ref()
                .and_then(|name| self.user_type_identity_for_name(name))
                != self.user_type_identity_for_name(&expected)
            {
                self.diagnostics.push(Diagnostic::error(
                    "E_UDT_MATRIX_ARG",
                    "matrix operation changes the UDT element identity",
                    arg.span,
                ));
            }
        }
    }
    pub(crate) fn udt_matrix_constructor_name<'a>(&self, name: &'a str) -> Option<&'a str> {
        let element = name.strip_prefix("matrix.new<")?.strip_suffix('>')?;
        (self.local_user_type_array_is_supported(element)
            || self.imported_user_type_array_is_supported(element))
        .then_some(element)
    }

    pub(crate) fn udt_matrix_constructor_args(
        &self,
        args: &[CallArg],
        span: Span,
    ) -> Option<Vec<Expr>> {
        let mut slots = vec![None; 3];
        let mut named = false;
        for (position, arg) in args.iter().enumerate() {
            let index = if let Some(name) = &arg.name {
                named = true;
                match name.as_str() {
                    "rows" => 0,
                    "columns" => 1,
                    "initial_value" => 2,
                    _ => return None,
                }
            } else {
                if named {
                    return None;
                }
                position
            };
            if index >= 3 || slots[index].is_some() {
                return None;
            }
            slots[index] = Some(arg.value.clone());
        }
        Some(
            slots
                .into_iter()
                .enumerate()
                .map(|(index, value)| {
                    value.unwrap_or_else(|| Expr {
                        span,
                        kind: if index < 2 {
                            ExprKind::Literal(pine_syntax::Literal::Int(0))
                        } else {
                            ExprKind::Identifier("na".to_owned())
                        },
                    })
                })
                .collect(),
        )
    }

    pub(crate) fn analyze_udt_matrix_constructor(
        &mut self,
        name: &str,
        args: &[CallArg],
        span: Span,
    ) -> Option<Option<PineType>> {
        let element = self.udt_matrix_constructor_name(name)?.to_owned();
        let Some(values) = self.udt_matrix_constructor_args(args, span) else {
            self.diagnostics.push(Diagnostic::error(
                "E_UDT_MATRIX_ARG",
                "invalid UDT matrix constructor arguments",
                span,
            ));
            return Some(None);
        };
        for (index, value) in values.iter().enumerate() {
            let ty = self.analyze_expr(value);
            let valid = ty.is_some_and(|ty| {
                if index < 2 {
                    ty.kind == ValueKind::Int
                } else {
                    ty.kind == ValueKind::Na
                        || self
                            .user_type_name_of_expr(value)
                            .and_then(|name| self.user_type_identity_for_name(&name))
                            == self.user_type_identity_for_name(&element)
                }
            });
            if !valid {
                self.diagnostics.push(Diagnostic::error("E_UDT_MATRIX_ARG","matrix dimensions must be integers and initial value must match the UDT element type",value.span));
            }
        }
        self.expr_user_type_matrices
            .insert(self.expr_key(span), element);
        Some(Some(PineType::new(
            Qualifier::Series,
            ValueKind::UserTypeMatrix,
        )))
    }

    pub(crate) fn user_type_matrix_name(&self, expr: &Expr) -> Option<String> {
        if let Some(name) = self.expr_user_type_matrices.get(&self.expr_key(expr.span)) {
            return Some(name.clone());
        }
        match &expr.kind {
            ExprKind::Identifier(name) => self
                .bound_symbol(name, expr.span)
                .or_else(|| self.scope.resolve(name))
                .and_then(|symbol| self.symbol_user_type_matrices.get(&symbol.id).cloned()),
            ExprKind::Group(value) | ExprKind::History { expr: value, .. } => {
                self.user_type_matrix_name(value)
            }
            ExprKind::Call { callee, .. } => self
                .udt_matrix_constructor_name(&crate::analyzer::calls::expr_name(callee)?)
                .map(str::to_owned),
            _ => None,
        }
    }

    pub(crate) fn record_udt_matrix_statement(&mut self, statement: &Stmt) {
        let (name, value, declared) = match &statement.kind {
            StmtKind::Decl {
                name,
                value,
                declared_type,
                ..
            } => (name, value, declared_type.as_ref()),
            StmtKind::Reassign { name, value } => (name, value, None),
            _ => return,
        };
        let Some(symbol) = self
            .bound_symbol(name, statement.span)
            .or_else(|| self.scope.resolve(name))
        else {
            return;
        };
        if symbol.pine_type.kind != ValueKind::UserTypeMatrix {
            return;
        }
        let actual = self.user_type_matrix_name(value);
        let identity = if let Some(pine_syntax::DeclaredType::Matrix { element_type }) = declared {
            if actual
                .as_ref()
                .and_then(|name| self.user_type_identity_for_name(name))
                != self.user_type_identity_for_name(element_type)
                && !self
                    .type_of_expr_with_params(value, &HashMap::new())
                    .is_some_and(|ty| ty.kind == ValueKind::Na)
            {
                self.diagnostics.push(Diagnostic::error(
                    "E_UDT_MATRIX_ARG",
                    "matrix initializer must have the declared UDT element identity",
                    statement.span,
                ));
            }
            Some(element_type.clone())
        } else {
            actual
        };
        if let Some(identity) = identity {
            if let Some(old) = self.symbol_user_type_matrices.get(&symbol.id)
                && self.user_type_identity_for_name(old)
                    != self.user_type_identity_for_name(&identity)
            {
                self.diagnostics.push(Diagnostic::error(
                    "E_UDT_MATRIX_ARG",
                    "matrix assignment changes the UDT element identity",
                    statement.span,
                ));
            } else {
                self.symbol_user_type_matrices.insert(symbol.id, identity);
            }
        }
    }

    pub(crate) fn mark_udt_matrix_result(&mut self, name: &str, span: Span, args: &[CallArg]) {
        if !name.starts_with("matrix.") {
            return;
        }
        let arg = args
            .iter()
            .find(|arg| matches!(arg.name.as_deref(), Some("id" | "id1")))
            .or_else(|| args.first());
        let Some(identity) = arg.and_then(|arg| self.user_type_matrix_name(&arg.value)) else {
            return;
        };
        match name {
            "matrix.get" => self.mark_expr_user_type(span, identity),
            "matrix.row" | "matrix.col" | "matrix.remove_row" | "matrix.remove_col" => {
                self.mark_expr_user_type_array(span, identity)
            }
            "matrix.copy" | "matrix.transpose" | "matrix.submatrix" | "matrix.concat" => {
                self.expr_user_type_matrices
                    .insert(self.expr_key(span), identity);
            }
            _ => {}
        }
    }
}
