use super::{ArrayElementKind, ArraySlice, normalize_array_index, normalize_array_insert_index};
use crate::runtime::array_values::ArrayView;
use crate::runtime::collection_gc::collection_values_allocation_bytes;
use crate::{HistoricalRuntime, MAX_ARRAY_ELEMENTS, PineValue, RuntimeError};

fn array_payload_allocation_bytes(kind: Option<&ArrayElementKind>, values: ArrayView<'_>) -> usize {
    if matches!(
        kind,
        Some(ArrayElementKind::String | ArrayElementKind::UserType | ArrayElementKind::ChartPoint)
    ) || kind.is_none()
    {
        collection_values_allocation_bytes(values)
    } else {
        values
            .len()
            .saturating_mul(std::mem::size_of::<PineValue>())
    }
}

impl<'a> HistoricalRuntime<'a> {
    fn array_index_out_of_bounds(index: i64, len: usize) -> RuntimeError {
        RuntimeError {
            message: format!("array index {index} is out of bounds for array of size {len}"),
        }
    }

    pub(crate) fn array_values_clone(
        &self,
        id: u32,
    ) -> Result<Option<Vec<PineValue>>, RuntimeError> {
        Ok(self
            .array_values(id)?
            .map(|values| values.iter().cloned().collect()))
    }

