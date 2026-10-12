use super::changes::{
    MIN_RUNTIME_CHANGES_SCHEMA_VERSION, PUBLIC_RUNTIME_CHANGES_SCHEMA_VERSION, RuntimeChanges,
    apply_runtime_changes_in_place,
};
use crate::{RuntimeError, RuntimeResult};

/// A consumer bound to one host-selected stream. Snapshot, revision and
/// `retained_from` must be captured together; the host owns routing.
pub struct RuntimeReplica {
    result: RuntimeResult,
    revision: u64,
    origin: usize,
    last_changes: Option<RuntimeChanges>,
}

impl RuntimeReplica {
    #[must_use]
    pub fn new(result: RuntimeResult, revision: u64) -> Self {
        Self::with_retained_from(result, revision, 0)
    }

    #[must_use]
    pub fn with_retained_from(result: RuntimeResult, revision: u64, retained_from: usize) -> Self {
        Self {
            result,
            revision,
            origin: retained_from,
            last_changes: None,
        }
    }

    #[must_use]
    pub fn revision(&self) -> u64 {
        self.revision
    }

    #[must_use]
    pub fn retained_from(&self) -> usize {
        self.origin
    }

    #[must_use]
    pub fn result(&self) -> &RuntimeResult {
        &self.result
    }

    /// Consume a completed replica, transferring its full snapshot without a
    /// copy. Its stream cursor and retransmission state are discarded.
    #[must_use]
    pub fn into_result(self) -> RuntimeResult {
        self.result
    }

    /// Replace state with an authoritative snapshot after a gap or reconnect.
    pub fn reset(&mut self, result: RuntimeResult, revision: u64) {
        *self = Self::new(result, revision);
    }

    pub fn reset_with_retained_from(
        &mut self,
        result: RuntimeResult,
        revision: u64,
        retained_from: usize,
    ) {
        *self = Self::with_retained_from(result, revision, retained_from);
    }

    /// Returns false for an identical retransmission. Rejected updates cannot
    /// mutate result or cursor. Payloads must belong to this replica's stream.
    pub fn apply(&mut self, changes: &RuntimeChanges) -> Result<bool, RuntimeError> {
        if changes.fills.iter().any(|fill| match &fill.action {
            crate::FillAction::Add(fill) => fill.gradient.as_ref().is_some_and(|samples| {
                samples.len() != fill.colors.len()
                    || samples.iter().any(|sample| !sample.is_valid())
            }),
            crate::FillAction::SetGradient { values, .. } => {
                values.iter().any(|sample| !sample.is_valid())
            }
            _ => false,
        }) {
            return Err(error("E_STREAM_GRADIENT: invalid gradient samples"));
        }
        if changes.schema_version < 4
            && changes.fills.iter().any(|fill| match &fill.action {
                crate::FillAction::SetGradient { .. } => true,
                crate::FillAction::Add(fill) => fill.gradient.is_some(),
                _ => false,
            })
        {
            return Err(error(
                "E_STREAM_SCHEMA: gradient fill requires changes schema 4",
            ));
        }
        if changes.schema_version < MIN_RUNTIME_CHANGES_SCHEMA_VERSION
            || changes.schema_version > PUBLIC_RUNTIME_CHANGES_SCHEMA_VERSION
        {
            return Err(error("E_STREAM_SCHEMA: unsupported changes schema"));
        }
        let retained_from = if changes.schema_version < 3 {
            0
        } else {
            changes.retained_from
        };
        if changes.base_revision.checked_add(1) != Some(changes.revision) {
            return Err(error(
                "E_STREAM_REVISION: revision must follow baseRevision",
            ));
        }
        if changes.revision == self.revision {
            return if self.last_changes.as_ref() == Some(changes) {
                Ok(false)
            } else {
                Err(error(
                    "E_STREAM_CONFLICT: revision already has a different or unknown payload",
                ))
            };
        }
        if changes.revision < self.revision {
            return Err(error(
                "E_STREAM_STALE: change precedes the current revision",
            ));
        }
        if changes.base_revision != self.revision {
            return Err(error(
                "E_STREAM_GAP: missing changes; restore an authoritative snapshot",
            ));
        }
        if retained_from < self.origin {
            return Err(error(
                "E_STREAM_ORIGIN: retainedFrom cannot move earlier; restore a snapshot",
            ));
        }
        let mut payload = changes.clone();
        payload.retained_from = retained_from;
        apply_runtime_changes_in_place(&mut self.result, &payload, self.origin);
        self.origin = retained_from;
        self.revision = changes.revision;
        self.last_changes = Some(changes.clone());
        Ok(true)
    }
}

fn error(message: &str) -> RuntimeError {
    RuntimeError {
        message: message.to_owned(),
    }
}
