use pine_runtime::{
    FillAction, FillGradientSample, PineValue, RuntimeReplica, public_runtime_changes_json,
    public_runtime_result_json, runtime_changes_from_json, runtime_result_from_json,
};
use serde_json::{Value, json};

#[test]
fn public_parsers_preserve_non_object_and_malformed_json_errors() {
    for wire in ["null", "[]", "true", "0", "-0", "\"text\""] {
        assert_eq!(
            runtime_result_from_json(wire).unwrap_err(),
            "runtime result must be a JSON object",
            "{wire}"
        );
        assert_eq!(
            runtime_changes_from_json(wire).unwrap_err(),
            "runtime changes must be a JSON object",
            "{wire}"
        );
    }
    for wire in ["", "{", "{\"plots\":[}", "{} trailing", "{\"x\":1e400}"] {
        let error = serde_json::from_str::<Value>(wire).unwrap_err();
        assert_eq!(
            runtime_result_from_json(wire).unwrap_err(),
            format!("runtime result must be JSON: {error}"),
            "{wire}"
        );
        assert_eq!(
            runtime_changes_from_json(wire).unwrap_err(),
            format!("runtime changes must be JSON: {error}"),
            "{wire}"
        );
    }
    assert_eq!(
        runtime_result_from_json("{}").unwrap(),
        pine_runtime::RuntimeResult::default()
    );
    assert_eq!(
        runtime_changes_from_json("{}").unwrap_err(),
        "missing or invalid `visibility`"
    );
}

fn assert_float(value: &PineValue, expected: f64) {
    let PineValue::Float(value) = value else {
        panic!("expected Float({expected}), got {value:?}");
    };
    assert_eq!(value.to_bits(), expected.to_bits());
}

#[test]
fn parsed_snapshot_owns_nested_pine_values_and_ignores_extension_fields() {
    let mut wire = r#"{
        "plots":[{"id":7,"title":"owned \u96ea \n \u0000","values":[
            null,true,-9223372036854775808,18446744073709551615,-0,5e-324,
            1.7976931348623157e308,["tuple \u96ea",{
                "time":null,"index":7,"price":-0,"extension":{"unused":[1,2,3]}
            }]
        ],"extension":[{"unused":"plot"}]}],
        "extension":{"unused":[{"nested":"root"}]}
    }"#
    .to_owned();
    let parsed = runtime_result_from_json(&wire).unwrap();
    wire.clear();
    drop(wire);
    let plot = &parsed.plots[0];
    assert_eq!(
        plot.metadata.title,
        PineValue::String("owned 雪 \n \0".into())
    );
    assert_eq!(plot.values[0], PineValue::Na);
    assert_eq!(plot.values[1], PineValue::Bool(true));
    assert_eq!(plot.values[2], PineValue::Int(i64::MIN));
    assert_eq!(plot.values[3], PineValue::Color(u64::MAX));
    assert_float(&plot.values[4], -0.0);
    assert_float(&plot.values[5], f64::from_bits(1));
    assert_float(&plot.values[6], f64::MAX);
    let PineValue::Tuple(tuple) = &plot.values[7] else {
        panic!("expected nested tuple");
    };
    assert_eq!(tuple[0], PineValue::String("tuple 雪".into()));
    let PineValue::ChartPoint(point) = &tuple[1] else {
        panic!("expected nested chart point");
    };
    assert_eq!(*point.time, PineValue::Na);
    assert_eq!(*point.index, PineValue::Int(7));
    assert_float(&point.price, -0.0);
    let encoded = public_runtime_result_json(&parsed);
    assert!(!encoded.contains("extension"));
    assert_eq!(
        public_runtime_result_json(&runtime_result_from_json(&encoded).unwrap()),
        encoded
    );
}

fn gradient_snapshot(sample: &Value) -> String {
    format!(
        r#"{{"schemaVersion":9,"fills":[{{"id":3,"firstId":1,"secondId":2,"colors":[null],"gradient":[{sample}]}}]}}"#
    )
}

fn parsed_sample(sample: &Value) -> Result<FillGradientSample, String> {
    runtime_result_from_json(&gradient_snapshot(sample))
        .map(|result| result.fills[0].gradient.as_ref().unwrap()[0].clone())
}

fn assert_sample_bits(actual: &FillGradientSample, expected: &FillGradientSample) {
    assert_eq!(
        actual.top_value.map(f64::to_bits),
        expected.top_value.map(f64::to_bits)
    );
    assert_eq!(
        actual.bottom_value.map(f64::to_bits),
        expected.bottom_value.map(f64::to_bits)
    );
    assert_eq!(actual.top_color, expected.top_color);
    assert_eq!(actual.bottom_color, expected.bottom_color);
}

