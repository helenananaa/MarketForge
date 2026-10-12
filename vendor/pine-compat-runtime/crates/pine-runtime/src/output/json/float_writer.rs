//! Shared scalar and record float encoding without temporary strings.
use serde_json::ser::{CompactFormatter, Formatter};
use std::io::{self, Write};

const MIN_DISPLAY_BITS: u64 = 1e-32_f64.to_bits();
const DISPLAY_BITS_SPAN: u64 = 1e32_f64.to_bits() - MIN_DISPLAY_BITS;

#[inline]
pub(super) fn float<W: Write + ?Sized>(value: f64, output: &mut W) -> io::Result<()> {
    // Positive IEEE magnitude bits are monotone. One unsigned range check keeps
    // ordinary formatting on its hot path; either signed zero also uses Display.
    let magnitude_bits = value.to_bits() & 0x7fff_ffff_ffff_ffff;
    if magnitude_bits.wrapping_sub(MIN_DISPLAY_BITS) < DISPLAY_BITS_SPAN || magnitude_bits == 0 {
        write!(output, "{value}")
    } else {
        exceptional_float(value, output)
    }
}

#[cold]
#[inline(never)]
fn exceptional_float<W: Write + ?Sized>(value: f64, output: &mut W) -> io::Result<()> {
    if !value.is_finite() {
        return output.write_all(b"null");
    }
    // Display expands hundreds of zeroes outside the ordinary range. Serde's
    // stack formatter emits a bounded, round-trippable number into the same sink.
    CompactFormatter.write_f64(output, value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encoded(value: f64) -> Vec<u8> {
        let mut bytes = Vec::new();
        float(value, &mut bytes).unwrap();
        bytes
    }

    #[test]
    fn ordinary_numbers_and_threshold_neighbors_preserve_display_bytes() {
        let large = 1e32_f64;
        let small = 1e-32_f64;
        for value in [
            0.0,
            -0.0,
            1.0,
            -1.25,
            96118.61666666665,
            1e-31,
            f64::from_bits(large.to_bits() - 1),
            -f64::from_bits(large.to_bits() - 1),
            small,
            -small,
            f64::from_bits(small.to_bits() + 1),
            -f64::from_bits(small.to_bits() + 1),
        ] {
            assert_eq!(encoded(value), value.to_string().as_bytes());
        }
        assert_eq!(encoded(0.0), b"0");
        assert_eq!(encoded(-0.0), b"-0");
        for value in [
            large,
            -large,
            f64::from_bits(large.to_bits() + 1),
            -f64::from_bits(large.to_bits() + 1),
            f64::from_bits(small.to_bits() - 1),
            -f64::from_bits(small.to_bits() - 1),
        ] {
            let bytes = encoded(value);
            assert!(bytes.contains(&b'e'), "{value:?}");
            let parsed: f64 = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(parsed.to_bits(), value.to_bits());
        }
    }

    #[test]
    fn compact_extremes_round_trip_across_exponents_and_mantissas() {
        assert_eq!(encoded(f64::MAX), b"1.7976931348623157e+308");
        assert_eq!(encoded(-f64::from_bits(1)), b"-5e-324");
        // Include both sides of every binary exponent, normal/subnormal edges,
        // exact powers, and irregular mantissas without a random test seed.
        for exponent in 0..2047_u64 {
            for mantissa in [0, 1, 0x0005_5555_5555_5555, 0x000f_ffff_ffff_ffff] {
                for sign in [0, 1_u64 << 63] {
                    let value = f64::from_bits(sign | exponent << 52 | mantissa);
                    let bytes = encoded(value);
                    let parsed: f64 = serde_json::from_slice(&bytes).unwrap();
                    assert_eq!(parsed.to_bits(), value.to_bits(), "{value:?}");
                    if bytes.contains(&b'e') {
                        assert!(bytes.len() <= 24, "{value:?}: {}", bytes.len());
                    }
                }
            }
        }
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(encoded(value), b"null");
        }
    }

    #[test]
    fn compact_floats_retry_short_writes_and_preserve_io_failures() {
        struct Sink {
            bytes: Vec<u8>,
            limit: usize,
            interrupted: bool,
        }
        impl Write for Sink {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                if !self.interrupted {
                    self.interrupted = true;
                    return Err(io::ErrorKind::Interrupted.into());
                }
                if self.bytes.len() == self.limit {
                    return Err(io::Error::new(io::ErrorKind::StorageFull, "full"));
                }
                let count = bytes.len().min(1).min(self.limit - self.bytes.len());
                self.bytes.extend_from_slice(&bytes[..count]);
                Ok(count)
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        for value in [f64::MAX, -f64::from_bits(1), -0.0, f64::INFINITY] {
            let expected = encoded(value);
            for limit in [0, expected.len() / 2, expected.len()] {
                let mut sink = Sink {
                    bytes: Vec::new(),
                    limit,
                    interrupted: false,
                };
                let outcome = float(value, &mut sink);
                if limit == expected.len() {
                    outcome.unwrap();
                } else {
                    assert_eq!(outcome.unwrap_err().kind(), io::ErrorKind::StorageFull);
                }
                assert_eq!(sink.bytes, expected[..limit]);
            }
            let error = float(value, &mut &mut [][..]).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::WriteZero);
        }
    }
}
