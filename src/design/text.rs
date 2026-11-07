//! Renderer-independent text policy shared by login and lock: initials,
//! single-line fitting with an ellipsis, and where a PAM prompt is shown.
//!
//! The fitting helpers take a width-measuring function so the iced greeter
//! and the lock's CPU renderer can share them; [`measure`] is the iced one.

use std::borrow::Cow;

use iced::advanced::graphics::text::{font_system, Paragraph};
use iced::advanced::text::{self as core_text, Paragraph as _};
use iced::{alignment, Pixels, Size};

use super::font::INTER_VARIABLE;
use super::iced as convert;
use super::tokens::TextStyle;

const ELLIPSIS: char = '…';

/// The placeholder for a PAM prompt that carries no text.
pub(crate) const DEFAULT_PROMPT: &str = "Enter Password";

/// Where a PAM prompt's text is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PromptPlacement<'a> {
    /// Inside the empty pill.
    Placeholder(&'a str),
    /// As a StatusMessage above the pill, which then has no placeholder.
    Above(&'a str),
}

/// A short secret prompt is the pill's placeholder. A visible prompt, or one
/// too long for the pill, is shown above it so it stays readable while typing.
pub(crate) fn prompt_placement(
    prompt: &str,
    secret: bool,
    fits: impl FnOnce(&str) -> bool,
) -> PromptPlacement<'_> {
    if prompt.trim().is_empty() {
        PromptPlacement::Placeholder(DEFAULT_PROMPT)
    } else if secret && fits(prompt) {
        PromptPlacement::Placeholder(prompt)
    } else {
        PromptPlacement::Above(prompt)
    }
}

/// The first letters of the first two words, uppercased.
pub(crate) fn initials(name: &str) -> String {
    name.split_whitespace()
        .filter_map(|word| word.chars().next())
        .take(2)
        .collect::<String>()
        .to_uppercase()
}

/// Fits `text` on one line no wider than `width`, truncating the end with an
/// ellipsis. Runs of whitespace, including newlines, collapse to one space.
pub(crate) fn fit_single_line_by(
    text: &str,
    width: f32,
    mut measure: impl FnMut(&str) -> f32,
) -> String {
    let normalized = normalize(text);
    if measure(&normalized) <= width {
        return normalized;
    }
    if measure("…") > width {
        return String::new();
    }
    let characters = normalized.chars().collect::<Vec<_>>();
    let candidate = |kept: usize| -> String {
        characters[..kept]
            .iter()
            .copied()
            .chain(std::iter::once(ELLIPSIS))
            .collect()
    };
    let kept = largest_fitting(characters.len(), |kept| measure(&candidate(kept)) <= width);
    candidate(kept)
}

/// Fits `text` on one line no wider than `width`, replacing the middle with an
/// ellipsis so that labels sharing a long prefix stay distinguishable.
pub(crate) fn fit_middle_by(
    text: &str,
    width: f32,
    mut measure: impl FnMut(&str) -> f32,
) -> String {
    let normalized = normalize(text);
    if measure(&normalized) <= width {
        return normalized;
    }
    if measure("…") > width {
        return String::new();
    }
    let characters = normalized.chars().collect::<Vec<_>>();
    let candidate = |kept: usize| -> String {
        let tail = kept / 2;
        let head = kept - tail;
        characters[..head]
            .iter()
            .copied()
            .chain(std::iter::once(ELLIPSIS))
            .chain(characters[characters.len() - tail..].iter().copied())
            .collect()
    };
    let kept = largest_fitting(characters.len(), |kept| measure(&candidate(kept)) <= width);
    candidate(kept)
}

/// Fits each label like [`fit_single_line_by`], but switches to middle
/// truncation when end truncation would make different labels look the same.
pub(crate) fn fit_distinct_by(
    labels: &[&str],
    width: f32,
    mut measure: impl FnMut(&str) -> f32,
) -> Vec<String> {
    let ends = labels
        .iter()
        .map(|label| fit_single_line_by(label, width, &mut measure))
        .collect::<Vec<_>>();
    let collides = ends.iter().enumerate().any(|(index, fitted)| {
        ends.iter().enumerate().any(|(other, candidate)| {
            other != index
                && candidate == fitted
                && normalize(labels[other]) != normalize(labels[index])
        })
    });
    if !collides {
        return ends;
    }
    labels
        .iter()
        .map(|label| fit_middle_by(label, width, &mut measure))
        .collect()
}

/// The advance width of `text` set on one line in `style`, measured with
/// iced's shared font system and the bundled typeface.
pub(crate) fn measure(text: &str, style: TextStyle) -> f32 {
    measure_scaled(text, style, 1.0)
}

/// [`measure`] with the style's size multiplied by `scale`.
pub(crate) fn measure_scaled(text: &str, style: TextStyle, scale: f32) -> f32 {
    ensure_font_loaded();
    Paragraph::with_text(core_text::Text {
        content: text,
        bounds: Size::INFINITE,
        size: Pixels(style.size * scale),
        line_height: core_text::LineHeight::Absolute(Pixels(style.line_height * scale)),
        font: convert::font(style),
        align_x: core_text::Alignment::Default,
        align_y: alignment::Vertical::Top,
        shaping: core_text::Shaping::Advanced,
        wrapping: core_text::Wrapping::None,
    })
    .min_width()
}

