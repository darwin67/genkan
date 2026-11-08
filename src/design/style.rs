//! iced styles for the login components, built only from design tokens.

use std::f32::consts::PI;

use iced::gradient::Linear;
use iced::widget::{button, container, scrollable, text_input};
use iced::{Background, Border, Color, Radians, Shadow, Theme};

use super::iced::color;
use super::tokens::{color as palette, component, radius, Rgba};

pub(crate) const FALLBACK_BASE: Color = Color::from_rgb(5.0 / 255.0, 9.0 / 255.0, 24.0 / 255.0);
pub(crate) const FALLBACK_BLOBS: [Color; 3] = [
    Color::from_rgba(51.0 / 255.0, 72.0 / 255.0, 181.0 / 255.0, 0.42),
    Color::from_rgba(142.0 / 255.0, 44.0 / 255.0, 118.0 / 255.0, 0.34),
    Color::from_rgba(23.0 / 255.0, 111.0 / 255.0, 140.0 / 255.0, 0.35),
];

fn fill(token: Rgba) -> Option<Background> {
    Some(Background::Color(color(token)))
}

fn hairline(token: Rgba, corner: f32) -> Border {
    Border {
        color: color(token),
        width: component::HAIRLINE,
        radius: corner.into(),
    }
}

fn focus_border(focused: bool, corner: f32) -> Border {
    if focused {
        Border {
            color: color(palette::FOCUS_RING),
            width: component::FOCUS_RING_WIDTH,
            radius: corner.into(),
        }
    } else {
        Border {
            radius: corner.into(),
            ..Border::default()
        }
    }
}

/// A status-bar item: transparent, `item-active` while its menu is open, and
/// a focus ring while it has keyboard focus.
pub(crate) fn status_item(active: bool, focused: bool) -> button::Style {
    button::Style {
        background: active.then(|| Background::Color(color(palette::ITEM_ACTIVE))),
        text_color: color(palette::TEXT_PRIMARY),
        border: focus_border(focused, radius::ITEM),
        ..button::Style::default()
    }
}

/// A dropdown menu surface.
pub(crate) fn menu(_theme: &Theme) -> container::Style {
    container::Style {
        text_color: Some(color(palette::TEXT_PRIMARY)),
        background: fill(palette::MENU_MATERIAL),
        border: hairline(palette::MENU_BORDER, radius::MENU),
        ..container::Style::default()
    }
}

/// A menu item, filled with the accent while highlighted.
pub(crate) fn menu_item(highlighted: bool) -> button::Style {
    button::Style {
        background: highlighted.then(|| Background::Color(color(palette::ACCENT))),
        text_color: color(palette::TEXT_PRIMARY),
        border: Border {
            radius: radius::MENU_ITEM.into(),
            ..Border::default()
        },
        ..button::Style::default()
    }
}

/// The darkening behind an alert.
pub(crate) fn scrim(_theme: &Theme) -> container::Style {
    container::Style {
        background: fill(palette::SCRIM_MODAL),
        ..container::Style::default()
    }
}

/// An alert surface.
pub(crate) fn alert(_theme: &Theme) -> container::Style {
    container::Style {
        text_color: Some(color(palette::TEXT_PRIMARY)),
        background: fill(palette::ALERT_MATERIAL),
        border: hairline(palette::MENU_BORDER, radius::ALERT),
        ..container::Style::default()
    }
}

/// The disc behind an alert's icon.
pub(crate) fn alert_icon(_theme: &Theme) -> container::Style {
    container::Style {
        background: fill(palette::PILL_SECONDARY),
        border: Border {
            radius: (component::ALERT_ICON * radius::ROUND).into(),
            ..Border::default()
        },
        ..container::Style::default()
    }
}

/// A pill button: the accent for the default action, otherwise secondary.
pub(crate) fn pill(default: bool, focused: bool, status: button::Status) -> button::Style {
    let background = if default {
        palette::ACCENT
    } else {
        palette::PILL_SECONDARY
    };
    let text = if status == button::Status::Disabled {
        palette::TEXT_SECONDARY
    } else {
        palette::TEXT_PRIMARY
    };
    button::Style {
        background: fill(background),
        text_color: color(text),
        border: focus_border(focused, radius::PILL),
        ..button::Style::default()
    }
}

