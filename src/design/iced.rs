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

/// Wallpaper text has a small dark outline; it never dims the wallpaper.
/// Both layers shape and wrap identically, including long PAM notices.
pub(crate) fn wallpaper_label<'a, M: 'a>(
    content: impl Into<String>,
    style: TextStyle,
    color: Rgba,
    scale: f32,
    wrapping: Wrapping,
) -> iced::Element<'a, M> {
    let content = content.into();
    iced::Element::new(OutlinedLabel {
        shadow: scaled_label(
            content.clone(),
            style,
            super::tokens::color::TEXT_SHADOW,
            scale,
        )
        .wrapping(wrapping)
        .align_x(iced::Alignment::Center)
        .into(),
        foreground: scaled_label(content, style, color, scale)
            .wrapping(wrapping)
            .align_x(iced::Alignment::Center)
            .into(),
    })
}

struct OutlinedLabel<'a, M> {
    foreground: iced::Element<'a, M>,
    shadow: iced::Element<'a, M>,
}

impl<M> iced::advanced::Widget<M, iced::Theme, iced::Renderer> for OutlinedLabel<'_, M> {
    fn children(&self) -> Vec<iced::advanced::widget::Tree> {
        vec![
            iced::advanced::widget::Tree::new(&self.foreground),
            iced::advanced::widget::Tree::new(&self.shadow),
        ]
    }
    fn diff(&self, tree: &mut iced::advanced::widget::Tree) {
        tree.diff_children(&[&self.foreground, &self.shadow]);
    }
    fn size(&self) -> iced::Size<iced::Length> {
        self.foreground.as_widget().size()
    }
    fn layout(
        &mut self,
        tree: &mut iced::advanced::widget::Tree,
        renderer: &iced::Renderer,
        limits: &iced::advanced::layout::Limits,
    ) -> iced::advanced::layout::Node {
        let foreground =
            self.foreground
                .as_widget_mut()
                .layout(&mut tree.children[0], renderer, limits);
        let shadow = self
            .shadow
            .as_widget_mut()
            .layout(&mut tree.children[1], renderer, limits);
        iced::advanced::layout::Node::with_children(foreground.size(), vec![foreground, shadow])
    }
    fn draw(
        &self,
        tree: &iced::advanced::widget::Tree,
        renderer: &mut iced::Renderer,
        theme: &iced::Theme,
        style: &iced::advanced::renderer::Style,
        layout: iced::advanced::Layout<'_>,
        cursor: iced::mouse::Cursor,
        viewport: &iced::Rectangle,
    ) {
        use iced::advanced::Renderer as _;
        let mut children = layout.children();
        let foreground = children.next().expect("foreground layout");
        let shadow = children.next().expect("shadow layout");
        // These are ordinary renderer translations, not Float overlays:
        // shadow first, foreground last, and all below menus and alerts.
        for (x, y) in [(-1.0, 0.0), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0)] {
            renderer.with_translation(iced::Vector::new(x, y), |renderer| {
                self.shadow.as_widget().draw(
                    &tree.children[1],
                    renderer,
                    theme,
                    style,
                    shadow,
                    cursor,
                    viewport,
                );
            });
        }
        self.foreground.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            foreground,
            cursor,
            viewport,
        );
    }
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
