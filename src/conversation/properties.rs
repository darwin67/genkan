use super::*;
use proptest::prelude::*;

fn text() -> impl Strategy<Value = String> {
    prop::collection::vec(any::<char>(), 0..1100).prop_map(|chars| chars.into_iter().collect())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn text_bounds(value in text()) {
        let bounded = bounded_text(&value);
        prop_assert!(bounded.chars().count() <= MAX_TEXT_CHARS);
        prop_assert!(bounded.len() <= MAX_INPUT_BYTES);
        prop_assert_eq!(bounded_text(&bounded), bounded);
        let prompt = clean_prompt(&value);
        prop_assert!(!prompt.is_empty());
        prop_assert!(prompt.chars().count() <= MAX_TEXT_CHARS);
        // Cleanup is one pass, not a canonicalizer: "x: :" -> "x:" -> "x".
        // On canonical prompts (no edge whitespace/colon) it is idempotent.
        if !prompt.ends_with(':') && prompt.trim() == prompt {
            prop_assert_eq!(clean_prompt(&prompt), prompt);
        }
    }

    #[test]
    fn append_preserves_prefix_and_saturates(first in text(), second in text()) {
        let mut actual = response_buffer();
        append_bounded(&mut actual, &first);
        let prefix = actual.clone();
        append_bounded(&mut actual, &second);
        let expected: String = first.chars().chain(second.chars()).take(MAX_TEXT_CHARS).collect();
        prop_assert!(actual == expected, "synthetic text mismatch (redacted)");
        prop_assert!(actual.starts_with(&prefix));
        prop_assert!(actual.len() <= MAX_INPUT_BYTES);
        prop_assert!(actual.is_char_boundary(actual.len()));
        let unchanged = actual.clone();
        append_bounded(&mut actual, "");
        prop_assert!(actual == unchanged);
        if actual.chars().count() == MAX_TEXT_CHARS {
            append_bounded(&mut actual, &second);
            prop_assert!(actual == unchanged);
        }
    }
}

#[test]
fn cleanup_and_accumulation_are_not_general_idempotent_operations() {
    assert_eq!(clean_prompt("x: :"), "x:");
    assert_eq!(clean_prompt(&clean_prompt("x: :")), "x");
    let mut text = String::new();
    append_bounded(&mut text, "a");
    append_bounded(&mut text, "a");
    assert_eq!(text, "aa");
}
