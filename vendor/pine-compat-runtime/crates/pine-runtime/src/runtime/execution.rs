//! Deterministic work limits and typed control flow for one chart execution.

use crate::error::RuntimeLoopControl;
use crate::{HistoricalRuntime, RuntimeError};

/// Work limits shared by a chart-bar execution and its requested evaluations.
///
/// Steps count evaluated HIR expressions/statements and explicit preparation
/// work. Loop iterations are counted across all loops, including nested loops
/// and requested contexts. These are deterministic counters, not wall clocks.
/// Individual built-in kernels remain bounded by their collection/input limits;
/// this does not assign a machine-instruction cost to each built-in operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutionLimits {
    pub max_steps_per_bar: u64,
    pub max_loop_iterations_per_bar: u64,
}

impl Default for ExecutionLimits {
    fn default() -> Self {
        Self {
            max_steps_per_bar: 10_000_000,
            max_loop_iterations_per_bar: 1_000_000,
        }
    }
}

impl HistoricalRuntime<'_> {
    /// Host cancellation boundary; never changes successful numerical execution.
    pub fn set_host_deadline(&mut self, deadline: Option<std::time::Instant>) {
        self.host_deadline = deadline;
    }
    #[must_use]
    pub fn with_execution_limits(mut self, limits: ExecutionLimits) -> Self {
        self.execution_limits = limits;
        self.reset_execution_budget();
        self
    }

    #[must_use]
    pub fn execution_limits(&self) -> ExecutionLimits {
        self.execution_limits
    }

    pub(crate) fn reset_execution_budget(&mut self) {
        self.execution_steps_remaining = self.execution_limits.max_steps_per_bar;
        self.loop_iterations_remaining = self.execution_limits.max_loop_iterations_per_bar;
        self.pending_loop_control = None;
        self.resource_budget.reset();
    }

    #[inline]
    pub(crate) fn charge_execution_steps(&mut self, steps: u64) -> Result<(), RuntimeError> {
        if self
            .host_deadline
            .is_some_and(|deadline| std::time::Instant::now() >= deadline)
        {
            return Err(RuntimeError {
                message: "E_HOST_DEADLINE: Pine decision timed out".into(),
            });
        }
        if steps > self.execution_steps_remaining {
            return Err(RuntimeError {
                message: format!(
                    "E_EXECUTION_BUDGET: per-bar evaluation step budget exceeded (limit {})",
                    self.execution_limits.max_steps_per_bar
                ),
            });
        }
        self.execution_steps_remaining -= steps;
        Ok(())
    }

    #[inline]
    pub(crate) fn charge_loop_iteration(&mut self) -> Result<(), RuntimeError> {
        if self.loop_iterations_remaining == 0 {
            return Err(RuntimeError {
                message: format!(
                    "E_EXECUTION_BUDGET: per-bar loop iteration budget exceeded (limit {})",
                    self.execution_limits.max_loop_iterations_per_bar
                ),
            });
        }
        self.loop_iterations_remaining -= 1;
        Ok(())
    }

    pub(crate) fn inherit_execution_budget(&mut self, parent: &Self) {
        self.host_deadline = parent.host_deadline;
        self.execution_limits = parent.execution_limits;
        self.resource_budget = parent.resource_budget.clone();
        self.execution_steps_remaining = parent.execution_steps_remaining;
        self.loop_iterations_remaining = parent.loop_iterations_remaining;
        self.pending_loop_control = None;
    }

    pub(crate) fn accept_execution_budget(&mut self, child: &Self) {
        self.resource_budget = child.resource_budget.clone();
        self.execution_steps_remaining = child.execution_steps_remaining;
        self.loop_iterations_remaining = child.loop_iterations_remaining;
        self.pending_loop_control = child.pending_loop_control;
    }

    // Expression blocks use Result to unwind through scalar evaluators. The
    // associated control signal lives in typed execution state; error text is
    // never interpreted as control flow, and public RuntimeError stays stable.
    pub(crate) fn raise_loop_control(&mut self, control: RuntimeLoopControl) -> RuntimeError {
        self.pending_loop_control = Some(control);
        RuntimeError::escaped_loop_control()
    }
}
