//! Position history advances on every strategy script pass, including guarded
//! reads, but retains only the depth demanded by position itself.
use pine_ir::{HirExprKind, HirHistoryOffset, HirProgram};

pub(crate) fn position_history_depth(program: &HirProgram) -> Option<usize> {
    let mut depth = Some(0);
    super::hir_walk::statements(&program.statements, &mut |expression| {
        let HirExprKind::History { expr, offset } = &expression.kind else {
            return;
        };
        if !matches!(&expr.kind, HirExprKind::Builtin(name) if name == "strategy.position_size") {
            return;
        }
        let required = match offset {
            HirHistoryOffset::Constant(offset) => Some(*offset as usize),
            HirHistoryOffset::Dynamic(_) => program.max_bars_back.map(|depth| depth as usize),
        };
        // Missing series metadata in manually constructed HIR still gets its
        // requirement from the AST. Per-series overrides use the source ID.
        let limit = expr.series_id.and_then(|id| {
            program
                .series_max_bars_back
                .iter()
                .filter(|requirement| requirement.series_id == id)
                .map(|requirement| requirement.max_bars_back as usize)
                .min()
        });
        let required = match (required, limit) {
            (Some(required), Some(limit)) => Some(required.min(limit)),
            (None, Some(limit)) => Some(limit),
            (required, None) => required,
        };
        depth = match (depth, required) {
            (Some(current), Some(required)) => Some(current.max(required)),
            _ => None,
        };
    });
    depth
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Bar, BarUpdate, HistoricalRuntime, PineValue, RealtimeRuntime};

    fn program(source: &str) -> HirProgram {
        let analysis =
            pine_sema::analyze_source(&pine_syntax::SourceFile::new("position.pine", source));
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        analysis.hir.unwrap()
    }

    fn bar(index: i64) -> Bar {
        Bar {
            time: index * 60000,
            open: 10.,
            high: 11.,
            low: 9.,
            close: 10.,
            volume: 1.,
        }
    }

    #[test]
    fn unrelated_dynamic_history_does_not_retain_position_history() {
        let hir = program("//@version=6\nstrategy(\"history\")\nplot(close[bar_index % 2])\n");
        let mut runtime = HistoricalRuntime::new(&hir);
        for index in 0..4096 {
            runtime.append_bar(bar(index)).unwrap();
        }
        assert_eq!(runtime.strategy_position_size_history_depth, Some(0));
        assert_eq!(runtime.strategy_position_size_at_script_pass.len(), 0);
    }

    #[test]
    fn guarded_constant_read_keeps_only_its_depth_even_with_unrelated_dynamic_history() {
        let hir = program(
            "//@version=6\nstrategy(\"history\")\nif bar_index == 0\n    strategy.entry(\"L\", strategy.long, qty=1)\nplot(close[bar_index % 2])\nplot(bar_index > 100 ? strategy.position_size[2] : na)\n",
        );
        let mut runtime = HistoricalRuntime::new(&hir);
        for index in 0..4096 {
            runtime.append_bar(bar(index)).unwrap();
        }
        assert_eq!(runtime.strategy_position_size_history_depth, Some(2));
        assert_eq!(runtime.strategy_position_size_at_script_pass.len(), 3);
        assert_eq!(
            runtime.result().plots[1].values.last(),
            Some(&PineValue::Float(1.))
        );
    }

    #[test]
    fn dynamic_position_history_shares_checkpoint_and_realtime_fill_passes_roll_back() {
        let hir = program(
            "//@version=6\nstrategy(\"history\", calc_on_order_fills=true, calc_on_every_tick=true)\nif bar_index == 0\n    strategy.entry(\"L\", strategy.long, qty=1)\nif bar_index == 2\n    strategy.close(\"L\", immediately=true)\nplot(strategy.position_size[bar_index])\nplot(strategy.position_size[1])\n",
        );
        let mut history = HistoricalRuntime::new(&hir);
        for index in 0..4096 {
            history.append_bar(bar(index)).unwrap();
        }
        let checkpoint = history.clone();
        assert!(std::ptr::eq(
            history
                .strategy_position_size_at_script_pass
                .get(0)
                .unwrap(),
            checkpoint
                .strategy_position_size_at_script_pass
                .get(0)
                .unwrap(),
        ));
        history.append_bar(bar(4096)).unwrap();
        assert_eq!(checkpoint.strategy_position_size_at_script_pass.len(), 4096);
        assert_eq!(history.strategy_position_size_at_script_pass.len(), 4097);
        assert_eq!(
            history.result().plots[0].values.last(),
            Some(&PineValue::Float(0.))
        );
        let mut realtime = RealtimeRuntime::new(&hir);
        let mut replica = realtime.replica();
        for index in 0..8 {
            let confirmed = realtime.confirmed_result();
            for (pass, update) in [
                BarUpdate::forming(bar(index)),
                BarUpdate::forming(bar(index)),
                BarUpdate::confirmed(bar(index)),
            ]
            .into_iter()
            .enumerate()
            {
                let changes = realtime.apply_update(update).unwrap();
                assert!(replica.apply(&changes).unwrap());
                let current = realtime.result();
                assert_eq!(replica.result(), &current);
                assert_eq!(current.plots[0].values.len(), index as usize + 1);
                // The live broker keeps the bar-zero entry between updates,
                // so its second forming pass fills it before confirmation.
                let first_position = if index == 0 && pass == 0 { 0. } else { 1. };
                let previous_position = match index {
                    0 => PineValue::Na,
                    1 | 2 => PineValue::Float(1.),
                    _ => PineValue::Float(0.),
                };
                assert_eq!(
                    current.plots[0].values.last(),
                    Some(&PineValue::Float(first_position)),
                    "bar {index}, pass {pass}",
                );
                assert_eq!(
                    current.plots[1].values.last(),
                    Some(&previous_position),
                    "bar {index}, pass {pass}",
                );
                for (committed, preview) in confirmed.plots.iter().zip(&current.plots) {
                    assert_eq!(committed.values, preview.values[..index as usize]);
                }
                if pass < 2 {
                    assert_eq!(realtime.confirmed_result(), confirmed);
                } else {
                    assert_eq!(realtime.confirmed_result(), current);
                }
            }
        }
        assert_eq!(
            realtime.confirmed_result().plots[0].values,
            vec![PineValue::Float(1.); 8]
        );
        assert_eq!(
            realtime.confirmed_result().plots[1].values,
            vec![
                PineValue::Na,
                PineValue::Float(1.),
                PineValue::Float(1.),
                PineValue::Float(0.),
                PineValue::Float(0.),
                PineValue::Float(0.),
                PineValue::Float(0.),
                PineValue::Float(0.),
            ]
        );
    }

    #[test]
    fn manual_hir_without_history_metadata_and_dynamic_limits_remain_supported() {
        let mut hir =
            program("//@version=6\nstrategy(\"history\")\nplot(strategy.position_size[3])\n");
        hir.history = Default::default();
        hir.series_history.clear();
        assert_eq!(position_history_depth(&hir), Some(3));
        let limited = program(
            "//@version=6\nstrategy(\"history\", max_bars_back=7)\nplot(strategy.position_size[bar_index])\n",
        );
        assert_eq!(position_history_depth(&limited), Some(7));
    }
}
