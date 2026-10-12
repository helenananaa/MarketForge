use pine_runtime::*;

pub fn cases() -> Vec<RuntimeChanges> {
    let mut series = RuntimeChanges::new(4, StreamingVisibility::Preview);
    series.retained_from = 2;
    for (id, family) in [
        SeriesFamily::Plot,
        SeriesFamily::PlotChar,
        SeriesFamily::PlotShape,
        SeriesFamily::PlotArrow,
        SeriesFamily::PlotBar,
        SeriesFamily::PlotCandle,
        SeriesFamily::BgColor,
        SeriesFamily::BarColor,
    ]
    .into_iter()
    .enumerate()
    {
        series.series.push(SeriesChange {
            family,
            id: id as u32,
            op: if id % 2 == 0 {
                SeriesChangeOp::Append
            } else {
                SeriesChangeOp::ReplaceLast
            },
            start: id + 2,
            fields: SeriesFields {
                values: vec![PineValue::Int(id as i64)],
                ..SeriesFields::default()
            },
            header: (id == 0).then(SeriesHeader::default),
        });
    }
    series.series.push(SeriesChange {
        family: SeriesFamily::PlotCandle,
        id: 99,
        op: SeriesChangeOp::ReplaceLast,
        start: 100,
        fields: SeriesFields {
            values: vec![
                PineValue::Float(-0.0),
                PineValue::Float(f64::NAN),
                PineValue::Float(f64::INFINITY),
                PineValue::Na,
                PineValue::String("\"\\\n\0汉🙂".into()),
            ],
            colors: vec![PineValue::Color(0)],
            chars: vec![PineValue::String("c".into())],
            locations: vec![PineValue::Na],
            texts: vec![PineValue::String("t".into())],
            text_colors: vec![PineValue::Color(1)],
            sizes: vec![PineValue::String("size.tiny".into())],
            styles: vec![PineValue::String("s".into())],
            color_ups: vec![PineValue::Color(2)],
            color_downs: vec![PineValue::Color(3)],
            min_heights: vec![PineValue::Float(1.5)],
            max_heights: vec![PineValue::Int(4)],
            opens: vec![PineValue::Tuple(vec![PineValue::Int(1), PineValue::Na])],
            highs: vec![PineValue::Int(5)],
            lows: vec![PineValue::Int(6)],
            closes: vec![PineValue::Int(7)],
            wick_colors: vec![PineValue::Color(8)],
            border_colors: vec![PineValue::Color(9)],
        },
        header: Some(SeriesHeader {
            metadata: OutputMetadata {
                title: PineValue::String("title\"汉".into()),
                offset: PineValue::Float(-0.0),
                editable: PineValue::Bool(false),
                show_last: PineValue::Int(4),
                display: PineValue::Na,
                force_overlay: PineValue::Bool(true),
            },
            linewidth: PineValue::Float(1.0),
            style: PineValue::Na,
            track_price: PineValue::Bool(true),
            hist_base: PineValue::Int(-2),
            join: PineValue::Bool(true),
            format: PineValue::String("format.price".into()),
            precision: PineValue::Int(2),
            linestyle: PineValue::String("dashed".into()),
        }),
    });

    let mut objects = RuntimeChanges::new(5, StreamingVisibility::Confirmed);
    let hline = HLineOutput {
        id: 1,
        price: PineValue::Float(-0.0),
        title: PineValue::String("".into()),
        color: PineValue::Color(0x787B86),
        style: PineValue::String("hline.style_solid".into()),
        linewidth: PineValue::Int(1),
        editable: PineValue::Bool(true),
        display: PineValue::String("display.all".into()),
    };
    objects.hlines.extend([
        HLineChange {
            id: 1,
            action: HLineAction::Add(hline.clone()),
        },
        HLineChange {
            id: 1,
            action: HLineAction::Replace(HLineOutput {
                price: PineValue::Float(f64::NEG_INFINITY),
                title: PineValue::String("new\t".into()),
                ..hline
            }),
        },
        HLineChange {
            id: 1,
            action: HLineAction::Delete,
        },
    ]);
    let gradient = FillGradientSample {
        top_value: Some(-0.0),
        bottom_value: Some(1.0),
        top_color: Some(0),
        bottom_color: None,
    };
    objects.fills.extend([
        FillChange {
            id: 3,
            action: FillAction::Add(FillOutput {
                id: 3,
                first_id: 1,
                second_id: 2,
                first_is_hline: true,
                second_is_hline: false,
                colors: vec![PineValue::Color(4)],
                gradient: Some(vec![gradient.clone()]),
                title: PineValue::String("fill".into()),
                editable: PineValue::Bool(false),
                show_last: PineValue::Int(0),
                fill_gaps: PineValue::Bool(false),
                display: PineValue::Na,
            }),
        },
        FillChange {
            id: 3,
            action: FillAction::SetColors {
                start: 4,
                values: vec![PineValue::Na, PineValue::Color(5)],
            },
        },
        FillChange {
            id: 3,
            action: FillAction::SetGradient {
                start: 6,
                values: vec![gradient, FillGradientSample::default()],
            },
        },
        FillChange {
            id: 3,
            action: FillAction::Delete,
        },
    ]);
    for object in drawing_objects() {
        let family = object.family();
        let id = object.id();
        objects.drawings.extend([
            DrawingChange {
                family,
                id,
                action: DrawingAction::Add(object.clone()),
            },
            DrawingChange {
                family,
                id,
                action: DrawingAction::SetTail { start: 2, object },
            },
            DrawingChange {
                family,
                id,
                action: DrawingAction::Delete,
            },
        ]);
    }

    let mut records = RuntimeChanges::new(6, StreamingVisibility::Preview);
    let alert = AlertEvent {
        id: 1,
        bar_index: 2,
        time: -3,
        message: "hello\"\\\n汉🙂".into(),
        source: "alert".into(),
    };
    records.alerts.extend([
        EventChange {
            action: EventAction::Add,
            event: alert.clone(),
        },
        EventChange {
            action: EventAction::Remove,
            event: alert,
        },
    ]);
    records.strategy = Some(StrategyChanges {
        orders: Some(ListSplice {
            start: 1,
            items: vec![StrategyOrderEvent {
                id: "o\"".into(),
                bar_index: 2,
                time: 3,
                direction: "long".into(),
                qty: -0.0,
                price: f64::NAN,
            }],
        }),
        trades: Some(ListSplice {
            start: 2,
            items: vec![StrategyTrade {
                id: "t".into(),
                exit_id: "x".into(),
                entry_bar_index: 1,
                exit_bar_index: 2,
                entry_time: 3,
                exit_time: 4,
                entry_price: f64::INFINITY,
                exit_price: f64::NEG_INFINITY,
                qty: 1.0,
                profit: -0.0,
            }],
        }),
        alerts: Some(ListSplice {
            start: 3,
            items: vec![StrategyOrderFillAlertOutput {
                id: "a".into(),
                bar_index: 4,
                time: 5,
                direction: "exit".into(),
                qty: 1.0,
                price: 2.0,
                entry_id: None,
                exit_id: Some("x".into()),
                message: "\t\0".into(),
            }],
        }),
        position: Some(ListSplice {
            start: 4,
            items: vec![StrategyPositionSnapshot {
                bar_index: 5,
                size: -0.0,
                avg_price: Some(f64::NAN),
            }],
        }),
        equity: Some(ListSplice {
            start: 5,
            items: vec![StrategyEquitySnapshot {
                bar_index: 6,
                cash: 1.0,
                market_value: f64::INFINITY,
                equity: f64::NAN,
                net_profit: -0.0,
            }],
        }),
        diagnostics: Some(vec![RuntimeDiagnostic {
            code: "c".into(),
            message: "m\r".into(),
        }]),
    });
    records.diagnostics.push(RuntimeDiagnostic {
        code: "e".into(),
        message: "\u{8}\u{c}".into(),
    });

    let mut empty_strategy = RuntimeChanges::new(7, StreamingVisibility::Confirmed);
    empty_strategy.strategy = Some(StrategyChanges::default());
    let mut empty_splices = RuntimeChanges::new(8, StreamingVisibility::Confirmed);
    empty_splices.strategy = Some(StrategyChanges {
        orders: Some(ListSplice {
            start: 0,
            items: vec![],
        }),
        diagnostics: Some(vec![]),
        ..StrategyChanges::default()
    });
    empty_splices.series.push(SeriesChange {
        family: SeriesFamily::Plot,
        id: 1,
        op: SeriesChangeOp::Append,
        start: 0,
        fields: SeriesFields::default(),
        header: Some(SeriesHeader::default()),
    });
    empty_splices.fills.extend([
        FillChange {
            id: 1,
            action: FillAction::SetColors {
                start: 0,
                values: vec![],
            },
        },
        FillChange {
            id: 1,
            action: FillAction::SetGradient {
                start: 0,
                values: vec![],
            },
        },
    ]);
    vec![series, objects, records, empty_strategy, empty_splices]
}