/// The password pill.
pub(crate) fn field(_theme: &Theme, _status: text_input::Status) -> text_input::Style {
    text_input::Style {
        background: Background::Color(color(palette::FIELD_GLASS)),
        border: hairline(palette::FIELD_BORDER, radius::FIELD),
        icon: color(palette::TEXT_PRIMARY),
        placeholder: color(palette::TEXT_PLACEHOLDER),
        value: color(palette::TEXT_PRIMARY),
        selection: color(palette::ACCENT),
    }
}

/// An outlined circle button of the given diameter: submit and back.
pub(crate) fn circle(diameter: f32, focused: bool) -> button::Style {
    let corner = diameter * radius::ROUND;
    button::Style {
        background: None,
        text_color: color(palette::TEXT_PRIMARY),
        border: if focused {
            focus_border(true, corner)
        } else {
            Border {
                color: color(component::CIRCLE_OUTLINE),
                width: component::CIRCLE_OUTLINE_WIDTH,
                radius: corner.into(),
            }
        },
        ..button::Style::default()
    }
}

/// An initials avatar of the given diameter.
pub(crate) fn avatar(diameter: f32) -> container::Style {
    container::Style {
        text_color: Some(color(palette::TEXT_PRIMARY)),
        background: Some(Background::Gradient(
            Linear::new(Radians(PI))
                .add_stop(0.0, color(palette::AVATAR_TOP))
                .add_stop(1.0, color(palette::AVATAR_BOTTOM))
                .into(),
        )),
        border: Border {
            color: color(palette::AVATAR_RING),
            width: component::AVATAR_RING_WIDTH,
            radius: (diameter * radius::ROUND).into(),
        },
        ..container::Style::default()
    }
}

/// The ring around a keyboard-focused avatar of the given outer diameter.
pub(crate) fn avatar_focus(diameter: f32, focused: bool) -> container::Style {
    container::Style {
        border: if focused {
            focus_border(true, diameter * radius::ROUND)
        } else {
            Border::default()
        },
        ..container::Style::default()
    }
}

/// The non-interactive preview badge.
pub(crate) fn badge(corner: f32) -> container::Style {
    container::Style {
        text_color: Some(color(palette::TEXT_PRIMARY)),
        background: fill(palette::MENU_MATERIAL),
        border: hairline(palette::MENU_BORDER, corner),
        ..container::Style::default()
    }
}

/// Scrollbars stay quiet: a translucent thumb on no rail.
pub(crate) fn scrollbar(_theme: &Theme, status: scrollable::Status) -> scrollable::Style {
    let thumb = match status {
        scrollable::Status::Active { .. } => palette::TEXT_MENU_HEADING,
        scrollable::Status::Hovered { .. } | scrollable::Status::Dragged { .. } => {
            palette::TEXT_PLACEHOLDER
        }
    };
    let rail = scrollable::Rail {
        background: None,
        border: Border::default(),
        scroller: scrollable::Scroller {
            background: Background::Color(color(thumb)),
            border: Border {
                radius: radius::ITEM.into(),
                ..Border::default()
            },
        },
    };
    scrollable::Style {
        container: container::Style::default(),
        horizontal_rail: rail,
        vertical_rail: rail,
        gap: None,
        auto_scroll: scrollable::AutoScroll {
            background: Background::Color(color(palette::MENU_MATERIAL)),
            border: hairline(palette::MENU_BORDER, radius::ITEM),
            shadow: Shadow::default(),
            icon: color(palette::TEXT_PRIMARY),
        },
    }
}

#[cfg(test)]
mod tests {
    //! Renderer-aware color-role checks. Wallpaper text is measured against
    //! its outline, not falsely claimed to contrast against every bright pixel.
    use super::*;
    use iced::Color;

    const NORMAL_TEXT_CONTRAST: f32 = 4.5;
    const COVERAGE_STEPS: u8 = 4;

    #[derive(Clone, Copy)]
    enum BlendSpace {
        LinearSrgb,
        EncodedSrgb,
    }

    fn channel_luminance(channel: f32) -> f32 {
        if channel <= 0.04045 {
            channel / 12.92
        } else {
            ((channel + 0.055) / 1.055).powf(2.4)
        }
    }

    fn encode_channel(channel: f32) -> f32 {
        if channel <= 0.003_130_8 {
            channel * 12.92
        } else {
            1.055 * channel.powf(1.0 / 2.4) - 0.055
        }
    }