    pub(crate) fn array_values(&self, id: u32) -> Result<Option<ArrayView<'_>>, RuntimeError> {
        if let Some(slice) = self.array_slices.get(&id).copied() {
            self.validate_array_slice(slice)?;
            return Ok(self
                .array_store
                .get(&slice.parent_id)
                .map(|values| values.view(slice.start, slice.len)));
        }
        Ok(self
            .array_store
            .get(&id)
            .map(|values| values.view(0, values.len())))
    }

    #[cfg(test)]
    pub(crate) fn array_user_type_name(&self, id: u32) -> Option<&str> {
        self.array_user_types.get(&id).map(String::as_str)
    }

    #[cfg(test)]
    pub(crate) fn mark_array_user_type_for_test(&mut self, id: u32, type_name: impl Into<String>) {
        self.array_user_types.insert(id, type_name.into());
    }

    pub(crate) fn copy_array_user_type_metadata(&mut self, source_id: u32, target_id: u32) {
        if let Some(type_name) = self.array_user_types.get(&source_id).cloned() {
            self.array_user_types.insert(target_id, type_name);
        }
    }

    pub(crate) fn copy_array_user_type_metadata_from(
        &mut self,
        previous: &Self,
        source_id: u32,
        target_id: u32,
    ) {
        if let Some(type_name) = previous.array_user_types.get(&source_id).cloned() {
            self.array_user_types.insert(target_id, type_name);
        }
    }

    pub(crate) fn new_array_from_values_with_user_type_metadata(
        &mut self,
        source_id: u32,
        kind: ArrayElementKind,
        values: Vec<PineValue>,
    ) -> PineValue {
        let value = self.new_array_from_values(kind, values);
        if let PineValue::Array(target_id) = value {
            self.copy_array_user_type_metadata(source_id, target_id);
        }
        value
    }

    pub(crate) fn array_len(&self, id: u32) -> Result<Option<usize>, RuntimeError> {
        if let Some(slice) = self.array_slices.get(&id).copied() {
            self.validate_array_slice(slice)?;
            return Ok(Some(slice.len));
        }

        Ok(self.array_store.get(&id).map(|values| values.len()))
    }

    fn validate_array_slice(&self, slice: ArraySlice) -> Result<(), RuntimeError> {
        let Some(values) = self.array_store.get(&slice.parent_id) else {
            return Err(RuntimeError {
                message: "array slice parent is not available".to_owned(),
            });
        };
        let Some(end) = slice.start.checked_add(slice.len) else {
            return Err(RuntimeError {
                message: "array slice is out of bounds of the parent array".to_owned(),
            });
        };
        if end > values.len() {
            return Err(RuntimeError {
                message: "array slice is out of bounds of the parent array".to_owned(),
            });
        }
        Ok(())
    }

    fn array_read_index(&self, id: u32, index: i64) -> Result<Option<(u32, usize)>, RuntimeError> {
        if let Some(slice) = self.array_slices.get(&id).copied() {
            self.validate_array_slice(slice)?;
            let Some(index) = normalize_array_index(index, slice.len) else {
                return Err(Self::array_index_out_of_bounds(index, slice.len));
            };
            return Ok(Some((slice.parent_id, slice.start + index)));
        }

        let Some(values) = self.array_store.get(&id) else {
            return Ok(None);
        };
        let Some(index) = normalize_array_index(index, values.len()) else {
            return Err(Self::array_index_out_of_bounds(index, values.len()));
        };
        Ok(Some((id, index)))
    }

    fn array_insert_index(
        &self,
        id: u32,
        index: i64,
    ) -> Result<Option<(u32, usize)>, RuntimeError> {
        if let Some(slice) = self.array_slices.get(&id).copied() {
            self.validate_array_slice(slice)?;
            let Some(index) = normalize_array_insert_index(index, slice.len) else {
                return Err(Self::array_index_out_of_bounds(index, slice.len));
            };
            return Ok(Some((slice.parent_id, slice.start + index)));
        }

        let Some(values) = self.array_store.get(&id) else {
            return Ok(None);
        };
        let Some(index) = normalize_array_insert_index(index, values.len()) else {
            return Err(Self::array_index_out_of_bounds(index, values.len()));
        };
        Ok(Some((id, index)))
    }

    pub(super) fn array_parent_len_for_insert(&self, id: u32) -> Option<usize> {
        let target_id = self
            .array_slices
            .get(&id)
            .map_or(id, |slice| slice.parent_id);
        self.array_store.get(&target_id).map(|values| values.len())
    }

    pub(crate) fn array_get_cloned(
        &self,
        id: u32,
        index: i64,
    ) -> Result<Option<PineValue>, RuntimeError> {
        let Some((target_id, index)) = self.array_read_index(id, index)? else {
            return Ok(None);
        };
        Ok(self
            .array_store
            .get(&target_id)
            .and_then(|values| values.get(index))
            .cloned())
    }

    pub(crate) fn array_set_value(
        &mut self,
        id: u32,
        index: i64,
        value: PineValue,
    ) -> Result<(), RuntimeError> {
        self.array_set_value_impl(id, index, value, false)
    }

    fn prepare_array_set(
        &mut self,
        id: u32,
        index: i64,
        value: &PineValue,
        incoming_precharged: bool,
    ) -> Result<Option<(u32, usize)>, RuntimeError> {
        let Some((target_id, index)) = self.array_read_index(id, index)? else {
            return Ok(None);
        };
        let copied =
            self.array_store.get(&target_id).map_or(0, |values| {
                collection_values_allocation_bytes(values.write_allocation_values(
                    index,
                    self.array_store.get_mut_clones_value(&target_id),
                ))
            });
        if !self.record_collection_bytes(copied) {
            return Err(self.resource_budget.collection_error());
        }
        if !incoming_precharged && !self.record_collection_values(std::iter::once(value)) {
            return Err(self.resource_budget.collection_error());
        }
        Ok(Some((target_id, index)))
    }

    fn array_set_value_impl(
        &mut self,
        id: u32,
        index: i64,
        value: PineValue,
        incoming_precharged: bool,
    ) -> Result<(), RuntimeError> {
        let Some((target_id, index)) =
            self.prepare_array_set(id, index, &value, incoming_precharged)?
        else {
            return Ok(());
        };
        if let Some(slot) = self
            .array_store
            .get_mut(&target_id)
            .and_then(|values| values.get_mut(index))
        {
            *slot = value;
        }
        Ok(())
    }

    pub(super) fn array_set_borrowed_value(
        &mut self,
        id: u32,
        index: i64,
        value: &PineValue,
    ) -> Result<(), RuntimeError> {
        let Some((target_id, index)) = self.prepare_array_set(id, index, value, false)? else {
            return Ok(());
        };
        if let Some(slot) = self
            .array_store
            .get_mut(&target_id)
            .and_then(|values| values.get_mut(index))
        {
            *slot = value.clone();
        }
        Ok(())
    }

    pub(super) fn array_insert_value(
        &mut self,
        id: u32,
        index: i64,
        value: PineValue,
    ) -> Result<(), RuntimeError> {
        let Some((target_id, index)) = self.array_insert_index(id, index)? else {
            return Ok(());
        };
        let Some(parent_len) = self.array_parent_len_for_insert(id) else {
            return Ok(());
        };
        if parent_len >= MAX_ARRAY_ELEMENTS {
            return Err(RuntimeError {
                message: format!("array.insert cannot exceed {MAX_ARRAY_ELEMENTS} elements"),
            });
        }
        let copied = self.array_store.get(&target_id).map_or(0, |values| {
            if index < parent_len {
                // Middle edits materialize paged payloads. Including moved
                // small contents is a conservative allocation estimate.
                array_payload_allocation_bytes(
                    self.array_kinds.get(&target_id),
                    values.view(0, values.len()),
                )
            } else {
                collection_values_allocation_bytes(values.write_allocation_values(
                    parent_len,
                    self.array_store.get_mut_clones_value(&target_id),
                ))
            }
        });
        if !self.record_collection_bytes(copied) {
            return Err(self.resource_budget.collection_error());
        }
        if !self.record_collection_values(std::iter::once(&value)) {
            return Err(self.resource_budget.collection_error());
        }
        if let Some(values) = self.array_store.get_mut(&target_id) {
            values.insert(index, value);
        }
        if let Some(slice) = self.array_slices.get_mut(&id) {
            slice.len += 1;
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn array_insert_values(
        &mut self,
        id: u32,
        index: i64,
        incoming: Vec<PineValue>,
    ) -> Result<(), RuntimeError> {
        self.array_insert_values_impl(id, index, incoming, false)
    }

    pub(super) fn array_insert_precharged_values(
        &mut self,
        id: u32,
        index: i64,
        incoming: Vec<PineValue>,
    ) -> Result<(), RuntimeError> {
        self.array_insert_values_impl(id, index, incoming, true)
    }

    fn array_insert_values_impl(
        &mut self,
        id: u32,
        index: i64,
        incoming: Vec<PineValue>,
        incoming_precharged: bool,
    ) -> Result<(), RuntimeError> {
        if incoming.is_empty() {
            return Ok(());
        }
        let Some((target_id, index)) = self.array_insert_index(id, index)? else {
            return Ok(());
        };
        let parent = self.array_store.get(&target_id).expect("validated parent");
        let parent_len = parent.len();
        let added = incoming.len();
        if parent_len.saturating_add(added) > MAX_ARRAY_ELEMENTS {
            return Err(RuntimeError {
                message: format!("array.concat cannot exceed {MAX_ARRAY_ELEMENTS} elements"),
            });
        }
        let copied = if index < parent_len {
            array_payload_allocation_bytes(
                self.array_kinds.get(&target_id),
                parent.view(0, parent_len),
            )
        } else {
            collection_values_allocation_bytes(
                parent.append_allocation_values(self.array_store.get_mut_clones_value(&target_id)),
            )
        };
        if !self.record_collection_bytes(copied) {
            return Err(self.resource_budget.collection_error());
        }
        if !incoming_precharged && !self.record_collection_values(&incoming) {
            return Err(self.resource_budget.collection_error());
        }
        if index < parent_len {
            let replacement = self
                .array_store
                .get(&target_id)
                .expect("validated parent")
                .with_inserted(index, incoming);
            self.array_store.insert(target_id, replacement);
        } else {
            self.array_store
                .get_mut(&target_id)
                .expect("validated parent")
                .extend(incoming);
        }
        if let Some(slice) = self.array_slices.get_mut(&id) {
            slice.len += added;
        }
        Ok(())
    }

    pub(super) fn array_remove_value(
        &mut self,
        id: u32,
        index: i64,
    ) -> Result<Option<PineValue>, RuntimeError> {
        let Some((target_id, index)) = self.array_read_index(id, index)? else {
            return Ok(None);
        };
        let copied = self.array_store.get(&target_id).map_or(0, |values| {
            if index + 1 < values.len() {
                array_payload_allocation_bytes(
                    self.array_kinds.get(&target_id),
                    values.view(0, values.len()),
                )
            } else {
                collection_values_allocation_bytes(values.write_allocation_values(
                    index,
                    self.array_store.get_mut_clones_value(&target_id),
                ))
            }
        });
        if !self.record_collection_bytes(copied) {
            return Err(self.resource_budget.collection_error());
        }
        let removed = self
            .array_store
            .get_mut(&target_id)
            .map(|values| values.remove(index));
        if removed.is_some()
            && let Some(slice) = self.array_slices.get_mut(&id)
        {
            slice.len = slice.len.saturating_sub(1);
        }
        Ok(removed)
    }

    #[cfg(test)]
    pub(super) fn array_replace_values(
        &mut self,
        id: u32,
        replacement: Vec<PineValue>,
    ) -> Result<(), RuntimeError> {
        self.array_replace_values_impl(id, replacement, false)
    }

    pub(super) fn array_replace_precharged_values(
        &mut self,
        id: u32,
        replacement: Vec<PineValue>,
    ) -> Result<(), RuntimeError> {
        self.array_replace_values_impl(id, replacement, true)
    }

    fn array_replace_values_impl(
        &mut self,
        id: u32,
        replacement: Vec<PineValue>,
        incoming_precharged: bool,
    ) -> Result<(), RuntimeError> {
        if let Some(slice) = self.array_slices.get(&id).copied() {
            self.validate_array_slice(slice)?;
            for (offset, value) in replacement.into_iter().enumerate() {
                if offset >= slice.len {
                    break;
                }
                self.array_set_value_impl(id, offset as i64, value, incoming_precharged)?;
            }
            return Ok(());
        }

        if self.array_store.contains_key(&id) {
            // Replacing a payload must not first clone its discarded contents.
            if !incoming_precharged && !self.record_collection_values(&replacement) {
                return Err(self.resource_budget.collection_error());
            }
            self.array_store.insert(id, replacement.into());
        }
        Ok(())
    }

    pub(super) fn array_clear_values(&mut self, id: u32) -> Result<(), RuntimeError> {
        if let Some(slice) = self.array_slices.get(&id).copied() {
            self.validate_array_slice(slice)?;
            if slice.len == 0 {
                return Ok(());
            }
            let parent = self
                .array_store
                .get(&slice.parent_id)
                .expect("validated slice parent");
            let copied = array_payload_allocation_bytes(
                self.array_kinds.get(&slice.parent_id),
                parent.view(0, parent.len()),
            );
            if !self.record_collection_bytes(copied) {
                return Err(self.resource_budget.collection_error());
            }
            let mut values = self
                .array_store
                .get(&slice.parent_id)
                .expect("validated slice parent")
                .to_vec();
            values.drain(slice.start..slice.start + slice.len);
            self.array_store.insert(slice.parent_id, values.into());
            self.array_slices.get_mut(&id).expect("existing slice").len = 0;
        } else if self.array_store.contains_key(&id) {
            self.array_store.insert(id, Vec::new().into());
        }
        Ok(())
    }

    pub(super) fn new_array_slice(
        &mut self,
        source_id: u32,
        index_from: usize,
        index_to: usize,
    ) -> PineValue {
        let id = self.next_array_id;
        self.next_array_id += 1;
        let Some(kind) = self.array_kinds.get(&source_id).copied() else {
            return PineValue::Na;
        };
        let (parent_id, start) = if let Some(parent) = self.array_slices.get(&source_id).copied() {
            (parent.parent_id, parent.start + index_from)
        } else {
            (source_id, index_from)
        };
        self.array_kinds.insert(id, kind);
        self.array_slices.insert(
            id,
            ArraySlice {
                parent_id,
                start,
                len: index_to - index_from,
            },
        );
        self.copy_array_user_type_metadata(source_id, id);
        PineValue::Array(id)
    }
}
