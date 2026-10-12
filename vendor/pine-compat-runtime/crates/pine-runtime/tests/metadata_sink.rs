use pine_runtime::{
    FillGradientSample, FillOutput, HLineOutput, PineValue, PlotSeries, RuntimeResult,
    into_public_runtime_result_json, public_runtime_result_json,
    write_public_runtime_result_view_json,
};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    io::{self, Write},
};

struct Allocator;
thread_local! {
    static TRACK: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}
fn record() {
    if TRACK.try_with(Cell::get).unwrap_or(false) {
        let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
    }
}
unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record();
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record();
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record();
        unsafe { System.realloc(ptr, layout, size) }
    }
}
#[global_allocator]
static ALLOCATOR: Allocator = Allocator;

fn fill(samples: Option<Vec<FillGradientSample>>) -> FillOutput {
    FillOutput {
        id: 3,
        first_id: 1,
        second_id: 2,
        first_is_hline: false,
        second_is_hline: true,
        colors: vec![],
        gradient: samples,
        title: PineValue::String(String::new()),
        editable: PineValue::Bool(true),
        show_last: PineValue::Na,
        fill_gaps: PineValue::Bool(true),
        display: PineValue::String("display.all".to_owned()),
    }
}
fn hline() -> HLineOutput {
    HLineOutput {
        id: 2,
        price: PineValue::Float(-0.0),
        title: PineValue::String(String::new()),
        color: PineValue::Color(0x787B86),
        style: PineValue::String("hline.style_solid".to_owned()),
        linewidth: PineValue::Int(1),
        editable: PineValue::Bool(true),
        display: PineValue::String("display.all".to_owned()),
    }
}

#[test]
fn metadata_defaults_are_type_sensitive_and_keep_exact_field_order() {
    let mut plot = PlotSeries::new(1, vec![PineValue::Float(-0.0)]);
    plot.linewidth = PineValue::Float(1.0);
    plot.track_price = PineValue::Int(0);
    plot.hist_base = PineValue::Float(0.0);
    plot.join = PineValue::Float(f64::NAN);
    plot.format = PineValue::String(String::new());
    plot.precision = PineValue::Void;
    plot.metadata.title = PineValue::String("标题\n\"".to_owned());
    plot.metadata.offset = PineValue::Float(-0.0);
    plot.metadata.editable = PineValue::Int(1);
    plot.metadata.show_last = PineValue::Float(f64::INFINITY);
    plot.metadata.display = PineValue::String("custom".to_owned());
    plot.metadata.force_overlay = PineValue::Bool(true);
    let mut line = hline();
    line.color = PineValue::Int(0x787B86);
    line.style = PineValue::Na;
    line.linewidth = PineValue::Float(1.0);
    line.editable = PineValue::Int(1);
    line.display = PineValue::String(String::new());
    let mut gradient = fill(Some(vec![
        FillGradientSample {
            top_value: Some(1.0),
            bottom_value: Some(-0.0),
            top_color: Some(0x1_0000_0000),
            bottom_color: None,
        },
        FillGradientSample {
            top_value: Some(f64::NAN),
            bottom_value: Some(f64::INFINITY),
            ..FillGradientSample::default()
        },
    ]));
    gradient.editable = PineValue::Float(1.0);
    gradient.show_last = PineValue::Void;
    gradient.fill_gaps = PineValue::Bool(false);
    gradient.display = PineValue::Na;
    let result = RuntimeResult {
        plots: vec![plot, PlotSeries::new(4, vec![])],
        hlines: vec![line, hline()],
        fills: vec![gradient, fill(None)],
        ..RuntimeResult::default()
    };
    let output = public_runtime_result_json(&result);
    assert!(output.contains(concat!(
        "\"plots\":[{\"id\":1,\"values\":[-0],\"linewidth\":1,\"trackPrice\":0,",
        "\"histBase\":0,\"join\":null,\"format\":\"\",\"precision\":null,",
        "\"title\":\"标题\\n\\\"\",\"offset\":-0,\"editable\":1,\"showLast\":null,",
        "\"display\":\"custom\",\"forceOverlay\":true},{\"id\":4,\"values\":[]}]"
    )));
    assert!(output.contains(concat!(
        "\"hlines\":[{\"id\":2,\"price\":-0,\"color\":7895942,\"style\":null,",
        "\"linewidth\":1,\"editable\":1,\"display\":\"\"},{\"id\":2,\"price\":-0}]"
    )));
    assert!(output.contains(concat!(
        "\"fills\":[{\"id\":3,\"firstId\":1,\"secondId\":2,",
        "\"firstIsHLine\":false,\"secondIsHLine\":true,\"colors\":[],\"gradient\":[",
        "{\"topValue\":1.0,\"bottomValue\":-0.0,\"topColor\":4294967296,\"bottomColor\":null},",
        "{\"topValue\":null,\"bottomValue\":null,\"topColor\":null,\"bottomColor\":null}],",
        "\"editable\":1,\"showLast\":null,\"fillGaps\":false,\"display\":null},",
        "{\"id\":3,\"firstId\":1,\"secondId\":2,",
        "\"firstIsHLine\":false,\"secondIsHLine\":true,\"colors\":[]}]"
    )));
    let mut borrowed = Vec::new();
    write_public_runtime_result_view_json(&result.view(), &mut borrowed).unwrap();
    assert_eq!(borrowed, output.as_bytes());
    assert_eq!(into_public_runtime_result_json(result), output);
}

