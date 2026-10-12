//! Join writes borrowed string fields into one character-bounded buffer.
//! No intermediate UDT element string or materialized object tree is needed.
use super::format_number;
use crate::{HistoricalRuntime, MAX_STRING_CHARS, PineValue, RuntimeError};
use std::collections::HashSet;

#[derive(Default)]
pub(crate) struct BoundedJoin {
    output: String,
    chars: usize,
}

impl BoundedJoin {
    pub(crate) fn push_str(&mut self, value: &str) -> Result<(), RuntimeError> {
        let remaining = MAX_STRING_CHARS - self.chars;
        let added = value.chars().take(remaining + 1).count();
        if added > remaining {
            return Err(RuntimeError {
                message: format!("array.join result cannot exceed {MAX_STRING_CHARS} characters"),
            });
        }
        self.output.push_str(value);
        self.chars += added;
        Ok(())
    }

    pub(crate) fn finish(self) -> PineValue {
        PineValue::String(self.output)
    }

    pub(crate) fn push_element(
        &mut self,
        value: &PineValue,
        type_name: Option<&str>,
        runtime: &HistoricalRuntime<'_>,
    ) -> Result<(), RuntimeError> {
        if let Some(type_name) = type_name {
            // Retain materialize_object's invalid-reference/cycle checks while
            // borrowing the actual fields instead of cloning their strings.
            validate_object_tree(value, runtime, &mut HashSet::new(), &mut HashSet::new())?;
            self.push_user_type(value, type_name, runtime, &mut Vec::new(), true)
        } else {
            self.push_scalar(value)
        }
    }

    fn push_scalar(&mut self, value: &PineValue) -> Result<(), RuntimeError> {
        match value {
            PineValue::String(value) => self.push_str(value),
            PineValue::Int(value) => self.push_str(&format_number(*value as f64, "#.########")),
            PineValue::Float(value) => self.push_str(&format_number(*value, "#.########")),
            PineValue::Bool(value) => self.push_str(if *value { "true" } else { "false" }),
            PineValue::Color(value) => self.push_str(&value.to_string()),
            _ => self.push_str("NaN"),
        }
    }

    fn push_user_type<'a>(
        &mut self,
        value: &PineValue,
        type_name: &'a str,
        runtime: &'a HistoricalRuntime<'_>,
        seen_types: &mut Vec<&'a str>,
        resolve_refs: bool,
    ) -> Result<(), RuntimeError> {
        let (fields, resolve_children) = match value {
            PineValue::UserType(fields) => (fields.as_slice(), false),
            PineValue::UserTypeRef(id) if resolve_refs => (
                runtime
                    .object_store
                    .get(id)
                    .expect("validated object")
                    .as_slice(),
                true,
            ),
            _ => return self.push_scalar(value),
        };
        if seen_types.contains(&type_name) {
            return self.push_str("NaN");
        }
        seen_types.push(type_name);
        let shape = runtime
            .program
            .user_types
            .iter()
            .find(|ty| ty.identity.type_name == type_name);
        self.push_str(type_name)?;
        self.push_str("(")?;
        for (index, field) in fields.iter().enumerate() {
            if index > 0 {
                self.push_str(", ")?;
            }
            if let Some(field_type) = shape
                .and_then(|shape| shape.fields.get(index))
                .and_then(|field| field.user_type_name.as_deref())
            {
                self.push_user_type(field, field_type, runtime, seen_types, resolve_children)?;
            } else {
                self.push_scalar(field)?;
            }
        }
        self.push_str(")")?;
        seen_types.pop();
        Ok(())
    }
}

fn validate_object_tree(
    value: &PineValue,
    runtime: &HistoricalRuntime<'_>,
    seen: &mut HashSet<u32>,
    completed: &mut HashSet<u32>,
) -> Result<(), RuntimeError> {
    let PineValue::UserTypeRef(id) = value else {
        return Ok(());
    };
    if completed.contains(id) {
        return Ok(());
    }
    if !seen.insert(*id) {
        return Err(RuntimeError {
            message: "cyclic UDT cannot be materialized as a value tree".to_owned(),
        });
    }
    #[cfg(test)]
    tests::record_object_visit();
    let fields = runtime.object_store.get(id).ok_or_else(|| RuntimeError {
        message: "invalid UDT object reference".to_owned(),
    })?;
    for field in fields {
        validate_object_tree(field, runtime, seen, completed)?;
    }
    seen.remove(id);
    // A shared subtree has already passed every reference and cycle check.
    // Only completed nodes are memoized; active ancestors still detect cycles.
    completed.insert(*id);
    Ok(())
}

#[cfg(test)]
mod tests;
