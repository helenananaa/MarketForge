use std::cell::Cell;

use super::{
    OutputMetadata, PineValue, SeriesChangeOp, SeriesFamily, SeriesFields, SeriesHeader,
    series_change_from_lens,
};

#[test]
fn lazy_header_factory_runs_only_for_new_nonempty_series() {
    // Literal expected cases preserve the existing operation and absolute-start
    // contract, including shrinking retention windows and an empty old series.
    let cases = [
        (None, 0, 0, None),
        (Some(0), 0, 0, None),
        (None, 3, 0, Some((SeriesChangeOp::Append, 0, true))),
        (None, 8, 5, Some((SeriesChangeOp::Append, 5, true))),
        (Some(0), 1, 0, Some((SeriesChangeOp::Append, 0, false))),
        (Some(2), 3, 0, Some((SeriesChangeOp::Append, 2, false))),
        (Some(2), 2, 0, Some((SeriesChangeOp::ReplaceLast, 1, false))),
        (Some(5), 5, 2, Some((SeriesChangeOp::ReplaceLast, 4, false))),
        (Some(5), 3, 2, Some((SeriesChangeOp::Append, 2, false))),
        (Some(5), 2, 2, Some((SeriesChangeOp::Append, 2, false))),
        (Some(5), 0, 0, Some((SeriesChangeOp::Append, 0, false))),
        (Some(5), 5, 5, Some((SeriesChangeOp::Append, 5, false))),
    ];
    for family in [
        SeriesFamily::Plot,
        SeriesFamily::PlotChar,
        SeriesFamily::PlotShape,
        SeriesFamily::PlotArrow,
        SeriesFamily::PlotBar,
        SeriesFamily::PlotCandle,
        SeriesFamily::BgColor,
        SeriesFamily::BarColor,
    ] {
        for (old_len, new_len, origin, expected) in cases {
            let header_calls = Cell::new(0);
            let fields_calls = Cell::new(0);
            let observed_start = Cell::new(None);
            let change = series_change_from_lens(
                family,
                71,
                old_len,
                new_len,
                origin,
                |start| {
                    fields_calls.set(fields_calls.get() + 1);
                    observed_start.set(Some(start));
                    SeriesFields {
                        values: (start..new_len)
                            .map(|index| PineValue::Int(index as i64))
                            .collect(),
                        texts: vec![PineValue::String(format!("start:{start}"))],
                        ..SeriesFields::default()
                    }
                },
                || {
                    header_calls.set(header_calls.get() + 1);
                    Some(SeriesHeader::default())
                },
            );
            let Some((op, start, has_header)) = expected else {
                assert_eq!(change, None);
                assert_eq!(fields_calls.get(), 0);
                assert_eq!(observed_start.get(), None);
                assert_eq!(header_calls.get(), 0);
                continue;
            };
            let change = change.unwrap();
            assert_eq!(change.family, family);
            assert_eq!(change.id, 71);
            assert_eq!(change.op, op);
            assert_eq!(change.start, start);
            assert_eq!(fields_calls.get(), 1);
            assert_eq!(observed_start.get(), Some(start));
            assert_eq!(header_calls.get(), usize::from(has_header));
            assert_eq!(change.header.is_some(), has_header);
            assert_eq!(
                change.fields,
                SeriesFields {
                    values: (start..new_len)
                        .map(|index| PineValue::Int(index as i64))
                        .collect(),
                    texts: vec![PineValue::String(format!("start:{start}"))],
                    ..SeriesFields::default()
                }
            );
        }
    }
    // A factory is still evaluated once when a new nonempty series deliberately
    // has no header. FnOnce permits consuming owned capture state.
    let calls = Cell::new(0);
    let captured = String::from("consumed factory capture");
    let change = series_change_from_lens(
        SeriesFamily::Plot,
        9,
        None,
        1,
        0,
        |_| SeriesFields::default(),
        || {
            drop(captured);
            calls.set(calls.get() + 1);
            None
        },
    )
    .unwrap();
    assert_eq!(calls.get(), 1);
    assert_eq!(change.header, None);
}

#[test]
fn lazy_header_new_series_preserves_complete_owned_metadata() {
    let mut source = SeriesHeader {
        metadata: OutputMetadata {
            title: PineValue::String("标题\n\"原件\"".repeat(257)),
            offset: PineValue::Int(-17),
            editable: PineValue::Bool(false),
            show_last: PineValue::Int(129),
            display: PineValue::String("display.data_window".into()),
            force_overlay: PineValue::Bool(true),
        },
        linewidth: PineValue::Int(7),
        style: PineValue::String("plot.style_stepline_diamond".into()),
        track_price: PineValue::Bool(true),
        hist_base: PineValue::Float(-0.0),
        join: PineValue::Bool(true),
        format: PineValue::String("format.price".into()),
        precision: PineValue::Int(4),
        linestyle: PineValue::String("plot.linestyle_dotted".into()),
    };
    let expected = source.clone();
    let captured = source.clone();
    let fields = SeriesFields {
        values: vec![PineValue::Float(-0.0)],
        colors: vec![PineValue::Color(0x123456)],
        texts: vec![PineValue::String("owned fields".into())],
        ..SeriesFields::default()
    };
    let calls = Cell::new(0);
    let change = series_change_from_lens(
        SeriesFamily::Plot,
        129,
        None,
        18,
        17,
        |_| fields.clone(),
        || {
            calls.set(calls.get() + 1);
            Some(captured)
        },
    )
    .unwrap();
    assert_eq!(calls.get(), 1);
    assert_eq!(change.op, SeriesChangeOp::Append);
    assert_eq!(change.start, 17);
    assert_eq!(change.fields, fields);
    let header = change.header.as_ref().unwrap();
    assert_eq!(header, &expected);
    let PineValue::Float(hist_base) = header.hist_base else {
        panic!("hist_base must remain Float");
    };
    assert_eq!(hist_base.to_bits(), (-0.0_f64).to_bits());

    // The emitted header and retained consumer snapshot own their String values.
    // Changing either producer metadata or a second consumer cannot alter them.
    source.metadata.title = PineValue::String("later producer title".into());
    source.style = PineValue::String("later producer style".into());
    source.linestyle = PineValue::String("later producer linestyle".into());
    let mut other_consumer = change.clone();
    let other_header = other_consumer.header.as_mut().unwrap();
    other_header.metadata.display = PineValue::String("consumer display".into());
    other_header.format = PineValue::String("consumer format".into());
    assert_eq!(change.header.as_ref(), Some(&expected));
    assert_ne!(source, expected);
    assert_ne!(other_consumer.header.as_ref(), Some(&expected));
}
