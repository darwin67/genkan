//! Conversions from design tokens to iced values.

use iced::font::Weight;
use iced::widget::text::{IntoFragment, LineHeight, Shaping, Wrapping};
use iced::widget::{text, Text};
use iced::{Color, Font, Pixels};

use super::font;
use super::tokens::{Rgba, TextStyle};

/// An iced color for a token color.
pub(crate) fn color(color: Rgba) -> Color {
    Color::from_rgba8(color.red, color.green, color.blue, color.alpha)
}

/// The bundled family at the style's weight.
pub(crate) fn font(style: TextStyle) -> Font {
    Font {
        weight: weight(style.weight),
        ..font::iced_font()
    }
}

/// The style's line height as an absolute iced line height.
pub(crate) fn line_height(style: TextStyle) -> LineHeight {
    LineHeight::Absolute(Pixels(style.line_height))
}

/// A single-line text widget in the given style and color.
///
/// Callers that want wrapping set it explicitly; there is no unbounded
/// default. Letter spacing is not applied because iced does not support it.
pub(crate) fn label<'a>(content: impl IntoFragment<'a>, style: TextStyle, color: Rgba) -> Text<'a> {
    scaled_label(content, style, color, 1.0)
}

/// [`label`] with the size and line height multiplied by `scale`.
pub(crate) fn scaled_label<'a>(
    content: impl IntoFragment<'a>,
    style: TextStyle,
    color: Rgba,
    scale: f32,
) -> Text<'a> {
    text(content)
        .font(font(style))
        .size(style.size * scale)
        .line_height(LineHeight::Absolute(Pixels(style.line_height * scale)))
        .color(self::color(color))
        .shaping(Shaping::Advanced)
        .wrapping(Wrapping::None)
}

fn weight(weight: u16) -> Weight {
    match weight {
        0..=150 => Weight::Thin,
        151..=250 => Weight::ExtraLight,
        251..=350 => Weight::Light,
        351..=450 => Weight::Normal,
        451..=550 => Weight::Medium,
        551..=650 => Weight::Semibold,
        651..=750 => Weight::Bold,
        751..=850 => Weight::ExtraBold,
        _ => Weight::Black,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::design::tokens::{color as palette, text_style};

    #[test]
    fn token_weights_map_to_iced_weights() {
        assert_eq!(font(text_style::CLOCK).weight, Weight::ExtraBold);
        assert_eq!(font(text_style::NAME).weight, Weight::Bold);
        assert_eq!(font(text_style::DATE).weight, Weight::Semibold);
        assert_eq!(font(text_style::FIELD).weight, Weight::Medium);
        assert_eq!(font(text_style::FIELD).family, font::iced_font().family);
    }

    #[test]
    fn token_colors_keep_straight_alpha() {
        let converted = color(palette::FIELD_GLASS);
        assert_eq!(converted, Color::from_rgba8(78, 84, 92, 0.5));
        assert_eq!(color(palette::TEXT_PRIMARY), Color::WHITE);
    }
}