    fn composite(foreground: Color, background: Color, blend_space: BlendSpace) -> Color {
        let blend = |top: f32, bottom: f32| match blend_space {
            BlendSpace::LinearSrgb => encode_channel(
                channel_luminance(top) * foreground.a
                    + channel_luminance(bottom) * (1.0 - foreground.a),
            ),
            BlendSpace::EncodedSrgb => top * foreground.a + bottom * (1.0 - foreground.a),
        };
        Color::from_rgb(
            blend(foreground.r, background.r),
            blend(foreground.g, background.g),
            blend(foreground.b, background.b),
        )
    }

    fn luminance(color: Color) -> f32 {
        0.2126 * channel_luminance(color.r)
            + 0.7152 * channel_luminance(color.g)
            + 0.0722 * channel_luminance(color.b)
    }

    fn contrast(first: Color, second: Color) -> f32 {
        let (lighter, darker) = if luminance(first) > luminance(second) {
            (luminance(first), luminance(second))
        } else {
            (luminance(second), luminance(first))
        };
        (lighter + 0.05) / (darker + 0.05)
    }

    fn solid(background: Option<Background>) -> Color {
        match background.expect("surface has a fill") {
            Background::Color(color) => color,
            Background::Gradient(_) => panic!("contrast-tested surfaces are solid"),
        }
    }

    fn assert_text(foreground: Color, surface: Color, blend_space: BlendSpace, label: &str) {
        let rendered = composite(foreground, surface, blend_space);
        let ratio = contrast(rendered, surface);
        assert!(
            ratio >= NORMAL_TEXT_CONTRAST,
            "{label} contrast was {ratio}"
        );
    }

    fn posters() -> Vec<image::RgbaImage> {
        [
            include_bytes!("../../assets/wallpapers/tahoe-beach-poster.jpg").as_slice(),
            include_bytes!("../../assets/wallpapers/golden-gate-poster.jpg").as_slice(),
            include_bytes!("../../assets/wallpapers/sequoia-morning-poster.jpg").as_slice(),
            include_bytes!("../../assets/wallpapers/sequoia-night-poster.jpg").as_slice(),
            include_bytes!("../../assets/wallpapers/sequoia-sunrise-poster.jpg").as_slice(),
        ]
        .into_iter()
        .map(|bytes| image::load_from_memory(bytes).unwrap().to_rgba8())
        .collect()
    }

    fn underlays() -> Vec<Color> {
        let mut result = Vec::new();
        // RGB cube vertices include pure white: blur cannot make a white
        // wallpaper darker, so the actual material backing must do that.
        for r in [0, 255] {
            for g in [0, 255] {
                for b in [0, 255] {
                    let source =
                        genkan_session_lock::RgbaFrame::new(1, 1, vec![r, g, b, 255].into())
                            .unwrap();
                    let blur = genkan_session_lock::material::blurred(&source);
                    let pixel = blur.pixels();
                    result.push(Color::from_rgb8(pixel[0], pixel[1], pixel[2]));
                }
            }
        }
        for poster in posters() {
            let (width, height) = poster.dimensions();
            let frame =
                genkan_session_lock::RgbaFrame::new(width, height, poster.into_raw().into())
                    .unwrap();
            let blur = genkan_session_lock::material::blurred(&frame);
            result.extend(
                blur.pixels()
                    .chunks_exact(4)
                    .step_by(173)
                    .map(|p| Color::from_rgb8(p[0], p[1], p[2])),
            );
        }
        result
    }

    #[test]
    fn menu_and_alert_text_meet_contrast_over_wallpaper_underlays() {
        let theme = Theme::Dark;
        let underlays = underlays();
        for blend_space in [BlendSpace::LinearSrgb, BlendSpace::EncodedSrgb] {
            for &underlay in &underlays {
                let field_surface = composite(color(palette::FIELD_GLASS), underlay, blend_space);
                for (label, token) in [
                    ("response", palette::TEXT_PRIMARY),
                    ("placeholder", palette::TEXT_PLACEHOLDER),
                ] {
                    assert_text(color(token), field_surface, blend_space, label);
                }
                let menu_surface = composite(solid(menu(&theme).background), underlay, blend_space);
                assert_text(
                    color(palette::TEXT_MENU_HEADING),
                    menu_surface,
                    blend_space,
                    "menu heading",
                );
                assert_text(
                    color(palette::TEXT_PRIMARY),
                    menu_surface,
                    blend_space,
                    "menu item",
                );
                let highlighted =
                    composite(solid(menu_item(true).background), menu_surface, blend_space);
                assert_text(
                    menu_item(true).text_color,
                    highlighted,
                    blend_space,
                    "highlighted menu item",
                );

                let alert_surface =
                    composite(solid(alert(&theme).background), underlay, blend_space);
                assert_text(
                    color(palette::TEXT_PRIMARY),
                    alert_surface,
                    blend_space,
                    "alert title",
                );
                assert_text(
                    color(palette::TEXT_SECONDARY),
                    alert_surface,
                    blend_space,
                    "alert body",
                );
                let default = pill(true, false, button::Status::Active);
                let surface = composite(solid(default.background), alert_surface, blend_space);
                assert_text(
                    default.text_color,
                    surface,
                    blend_space,
                    "default alert button",
                );
                let cancel = pill(false, false, button::Status::Active);
                let surface = composite(solid(cancel.background), alert_surface, blend_space);
                assert_text(
                    cancel.text_color,
                    surface,
                    blend_space,
                    "cancel alert button",
                );
            }
        }
    }

