use crate::*;

impl<'a> HistoricalRuntime<'a> {
    pub(crate) fn eval_decl(
        &mut self,
        symbol: SymbolId,
        value: &HirExpr,
    ) -> Result<PineValue, RuntimeError> {
        let Some((_persistence, var_slot_id)) = self.persistent_slot_for_symbol(symbol) else {
            return self.eval_expr(value);
        };

        if let Some(value) = self.var_store.get(&var_slot_id).cloned() {
            Ok(value)
        } else {
            let value = self.eval_expr(value)?;
            self.var_store.insert(var_slot_id, value.clone());
            Ok(value)
        }
    }

    pub(crate) fn assign_persistent_symbol(&mut self, symbol: SymbolId, value: PineValue) {
        if let Some((_persistence, var_slot_id)) = self.persistent_slot_for_symbol(symbol) {
            self.var_store.insert(var_slot_id, value);
        }
    }

    pub(crate) fn seed_intrabar_persistence_from(
        &mut self,
        previous: &Self,
    ) -> Result<(), RuntimeError> {
        let mut object_roots = Vec::new();
        for var_slot_id in self.persistent_slots_for_kind(PersistenceKind::Varip) {
            if let Some(value) = previous.var_store.get(&var_slot_id).cloned() {
                object_roots.push(value.clone());
                self.var_store.insert(var_slot_id, value);
            }
        }
        self.seed_intrabar_objects_from(previous, object_roots)
    }

    pub(crate) fn persistent_slot_for_symbol(
        &self,
        symbol_id: SymbolId,
    ) -> Option<(PersistenceKind, VarSlotId)> {
        let symbol = &self.program.symbols[self.metadata.symbol_index(symbol_id)?];

        match symbol.persistence {
            PersistenceKind::None => None,
            PersistenceKind::Var | PersistenceKind::Varip => symbol
                .var_slot_id
                .map(|var_slot_id| (symbol.persistence, var_slot_id)),
        }
    }

    fn persistent_slots_for_kind(&self, kind: PersistenceKind) -> Vec<VarSlotId> {
        self.metadata.persistent_slots(kind).to_vec()
    }

    pub(crate) fn seed_intrabar_array_from(&mut self, previous: &Self, id: u32) {
        self.next_array_id = self.next_array_id.max(id.saturating_add(1));
        if let Some(slice) = previous.array_slices.get(&id).copied() {
            let Some(kind) = previous.array_kinds.get(&id).copied() else {
                return;
            };
            self.array_slices.insert(id, slice);
            self.array_kinds.insert(id, kind);
            self.copy_array_user_type_metadata_from(previous, id, id);
            self.seed_intrabar_array_from(previous, slice.parent_id);
            return;
        }
        let Some(kind) = previous.array_kinds.get(&id).copied() else {
            return;
        };
        if !self.array_store.copy_entry_from(&previous.array_store, id) {
            return;
        }
        self.array_kinds.insert(id, kind);
        self.copy_array_user_type_metadata_from(previous, id, id);
    }

    pub(crate) fn seed_intrabar_map_from(&mut self, previous: &Self, id: u32) {
        self.next_map_id = self.next_map_id.max(id.saturating_add(1));
        self.map_store.copy_entry_from(&previous.map_store, id);
    }

    pub(crate) fn seed_intrabar_matrix_from(&mut self, previous: &Self, id: u32) {
        self.next_matrix_id = self.next_matrix_id.max(id.saturating_add(1));
        self.matrix_store
            .copy_entry_from(&previous.matrix_store, id);
    }
}

#[cfg(test)]
mod tests {
    use pine_ir::{
        DrawingSettings, HirHistoryRequirements, HirProgram, ScriptMode, StrategySettings,
    };

    use super::*;

    fn runtime() -> HistoricalRuntime<'static> {
        let program = Box::leak(Box::new(HirProgram {
            language_version: None,
            script_mode: ScriptMode::Indicator,
            timenow_symbol: None,
            strategy_settings: StrategySettings::default(),
            drawing_settings: DrawingSettings::default(),
            user_types: Vec::new(),
            symbols: Vec::new(),
            statements: Vec::new(),
            next_series_id: 0,
            next_call_site_id: 0,
            call_site_sources: Vec::new(),
            lower_tf_tuple_types: Vec::new(),
            next_var_slot_id: 0,
            max_bars_back: None,
            calc_bars_count: None,
            series_max_bars_back: Vec::new(),
            history: HirHistoryRequirements::default(),
            series_history: Vec::new(),
            execution_scoped_series: Vec::new(),
        }));
        HistoricalRuntime::new(program)
    }

    #[test]
    fn seed_intrabar_array_preserves_user_type_array_metadata() {
        let mut previous = runtime();
        previous.array_store.insert(
            3,
            vec![PineValue::UserType(vec![PineValue::Float(1.0)])].into(),
        );
        previous.array_kinds.insert(3, ArrayElementKind::UserType);
        previous.mark_array_user_type_for_test(3, "Point");
        previous.array_slices.insert(
            8,
            ArraySlice {
                parent_id: 3,
                start: 0,
                len: 1,
            },
        );
        previous.array_kinds.insert(8, ArrayElementKind::UserType);
        previous.mark_array_user_type_for_test(8, "Point");

        let mut current = runtime();
        current.seed_intrabar_array_from(&previous, 8);

        assert_eq!(
            current.array_kinds.get(&8),
            Some(&ArrayElementKind::UserType)
        );
        assert_eq!(current.array_user_type_name(8), Some("Point"));
        assert_eq!(
            current.array_kinds.get(&3),
            Some(&ArrayElementKind::UserType)
        );
        assert_eq!(current.array_user_type_name(3), Some("Point"));
    }
}
