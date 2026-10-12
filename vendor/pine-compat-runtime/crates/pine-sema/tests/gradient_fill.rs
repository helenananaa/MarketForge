use pine_sema::analyze_source;
use pine_syntax::SourceFile;

#[test]
fn gradient_invalid_shapes_retain_diagnostics() {
    for call in [
        "fill(a,b,top_value=1,bottom_value=0,top_color=color.green)",
        "fill(a,b,top_value=1,bottom_value=0,title=\"missing colors\",editable=true)",
        "fill(a,b,1,0,color.green,color.red,top_value=2)",
        "fill(a,b,1,0,3,color.red)",
        "fill(a,b,\"bad\",0,top_color=color.green,bottom_color=color.red)",
        "fill(a,b,1,0,color.green,color.red,show_last=2)",
        "fill(h,h,1,0,color.green,color.red)",
    ] {
        let source = format!(
            "//@version=6\nindicator(\"negative\")\na=plot(close)\nb=plot(open)\nh=hline(1)\n{call}\n"
        );
        let analysis = analyze_source(&SourceFile::new("negative.pine", source));
        assert!(analysis.hir.is_none(), "admitted {call}");
        assert!(!analysis.diagnostics.is_empty());
    }
}
