use super::*;

fn program(text: &str) -> pine_ir::HirProgram {
    let analysis = analyze_source(&SourceFile::new("weighted_recovery.pine", text));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    analysis.hir.expect("HIR")
}

fn assert_one_ulp(value: &PineValue, expected: f64) {
    let actual = value
        .as_f64()
        .unwrap_or_else(|| panic!("expected {expected}, got {value:?}"));
    assert!(actual.is_finite());
    if expected == 0.0 {
        assert_eq!(actual, expected);
    } else {
        assert_eq!(actual.is_sign_negative(), expected.is_sign_negative());
        assert!(
            actual.to_bits().abs_diff(expected.to_bits()) <= 1,
            "expected {expected} ({:x}), got {actual} ({:x})",
            expected.to_bits(),
            actual.to_bits()
        );
    }
}

fn assert_plot_bits(actual: &RuntimeResult, expected: &RuntimeResult) {
    assert_eq!(actual.plots.len(), expected.plots.len());
    for (actual, expected) in actual.plots.iter().zip(&expected.plots) {
        assert_eq!(actual.values.len(), expected.values.len());
        for (actual, expected) in actual.values.iter().zip(&expected.values) {
            match (actual, expected) {
                (PineValue::Float(actual), PineValue::Float(expected)) => {
                    assert_eq!(actual.to_bits(), expected.to_bits());
                }
                _ => assert_eq!(actual, expected),
            }
        }
    }
}

#[test]
fn wma_hma_extreme_constants_keep_warmup_and_recover_constant_bits() {
    for length in [1_usize, 2, 3, 16] {
        let hir = program(&format!(
            "indicator(\"extreme weighted constants\")\nplot(ta.wma(close, {length}))\nplot(ta.hma(close, {length}))\n"
        ));
        let count = length + (length as f64).sqrt().round() as usize + 2;
        let reference = run_historical(&hir, &vec![bar(1.0); count]).unwrap();
        for value in [1e308, -1e308, f64::MAX, -f64::MAX] {
            let actual = run_historical(&hir, &vec![bar(value); count]).unwrap();
            for (actual, expected) in actual.plots.iter().zip(&reference.plots) {
                for (actual, expected) in actual.values.iter().zip(&expected.values) {
                    match expected {
                        PineValue::Na => assert_eq!(actual, &PineValue::Na),
                        PineValue::Float(expected) => {
                            assert_eq!(*expected, 1.0);
                            assert_eq!(
                                actual.as_f64().expect("finite constant").to_bits(),
                                value.to_bits()
                            );
                        }
                        _ => panic!("unexpected reference {expected:?}"),
                    }
                }
            }
        }
    }
}

#[test]
fn wma_hma_nonconstant_extremes_match_independent_fraction_oracles() {
    let hir = program(
        "indicator(\"weighted fractions\")\nplot(ta.wma(close, 2))\nplot(ta.hma(close, 2))\n",
    );
    // WMA uses exact weights [1, 2]. For HMA length 2 the exact source formula
    // is (4 * newest - oldest) / 3; each stage permits one f64 rounding.
    for (values, wma_bits, hma_bits) in [
        ([8e307, 1e308], 0x7fe0_9d27_8e0f_43c8, 0x7fe2_fcbf_7dc8_4d78),
        ([1e308, 8e307], 0x7fde_dab7_2c65_7de2, 0x7fda_1b87_4cf3_6a84),
        (
            [1e308, -1e308],
            0xffc7_bbef_5d3a_60d5,
            0xffed_aaeb_3488_f90b,
        ),
    ] {
        for sign in [1.0, -1.0] {
            let bars: Vec<_> = values.into_iter().map(|value| bar(sign * value)).collect();
            let actual = run_historical(&hir, &bars).unwrap();
            assert_eq!(actual.plots[0].values[0], PineValue::Na);
            assert_eq!(actual.plots[1].values[0], PineValue::Na);
            assert_one_ulp(&actual.plots[0].values[1], sign * f64::from_bits(wma_bits));
            assert_one_ulp(&actual.plots[1].values[1], sign * f64::from_bits(hma_bits));
        }
    }
    let hir = program("indicator(\"weighted cancellation\")\nplot(ta.wma(close, 4))\n");
    let actual = run_historical(&hir, &[bar(1e308), bar(1e308), bar(-1e308), bar(1.0)]).unwrap();
    assert_one_ulp(&actual.plots[0].values[3], 0.4);
}

