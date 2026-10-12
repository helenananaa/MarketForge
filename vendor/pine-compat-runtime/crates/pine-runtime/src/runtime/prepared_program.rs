//! Immutable execution data shared by independent sessions.
use std::{ops::Deref, sync::Arc};

use pine_ir::HirProgram;

use super::metadata::RuntimeMetadata;
use crate::retention::SeriesRetention;

/// A program whose immutable runtime lookups have been prepared once.
///
/// Clones share the HIR and execution metadata. Each runtime created from it
/// owns independent mutable state and host inputs.
#[derive(Clone, Debug)]
pub struct PreparedProgram {
    hir: Arc<HirProgram>,
    pub(crate) metadata: Arc<RuntimeMetadata>,
    pub(crate) retention: Arc<SeriesRetention>,
    pub(crate) position_history_depth: Option<usize>,
}

impl PreparedProgram {
    #[must_use]
    pub fn new(hir: HirProgram) -> Self {
        Self::from_shared_hir(Arc::new(hir))
    }

    #[must_use]
    pub fn from_shared_hir(hir: Arc<HirProgram>) -> Self {
        let retention = Arc::new(SeriesRetention::from_program(&hir));
        Self {
            metadata: Arc::new(RuntimeMetadata::from_program(&hir, &retention)),
            retention,
            position_history_depth: super::strategy_history::position_history_depth(&hir),
            hir,
        }
    }

    #[must_use]
    pub fn hir(&self) -> &HirProgram {
        &self.hir
    }
}

impl Deref for PreparedProgram {
    type Target = HirProgram;

    fn deref(&self) -> &Self::Target {
        self.hir()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Bar, HistoricalRuntime, PineValue, RealtimeRuntime};

    #[test]
    fn seed_without_output_preserves_transactions_clocks_retention_and_replicas() {
        let source = pine_syntax::SourceFile::new(
            "seed.pine",
            "//@version=6\nindicator(\"seed\")\nif close < 0\n    runtime.error(\"negative\")\nplot(close + timenow)\n",
        );
        let program = PreparedProgram::new(pine_sema::analyze_source(&source).hir.unwrap());
        let mut full = RealtimeRuntime::from_prepared(&program);
        let mut state = RealtimeRuntime::from_prepared(&program);
        let retention = crate::OutputRetention::keep_confirmed_bars(2);
        full.set_output_retention(retention);
        state.set_output_retention(retention);
        let bar = |i: i64, close: f64| Bar {
            time: i * 60_000,
            open: close,
            high: close,
            low: close,
            close,
            volume: 1.0,
        };
        let bars = [bar(0, 1.0), bar(1, 2.0), bar(2, 3.0)];
        let expected = full
            .seed_historical_with_execution_times(&bars, &[1, 2, 3])
            .unwrap();
        state
            .seed_historical_with_execution_times_without_output(&bars, &[1, 2, 3])
            .unwrap();
        assert_eq!(state.result(), expected);
        assert_eq!(state.revision(), full.revision());
        assert_eq!(state.display_origin(), full.display_origin());
        assert_eq!(state.replica().result(), full.replica().result());
        let revision = state.revision();
        assert!(
            state
                .seed_historical_without_output(&[bar(3, 4.0)])
                .is_err()
        );
        assert!(
            state
                .seed_historical_with_execution_times_without_output(&[bar(3, -1.0)], &[4])
                .is_err()
        );
        assert_eq!(state.revision(), revision);
        assert_eq!(state.result(), expected);
        assert_eq!(state.confirmed_bar_count(), 3);
        let update = crate::BarUpdate::forming(bar(3, 5.0));
        let a = state.apply_update_with_execution_time(update, 5).unwrap();
        let b = full.apply_update_with_execution_time(update, 5).unwrap();
        assert_eq!(a, b);
        assert_eq!(state.result(), full.result());
    }

    #[test]
    fn prepared_sessions_share_only_immutable_data_and_outlive_the_program_handle() {
        let source = pine_syntax::SourceFile::new(
            "shared.pine",
            "//@version=6\nindicator(\"shared\")\nvar total=0.0\ntotal+=close\nplot(total)\n",
        );
        let prepared = PreparedProgram::new(pine_sema::analyze_source(&source).hir.unwrap());
        let mut first = HistoricalRuntime::from_prepared(&prepared);
        let mut second = HistoricalRuntime::from_prepared(&prepared);
        assert!(Arc::ptr_eq(&first.metadata, &second.metadata));
        assert!(Arc::ptr_eq(
            &first.series_retention,
            &second.series_retention
        ));
        let mut live = RealtimeRuntime::from_prepared(&prepared);
        drop(prepared);
        let bar = |close| Bar {
            time: 60_000,
            open: close,
            high: close,
            low: close,
            close,
            volume: 1.0,
        };
        first.append_bar(bar(2.0)).unwrap();
        second.append_bar(bar(7.0)).unwrap();
        live.seed_historical_without_output(&[bar(11.0)]).unwrap();
        assert_eq!(first.result().plots[0].values, vec![PineValue::Float(2.0)]);
        assert_eq!(second.result().plots[0].values, vec![PineValue::Float(7.0)]);
        assert_eq!(live.result().plots[0].values, vec![PineValue::Float(11.0)]);
    }
}
