use super::*;

impl Analyzer {
    pub(crate) fn lower_program(&mut self, program: &Program) -> Option<HirProgram> {
        debug_assert!(self.source_context_stack_is_restored());
        self.lower_reassigned_symbols = self.collect_lower_reassigned_symbols(&program.statements);
        let mut statements = Vec::new();
        for statement in self
            .legacy_v2_declaration_plan
            .lowering_order(&program.statements)
        {
            if matches!(
                statement.kind,
                StmtKind::Function { .. }
                    | StmtKind::Import(_)
                    | StmtKind::Library(_)
                    | StmtKind::Export(_)
                    | StmtKind::UserType(_)
                    | StmtKind::Method(_)
                    | StmtKind::Unsupported { .. }
            ) {
                continue;
            }
            statements.push(self.lower_stmt(statement)?);
        }
        if self.has_errors() {
            return None;
        }

        let symbols = self.lower_symbols();
        let history = infer_history_requirements(&statements, &symbols);
        let max_bars_back = infer_max_bars_back(&statements);
        let series_max_bars_back = infer_series_max_bars_back(&statements);
        let mut execution_scoped_series = self
            .execution_scoped_series_ids
            .iter()
            .copied()
            .collect::<Vec<_>>();
        execution_scoped_series.sort_unstable();
        let script_mode = self
            .script_declaration
            .map_or(ScriptMode::Indicator, |(mode, _)| mode);
        let strategy_settings = if script_mode == ScriptMode::Strategy {
            self.strategy_settings
                .with_language_defaults(self.compatibility.language_version)
        } else {
            self.strategy_settings
        };
        let program = HirProgram {
            language_version: self.compatibility.language_version,
            script_mode,
            timenow_symbol: self.timenow_symbol,
            strategy_settings,
            drawing_settings: self.drawing_settings,
            user_types: self.lower_user_types(),
            symbols,
            statements,
            next_series_id: self.next_series_id,
            next_call_site_id: self.next_call_site_id,
            call_site_sources: self.call_site_sources.clone(),
            lower_tf_tuple_types: self.lower_tf_tuple_types.clone(),
            next_var_slot_id: self.next_var_slot_id,
            max_bars_back,
            calc_bars_count: self.calc_bars_count,
            series_max_bars_back,
            history: history.program,
            series_history: history.series,
            execution_scoped_series,
        };
        debug_assert!(self.source_context_stack_is_restored());
        Some(program)
    }

    fn lower_user_types(&self) -> Vec<HirUserTypeInfo> {
        let mut local_user_types = self.user_types.values().collect::<Vec<_>>();
        local_user_types.sort_by(|left, right| left.name.cmp(&right.name));
        let mut imported_user_types = self.imported_user_types.iter().collect::<Vec<_>>();
        imported_user_types.sort_by(|left, right| left.0.cmp(right.0));

        let mut user_types = local_user_types
            .into_iter()
            .map(|user_type| {
                let type_name = user_type.name.clone();
                HirUserTypeInfo {
                    declaration_name: user_type.identity.name.clone(),
                    identity: HirUserTypeIdentity {
                        source_id: user_type.identity.source_id.get(),
                        type_name: type_name.clone(),
                    },
                    fields: user_type
                        .fields
                        .iter()
                        .map(|field| HirUserTypeField {
                            varip: field.varip,
                            name: field.name.clone(),
                            user_type_name: field.user_type_name.clone(),
                        })
                        .collect(),
                }
            })
            .collect::<Vec<_>>();

        user_types.extend(
            imported_user_types
                .into_iter()
                .map(|(type_name, user_type)| HirUserTypeInfo {
                    declaration_name: user_type.identity.name.clone(),
                    identity: HirUserTypeIdentity {
                        source_id: user_type.identity.source_id.get(),
                        type_name: type_name.clone(),
                    },
                    fields: user_type
                        .fields
                        .iter()
                        .map(|field| HirUserTypeField {
                            varip: field.varip,
                            name: field.name.clone(),
                            user_type_name: field.pine_type.is_none().then(|| {
                                self.imported_user_types
                                    .iter()
                                    .find(|(_, nested)| {
                                        nested.identity.source_id == user_type.identity.source_id
                                            && nested.identity.name == field.type_name
                                    })
                                    .map_or_else(
                                        || field.type_name.clone(),
                                        |(name, _)| name.clone(),
                                    )
                            }),
                        })
                        .collect(),
                }),
        );
        user_types
    }
}
