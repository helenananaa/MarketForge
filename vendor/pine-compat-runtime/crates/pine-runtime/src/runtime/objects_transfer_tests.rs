use super::*;

fn program() -> pine_ir::HirProgram {
    let analysis = pine_sema::analyze_source(&pine_syntax::SourceFile::new(
        "transfer.pine",
        "//@version=6\nindicator(\"transfer\")\ntype Holder\n    varip int sticky\n    int ordinary\nplot(close)\n",
    ));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    analysis.hir.unwrap()
}

#[test]
fn scalar_elements_never_enter_or_allocate_the_reference_queue() {
    let values = vec![PineValue::String("scalar payload".repeat(8)); 100_000];
    let mut pending = Vec::new();
    for value in &values {
        enqueue_intrabar_references(&mut pending, value, true);
    }
    assert!(pending.is_empty());
    assert_eq!(pending.capacity(), 0);
    for kind in [
        ArrayElementKind::Float,
        ArrayElementKind::Int,
        ArrayElementKind::Bool,
        ArrayElementKind::String,
        ArrayElementKind::Color,
    ] {
        assert!(scalar_array_kind(Some(&kind)));
    }
    assert!(!scalar_array_kind(Some(&ArrayElementKind::UserType)));
    assert!(!scalar_array_kind(None));
}

#[test]
fn inline_values_queue_only_nested_handles_and_preserve_retention_flags() {
    let value = PineValue::Tuple(vec![
        PineValue::String("unused scalar".repeat(1024)),
        PineValue::UserType(vec![
            PineValue::Array(3),
            PineValue::Tuple(vec![PineValue::Bool(true), PineValue::UserTypeRef(5)]),
        ]),
        PineValue::Matrix(7),
        PineValue::Map(11),
        PineValue::Label(13),
        PineValue::ChartPoint(ChartPointValue::new(
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Float(3.),
        )),
    ]);
    let mut pending = Vec::new();
    enqueue_intrabar_references(&mut pending, &value, false);
    assert_eq!(
        pending,
        [
            (IntrabarReference::Array(3), false),
            (IntrabarReference::Object(5), false),
            (IntrabarReference::Matrix(7), false),
            (IntrabarReference::Map(11), false),
        ]
    );
}

#[test]
fn typed_scalar_slices_import_shared_payloads_without_changing_checkpoints() {
    use crate::builtins::arrays::ArraySlice;
    let hir = program();
    let mut committed = HistoricalRuntime::new(&hir);
    let mut forming = committed.clone();
    let PineValue::Array(parent) =
        forming.new_array_from_values(ArrayElementKind::Float, vec![PineValue::Float(7.); 100_000])
    else {
        panic!("array")
    };
    let slice = forming.next_array_id;
    forming.next_array_id += 1;
    forming.array_slices.insert(
        slice,
        ArraySlice {
            parent_id: parent,
            start: 127,
            len: 130,
        },
    );
    forming.array_kinds.insert(slice, ArrayElementKind::Float);
    committed
        .seed_intrabar_objects_from(&forming, vec![PineValue::Array(slice)])
        .unwrap();
    assert!(std::ptr::eq(
        committed.array_store.get(&parent).unwrap(),
        forming.array_store.get(&parent).unwrap()
    ));
    assert_eq!(
        committed.array_get_cloned(slice, 129).unwrap(),
        Some(PineValue::Float(7.))
    );
    committed
        .array_set_value(slice, 1, PineValue::Float(9.))
        .unwrap();
    assert_eq!(
        forming.array_get_cloned(parent, 128).unwrap(),
        Some(PineValue::Float(7.))
    );
    assert_eq!(
        committed.array_get_cloned(parent, 128).unwrap(),
        Some(PineValue::Float(9.))
    );
}

#[test]
fn inline_nested_object_arrays_keep_varip_fields_and_roll_back_ordinary_fields() {
    let hir = program();
    let mut committed = HistoricalRuntime::new(&hir);
    let sticky_array =
        committed.new_array_from_values(ArrayElementKind::Float, vec![PineValue::Float(1.)]);
    let ordinary_array =
        committed.new_array_from_values(ArrayElementKind::Float, vec![PineValue::Float(2.)]);
    let held = committed
        .allocate_object(
            vec![sticky_array.clone(), ordinary_array.clone()],
            &hir.user_types[0].identity,
        )
        .unwrap();
    let PineValue::Array(sticky_id) = sticky_array else {
        panic!("array")
    };
    let PineValue::Array(ordinary_id) = ordinary_array else {
        panic!("array")
    };
    let PineValue::UserTypeRef(held_id) = held else {
        panic!("object")
    };
    let checkpoint = committed.clone();
    let mut forming = committed.clone();
    forming
        .array_set_value(sticky_id, 0, PineValue::Float(10.))
        .unwrap();
    forming
        .array_set_value(ordinary_id, 0, PineValue::Float(20.))
        .unwrap();
    let nested = forming
        .allocate_object(
            vec![PineValue::Float(3.), PineValue::Float(4.)],
            &hir.user_types[0].identity,
        )
        .unwrap();
    let reference_array = forming.new_array_from_values(
        ArrayElementKind::UserType,
        vec![PineValue::UserType(vec![PineValue::Tuple(vec![
            nested.clone(),
            PineValue::UserTypeRef(held_id),
        ])])],
    );
    committed
        .seed_intrabar_objects_from(
            &forming,
            vec![PineValue::Tuple(vec![PineValue::UserType(vec![
                reference_array.clone(),
            ])])],
        )
        .unwrap();
    assert_eq!(
        committed.array_get_cloned(sticky_id, 0).unwrap(),
        Some(PineValue::Float(10.))
    );
    assert_eq!(
        committed.array_get_cloned(ordinary_id, 0).unwrap(),
        Some(PineValue::Float(2.))
    );
    assert_eq!(
        checkpoint.array_get_cloned(sticky_id, 0).unwrap(),
        Some(PineValue::Float(1.))
    );
    let PineValue::UserTypeRef(nested_id) = nested else {
        panic!("object")
    };
    assert_eq!(
        committed.object_field(nested_id, 1).unwrap(),
        PineValue::Float(4.)
    );
    let PineValue::Array(reference_id) = reference_array else {
        panic!("array")
    };
    assert!(std::ptr::eq(
        committed.array_store.get(&reference_id).unwrap(),
        forming.array_store.get(&reference_id).unwrap()
    ));
}
