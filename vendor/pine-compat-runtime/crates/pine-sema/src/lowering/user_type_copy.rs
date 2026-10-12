use super::*;

impl Analyzer {
    pub(super) fn lower_udt_copy(
        &mut self,
        expr: &Expr,
        receiver: &Expr,
        param_exprs: &HashMap<String, HirExpr>,
        param_types: &HashMap<String, PineType>,
    ) -> Option<HirExpr> {
        let type_name = self.user_type_name_of_expr_with_params(receiver, param_exprs)?;
        let identity = self.user_type_identity_for_name(&type_name)?;
        let names = if let Some(ty) = self.user_types.get(&type_name) {
            ty.fields
                .iter()
                .map(|field| field.name.clone())
                .collect::<Vec<_>>()
        } else {
            self.imported_user_types
                .get(&type_name)?
                .fields
                .iter()
                .map(|field| field.name.clone())
                .collect()
        };
        let value = self.lower_expr_with_params(receiver, param_exprs, param_types)?;
        if !self.record_lowering_temp_symbol(expr.span) {
            return None;
        }
        let temp = self.fresh_temp_symbol("UDT.copy.receiver", value.pine_type);
        let mut fields = Vec::new();
        for name in names {
            let (ty, _, path) =
                self.user_type_field_path(&type_name, Qualifier::Series, &[name])?;
            fields.push(HirExpr {
                pine_type: ty,
                series_id: None,
                kind: HirExprKind::FieldAccess {
                    value: Box::new(HirExpr {
                        pine_type: value.pine_type,
                        series_id: None,
                        kind: HirExprKind::Symbol(temp.id),
                    }),
                    index: path[0].index,
                },
            });
        }
        let pine_type = PineType::new(Qualifier::Series, ValueKind::UserType);
        let result = HirExpr {
            pine_type,
            series_id: None,
            kind: HirExprKind::UserTypeConstruct {
                identity: HirUserTypeIdentity {
                    source_id: identity.source_id.get(),
                    type_name: identity.name,
                },
                fields,
            },
        };
        Some(HirExpr {
            pine_type,
            series_id: self.lower_expr_series_id(expr, pine_type),
            kind: HirExprKind::Block {
                statements: vec![HirStmt {
                    kind: HirStmtKind::Decl {
                        symbol: temp.id,
                        value,
                    },
                }],
                result: Box::new(result),
            },
        })
    }
}
