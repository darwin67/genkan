use iced::overlay::menu;
use iced::widget::{button, container, pick_list, text_input};
use iced::{Background, Border, Color, Shadow, Theme};

const CONTROL_RADIUS: f32 = 18.0;
const EMPHASIS_WIDTH: f32 = 3.0;

pub fn primary_text() -> Color {
    Color::WHITE
}

pub fn strong_secondary_text() -> Color {
    Color::from_rgba8(255, 255, 255, 0.85)
}

pub fn secondary_text() -> Color {
    Color::from_rgba8(255, 255, 255, 0.78)
}

pub fn muted_text() -> Color {
    Color::from_rgba8(255, 255, 255, 0.68)
}

pub fn status_text(error: bool) -> Color {
    if error {
        Color::from_rgb8(255, 171, 171)
    } else {
        secondary_text()
    }
}

fn material(alpha: f32) -> Background {
    Background::Color(Color::from_rgba8(7, 10, 24, alpha))
}

fn outline(alpha: f32, emphasized: bool) -> Border {
    Border {
        color: Color::from_rgba8(
            255,
            255,
            255,
            if emphasized { 0.95 } else { alpha.max(0.44) },
        ),
        width: if emphasized { EMPHASIS_WIDTH } else { 1.0 },
        radius: CONTROL_RADIUS.into(),
    }
}

fn elevation() -> Shadow {
    Shadow {
        color: Color::from_rgba8(0, 0, 0, 0.3),
        offset: iced::Vector::new(0.0, 6.0),
        blur_radius: 18.0,
    }
}

pub fn dialog(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(material(0.82)),
        border: outline(0.28, false),
        shadow: Shadow {
            color: Color::from_rgba8(0, 0, 0, 0.45),
            offset: iced::Vector::new(0.0, 12.0),
            blur_radius: 32.0,
        },
        ..Default::default()
    }
}

pub fn input(_theme: &Theme, status: text_input::Status) -> text_input::Style {
    let focused = matches!(status, text_input::Status::Focused);
    text_input::Style {
        background: material(0.72),
        border: outline(0.42, focused),
        icon: primary_text(),
        placeholder: Color::from_rgba8(255, 255, 255, 0.55),
        value: primary_text(),
        selection: Color::from_rgb8(65, 105, 225),
    }
}

pub fn selector(_theme: &Theme, status: pick_list::Status, focused: bool) -> pick_list::Style {
    let hovered = matches!(status, pick_list::Status::Hovered);
    let opened = matches!(status, pick_list::Status::Opened);
    pick_list::Style {
        text_color: primary_text(),
        placeholder_color: Color::from_rgba8(255, 255, 255, 0.72),
        handle_color: Color::from_rgba8(255, 255, 255, 0.8),
        background: Background::Color(Color::from_rgba8(
            255,
            255,
            255,
            if hovered || opened { 0.18 } else { 0.12 },
        )),
        border: outline(if hovered { 0.5 } else { 0.28 }, opened || focused),
    }
}

pub fn selector_menu(_theme: &Theme) -> menu::Style {
    menu::Style {
        background: Background::Color(Color::from_rgba8(22, 27, 56, 0.98)),
        border: outline(0.28, false),
        text_color: primary_text(),
        selected_text_color: primary_text(),
        selected_background: Background::Color(Color::from_rgba8(255, 255, 255, 0.18)),
    }
}

pub fn inactive_control(_theme: &Theme) -> container::Style {
    container::Style {
        text_color: Some(primary_text()),
        background: Some(Background::Color(Color::from_rgba8(255, 255, 255, 0.12))),
        border: outline(0.28, false),
        ..Default::default()
    }
}

pub fn preview_badge(_theme: &Theme) -> container::Style {
    container::Style {
        text_color: Some(primary_text()),
        background: Some(Background::Color(Color::from_rgb8(22, 27, 56))),
        border: outline(0.28, false),
        ..Default::default()
    }
}

pub fn avatar(radius: f32) -> container::Style {
    container::Style {
        background: Some(material(0.76)),
        border: Border {
            color: Color::from_rgba8(255, 255, 255, 0.46),
            width: 2.0,
            radius: radius.into(),
        },
        shadow: elevation(),
        ..Default::default()
    }
}

pub fn account_tile(_theme: &Theme, status: button::Status, focused: bool) -> button::Style {
    let (background, border) = match status {
        button::Status::Hovered => (0.72, 0.58),
        button::Status::Pressed => (0.82, 0.68),
        button::Status::Disabled => (0.34, 0.18),
        button::Status::Active => (0.58, 0.34),
    };
    button::Style {
        background: Some(material(background)),
        text_color: primary_text(),
        border: outline(border, focused),
        shadow: elevation(),
    }
}

