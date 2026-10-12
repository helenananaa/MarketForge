//! Optional deterministic limits for logical `ta.valuewhen` event histories.

use crate::{HistoricalRuntime, RuntimeError};

/// Limits for retained and actively evaluated `ta.valuewhen` events.
///
/// One event is one retained Pine value, including `na`, regardless of its
/// payload size. The limit covers all call sites in a runtime and its retained
/// requested-context checkpoints. Requested evaluators also spend the remaining
/// allowance while executing, including evaluators discarded after evaluation.
/// Replacing a requested checkpoint replaces its allowance instead of counting
/// both its old checkpoint and its active replacement.
///
/// Separate historical clones and confirmed/forming states each have their own
/// allowance. Physical shared leaves, rollback snapshots, requested output
/// caches, strings/tuples inside values, and other runtime state are not counted.
/// This is a logical event limit, not a heap-byte or process-memory limit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ValueWhenLimits {
    /// Maximum logical events in one execution-state tree. `None` preserves
    /// existing behavior, including the existing per-call-site retention limit.
    pub max_retained_values: Option<usize>,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ValueWhenBudget {
    pub(crate) limits: ValueWhenLimits,
    pub(crate) local_values: usize,
    requested_values: usize,
    external_values: usize,
}

fn overflow() -> RuntimeError {
    RuntimeError {
        message: "E_VALUEWHEN_BUDGET: logical event count overflow".to_owned(),
    }
}

impl ValueWhenBudget {
    pub(crate) fn replace_local_values(
        &mut self,
        previous: usize,
        replacement: usize,
    ) -> Result<(), RuntimeError> {
        let mut budget = *self;
        budget.local_values = budget
            .local_values
            .checked_sub(previous)
            .and_then(|values| values.checked_add(replacement))
            .ok_or_else(overflow)?;
        budget.validate(budget.limits)?;
        *self = budget;
        Ok(())
    }

    fn retained_values(self) -> usize {
        // Every mutation validates this sum before publishing its counters.
        self.local_values + self.requested_values
    }

    fn validate(self, limits: ValueWhenLimits) -> Result<(), RuntimeError> {
        let total = self
            .local_values
            .checked_add(self.requested_values)
            .and_then(|values| values.checked_add(self.external_values))
            .ok_or_else(overflow)?;
        if let Some(limit) = limits.max_retained_values
            && total > limit
        {
            return Err(RuntimeError {
                message: format!(
                    "E_VALUEWHEN_BUDGET: retained valuewhen event limit exceeded (limit {limit}, required {total})"
                ),
            });
        }
        Ok(())
    }
}

impl HistoricalRuntime<'_> {
    /// Configure an optional event limit without changing existing Pine state.
    pub fn set_valuewhen_limits(&mut self, limits: ValueWhenLimits) -> Result<(), RuntimeError> {
        self.validate_valuewhen_limits(limits)?;
        self.valuewhen_budget.limits = limits;
        Ok(())
    }

    pub fn with_valuewhen_limits(mut self, limits: ValueWhenLimits) -> Result<Self, RuntimeError> {
        self.set_valuewhen_limits(limits)?;
        Ok(self)
    }

    #[must_use]
    pub fn valuewhen_limits(&self) -> ValueWhenLimits {
        self.valuewhen_budget.limits
    }

    /// Logical events in this state and its retained requested checkpoints.
    /// Temporary requested evaluators are counted against the limit while they
    /// execute, but disappear from this retained-state count when discarded.
    #[must_use]
    pub fn valuewhen_retained_values(&self) -> usize {
        self.valuewhen_budget.retained_values()
    }

    pub(crate) fn validate_valuewhen_limits(
        &self,
        limits: ValueWhenLimits,
    ) -> Result<(), RuntimeError> {
        self.valuewhen_budget.validate(limits)
    }

    pub(crate) fn replace_requested_valuewhen_values(
        &mut self,
        previous: usize,
        replacement: usize,
    ) -> Result<(), RuntimeError> {
        let mut budget = self.valuewhen_budget;
        budget.requested_values = budget
            .requested_values
            .checked_sub(previous)
            .and_then(|values| values.checked_add(replacement))
            .ok_or_else(overflow)?;
        budget.validate(budget.limits)?;
        self.valuewhen_budget = budget;
        Ok(())
    }

    pub(crate) fn inherit_valuewhen_budget(
        &mut self,
        parent: &Self,
        replaced_values: usize,
    ) -> Result<(), RuntimeError> {
        let mut budget = self.valuewhen_budget;
        budget.limits = parent.valuewhen_limits();
        budget.external_values = parent
            .valuewhen_retained_values()
            .checked_sub(replaced_values)
            .and_then(|values| values.checked_add(parent.valuewhen_budget.external_values))
            .ok_or_else(overflow)?;
        budget.validate(budget.limits)?;
        self.valuewhen_budget = budget;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn clear_request_evaluations(&mut self) {
        let removed = self
            .request_evaluations
            .values()
            .map(|saved| saved.valuewhen_retained_values())
            .sum();
        self.replace_requested_valuewhen_values(removed, 0)
            .expect("clearing requested checkpoints releases their events");
        self.request_evaluations.clear();
    }
}

#[cfg(test)]
#[path = "valuewhen_limits_tests.rs"]
mod tests;
