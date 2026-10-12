use std::collections::HashSet;

use super::ArrayElementKind;
use crate::runtime::collection_gc::{collection_values_allocation_bytes, value_allocation_bytes};
use crate::{HistoricalRuntime, MAX_RUNTIME_EVAL_DEPTH, PineValue, RuntimeError};

impl HistoricalRuntime<'_> {
    pub(super) fn array_values_clone_with_budget(
        &mut self,
        id: u32,
    ) -> Result<Option<Vec<PineValue>>, RuntimeError> {
        let Some(values) = self.array_values(id)? else {
            return Ok(None);
        };
        let bytes = match self.array_kinds.get(&id) {
            Some(
                ArrayElementKind::String
                | ArrayElementKind::UserType
                | ArrayElementKind::ChartPoint,
            )
            | None => collection_values_allocation_bytes(values),
            _ => values
                .len()
                .saturating_mul(std::mem::size_of::<PineValue>()),
        };
        if !self.record_collection_bytes(bytes) {
            return Err(self.resource_budget.collection_error());
        }
        self.array_values_clone(id)
    }

    pub(super) fn reserve_array_sort_keys(
        &mut self,
        values: &[PineValue],
    ) -> Result<(), RuntimeError> {
        let mut bytes = 0usize;
        let mut seen = HashSet::new();
        for value in values {
            bytes =
                bytes.saturating_add(self.array_sort_key_allocation_bytes(value, &mut seen, 0)?);
        }
        if !self.record_collection_bytes(bytes) {
            return Err(self.resource_budget.collection_error());
        }
        Ok(())
    }

    // Sorting materializes a separate value tree for every key. Measure that
    // tree before cloning it, including strings and repeated reference paths.
    fn array_sort_key_allocation_bytes(
        &self,
        value: &PineValue,
        seen: &mut HashSet<u32>,
        depth: u32,
    ) -> Result<usize, RuntimeError> {
        if depth >= MAX_RUNTIME_EVAL_DEPTH {
            return Err(RuntimeError {
                message: "UDT materialization exceeded maximum depth".to_owned(),
            });
        }
        let PineValue::UserTypeRef(id) = value else {
            return Ok(value_allocation_bytes(value));
        };
        if !seen.insert(*id) {
            return Err(RuntimeError {
                message: "cyclic UDT cannot be materialized as a value tree".to_owned(),
            });
        }
        let fields = self.object_store.get(id).ok_or_else(|| RuntimeError {
            message: "invalid UDT object reference".to_owned(),
        })?;
        let mut bytes = std::mem::size_of::<PineValue>();
        for field in fields {
            bytes = bytes.saturating_add(self.array_sort_key_allocation_bytes(
                field,
                seen,
                depth + 1,
            )?);
        }
        seen.remove(id);
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_udt_chain_is_rejected_before_sort_key_allocation() {
        let analysis = pine_sema::analyze_source(&pine_syntax::SourceFile::new(
            "sort-key-depth.pine",
            "//@version=6\nindicator(\"sort key depth\")\nplot(close)\n",
        ));
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        let program = analysis.hir.unwrap();
        let mut runtime = HistoricalRuntime::new(&program);
        for id in 0..=MAX_RUNTIME_EVAL_DEPTH {
            let field = if id == MAX_RUNTIME_EVAL_DEPTH {
                PineValue::Int(1)
            } else {
                PineValue::UserTypeRef(id + 1)
            };
            runtime.object_store.insert(id, vec![field]);
        }
        let error = runtime
            .reserve_array_sort_keys(&[PineValue::UserTypeRef(0)])
            .unwrap_err();
        assert_eq!(error.message, "UDT materialization exceeded maximum depth");
        assert_eq!(runtime.collection_gc_allocated_bytes, 0);
    }
}