pub fn primary_button(_theme: &Theme, status: button::Status, focused: bool) -> button::Style {
    let background = match status {
        button::Status::Active => Color::from_rgb8(56, 92, 204),
        button::Status::Hovered => Color::from_rgb8(65, 105, 225),
        button::Status::Pressed => Color::from_rgb8(47, 79, 180),
        button::Status::Disabled => Color::from_rgba8(255, 255, 255, 0.14),
    };
    button::Style {
        background: Some(Background::Color(background)),
        text_color: primary_text(),
        border: outline(0.34, focused),
        shadow: elevation(),
    }
}

pub fn dialog_button(
    theme: &Theme,
    status: button::Status,
    focused: bool,
    destructive: bool,
) -> button::Style {
    let mut style = primary_button(theme, status, focused);
    if destructive {
        style.background = Some(Background::Color(Color::from_rgba8(170, 42, 52, 0.72)));
    }
    if focused {
        style.border = outline(0.34, true);
    }
    style
}

pub fn secondary_button(_theme: &Theme, status: button::Status, focused: bool) -> button::Style {
    let alpha = match status {
        button::Status::Hovered => 0.24,
        button::Status::Pressed => 0.3,
        _ => 0.13,
    };
    button::Style {
        background: Some(Background::Color(Color::from_rgba8(255, 255, 255, alpha))),
        text_color: primary_text(),
        border: outline(0.28, focused),
        shadow: elevation(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NORMAL_TEXT_CONTRAST: f32 = 4.5;
    const NON_TEXT_CONTRAST: f32 = 3.0;

    fn composite(foreground: Color, background: Color) -> Color {
        Color::from_rgb(
            foreground.r * foreground.a + background.r * (1.0 - foreground.a),
            foreground.g * foreground.a + background.g * (1.0 - foreground.a),
            foreground.b * foreground.a + background.b * (1.0 - foreground.a),
        )
    }

    fn channel_luminance(channel: f32) -> f32 {
        if channel <= 0.04045 {
            channel / 12.92
        } else {
            ((channel + 0.055) / 1.055).powf(2.4)
        }
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

    fn background_color(background: Background) -> Color {
        match background {
            Background::Color(color) => color,
            Background::Gradient(_) => panic!("control styles use solid material colors"),
        }
    }

    fn assert_text_contrast(foreground: Color, surface: Color, label: &str) {
        let rendered = composite(foreground, surface);
        assert!(
            contrast(rendered, surface) >= NORMAL_TEXT_CONTRAST,
            "{label} contrast was {}",
            contrast(rendered, surface)
        );
    }

    fn assert_boundary_contrast(border: Border, backdrop: Color, label: &str) {
        let rendered = composite(border.color, backdrop);
        assert!(
            contrast(rendered, backdrop) >= NON_TEXT_CONTRAST,
            "{label} contrast was {}",
            contrast(rendered, backdrop)
        );
    }

    #[test]
    fn text_tokens_meet_wcag_contrast_on_every_background_combination() {
        for backdrop in crate::background::contrast_backdrops() {
            for (label, color) in [
                ("primary", primary_text()),
                ("strong secondary", strong_secondary_text()),
                ("secondary", secondary_text()),
                ("muted", muted_text()),
                ("error", status_text(true)),
            ] {
                assert_text_contrast(color, backdrop, label);
            }
        }
    }

    #[test]
    fn control_text_and_boundaries_meet_wcag_contrast() {
        let theme = Theme::Dark;
        for backdrop in crate::background::contrast_backdrops() {
            for status in [
                button::Status::Active,
                button::Status::Hovered,
                button::Status::Pressed,
                button::Status::Disabled,
            ] {
                for (label, style) in [
                    ("account tile", account_tile(&theme, status, false)),
                    ("primary button", primary_button(&theme, status, false)),
                    ("secondary button", secondary_button(&theme, status, false)),
                    (
                        "destructive button",
                        dialog_button(&theme, status, false, true),
                    ),
                ] {
                    let surface = composite(
                        background_color(style.background.expect("control material")),
                        backdrop,
                    );
                    assert_text_contrast(style.text_color, surface, label);
                    assert_boundary_contrast(style.border, backdrop, label);
                }
            }

            for status in [
                pick_list::Status::Active,
                pick_list::Status::Hovered,
                pick_list::Status::Opened,
            ] {
                let style = selector(&theme, status, false);
                let surface = composite(background_color(style.background), backdrop);
                assert_text_contrast(style.text_color, surface, "selector text");
                assert_text_contrast(style.placeholder_color, surface, "selector placeholder");
                assert_boundary_contrast(
                    Border {
                        color: style.handle_color,
                        width: 1.0,
                        ..Border::default()
                    },
                    surface,
                    "selector handle",
                );
                assert_boundary_contrast(style.border, backdrop, "selector boundary");
            }

            let input_style = input(&theme, text_input::Status::Active);
            let surface = composite(background_color(input_style.background), backdrop);
            assert_text_contrast(input_style.value, surface, "input value");
            assert_text_contrast(input_style.placeholder, surface, "input placeholder");
            assert_text_contrast(
                input_style.value,
                input_style.selection,
                "selected input value",
            );
            assert_boundary_contrast(input_style.border, backdrop, "input boundary");
            assert_boundary_contrast(avatar(50.0).border, backdrop, "avatar boundary");

            for (label, style) in [
                ("inactive control", inactive_control(&theme)),
                ("preview badge", preview_badge(&theme)),
                ("dialog", dialog(&theme)),
            ] {
                let surface = composite(
                    background_color(style.background.expect("container material")),
                    backdrop,
                );
                assert_text_contrast(
                    style.text_color.unwrap_or_else(primary_text),
                    surface,
                    label,
                );
                assert_boundary_contrast(style.border, backdrop, label);
            }

            for border in [
                input(&theme, text_input::Status::Focused).border,
                account_tile(&theme, button::Status::Active, true).border,
                primary_button(&theme, button::Status::Active, true).border,
                selector(&theme, pick_list::Status::Active, true).border,
            ] {
                assert_boundary_contrast(border, backdrop, "focus ring");
            }
        }

        let menu = selector_menu(&theme);
        let menu_surface = background_color(menu.background);
        assert_text_contrast(menu.text_color, menu_surface, "selector menu text");
        let selected_surface = composite(background_color(menu.selected_background), menu_surface);
        assert_text_contrast(
            menu.selected_text_color,
            selected_surface,
            "selected menu text",
        );
    }

    #[test]
    fn controls_share_corner_and_focus_treatment() {
        let theme = Theme::Dark;
        let active_input = input(&theme, text_input::Status::Active);
        let focused_input = input(&theme, text_input::Status::Focused);
        let focused_account = account_tile(&theme, button::Status::Active, true);
        let focused_dialog = dialog_button(&theme, button::Status::Active, true, false);
        let opened_selector = selector(&theme, pick_list::Status::Opened, false);
        let dialog = dialog(&theme);
        let selector_menu = selector_menu(&theme);

        assert_eq!(active_input.border.radius, CONTROL_RADIUS.into());
        assert_eq!(
            primary_button(&theme, button::Status::Active, false)
                .border
                .radius,
            active_input.border.radius
        );
        assert_eq!(
            secondary_button(&theme, button::Status::Active, false)
                .border
                .radius,
            active_input.border.radius
        );
        assert_eq!(opened_selector.border.radius, active_input.border.radius);
        assert_eq!(dialog.border.radius, active_input.border.radius);
        assert_eq!(selector_menu.border.radius, active_input.border.radius);
        for border in [
            focused_input.border,
            focused_account.border,
            focused_dialog.border,
        ] {
            assert_eq!(border.width, EMPHASIS_WIDTH);
            assert_eq!(border.color, Color::from_rgba8(255, 255, 255, 0.95));
        }
    }

    #[test]
    fn opened_selector_is_emphasized_without_claiming_keyboard_focus() {
        let theme = Theme::Dark;
        let closed = selector(&theme, pick_list::Status::Active, false);
        let opened = selector(&theme, pick_list::Status::Opened, false);

        assert_eq!(closed.border.width, 1.0);
        assert_eq!(opened.border.width, EMPHASIS_WIDTH);
        assert_eq!(opened.border.color, Color::from_rgba8(255, 255, 255, 0.95));
    }

    #[test]
    fn logical_focus_is_visible_on_non_focusable_iced_controls() {
        let theme = Theme::Dark;
        let selector = selector(&theme, pick_list::Status::Active, true);
        let primary = primary_button(&theme, button::Status::Active, true);
        let secondary = secondary_button(&theme, button::Status::Active, true);

        for border in [selector.border, primary.border, secondary.border] {
            assert_eq!(border.width, EMPHASIS_WIDTH);
            assert_eq!(border.color, Color::from_rgba8(255, 255, 255, 0.95));
        }
    }

    #[test]
    fn noninteractive_preview_badge_is_opaque() {
        assert_eq!(
            preview_badge(&Theme::Dark).background,
            Some(Background::Color(Color::from_rgb8(22, 27, 56)))
        );
    }
}
