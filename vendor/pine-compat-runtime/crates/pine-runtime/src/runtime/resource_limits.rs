//! Deterministic per-bar resource allowances, shared with requested evaluators.
use crate::{HistoricalRuntime, RuntimeError};

/// Logical collection allocation and matrix-kernel work limits.
/// Bytes include Pine value payloads and copies recorded by collection storage;
/// they are deliberately not an allocator/RSS quota. `None` disables a limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceLimits {
    pub max_collection_bytes_per_bar: Option<usize>,
    pub max_matrix_work_per_bar: Option<u64>,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            max_collection_bytes_per_bar: Some(64 * 1024 * 1024),
            max_matrix_work_per_bar: Some(100_000_000),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ResourceBudget {
    pub(crate) limits: ResourceLimits,
    collection_bytes: usize,
    collection_exhausted: bool,
    pub(crate) matrix: MatrixWorkBudget,
}

impl Default for ResourceBudget {
    fn default() -> Self {
        Self::new(ResourceLimits::default())
    }
}

impl ResourceBudget {
    fn new(limits: ResourceLimits) -> Self {
        Self {
            limits,
            collection_bytes: 0,
            collection_exhausted: false,
            matrix: MatrixWorkBudget::new(limits.max_matrix_work_per_bar),
        }
    }

    pub(crate) fn reset(&mut self) {
        *self = Self::new(self.limits);
    }

    pub(crate) fn reserve_collection(&mut self, bytes: usize) -> bool {
        let total = self.collection_bytes.checked_add(bytes);
        if self.collection_exhausted
            || total.is_none()
            || self
                .limits
                .max_collection_bytes_per_bar
                .is_some_and(|limit| total.unwrap() > limit)
        {
            self.collection_exhausted = true;
            return false;
        }
        self.collection_bytes = total.unwrap();
        true
    }

    pub(crate) fn collection_error(&self) -> RuntimeError {
        RuntimeError {
            message: format!(
                "E_RESOURCE_BUDGET: per-bar collection allocation budget exceeded (limit {} bytes)",
                self.limits
                    .max_collection_bytes_per_bar
                    .unwrap_or(usize::MAX)
            ),
        }
    }

    pub(crate) fn matrix_error(&self) -> RuntimeError {
        RuntimeError {
            message: format!(
                "E_RESOURCE_BUDGET: per-bar matrix work budget exceeded (limit {} units)",
                self.limits.max_matrix_work_per_bar.unwrap_or(u64::MAX)
            ),
        }
    }

    pub(crate) fn check(&self) -> Result<(), RuntimeError> {
        if self.collection_exhausted {
            return Err(self.collection_error());
        }
        if self.matrix.exhausted {
            return Err(self.matrix_error());
        }
        Ok(())
    }
}

/// A numerical kernel consumes work before entering each expensive phase.
/// Kernels can keep their numerical `Option` results; callers check this state
/// immediately afterwards to distinguish exhaustion from numerical failure.
#[derive(Debug, Clone)]
pub(crate) struct MatrixWorkBudget {
    remaining: Option<u64>,
    exhausted: bool,
}

impl MatrixWorkBudget {
    pub(crate) fn new(limit: Option<u64>) -> Self {
        Self {
            remaining: limit,
            exhausted: false,
        }
    }

    pub(crate) fn spend(&mut self, work: u64) -> bool {
        if self.exhausted {
            return false;
        }
        if let Some(remaining) = &mut self.remaining {
            if work > *remaining {
                self.exhausted = true;
                return false;
            }
            *remaining -= work;
        }
        true
    }
}

impl HistoricalRuntime<'_> {
    #[must_use]
    pub fn with_resource_limits(mut self, limits: ResourceLimits) -> Self {
        self.resource_budget = ResourceBudget::new(limits);
        self
    }

    #[must_use]
    pub fn resource_limits(&self) -> ResourceLimits {
        self.resource_budget.limits
    }
}
