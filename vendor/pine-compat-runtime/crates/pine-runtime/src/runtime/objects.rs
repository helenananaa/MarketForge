use super::collection_gc::collection_values_allocation_bytes;
use crate::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IntrabarReference {
    Object(u32),
    Array(u32),
    Map(u32),
    Matrix(u32),
}

fn enqueue_intrabar_references(
    pending: &mut Vec<(IntrabarReference, bool)>,
    value: &PineValue,
    retain_contents: bool,
) {
    let reference = match value {
        PineValue::UserTypeRef(id) => IntrabarReference::Object(*id),
        PineValue::Array(id) => IntrabarReference::Array(*id),
        PineValue::Map(id) => IntrabarReference::Map(*id),
        PineValue::Matrix(id) => IntrabarReference::Matrix(*id),
        PineValue::UserType(fields) | PineValue::Tuple(fields) => {
            for field in fields {
                enqueue_intrabar_references(pending, field, retain_contents);
            }
            return;
        }
        // Drawing handles and chart points are value leaves in this transfer;
        // their runtime families have separate rollback rules.
        _ => return,
    };
    pending.push((reference, retain_contents));
}

pub(super) fn scalar_array_kind(kind: Option<&ArrayElementKind>) -> bool {
    matches!(
        kind,
        Some(
            ArrayElementKind::Float
                | ArrayElementKind::Int
                | ArrayElementKind::Bool
                | ArrayElementKind::String
                | ArrayElementKind::Color
        )
    )
}