/// Fits `text` within `width` in `style`, truncating the end.
pub(crate) fn fit(text: &str, style: TextStyle, width: f32) -> String {
    fit_single_line_by(text, width, |candidate| measure(candidate, style))
}

/// Fits each label within `width` in `style`, keeping different labels
/// distinguishable.
pub(crate) fn fit_distinct(labels: &[&str], style: TextStyle, width: f32) -> Vec<String> {
    fit_distinct_by(labels, width, |candidate| measure(candidate, style))
}

/// iced loads the bundled face at startup; tests and early measurements may
/// run first. Loading is idempotent for the same static bytes.
fn ensure_font_loaded() {
    if let Ok(mut fonts) = font_system().write() {
        fonts.load_font(Cow::Borrowed(INTER_VARIABLE));
    }
}

fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The largest `kept` in `0..=upper` for which `fits(kept)` holds, assuming
/// `fits` is monotonic and `fits(0)` holds.
fn largest_fitting(upper: usize, mut fits: impl FnMut(usize) -> bool) -> usize {
    let (mut lower, mut upper) = (0, upper);
    while lower < upper {
        let midpoint = (lower + upper).div_ceil(2);
        if fits(midpoint) {
            lower = midpoint;
        } else {
            upper = midpoint - 1;
        }
    }
    lower
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::design::tokens::text_style;

    fn chars(text: &str) -> f32 {
        text.chars().count() as f32
    }

    #[test]
    fn creates_initials() {
        assert_eq!(initials("Darwin Wu"), "DW");
        assert_eq!(initials("Darwin"), "D");
        assert_eq!(initials("  zoë  ångström  extra "), "ZÅ");
        assert_eq!(initials(""), "");
    }

    #[test]
    fn end_truncation_keeps_short_text_and_bounds_long_text() {
        assert_eq!(fit_single_line_by("Niri", 10.0, chars), "Niri");
        assert_eq!(
            fit_single_line_by("Alexandria Montgomery", 8.0, chars),
            "Alexand…"
        );
        assert_eq!(fit_single_line_by("two\nlines", 20.0, chars), "two lines");
        assert_eq!(fit_single_line_by("anything", 0.5, chars), "");
        assert_eq!(fit_single_line_by("anything", 1.0, chars), "…");
    }

    #[test]
    fn end_truncation_measures_wide_characters_and_collapses_whitespace() {
        let measure = |text: &str| {
            text.chars()
                .map(|character| if character == '界' { 10.0 } else { 1.0 })
                .sum()
        };
        assert_eq!(
            fit_single_line_by(&"a".repeat(512), 8.0, measure),
            "aaaaaaa…"
        );
        assert_eq!(fit_single_line_by("界界界界", 22.0, measure), "界界…");
        assert_eq!(
            fit_single_line_by("Challenge:\n Enter   token", 40.0, measure),
            "Challenge: Enter token"
        );
    }

    #[test]
    fn prompts_are_placeholders_only_when_short_and_secret() {
        assert_eq!(
            prompt_placement("Password", true, |_| true),
            PromptPlacement::Placeholder("Password")
        );
        assert_eq!(
            prompt_placement("", true, |_| true),
            PromptPlacement::Placeholder("Enter Password")
        );
        assert_eq!(
            prompt_placement("Verification code", false, |_| true),
            PromptPlacement::Above("Verification code")
        );
        assert_eq!(
            prompt_placement("Enter the complete authentication challenge", true, |_| {
                false
            }),
            PromptPlacement::Above("Enter the complete authentication challenge")
        );
    }

    #[test]
    fn middle_truncation_keeps_both_ends() {
        assert_eq!(fit_middle_by("prefix-alpha", 7.0, chars), "pre…pha");
        assert_eq!(fit_middle_by("short", 7.0, chars), "short");
    }

    #[test]
    fn distinct_fitting_disambiguates_a_shared_prefix() {
        let shared = "SessionWithACommonUnbrokenPrefix".repeat(4);
        let first = format!("{shared}Selected");
        let second = format!("{shared}Alternate");
        let fitted = fit_distinct_by(&[&first, &second], 20.0, chars);
        assert_ne!(fitted[0], fitted[1]);
        assert!(fitted.iter().all(|label| chars(label) <= 20.0));

        let same = fit_distinct_by(&["Alex Morgan", "Alex Morgan"], 6.0, chars);
        assert_eq!(same[0], same[1], "identical labels stay identical");
    }

    #[test]
    fn measured_fit_stays_within_the_width() {
        let name = "Long duplicate account name ".repeat(4);
        let fitted = fit(&name, text_style::NAME, 110.0);
        assert!(fitted.ends_with('…'));
        assert!(measure(&fitted, text_style::NAME) <= 110.0);
        assert!(measure("Darwin Wu", text_style::NAME) > 0.0);
        assert!(
            measure_scaled("10:38", text_style::CLOCK, 0.5) < measure("10:38", text_style::CLOCK)
        );
    }
}

#[cfg(test)]
mod properties {
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
            let normalized = normalize(&text);
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
}