    #[test]
    fn wallpaper_text_with_its_outline_meets_contrast_over_packaged_posters() {
        for poster in posters() {
            for pixel in poster.pixels().step_by(997) {
                let background = Color::from_rgb8(pixel[0], pixel[1], pixel[2]);
                for space in [BlendSpace::LinearSrgb, BlendSpace::EncodedSrgb] {
                    let outlined = composite(color(palette::TEXT_SHADOW), background, space);
                    for (role, token) in [
                        ("name and status", palette::TEXT_PRIMARY),
                        ("date", palette::TEXT_DATE),
                        ("clock", palette::CLOCK_GLASS),
                        ("secondary name", palette::TEXT_SECONDARY),
                    ] {
                        assert_text(color(token), outlined, space, role);
                    }
                }
            }
        }
    }

    #[test]
    fn wallpaper_text_meets_contrast_on_the_generated_background() {
        let (base, blobs) = crate::background::contrast_palette();
        for blend_space in [BlendSpace::LinearSrgb, BlendSpace::EncodedSrgb] {
            for first in 0..=COVERAGE_STEPS {
                for second in 0..=COVERAGE_STEPS {
                    for third in 0..=COVERAGE_STEPS {
                        let mut backdrop = base;
                        for (blob, coverage) in blobs.into_iter().zip([first, second, third]) {
                            backdrop = composite(
                                Color {
                                    a: blob.a * f32::from(coverage) / f32::from(COVERAGE_STEPS),
                                    ..blob
                                },
                                backdrop,
                                blend_space,
                            );
                        }
                        for (label, token) in [
                            ("primary", palette::TEXT_PRIMARY),
                            ("date", palette::TEXT_DATE),
                            ("clock", palette::CLOCK_GLASS),
                            ("placeholder", palette::TEXT_PLACEHOLDER),
                        ] {
                            assert_text(color(token), backdrop, blend_space, label);
                        }
                        let field_surface = composite(
                            solid(Some(
                                field(&Theme::Dark, text_input::Status::Active).background,
                            )),
                            backdrop,
                            blend_space,
                        );
                        assert_text(
                            color(palette::TEXT_PLACEHOLDER),
                            field_surface,
                            blend_space,
                            "field placeholder",
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn focus_is_visible_without_color_alone() {
        assert_eq!(
            status_item(false, true).border.width,
            component::FOCUS_RING_WIDTH
        );
        assert_eq!(status_item(false, false).border.width, 0.0);
        assert_eq!(
            pill(false, true, button::Status::Active).border.width,
            component::FOCUS_RING_WIDTH
        );
        assert_eq!(circle(16.0, true).border.width, component::FOCUS_RING_WIDTH);
        assert_eq!(
            avatar_focus(80.0, true).border.width,
            component::FOCUS_RING_WIDTH
        );
        assert_eq!(avatar_focus(80.0, false).border.width, 0.0);
    }

    #[test]
    fn open_items_and_highlighted_menu_items_use_their_tokens() {
        assert_eq!(
            status_item(true, false).background,
            fill(palette::ITEM_ACTIVE)
        );
        assert_eq!(status_item(false, false).background, None);
        assert_eq!(menu_item(true).background, fill(palette::ACCENT));
        assert_eq!(menu_item(false).background, None);
        assert_eq!(menu_item(false).border.radius, radius::MENU_ITEM.into());
    }

    #[test]
    fn avatar_is_a_vertical_gradient_disc() {
        let style = avatar(75.0);
        assert!(matches!(style.background, Some(Background::Gradient(_))));
        assert_eq!(style.border.radius, 37.5.into());
        assert_eq!(style.border.width, component::AVATAR_RING_WIDTH);
    }
}