impl HistoricalRuntime<'_> {
    pub(crate) fn reject_request_object_graph(
        &self,
        value: &PineValue,
    ) -> Result<(), RuntimeError> {
        let mut pending = vec![value];
        let mut seen = std::collections::HashSet::new();
        while let Some(value) = pending.pop() {
            match value {
                PineValue::UserTypeRef(_) => {
                    return Err(RuntimeError {
                        message: "request.security UDT object graph transfer is not implemented"
                            .to_owned(),
                    });
                }
                PineValue::UserType(fields) | PineValue::Tuple(fields) => pending.extend(fields),
                PineValue::Array(id) if seen.insert((0, *id)) => {
                    if let Some(values) = self.array_values(*id)? {
                        pending.extend(values.iter());
                    }
                }
                PineValue::Matrix(id) if seen.insert((1, *id)) => {
                    if let Some(matrix) = self.matrix_store.get(id) {
                        pending.extend(matrix.values.iter());
                    }
                }
                PineValue::Map(id) if seen.insert((2, *id)) => {
                    if let Some(map) = self.map_store.get(id) {
                        pending.extend(map.entries.iter().flat_map(|(key, value)| [key, value]));
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    pub(crate) fn materialize_object(&self, value: &PineValue) -> Result<PineValue, RuntimeError> {
        self.materialize_object_inner(value, &mut std::collections::HashSet::new(), 0)
    }

    fn materialize_object_inner(
        &self,
        value: &PineValue,
        seen: &mut std::collections::HashSet<u32>,
        depth: u32,
    ) -> Result<PineValue, RuntimeError> {
        if depth >= MAX_RUNTIME_EVAL_DEPTH {
            return Err(RuntimeError {
                message: "UDT materialization exceeded maximum depth".to_owned(),
            });
        }
        let PineValue::UserTypeRef(id) = value else {
            return Ok(value.clone());
        };
        if !seen.insert(*id) {
            return Err(RuntimeError {
                message: "cyclic UDT cannot be materialized as a value tree".to_owned(),
            });
        }
        let fields = self.object_store.get(id).ok_or_else(|| RuntimeError {
            message: "invalid UDT object reference".to_owned(),
        })?;
        let fields = fields
            .iter()
            .map(|field| self.materialize_object_inner(field, seen, depth + 1))
            .collect::<Result<_, _>>()?;
        seen.remove(id);
        Ok(PineValue::UserType(fields))
    }

    pub(crate) fn allocate_object(
        &mut self,
        fields: Vec<PineValue>,
        identity: &pine_ir::HirUserTypeIdentity,
    ) -> Result<PineValue, RuntimeError> {
        let id = u32::try_from(self.next_object_id).map_err(|_| RuntimeError {
            message: "UDT object identity space exhausted".to_owned(),
        })?;
        let flags = self
            .program
            .user_types
            .iter()
            .find(|ty| {
                ty.identity.source_id == identity.source_id
                    && ty.declaration_name == identity.type_name
            })
            .map_or_else(
                || vec![false; fields.len()],
                |ty| ty.fields.iter().map(|field| field.varip).collect(),
            );
        if flags.iter().any(|flag| *flag) {
            self.object_varip_ids.insert(id, id);
        }
        self.next_object_id += 1;
        if !self.record_collection_values(&fields) {
            return Err(self.resource_budget.collection_error());
        }
        self.object_store.insert(id, fields);
        self.object_varip_fields.insert(id, flags);
        Ok(PineValue::UserTypeRef(id))
    }

    pub(crate) fn seed_intrabar_objects_from(
        &mut self,
        previous: &Self,
        roots: Vec<PineValue>,
    ) -> Result<(), RuntimeError> {
        self.next_object_id = self.next_object_id.max(previous.next_object_id);
        let mut pending = Vec::new();
        for value in &roots {
            enqueue_intrabar_references(&mut pending, value, true);
        }
        // Field-level varip on committed objects survives reassignment during
        // a forming pass. Ordinary fields still roll back to committed state.
        let varip_ids = self.object_varip_ids.clone();
        for &id in varip_ids.values() {
            if let (Some(fields), Some(flags)) = (
                previous.object_store.get(&id),
                self.object_varip_fields.get(&id).cloned(),
            ) {
                for (index, varip) in flags.iter().enumerate() {
                    if *varip && let Some(value) = fields.get(index) {
                        self.reserve_object_field_write(id, index, value)?;
                        self.object_store.get_mut(&id).expect("validated object")[index] =
                            value.clone();
                        enqueue_intrabar_references(&mut pending, value, true);
                    }
                }
            }
        }
        let mut seen = std::collections::HashSet::new();
        while let Some((value, retain_contents)) = pending.pop() {
            match value {
                IntrabarReference::Object(id) if seen.insert((0, id)) => {
                    let imported = !self.object_store.contains_key(&id);
                    if imported {
                        // Import only retained identities. Sparse storage leaves
                        // abandoned speculative allocations out of checkpoints.
                        self.object_store
                            .copy_entry_from(&previous.object_store, id);
                        self.object_varip_fields
                            .copy_entry_from(&previous.object_varip_fields, id);
                        self.object_varip_ids
                            .copy_entry_from(&previous.object_varip_ids, id);
                    }
                    if let Some(fields) = self.object_store.get(&id) {
                        let flags = self.object_varip_fields.get(&id);
                        for (index, value) in fields.iter().enumerate() {
                            enqueue_intrabar_references(
                                &mut pending,
                                value,
                                imported || flags.is_some_and(|flags| flags[index]),
                            );
                        }
                    }
                }
                IntrabarReference::Array(id)
                    if seen.insert((if retain_contents { 1 } else { 4 }, id)) =>
                {
                    if retain_contents {
                        self.seed_intrabar_array_from(previous, id);
                    }
                    if let Some(slice) = self.array_slices.get(&id) {
                        pending.push((IntrabarReference::Array(slice.parent_id), retain_contents));
                    }
                    // Array construction and writes enforce these scalar kinds.
                    // Their payload cannot add graph roots, so a 100k-element
                    // varip array needs neither an element scan nor a work queue.
                    if scalar_array_kind(self.array_kinds.get(&id)) {
                        continue;
                    }
                    if let Some(values) = self.array_store.get(&id) {
                        for value in values {
                            enqueue_intrabar_references(&mut pending, value, retain_contents);
                        }
                    }
                }
                IntrabarReference::Map(id)
                    if seen.insert((if retain_contents { 2 } else { 5 }, id)) =>
                {
                    if retain_contents {
                        self.seed_intrabar_map_from(previous, id);
                    }
                    if let Some(map) = self.map_store.get(&id) {
                        for (key, value) in &map.entries {
                            enqueue_intrabar_references(&mut pending, key, retain_contents);
                            enqueue_intrabar_references(&mut pending, value, retain_contents);
                        }
                    }
                }
                IntrabarReference::Matrix(id)
                    if seen.insert((if retain_contents { 3 } else { 6 }, id)) =>
                {
                    if retain_contents {
                        self.seed_intrabar_matrix_from(previous, id);
                    }
                    if let Some(matrix) = self.matrix_store.get(&id) {
                        for value in &matrix.values {
                            enqueue_intrabar_references(&mut pending, value, retain_contents);
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    pub(crate) fn object_field(&self, id: u32, index: usize) -> Result<PineValue, RuntimeError> {
        self.object_store
            .get(&id)
            .and_then(|fields| fields.get(index))
            .cloned()
            .ok_or_else(|| RuntimeError {
                message: "invalid UDT object reference or field".to_owned(),
            })
    }

    pub(crate) fn set_object_field(
        &mut self,
        id: u32,
        index: usize,
        value: PineValue,
    ) -> Result<(), RuntimeError> {
        self.reserve_object_field_write(id, index, &value)?;
        self.object_store.get_mut(&id).expect("validated object")[index] = value;
        Ok(())
    }

    // Reserve both the copy-on-write payload and incoming field before cloning
    // a retained varip value or making a shared object mutable.
    fn reserve_object_field_write(
        &mut self,
        id: u32,
        index: usize,
        value: &PineValue,
    ) -> Result<(), RuntimeError> {
        let fields = self
            .object_store
            .get(&id)
            .filter(|fields| index < fields.len())
            .ok_or_else(|| RuntimeError {
                message: "invalid UDT object reference or field".to_owned(),
            })?;
        let copied = if self.object_store.get_mut_clones_value(&id) {
            collection_values_allocation_bytes(fields)
        } else {
            0
        };
        if !self.record_collection_bytes(copied) {
            return Err(self.resource_budget.collection_error());
        }
        if !self.record_collection_values(std::iter::once(value)) {
            return Err(self.resource_budget.collection_error());
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "objects_transfer_tests.rs"]
mod transfer_tests;

#[cfg(test)]
mod materialization_depth_tests {
    use super::*;

    fn program() -> pine_ir::HirProgram {
        let analysis = pine_sema::analyze_source(&pine_syntax::SourceFile::new(
            "object-depth.pine",
            "//@version=6\nindicator(\"object depth\")\nplot(close)\n",
        ));
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        analysis.hir.unwrap()
    }

    #[test]
    fn long_reference_chains_return_a_depth_error_before_exhausting_the_stack() {
        let hir = program();
        let mut runtime = HistoricalRuntime::new(&hir);
        let length = MAX_RUNTIME_EVAL_DEPTH * 4;
        for id in 0..length {
            let field = if id + 1 == length {
                PineValue::Int(7)
            } else {
                PineValue::UserTypeRef(id + 1)
            };
            runtime.object_store.insert(id, vec![field]);
        }
        assert_eq!(
            runtime
                .materialize_object(&PineValue::UserTypeRef(0))
                .unwrap_err()
                .message,
            "UDT materialization exceeded maximum depth"
        );
    }

    #[test]
    fn shallow_values_cycles_and_invalid_references_keep_their_existing_contract() {
        let hir = program();
        let mut runtime = HistoricalRuntime::new(&hir);
        runtime
            .object_store
            .insert(0, vec![PineValue::Int(1), PineValue::UserTypeRef(1)]);
        runtime.object_store.insert(1, vec![PineValue::Int(2)]);
        assert_eq!(
            runtime
                .materialize_object(&PineValue::UserTypeRef(0))
                .unwrap(),
            PineValue::UserType(vec![
                PineValue::Int(1),
                PineValue::UserType(vec![PineValue::Int(2)])
            ])
        );
        runtime
            .object_store
            .insert(1, vec![PineValue::UserTypeRef(0)]);
        assert_eq!(
            runtime
                .materialize_object(&PineValue::UserTypeRef(0))
                .unwrap_err()
                .message,
            "cyclic UDT cannot be materialized as a value tree"
        );
        assert_eq!(
            runtime
                .materialize_object(&PineValue::UserTypeRef(2))
                .unwrap_err()
                .message,
            "invalid UDT object reference"
        );
    }
}

#[cfg(test)]
mod varip_index_tests {
    use super::*;

    #[test]
    fn field_varip_index_preserves_rollback_and_imported_identity_slots() {
        let source = pine_syntax::SourceFile::new(
            "varip-index.pine",
            "//@version=6\nindicator(\"index\")\ntype Plain\n    int n\ntype Sticky\n    varip int ticks\n    int n\nplot(close)\n",
        );
        let hir = pine_sema::analyze_source(&source).hir.unwrap();
        let plain = &hir
            .user_types
            .iter()
            .find(|ty| ty.declaration_name == "Plain")
            .unwrap()
            .identity;
        let sticky = &hir
            .user_types
            .iter()
            .find(|ty| ty.declaration_name == "Sticky")
            .unwrap()
            .identity;
        let mut committed = HistoricalRuntime::new(&hir);
        for _ in 0..2048 {
            committed
                .allocate_object(vec![PineValue::Int(0)], plain)
                .unwrap();
        }
        let PineValue::UserTypeRef(first) = committed
            .allocate_object(vec![PineValue::Int(1), PineValue::Int(10)], sticky)
            .unwrap()
        else {
            panic!("object")
        };
        let checkpoint = committed.clone();
        let mut forming = committed.clone();
        forming
            .set_object_field(first, 0, PineValue::Int(7))
            .unwrap();
        forming
            .set_object_field(first, 1, PineValue::Int(20))
            .unwrap();
        forming
            .allocate_object(vec![PineValue::Int(30)], plain)
            .unwrap();
        let retained = forming
            .allocate_object(vec![PineValue::Int(99), PineValue::Int(40)], sticky)
            .unwrap();
        committed
            .seed_intrabar_objects_from(&forming, vec![retained.clone(), retained])
            .unwrap();
        assert_eq!(committed.object_field(first, 0).unwrap(), PineValue::Int(7));
        assert_eq!(
            committed.object_field(first, 1).unwrap(),
            PineValue::Int(10)
        );
        assert_eq!(
            checkpoint.object_field(first, 0).unwrap(),
            PineValue::Int(1)
        );
        assert_eq!(committed.object_store.len() + 1, forming.object_store.len());
        assert_eq!(committed.next_object_id, forming.next_object_id);
        let expected: Vec<_> = committed
            .object_varip_fields
            .entries()
            .filter(|(_, flags)| flags.iter().any(|flag| *flag))
            .map(|(index, _)| index)
            .collect();
        assert_eq!(expected.len(), 2);
        assert_eq!(
            committed
                .object_varip_ids
                .values()
                .copied()
                .collect::<Vec<_>>(),
            expected
        );
        assert_eq!(
            checkpoint
                .object_varip_ids
                .values()
                .copied()
                .collect::<Vec<_>>(),
            vec![first]
        );
    }

    #[test]
    fn nested_sticky_graph_is_imported_while_ordinary_collection_fields_roll_back() {
        use crate::builtins::maps::MapStorage;
        use crate::builtins::matrices::{MatrixElementKind, MatrixStorage};
        let source = pine_syntax::SourceFile::new(
            "nested.pine",
            "//@version=6\nindicator(\"nested\")\ntype Holder\n    varip int sticky\n    int ordinary\nplot(close)\n",
        );
        let hir = pine_sema::analyze_source(&source).hir.unwrap();
        let identity = &hir.user_types[0].identity;
        let mut committed = HistoricalRuntime::new(&hir);
        // Synthetic graph isolates the storage/rollback contract from language
        // restrictions on which reference kinds each UDT declaration accepts.
        committed.map_store.insert(
            0,
            MapStorage {
                key_kind: ArrayElementKind::String,
                value_kind: ArrayElementKind::UserType,
                entries: vec![].into(),
            },
        );
        committed.matrix_store.insert(
            0,
            MatrixStorage {
                kind: MatrixElementKind::Float,
                rows: 1,
                columns: 1,
                values: vec![PineValue::Float(7.)].into(),
            },
        );
        let held = committed
            .allocate_object(vec![PineValue::Map(0), PineValue::Matrix(0)], identity)
            .unwrap();
        let checkpoint = committed.clone();
        assert!(std::ptr::eq(
            committed.map_store.get(&0).unwrap(),
            checkpoint.map_store.get(&0).unwrap()
        ));
        assert!(std::ptr::eq(
            committed.matrix_store.get(&0).unwrap(),
            checkpoint.matrix_store.get(&0).unwrap()
        ));
        let mut forming = committed.clone();
        forming.matrix_store.get_mut(&0).unwrap().values[0] = PineValue::Float(8.);
        let nested_array =
            forming.new_array_from_values(ArrayElementKind::Float, vec![PineValue::Float(99.)]);
        forming.matrix_store.insert(
            1,
            MatrixStorage {
                kind: MatrixElementKind::UserType(0),
                rows: 1,
                columns: 1,
                values: vec![PineValue::UserType(vec![nested_array])].into(),
            },
        );
        let nested = forming
            .allocate_object(vec![PineValue::Matrix(1), PineValue::Na], identity)
            .unwrap();
        forming
            .map_store
            .get_mut(&0)
            .unwrap()
            .entries
            .put(PineValue::String("nested".into()), nested);
        committed
            .seed_intrabar_objects_from(&forming, vec![held])
            .unwrap();
        assert_eq!(
            committed.matrix_store.get(&0).unwrap().values.to_vec(),
            [PineValue::Float(7.)]
        );
        assert_eq!(committed.object_field(1, 0).unwrap(), PineValue::Matrix(1));
        assert_eq!(
            committed.array_get_cloned(0, 0).unwrap(),
            Some(PineValue::Float(99.))
        );
        assert!(checkpoint.map_store.get(&0).unwrap().entries.is_empty());
        assert!(checkpoint.matrix_store.get(&1).is_none());
        // Intrabar imports share storage until the next actual mutation.
        assert!(std::ptr::eq(
            committed.map_store.get(&0).unwrap(),
            forming.map_store.get(&0).unwrap()
        ));
        committed.map_store.get_mut(&0).unwrap().entries.clear();
        assert_eq!(forming.map_store.get(&0).unwrap().entries.len(), 1);
    }
}
