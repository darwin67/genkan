use super::*;
use proptest::prelude::*;

// Deterministic positive widths, including multibyte characters and ellipsis.
fn measure(value: &str) -> f32 {
    value.chars().map(|c| (1 + u32::from(c) % 7) as f32).sum()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn fitting_preserves_prefix_and_is_monotone(
        chars in prop::collection::vec(prop_oneof![any::<char>(), prop::sample::select(vec![' ', '\t', '\n', '界', '…'])], 0..180),
        width in 0u16..1400, extra in 0u16..1400,
    ) {
        let text: String = chars.into_iter().collect();
        let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let result = fit_single_line_by(&text, f32::from(width), measure);
        prop_assert!(measure(&result) <= f32::from(width));
        let prefix_len = |result: &str| {
            if result == normalized { normalized.chars().count() }
            else { result.strip_suffix('…').unwrap_or("").chars().count() }
        };
        if result != normalized && !result.is_empty() {
            let prefix = result.strip_suffix('…').unwrap();
            prop_assert!(normalized.starts_with(prefix));
            prop_assert!(measure(&normalized) > f32::from(width));
        }
        if result.is_empty() && !normalized.is_empty() { prop_assert!(measure("…") > f32::from(width)); }
        let wider = fit_single_line_by(&text, f32::from(width) + f32::from(extra), measure);
        prop_assert!(prefix_len(&wider) >= prefix_len(&result));
    }
}
