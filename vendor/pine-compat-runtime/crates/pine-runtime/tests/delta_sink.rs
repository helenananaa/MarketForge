use pine_runtime::{public_runtime_changes_json, write_public_runtime_changes_json};
use std::io::{self, Write};

#[path = "support/delta_cases.rs"]
mod delta_cases;

// Captured before the repair with the frozen d4fab72a9 runtime library.
// Keep the reference bytes independent of the sink and convenience encoders.
const REFERENCES: &str = include_str!("fixtures/delta_actions.jsonl");

#[test]
fn delta_sink_preserves_exact_reference_bytes_for_every_action_and_optional_field() {
    let cases = delta_cases::cases();
    let references = REFERENCES.lines().collect::<Vec<_>>();
    assert_eq!(cases.len(), references.len());
    for (index, (changes, expected)) in cases.iter().zip(references).enumerate() {
        let mut output = Vec::new();
        write_public_runtime_changes_json(changes, &mut output).unwrap();
        assert_eq!(output, expected.as_bytes(), "sink case {index}");
        assert_eq!(
            public_runtime_changes_json(changes),
            expected,
            "String case {index}"
        );
    }
}

struct ShortSink {
    bytes: Vec<u8>,
    remaining: usize,
    failed: bool,
}
impl Write for ShortSink {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        assert!(!self.failed, "writer continued after a sink error");
        if bytes.is_empty() {
            return Ok(0);
        }
        if self.remaining == 0 {
            self.failed = true;
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "delta sink failed",
            ));
        }
        let count = bytes.len().min(self.remaining).min(3);
        self.bytes.extend_from_slice(&bytes[..count]);
        self.remaining -= count;
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> {
        panic!("the serializer must leave flush policy to its caller");
    }
}

#[test]
fn delta_sink_propagates_partial_write_failures_at_every_reference_byte() {
    for (changes, expected) in delta_cases::cases().iter().zip(REFERENCES.lines()) {
        for limit in 0..expected.len() {
            let mut sink = ShortSink {
                bytes: vec![],
                remaining: limit,
                failed: false,
            };
            let error = write_public_runtime_changes_json(changes, &mut sink).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::BrokenPipe, "offset {limit}");
            assert_eq!(error.to_string(), "delta sink failed", "offset {limit}");
            assert!(sink.failed);
            assert_eq!(sink.bytes, expected.as_bytes()[..limit], "offset {limit}");
        }
    }
}

#[test]
fn delta_sink_accepts_trait_objects_and_retries_short_writes() {
    for (changes, expected) in delta_cases::cases().iter().zip(REFERENCES.lines()) {
        let mut sink = ShortSink {
            bytes: vec![],
            remaining: expected.len(),
            failed: false,
        };
        let output: &mut dyn Write = &mut sink;
        write_public_runtime_changes_json(changes, output).unwrap();
        assert!(!sink.failed);
        assert_eq!(sink.bytes, expected.as_bytes());
    }
}

#[test]
fn delta_sink_reports_write_zero_as_an_io_error() {
    struct Zero;
    impl Write for Zero {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Ok(0)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let changes = &delta_cases::cases()[0];
    let error = write_public_runtime_changes_json(changes, &mut Zero).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::WriteZero);
}
