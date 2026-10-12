//! Callsite calculation state that rolls back together before a strategy pass.
//!
//! This also owns stateful scalar helpers (`fixnan`, seeded random calls) that
//! already followed the same checkpoint policy as TA calls. Pure caches and
//! scratch, execution/resource allowances, requested contexts, and `varip`
//! storage have separate lifetimes and must not enter this state group.

use std::collections::HashMap;

use pine_ir::CallSiteId;

use crate::PineValue;
use crate::algorithms::rolling_extreme::RollingExtremeState;
use crate::algorithms::rolling_window::{RollingWindowKey, RollingWindowState};
use crate::builtins::ta::{MacdState, PivotPointState, RsiState, VwapState};
use crate::runtime::valuewhen_history::ValueWhenHistory;

#[derive(Clone)]
pub(crate) struct CrossCallState {
    pub(crate) bar_index: usize,
    pub(crate) current_left: PineValue,
    pub(crate) current_right: PineValue,
    pub(crate) previous_left: PineValue,
    pub(crate) previous_right: PineValue,
}

#[derive(Clone, Default)]
pub(crate) struct TaRollbackState {
    pub(crate) rolling_windows: HashMap<RollingWindowKey, RollingWindowState>,
    pub(crate) extreme_windows: HashMap<CallSiteId, RollingExtremeState>,
    pub(crate) rsi_state: HashMap<CallSiteId, RsiState>,
    pub(crate) macd_state: HashMap<CallSiteId, MacdState>,
    pub(crate) call_state: HashMap<CallSiteId, PineValue>,
    pub(crate) cross_state: HashMap<CallSiteId, CrossCallState>,
    pub(crate) valuewhen_state: HashMap<CallSiteId, ValueWhenHistory>,
    pub(crate) vwap_call_state: HashMap<CallSiteId, VwapState>,
    pub(crate) pivot_point_state: HashMap<CallSiteId, PivotPointState>,
    pub(crate) random_state: HashMap<CallSiteId, u64>,
}

impl TaRollbackState {
    pub(crate) fn restore_from(&mut self, checkpoint: &Self) {
        // Exhaustive destructuring makes a newly added rollback field require
        // an explicit restore rule here. Retain HashMap::clone_from's reuse of
        // existing allocations instead of replacing the whole group.
        let Self {
            rolling_windows,
            extreme_windows,
            rsi_state,
            macd_state,
            call_state,
            cross_state,
            valuewhen_state,
            vwap_call_state,
            pivot_point_state,
            random_state,
        } = self;
        rolling_windows.clone_from(&checkpoint.rolling_windows);
        extreme_windows.clone_from(&checkpoint.extreme_windows);
        rsi_state.clone_from(&checkpoint.rsi_state);
        macd_state.clone_from(&checkpoint.macd_state);
        call_state.clone_from(&checkpoint.call_state);
        cross_state.clone_from(&checkpoint.cross_state);
        valuewhen_state.clone_from(&checkpoint.valuewhen_state);
        vwap_call_state.clone_from(&checkpoint.vwap_call_state);
        pivot_point_state.clone_from(&checkpoint.pivot_point_state);
        random_state.clone_from(&checkpoint.random_state);
    }
}

#[cfg(test)]
mod tests {
    use super::super::HistoricalRuntime;
    use crate::{ExecutionLimits, ResourceLimits};

    #[test]
    fn restoring_calculation_state_does_not_refund_per_bar_work_or_resources() {
        let source = pine_syntax::SourceFile::new(
            "checkpoint-budget.pine",
            "//@version=6\nindicator(\"checkpoint budget\")\nplot(close)\n",
        );
        let hir = pine_sema::analyze_source(&source).hir.unwrap();
        let mut runtime = HistoricalRuntime::new(&hir)
            .with_execution_limits(ExecutionLimits {
                max_steps_per_bar: 10,
                max_loop_iterations_per_bar: 2,
            })
            .with_resource_limits(ResourceLimits {
                max_collection_bytes_per_bar: Some(10),
                max_matrix_work_per_bar: Some(10),
            });
        runtime.snapshot_strategy_eval_checkpoint();
        runtime.charge_execution_steps(8).unwrap();
        runtime.charge_loop_iteration().unwrap();
        assert!(runtime.resource_budget.reserve_collection(8));
        assert!(runtime.resource_budget.matrix.spend(8));

        runtime.restore_strategy_eval_checkpoint();

        assert_eq!(runtime.execution_steps_remaining, 2);
        assert_eq!(runtime.loop_iterations_remaining, 1);
        assert!(!runtime.resource_budget.reserve_collection(3));
        assert!(!runtime.resource_budget.matrix.spend(3));
    }
}
