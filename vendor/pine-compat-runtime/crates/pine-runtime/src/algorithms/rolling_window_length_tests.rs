use super::{RollingWindowState, weighted_denominator};

#[test]
fn zero_length_preserves_empty_and_open_window() {
    let mut empty = RollingWindowState::default();
    empty.push(Some(9.0), 0);
    empty.push_for_bar(None, 0, 0);
    assert_eq!(empty, RollingWindowState::default());
    assert!(!empty.is_ready(0));

    let mut window = RollingWindowState::default();
    window.push_for_bar(Some(1.0), 2, 0);
    window.push_for_bar(Some(2.0), 2, 1);
    let open = window.clone();
    window.push(None, 0);
    window.push_for_bar(Some(99.0), 0, 1);
    window.push_for_bar(Some(99.0), 0, 2);
    assert_eq!(window, open);
    window.push_for_bar(Some(3.0), 2, 1);
    let mut expected = open;
    expected.push_for_bar(Some(3.0), 2, 1);
    assert_eq!(window, expected);
    assert_eq!(window.sum, 4.0);
}

#[test]
fn weighted_denominator_preserves_integer_rounding_and_never_wraps() {
    for length in [0_u32, 1, 2, 7, 255, 65_535, 65_536, 100_000] {
        let exact = u64::from(length) * (u64::from(length) + 1) / 2;
        assert_eq!(
            weighted_denominator(length as usize).to_bits(),
            (exact as f64).to_bits()
        );
    }
    assert_eq!(weighted_denominator(65_536), 2_147_516_416.0);
    let largest = usize::MAX as u128;
    let exact = largest * (largest + 1) / 2;
    assert_eq!(
        weighted_denominator(usize::MAX).to_bits(),
        (exact as f64).to_bits()
    );
}

#[test]
fn large_ready_weighted_windows_keep_full_and_tail_results() {
    let mut window = RollingWindowState::default();
    for _ in 0..65_536 {
        window.push(Some(1.0), 65_536);
    }
    assert_eq!(window.weighted_mean(65_536), 1.0);
    assert_eq!(window.weighted_mean_with_tail(65_536, 65_536), (1.0, 1.0));
    assert_eq!(window.weighted_mean_with_tail(65_536, 32_768), (1.0, 1.0));
}
