use crate::prelude::*;

impl Analyzer {
    pub(crate) fn udt_copy_receiver(
        &self,
        callee: &Expr,
        args: &[CallArg],
        params: &HashMap<String, PineType>,
    ) -> Option<(Expr, Option<String>, bool)> {
        let (receiver, expected, valid) = match &callee.kind {
            ExprKind::Member { receiver, name } if name == "copy" => {
                ((**receiver).clone(), None, args.is_empty())
            }
            ExprKind::QualifiedName(parts) if parts.last().is_some_and(|name| name == "copy") => {
                let owner = parts[..parts.len() - 1].join(".");
                if crate::analyzer::calls::postfix_call_result_method_parts(callee, args).is_some()
                {
                    (args[0].value.clone(), None, args.len() == 1)
                } else if self.user_types.contains_key(&owner)
                    || self.imported_user_types.contains_key(&owner)
                {
                    let receiver = args
                        .first()
                        .map(|arg| arg.value.clone())
                        .unwrap_or_else(|| Expr {
                            kind: ExprKind::Identifier("na".to_owned()),
                            span: callee.span,
                        });
                    (
                        receiver,
                        Some(owner),
                        args.len() == 1
                            && args[0].name.as_deref().is_none_or(|name| name == "object"),
                    )
                } else {
                    let mut receiver = callee.clone();
                    receiver.kind = if parts.len() == 2 {
                        ExprKind::Identifier(owner)
                    } else {
                        ExprKind::QualifiedName(parts[..parts.len() - 1].to_vec())
                    };
                    (receiver, None, args.is_empty())
                }
            }
            _ => return None,
        };
        if expected
            .clone()
            .or_else(|| self.user_type_name_of_expr(&receiver))
            .is_some_and(|name| self.methods.contains_key(&(name, "copy".to_owned())))
        {
            return None;
        }
        if expected.is_none()
            && !self
                .type_of_expr_with_params(&receiver, params)
                .is_some_and(|ty| ty.kind == ValueKind::UserType)
        {
            return None;
        }
        Some((receiver, expected, valid))
    }

    pub(crate) fn analyze_udt_copy(
        &mut self,
        callee: &Expr,
        args: &[CallArg],
        span: Span,
    ) -> Option<Option<PineType>> {
        let diagnostic_start = self.diagnostics.len();
        if let ExprKind::Member { receiver, name } = &callee.kind
            && name == "copy"
        {
            if self.analyze_expr(receiver).is_none() {
                return Some(None);
            }
        } else if crate::analyzer::calls::postfix_call_result_method_parts(callee, args)
            .is_some_and(|(_, name)| name == "copy")
            && self.analyze_expr(&args[0].value).is_none()
        {
            return Some(None);
        }
        if self.diagnostics[diagnostic_start..]
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error)
        {
            return Some(None);
        }
        let (receiver, expected, valid) = self.udt_copy_receiver(callee, args, &HashMap::new())?;
        if !valid {
            self.diagnostics.push(Diagnostic::error("E_UDT_COPY_ARG","UDT copy expects one object argument for type.copy, or no arguments for object.copy",span));
            return Some(None);
        }
        let ty = self.analyze_expr(&receiver)?;
        let identity = self.user_type_name_of_expr(&receiver);
        if ty.kind != ValueKind::UserType
            || identity.is_none()
            || expected.as_ref().is_some_and(|name| {
                self.user_type_identity_for_name(name)
                    != identity
                        .as_ref()
                        .and_then(|name| self.user_type_identity_for_name(name))
            })
        {
            self.diagnostics.push(Diagnostic::error(
                "E_UDT_COPY_ARG",
                "UDT copy receiver must have the matching user-defined type",
                span,
            ));
            return Some(None);
        }
        self.mark_expr_user_type(span, identity.unwrap());
        Some(Some(PineType::new(Qualifier::Series, ValueKind::UserType)))
    }
}
