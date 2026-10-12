use pine_syntax::SourceFile;

use super::*;

fn compile_fixture(name: &str, source: &str) -> pine_ir::HirProgram {
    let analysis = pine_sema::analyze_source(&SourceFile::new(name, source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{name}: {:?}",
        analysis.diagnostics
    );
    analysis.hir.expect("versioned arithmetic fixture HIR")
}

#[test]
fn v5_const_integer_division_matches_explicit_v6_casts() {
    let v5 = compile_fixture(
        "v5_const_integer_division.pine",
        include_str!("../../../../tests/fixtures/runtime/v5_const_integer_division.pine"),
    );
    let v6 = compile_fixture(
        "v6_fractional_integer_division.pine",
        include_str!("../../../../tests/fixtures/runtime/v6_fractional_integer_division.pine"),
    );
    let bars = [bar(1.0), bar(2.0), bar(3.0), bar(4.0)];

    let v5_result = run_historical(&v5, &bars).expect("v5 integer division run");
    let v6_result = run_historical(&v6, &bars).expect("v6 explicit division run");

    assert_eq!(v5_result, v6_result);
    assert_values_close(&v5_result.plots[0].values, &[2.0, 2.0, 2.0, 2.0]);
    assert_eq!(v5_result.plots[1].values, v5_result.plots[0].values);
    assert_values_close(&v5_result.plots[2].values, &[2.5, 2.5, 2.5, 2.5]);
    assert_values_close(&v5_result.plots[3].values, &[0.0, 0.5, 1.0, 1.5]);
    assert_eq!(
        v5_result.plots[4].values,
        vec![
            PineValue::Na,
            PineValue::Na,
            PineValue::Float(1.0),
            PineValue::Float(2.0),
        ]
    );
}