fn drawing_objects() -> Vec<DrawingObject> {
    vec![
        DrawingObject::Label(LabelOutput {
            id: 10,
            snapshots: vec![LabelSnapshot {
                bar_index: 1,
                exists: false,
                x: PineValue::Na,
                y: PineValue::Na,
                text: PineValue::Na,
                xloc: PineValue::Na,
                yloc: PineValue::Na,
                color: PineValue::Na,
                style: PineValue::Na,
                text_color: PineValue::Na,
                size: PineValue::Na,
                tooltip: PineValue::Na,
                text_align: PineValue::Na,
                text_font_family: PineValue::Na,
                text_formatting: PineValue::Na,
            }],
        }),
        DrawingObject::Line(LineOutput {
            id: 11,
            snapshots: vec![],
        }),
        DrawingObject::LineFill(LineFillOutput {
            id: 12,
            snapshots: vec![LineFillSnapshot {
                bar_index: 2,
                exists: true,
                line1: 11,
                line2: 13,
                color: PineValue::Color(0),
            }],
        }),
        DrawingObject::Polyline(PolylineOutput {
            id: 13,
            snapshots: vec![],
        }),
        DrawingObject::Box(BoxOutput {
            id: 14,
            snapshots: vec![],
        }),
        DrawingObject::Table(Box::new(TableOutput {
            id: 15,
            position: PineValue::String("p汉".into()),
            bg_color: PineValue::Na,
            frame_color: PineValue::Na,
            frame_width: PineValue::Int(0),
            border_color: PineValue::Na,
            border_width: PineValue::Int(0),
            columns: 2,
            rows: 3,
            snapshots: vec![TableSnapshot {
                bar_index: 3,
                exists: false,
                cells: vec![],
                merged_cells: vec![],
            }],
        })),
    ]
}
