use super::*;
use pine_sema::analyze_source;
use pine_syntax::SourceFile;

thread_local! {
    static OBJECT_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub(super) fn record_object_visit() {
    OBJECT_VISITS.set(OBJECT_VISITS.get() + 1);
}

fn program(source: &str) -> pine_ir::HirProgram {
    let analysis = analyze_source(&SourceFile::new("join.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    analysis.hir.unwrap()
}

#[test]
fn join_budget_counts_unicode_characters_and_checks_before_appending() {
    let value = "界💥".repeat(MAX_STRING_CHARS / 2);
    let mut output = BoundedJoin::default();
    output.push_str(&value).unwrap();
    assert_eq!(output.chars, MAX_STRING_CHARS);
    assert_eq!(output.output.len(), 7 * MAX_STRING_CHARS / 2);
    assert_eq!(
        output.push_str("é").unwrap_err().message,
        "array.join result cannot exceed 40960 characters"
    );
    assert_eq!(output.output, value);
}

#[test]
fn join_formats_borrowed_udt_fields_and_nested_values_without_element_buffers() {
    let program = program(
        "//@version=6\nindicator(\"join\")\ntype Inner\n    string value\ntype Outer\n    Inner child\n    bool flag\nplot(close)\n",
    );
    let mut runtime = HistoricalRuntime::new(&program);
    runtime
        .object_store
        .insert(0, vec![PineValue::String("界".into())]);
    runtime
        .object_store
        .insert(1, vec![PineValue::UserTypeRef(0), PineValue::Bool(true)]);
    let mut output = BoundedJoin::default();
    output
        .push_element(&PineValue::UserTypeRef(1), Some("Outer"), &runtime)
        .unwrap();
    assert_eq!(
        output.finish(),
        PineValue::String("Outer(Inner(界), true)".into())
    );
    let large = PineValue::UserType(vec![
        PineValue::String("x".repeat(30000)),
        PineValue::String("y".repeat(30000)),
    ]);
    let mut output = BoundedJoin::default();
    let error = output
        .push_element(&large, Some("Large"), &runtime)
        .unwrap_err();
    assert!(error.message.contains("array.join result cannot exceed"));
    assert_eq!(output.chars, 30008);
    assert!(output.output.len() <= MAX_STRING_CHARS);
}

#[test]
fn join_retains_invalid_reference_and_cycle_errors_before_formatting_an_object() {
    let program = program("//@version=6\nindicator(\"join\")\nplot(close)\n");
    let mut runtime = HistoricalRuntime::new(&program);
    runtime
        .object_store
        .insert(0, vec![PineValue::UserTypeRef(0)]);
    for (id, expected) in [
        (0, "cyclic UDT cannot be materialized as a value tree"),
        (1, "invalid UDT object reference"),
    ] {
        let mut output = BoundedJoin::default();
        let error = output
            .push_element(&PineValue::UserTypeRef(id), Some("Node"), &runtime)
            .unwrap_err();
        assert_eq!(error.message, expected);
        assert!(output.output.is_empty());
    }
}

#[test]
fn shared_object_dag_validation_visits_each_object_once() {
    let program = program("//@version=6\nindicator(\"join\")\nplot(close)\n");
    let mut runtime = HistoricalRuntime::new(&program);
    runtime
        .object_store
        .insert(0, vec![PineValue::String("leaf".into())]);
    // Thirteen objects describe 8,191 expanded tree nodes. Keep the depth
    // modest so removing the memo remains a bounded, deterministic failure.
    for id in 1..=12 {
        runtime
            .object_store
            .insert(id, vec![PineValue::UserTypeRef(id - 1); 2]);
    }
    OBJECT_VISITS.set(0);
    let mut completed = HashSet::new();
    validate_object_tree(
        &PineValue::UserTypeRef(12),
        &runtime,
        &mut HashSet::new(),
        &mut completed,
    )
    .unwrap();
    assert_eq!(OBJECT_VISITS.get(), 13);
    assert_eq!(completed.len(), 13);
}

#[test]
fn completed_subtree_memo_does_not_hide_cycles_or_invalid_references() {
    let program = program("//@version=6\nindicator(\"join\")\nplot(close)\n");
    let mut runtime = HistoricalRuntime::new(&program);
    runtime.object_store.insert(0, vec![PineValue::Int(1)]);
    runtime.object_store.insert(
        1,
        vec![PineValue::UserTypeRef(0), PineValue::UserTypeRef(2)],
    );
    runtime.object_store.insert(
        2,
        vec![PineValue::UserTypeRef(0), PineValue::UserTypeRef(1)],
    );
    runtime.object_store.insert(
        3,
        vec![PineValue::UserTypeRef(0), PineValue::UserTypeRef(99)],
    );
    for (id, expected) in [
        (1, "cyclic UDT cannot be materialized as a value tree"),
        (3, "invalid UDT object reference"),
    ] {
        let mut completed = HashSet::new();
        let error = validate_object_tree(
            &PineValue::UserTypeRef(id),
            &runtime,
            &mut HashSet::new(),
            &mut completed,
        )
        .unwrap_err();
        assert_eq!(error.message, expected);
        assert!(completed.contains(&0));
        assert!(!completed.contains(&id));
    }
}
