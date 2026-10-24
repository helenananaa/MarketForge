//! Schema-4 archives are immutable prefixes. Carry only newly appended chunks
//! and the bounded archive header; the outer Pine hint binds the full predecessor.
use super::*;

const KIND: &str = "marketforge.pine-replay/1";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReplayAppend {
    chunks: Append,
    metadata: Map<String, Value>,
}

impl ReplayAppend {
    pub(super) fn prepare(before: &Value, after: &Value) -> Option<Self> {
        let (before, after) = (before.as_object()?, after.as_object()?);
        if before.get("kind")?.as_str()? != KIND || after.get("kind")?.as_str()? != KIND {
            return None;
        }
        Some(Self {
            chunks: append(before.get("chunks")?, after.get("chunks")?)?,
            metadata: after
                .iter()
                .filter(|(key, _)| key.as_str() != "chunks")
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        })
    }

    pub(super) fn validate(&self) -> Result<(), JournalError> {
        if self.metadata.contains_key("chunks")
            || self.metadata.get("kind").and_then(Value::as_str) != Some(KIND)
            || self.metadata.get("bars").and_then(Value::as_u64).is_none()
            || self
                .metadata
                .get("tail_batches")
                .and_then(Value::as_array)
                .is_none()
            || !self.metadata.contains_key("previous_time")
            || self
                .chunks
                .base_len
                .checked_add(self.chunks.items.len())
                .is_none()
            || self.chunks.items.iter().any(|chunk| !chunk.is_string())
        {
            return Err(failure("invalid replay archive append"));
        }
        Ok(())
    }

    pub(super) fn materialize(&self, previous: &Value) -> Result<Value, JournalError> {
        self.validate()?;
        if previous.get("kind").and_then(Value::as_str) != Some(KIND) {
            return Err(failure("predecessor replay archive kind"));
        }
        let chunks = previous
            .get("chunks")
            .and_then(Value::as_array)
            .ok_or_else(|| failure("predecessor replay chunks missing"))?;
        if chunks.len() != self.chunks.base_len {
            return Err(failure("replay chunk prefix length mismatch"));
        }
        let mut chunks = chunks.clone();
        chunks.extend_from_slice(&self.chunks.items);
        let mut result = self.metadata.clone();
        result.insert("chunks".into(), Value::Array(chunks));
        Ok(Value::Object(result))
    }
}
