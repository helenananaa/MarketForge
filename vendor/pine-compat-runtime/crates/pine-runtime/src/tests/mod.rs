mod alerts;
mod alma_cache_tests;
mod arrays;
mod builtin_registry;
mod builtins_colors;
mod builtins_core;
mod builtins_inputs;
mod builtins_math;
mod builtins_strings;
mod builtins_ta_averages;
mod builtins_ta_conditionals;
mod builtins_ta_extremes;
mod builtins_ta_flow;
mod builtins_time;
mod drawing_indexes;
mod execution_clock;
mod external;
mod imports;
mod legacy_indicators;
mod magnifier;
mod matrices;
mod methods;
mod outputs;
mod realtime;
mod realtime_market_extremes;
mod realtime_opening_context;
mod request;
mod rolling_extrema;
mod runtime_const_history;
mod runtime_control_flow;
mod runtime_core;
mod runtime_history;
mod strategy;
mod strategy_margin_admission;
mod strategy_native_defaults;
mod strategy_regressions;
mod strategy_versioned_margin;
mod user_types;
mod versioned_arithmetic;

use super::*;

fn modern_source_file(name: impl Into<String>, text: impl Into<String>) -> pine_syntax::SourceFile {
    let name = name.into();
    let text = text.into();
    if text
        .lines()
        .any(|line| line.trim_start().starts_with("//@version="))
    {
        pine_syntax::SourceFile::new(name, text)
    } else {
        pine_syntax::SourceFile::new(name, format!("//@version=5\n{text}"))
    }
}

fn analyze_source(source: &pine_syntax::SourceFile) -> pine_sema::Analysis {
    pine_sema::analyze_source(&modern_source_file(source.name(), source.text()))
}

fn bar(close: f64) -> Bar {
    bar_ohlc(close, close, close, close)
}

fn bar_volume(close: f64, volume: f64) -> Bar {
    Bar {
        time: 0,
        open: close,
        high: close,
        low: close,
        close,
        volume,
    }
}

fn bar_ohlc(open: f64, high: f64, low: f64, close: f64) -> Bar {
    bar_ohlcv(open, high, low, close, 1.0)
}

fn bar_ohlcv(open: f64, high: f64, low: f64, close: f64, volume: f64) -> Bar {
    Bar {
        time: 0,
        open,
        high,
        low,
        close,
        volume,
    }
}

fn assert_values_close(actual: &[PineValue], expected: &[f64]) {
    assert_eq!(actual.len(), expected.len(), "actual={actual:?}");
    for (index, (value, expected)) in actual.iter().zip(expected).enumerate() {
        let Some(actual_number) = value.as_f64() else {
            panic!("expected numeric value at {index}, got {value:?} in {actual:?}");
        };
        assert!(
            (actual_number - expected).abs() < 1e-10,
            "expected {expected} at {index}, got {actual_number} in {actual:?}"
        );
    }
}

fn assert_na_prefix(actual: &[PineValue], count: usize) {
    assert!(
        actual.len() >= count,
        "series shorter than na prefix: {actual:?}"
    );
    for value in &actual[..count] {
        assert!(
            value.is_na(),
            "expected leading na, got {value:?} in {actual:?}"
        );
    }
}