#[derive(Default)]
struct Counter(usize);
impl Write for Counter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn borrowed_metadata_and_gradient_encoding_allocate_no_temporary_buffers() {
    let mut plot = PlotSeries::new(1, vec![]);
    plot.metadata.title = PineValue::String("大\"\n".repeat(32_768));
    let result = RuntimeResult {
        plots: vec![plot],
        hlines: vec![hline()],
        fills: vec![fill(Some(vec![
            FillGradientSample {
                top_value: Some(1e300),
                bottom_value: Some(f64::from_bits(1)),
                top_color: Some(0x1_0000_0000),
                bottom_color: None,
            };
            2_048
        ]))],
        ..RuntimeResult::default()
    };
    // Constructing the small view-header vectors is outside this encoder scope.
    let view = result.view();
    let expected_len = public_runtime_result_json(&result).len();
    let mut sink = Counter::default();
    ALLOCATIONS.set(0);
    TRACK.set(true);
    let encoded = write_public_runtime_result_view_json(&view, &mut sink);
    TRACK.set(false);
    encoded.unwrap();
    assert_eq!(sink.0, expected_len);
    assert_eq!(ALLOCATIONS.get(), 0);
}

struct FailAt(usize);
impl Write for FailAt {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.0 == 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "sink sentinel",
            ));
        }
        let written = bytes.len().min(self.0);
        self.0 -= written;
        Ok(written)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn gradient_encoding_preserves_sink_error_kind_and_message_at_every_byte() {
    let result = RuntimeResult {
        fills: vec![fill(Some(vec![FillGradientSample {
            top_value: Some(-0.0),
            bottom_value: Some(1e-300),
            top_color: Some(0x1_0000_0000),
            bottom_color: None,
        }]))],
        ..RuntimeResult::default()
    };
    let view = result.view();
    let output = public_runtime_result_json(&result);
    let start = output.find("{\"topValue\":").unwrap();
    let end = start + output[start..].find('}').unwrap() + 1;
    for offset in start..end {
        let error = write_public_runtime_result_view_json(&view, &mut FailAt(offset)).unwrap_err();
        assert_eq!(
            error.kind(),
            io::ErrorKind::PermissionDenied,
            "offset={offset}"
        );
        assert_eq!(error.to_string(), "sink sentinel", "offset={offset}");
    }
}