#[test]
fn borrowed_gradient_deserialization_preserves_owned_serde_error_contract() {
    let base = json!({"topValue":null,"bottomValue":null,"topColor":null,"bottomColor":null});
    // The former parser used owned serde deserialization. Check its number and
    // field errors while the new path borrows the already parsed JSON tree.
    for field in [
        "topValue",
        "bottomValue",
        "topColor",
        "bottomColor",
        "extension",
    ] {
        for token in [
            "null",
            "true",
            "\"value\"",
            "[]",
            "{}",
            "-1",
            "-0",
            "0",
            "1.5",
            "5e-324",
            "1.7976931348623157e308",
            "18446744073709551616",
        ] {
            let mut sample = base.clone();
            sample[field] = serde_json::from_str(token).unwrap();
            let former: Result<FillGradientSample, _> = serde_json::from_value(sample.clone());
            let actual = parsed_sample(&sample);
            match former {
                Ok(expected) => assert_sample_bits(&actual.unwrap(), &expected),
                Err(error) => assert_eq!(
                    actual.unwrap_err(),
                    format!("invalid gradient sample: {error}"),
                    "{field}: {token}"
                ),
            }
        }
    }
}

#[test]
fn gradient_validation_keeps_required_fields_color_and_schema_errors() {
    let base = json!({"topValue":null,"bottomValue":null,"topColor":null,"bottomColor":null});
    for field in ["topValue", "bottomValue", "topColor", "bottomColor"] {
        let mut sample = base.clone();
        sample.as_object_mut().unwrap().remove(field);
        assert_eq!(
            parsed_sample(&sample).unwrap_err(),
            format!("gradient sample requires {field}")
        );
    }
    for value in [Value::Null, json!([]), json!(true)] {
        assert_eq!(
            parsed_sample(&value).unwrap_err(),
            "gradient sample must be an object"
        );
    }
    for color in [4_311_744_512_u64, u64::MAX] {
        let mut sample = base.clone();
        sample["topColor"] = color.into();
        assert_eq!(
            parsed_sample(&sample).unwrap_err(),
            "invalid gradient color encoding"
        );
    }
    for color in [0_u64, u64::from(u32::MAX), (1 << 32) | 0x00FF_FFFF] {
        let mut sample = base.clone();
        sample["topColor"] = color.into();
        assert_eq!(parsed_sample(&sample).unwrap().top_color, Some(color));
    }
    let old = gradient_snapshot(&base).replacen("\"schemaVersion\":9", "\"schemaVersion\":8", 1);
    assert_eq!(
        runtime_result_from_json(&old).unwrap_err(),
        "gradient fill requires runtime schema 9"
    );
}

#[test]
fn parsed_gradient_changes_preserve_float_bits_and_replica_retransmission() {
    let seed = r#"{"schemaVersion":9,"fills":[{
        "id":3,"firstId":1,"secondId":2,"colors":[null,null],"gradient":[
            {"topValue":1.7976931348623157e308,"bottomValue":-0,"topColor":4294967296,"bottomColor":null},
            {"topValue":null,"bottomValue":5e-324,"topColor":null,"bottomColor":0}
        ]
    }],"extension":[{"ignored":true}]}"#;
    let parsed = runtime_result_from_json(seed).unwrap();
    let initial = parsed.fills[0].gradient.as_ref().unwrap();
    assert_eq!(initial[0].top_value.unwrap().to_bits(), f64::MAX.to_bits());
    assert_eq!(
        initial[0].bottom_value.unwrap().to_bits(),
        (-0.0_f64).to_bits()
    );
    assert_eq!(initial[1].bottom_value.unwrap().to_bits(), 1);
    let mut replica = RuntimeReplica::new(parsed, 10);
    let wire = r#"{"schemaVersion":4,"baseRevision":10,"revision":11,
        "retainedFrom":0,"visibility":"confirmed","fills":[{
            "id":3,"action":"setGradient","start":0,"values":[
                {"topValue":-0,"bottomValue":5e-324,"topColor":0,"bottomColor":null},
                {"topValue":1.7976931348623157e308,"bottomValue":null,"topColor":null,"bottomColor":4294967296}
            ]
        }],"extension":{"ignored":[null]}}"#;
    let changes = runtime_changes_from_json(wire).unwrap();
    let FillAction::SetGradient { values, .. } = &changes.fills[0].action else {
        panic!("expected gradient changes");
    };
    assert!(replica.apply(&changes).unwrap());
    let applied = replica.result().fills[0].gradient.as_ref().unwrap();
    assert_eq!(applied.len(), values.len());
    for (actual, expected) in applied.iter().zip(values) {
        assert_sample_bits(actual, expected);
    }
    let output = public_runtime_result_json(replica.result());
    let changes_wire = public_runtime_changes_json(&changes);
    assert!(!changes_wire.contains("extension"));
    let replay = runtime_changes_from_json(&changes_wire).unwrap();
    assert!(!replica.apply(&replay).unwrap());
    assert_eq!(replica.revision(), 11);
    assert_eq!(public_runtime_result_json(replica.result()), output);
    let old = wire.replacen("\"schemaVersion\":4", "\"schemaVersion\":3", 1);
    assert_eq!(
        runtime_changes_from_json(&old).unwrap_err(),
        "gradient fill requires changes schema 4"
    );
}