#[test]
fn hma_out_of_range_difference_stays_na_until_its_smoothing_tail_expires() {
    let hir = program(
        "indicator(\"weighted out of range\")\nplot(ta.wma(close, 2))\nplot(ta.hma(close, 2))\n",
    );
    // For length 2 the first difference is 5 * MAX / 3, which exceeds f64.
    let actual = run_historical(&hir, &[bar(-f64::MAX), bar(f64::MAX), bar(f64::MAX)]).unwrap();
    assert_one_ulp(
        &actual.plots[0].values[1],
        f64::from_bits(0x7fd5_5555_5555_5555),
    );
    assert_eq!(actual.plots[1].values[1], PineValue::Na);
    assert_eq!(
        actual.plots[1].values[2].as_f64().unwrap().to_bits(),
        f64::MAX.to_bits()
    );
}

#[test]
fn wma_hma_extreme_dynamic_lengths_and_na_keep_the_ordinary_ready_mask() {
    let hir = program(
        "indicator(\"weighted dynamic NA\")\nlength = bar_index % 4 == 2 ? 3 : 2\nsource = bar_index % 7 == 3 ? na : close\nplot(ta.wma(source, length))\nplot(ta.hma(source, length))\n",
    );
    let reference = run_historical(&hir, &vec![bar(1.0); 40]).unwrap();
    for value in [1e308, -1e308] {
        let actual = run_historical(&hir, &vec![bar(value); 40]).unwrap();
        for (actual, expected) in actual.plots.iter().zip(&reference.plots) {
            for (actual, expected) in actual.values.iter().zip(&expected.values) {
                if expected == &PineValue::Na {
                    assert_eq!(actual, expected);
                } else {
                    assert_eq!(expected.as_f64(), Some(1.0));
                    assert_eq!(actual.as_f64().unwrap().to_bits(), value.to_bits());
                }
            }
        }
    }
}

#[test]
fn weighted_extreme_forming_replacements_discard_and_confirm_match_fresh_history() {
    let hir = program(
        "indicator(\"weighted forming\")\nlength = volume > 1 ? 3 : 2\nsource = volume == 0 ? na : close\nplot(ta.wma(source, length))\nplot(ta.hma(source, length))\n",
    );
    let mut committed = vec![
        bar_volume(1e308, 1.0),
        bar_volume(8e307, 2.0),
        bar_volume(-6e307, 1.0),
    ];
    for (index, bar) in committed.iter_mut().enumerate() {
        bar.time = index as i64;
    }
    let mut runtime = RealtimeRuntime::new(&hir);
    for &bar in &committed {
        runtime.update(BarUpdate::historical(bar)).unwrap();
    }
    for (value, volume) in [(1e308, 1.0), (-1e308, 2.0), (1e308, 0.0), (8e307, 2.0)] {
        let mut forming = bar_volume(value, volume);
        forming.time = 3;
        let actual = runtime.update(BarUpdate::forming(forming)).unwrap();
        let mut bars = committed.clone();
        bars.push(forming);
        assert_plot_bits(&actual, &run_historical(&hir, &bars).unwrap());
    }
    let discarded = runtime.correct_historical(0, &committed).unwrap();
    assert_plot_bits(&discarded, &run_historical(&hir, &committed).unwrap());

    let mut final_bar = bar_volume(1e308, 2.0);
    final_bar.time = 3;
    runtime.update(BarUpdate::forming(final_bar)).unwrap();
    let actual = runtime.update(BarUpdate::confirmed(final_bar)).unwrap();
    committed.push(final_bar);
    assert_plot_bits(&actual, &run_historical(&hir, &committed).unwrap());
    for (index, value) in [1.0, 2.0, 3.0, 4.0].into_iter().enumerate() {
        let mut next = bar_volume(value, 1.0);
        next.time = (index + 4) as i64;
        let actual = runtime.update(BarUpdate::confirmed(next)).unwrap();
        committed.push(next);
        assert_plot_bits(&actual, &run_historical(&hir, &committed).unwrap());
    }
}
