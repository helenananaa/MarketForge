use pine_runtime::{Bar, BarUpdate, RealtimeRuntime, run_historical};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;

#[test]
fn tuple_switch_styles_follow_each_branch_in_v5_and_v6() {
    for version in [5, 6] {
        let source = format!(
            "//@version={version}\nindicator(\"tuple styles\")\nside=bar_index%2==0?\"Left\":\"Right\"\n[x,style]=switch side\n    \"Left\" => [bar_index,label.style_label_right]\n    \"Right\" => [bar_index,label.style_label_left]\nlabel.new(x,close,style=style)\n"
        );
        let analysis = analyze_source(&SourceFile::new("styles.pine", source));
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        let hir = analysis.hir.unwrap();
        let bars = (0..4)
            .map(|i| Bar {
                time: i * 60000,
                open: 10.,
                high: 10.,
                low: 10.,
                close: 10.,
                volume: 1.,
            })
            .collect::<Vec<_>>();
        let result = run_historical(&hir, &bars).unwrap();
        assert_eq!(result.labels.len(), 4);
        for (i, label) in result.labels.iter().enumerate() {
            assert_eq!(
                label.snapshots[0].style,
                pine_runtime::PineValue::String(
                    if i % 2 == 0 {
                        "label.style_label_right"
                    } else {
                        "label.style_label_left"
                    }
                    .to_owned()
                )
            );
        }
        let mut realtime = RealtimeRuntime::new(&hir);
        for bar in bars {
            realtime.update(BarUpdate::forming(bar)).unwrap();
            realtime.update(BarUpdate::forming(bar)).unwrap();
            realtime.update(BarUpdate::confirmed(bar)).unwrap();
        }
        assert_eq!(realtime.result(), result);
    }
}

#[test]
fn tuple_style_provenance_rejects_known_invalid_values() {
    for source in [
        "[x,style]=[bar_index,label.style_label_right]\nstyle:=\"invalid\"\nlabel.new(x,close,style=style)",
        "side=close>0?\"Left\":\"Right\"\n[x,style]=switch side\n    \"Left\" => [bar_index,label.style_label_right]\n    \"Right\" => [bar_index,\"invalid\"]\nlabel.new(x,close,style=style)",
    ] {
        let analysis = analyze_source(&SourceFile::new(
            "invalid.pine",
            format!("//@version=6\nindicator(\"invalid\")\n{source}\n"),
        ));
        assert!(analysis.hir.is_none());
        assert!(
            analysis
                .diagnostics
                .iter()
                .any(|d| d.code == "E_CALL_ARG_VALUE"),
            "{:?}",
            analysis.diagnostics
        );
    }
}

#[test]
fn tuple_style_provenance_accepts_unknown_domain_for_runtime_validation() {
    let source = "//@version=6\nindicator(\"dynamic style\")\nside=input.string(\"Left\",options=[\"Left\",\"Other\"])\n[x,style]=switch side\n    \"Left\" => [bar_index,label.style_label_right]\n    \"Right\" => [bar_index,label.style_label_left]\nlabel.new(x,close,style=style)\n";
    let analysis = analyze_source(&SourceFile::new("dynamic.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let bar = Bar {
        time: 0,
        open: 10.,
        high: 10.,
        low: 10.,
        close: 10.,
        volume: 1.,
    };
    let result = run_historical(&analysis.hir.unwrap(), &[bar]).unwrap();
    assert_eq!(result.labels.len(), 1);
}
