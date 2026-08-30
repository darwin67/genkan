use iced::overlay::menu;
use iced::widget::{button, container, pick_list, scrollable, text_input};
use iced::{Background, Border, Color, Theme};

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
            if emphasized { 0.95 } else { alpha.max(0.46) },
        ),
        width: if emphasized { EMPHASIS_WIDTH } else { 1.0 },
        radius: CONTROL_RADIUS.into(),
    }
}

pub fn dialog(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(material(0.82)),
        border: outline(0.28, false),
        ..Default::default()
    }
}

pub fn modal_scrim(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(Color::from_rgba8(0, 0, 0, 0.65))),
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
        background: material(if opened {
            0.76
        } else if hovered {
            0.68
        } else {
            0.58
        }),
        border: outline(if hovered { 0.5 } else { 0.28 }, opened || focused),
    }
}

pub fn selector_menu(_theme: &Theme) -> menu::Style {
    menu::Style {
        background: Background::Color(Color::from_rgb8(22, 27, 56)),
        border: outline(0.28, false),
        text_color: primary_text(),
        selected_text_color: primary_text(),
        selected_background: Background::Color(Color::from_rgb8(56, 92, 204)),
    }
}

pub fn inactive_control(_theme: &Theme) -> container::Style {
    container::Style {
        text_color: Some(primary_text()),
        background: Some(material(0.58)),
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
        ..Default::default()
    }
}

pub fn account_tile(_theme: &Theme, status: button::Status, focused: bool) -> button::Style {
    let (background, border) = match status {
        button::Status::Hovered => (0.72, 0.62),
        button::Status::Pressed => (0.82, 0.72),
        button::Status::Disabled => (0.34, 0.46),
        button::Status::Active => (0.58, 0.52),
    };
    button::Style {
        background: Some(material(background)),
        text_color: primary_text(),
        border: outline(border, focused),
        ..Default::default()
    }
}

pub fn primary_button(_theme: &Theme, status: button::Status, focused: bool) -> button::Style {
    let background = match status {
        button::Status::Active => Color::from_rgb8(56, 92, 204),
        button::Status::Hovered => Color::from_rgb8(65, 105, 225),
        button::Status::Pressed => Color::from_rgb8(47, 79, 180),
        button::Status::Disabled => Color::from_rgba8(7, 10, 24, 0.34),
    };
    button::Style {
        background: Some(Background::Color(background)),
        text_color: primary_text(),
        border: outline(0.34, focused),
        ..Default::default()
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
        button::Status::Hovered => 0.68,
        button::Status::Pressed => 0.76,
        button::Status::Disabled => 0.34,
        button::Status::Active => 0.58,
    };
    button::Style {
        background: Some(material(alpha)),
        text_color: primary_text(),
        border: outline(0.28, focused),
        ..Default::default()
    }
}

pub fn scrollbar(_theme: &Theme, status: scrollable::Status) -> scrollable::Style {
    let active = Color::from_rgb8(143, 151, 180);
    let hovered = Color::from_rgb8(190, 196, 220);
    let dragged = Color::from_rgb8(225, 229, 241);
    let (horizontal, vertical) = match status {
        scrollable::Status::Active => (active, active),
        scrollable::Status::Hovered {
            is_horizontal_scrollbar_hovered,
            is_vertical_scrollbar_hovered,
        } => (
            if is_horizontal_scrollbar_hovered {
                hovered
            } else {
                active
            },
            if is_vertical_scrollbar_hovered {
                hovered
            } else {
                active
            },
        ),
        scrollable::Status::Dragged {
            is_horizontal_scrollbar_dragged,
            is_vertical_scrollbar_dragged,
        } => (
            if is_horizontal_scrollbar_dragged {
                dragged
            } else {
                active
            },
            if is_vertical_scrollbar_dragged {
                dragged
            } else {
                active
            },
        ),
    };
    let rail = |color| scrollable::Rail {
        background: Some(Background::Color(Color::from_rgb8(7, 10, 24))),
        border: Border {
            radius: 2.0.into(),
            ..Default::default()
        },
        scroller: scrollable::Scroller {
            color,
            border: Border {
                radius: 2.0.into(),
                ..Default::default()
            },
        },
    };
    scrollable::Style {
        container: container::Style::default(),
        horizontal_rail: rail(horizontal),
        vertical_rail: rail(vertical),
        gap: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NORMAL_TEXT_CONTRAST: f32 = 4.5;
    const NON_TEXT_CONTRAST: f32 = 3.0;
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
        let blend = |foreground_channel: f32, background_channel: f32| match blend_space {
            BlendSpace::LinearSrgb => encode_channel(
                channel_luminance(foreground_channel) * foreground.a
                    + channel_luminance(background_channel) * (1.0 - foreground.a),
            ),
            BlendSpace::EncodedSrgb => {
                foreground_channel * foreground.a + background_channel * (1.0 - foreground.a)
            }
        };
        Color::from_rgb(
            blend(foreground.r, background.r),
            blend(foreground.g, background.g),
            blend(foreground.b, background.b),
        )
    }

    fn contrast_backdrops(blend_space: BlendSpace) -> Vec<Color> {
        let (base, blobs, dim) = crate::background::contrast_palette();
        let mut backdrops = Vec::with_capacity((usize::from(COVERAGE_STEPS) + 1).pow(3));
        for first in 0..=COVERAGE_STEPS {
            for second in 0..=COVERAGE_STEPS {
                for third in 0..=COVERAGE_STEPS {
                    let coverage = [first, second, third];
                    let mut color = base;
                    for (blob, coverage) in blobs.into_iter().zip(coverage) {
                        color = composite(
                            Color {
                                a: blob.a * f32::from(coverage) / f32::from(COVERAGE_STEPS),
                                ..blob
                            },
                            color,
                            blend_space,
                        );
                    }
                    backdrops.push(composite(dim, color, blend_space));
                }
            }
        }

        backdrops
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

    fn assert_text_contrast(
        foreground: Color,
        surface: Color,
        blend_space: BlendSpace,
        label: &str,
    ) {
        let rendered = composite(foreground, surface, blend_space);
        assert!(
            contrast(rendered, surface) >= NORMAL_TEXT_CONTRAST,
            "{label} contrast was {}",
            contrast(rendered, surface)
        );
    }

    fn assert_boundary_contrast(
        border: Border,
        wgpu_underlay: Color,
        tiny_skia_underlay: Color,
        adjacent: Color,
        blend_space: BlendSpace,
        label: &str,
    ) {
        let rendered = composite(
            border.color,
            match blend_space {
                BlendSpace::LinearSrgb => wgpu_underlay,
                BlendSpace::EncodedSrgb => tiny_skia_underlay,
            },
            blend_space,
        );
        assert!(
            contrast(rendered, adjacent) >= NON_TEXT_CONTRAST,
            "{label} contrast was {}",
            contrast(rendered, adjacent)
        );
    }

    fn assert_overlay_contrast(theme: &Theme, underlay: Color, blend_space: BlendSpace) {
        let menu = selector_menu(theme);
        let menu_surface = composite(background_color(menu.background), underlay, blend_space);
        assert_boundary_contrast(
            menu.border,
            underlay,
            menu_surface,
            menu_surface,
            blend_space,
            "selector menu inner boundary",
        );
        assert_text_contrast(
            menu.text_color,
            menu_surface,
            blend_space,
            "selector menu text",
        );
        let selected_surface = composite(
            background_color(menu.selected_background),
            menu_surface,
            blend_space,
        );
        assert_text_contrast(
            menu.selected_text_color,
            selected_surface,
            blend_space,
            "selected menu text",
        );

        let scrim = modal_scrim(theme);
        let scrimmed = composite(
            background_color(scrim.background.expect("modal scrim")),
            underlay,
            blend_space,
        );
        let dialog_style = dialog(theme);
        let dialog_surface = composite(
            background_color(dialog_style.background.expect("dialog material")),
            scrimmed,
            blend_space,
        );
        assert_text_contrast(primary_text(), dialog_surface, blend_space, "dialog text");
        assert_boundary_contrast(
            dialog_style.border,
            scrimmed,
            dialog_surface,
            dialog_surface,
            blend_space,
            "dialog inner boundary",
        );
        for status in [
            button::Status::Active,
            button::Status::Hovered,
            button::Status::Pressed,
            button::Status::Disabled,
        ] {
            for (label, style) in [
                (
                    "dialog cancel button",
                    dialog_button(theme, status, false, false),
                ),
                (
                    "dialog destructive button",
                    dialog_button(theme, status, false, true),
                ),
            ] {
                let surface = composite(
                    background_color(style.background.expect("dialog button material")),
                    dialog_surface,
                    blend_space,
                );
                assert_text_contrast(style.text_color, surface, blend_space, label);
                assert_boundary_contrast(
                    style.border,
                    dialog_surface,
                    surface,
                    dialog_surface,
                    blend_space,
                    label,
                );
            }
        }
    }

    #[test]
    fn text_tokens_meet_wcag_contrast_on_every_background_combination() {
        for blend_space in [BlendSpace::LinearSrgb, BlendSpace::EncodedSrgb] {
            for backdrop in contrast_backdrops(blend_space) {
                for (label, color) in [
                    ("primary", primary_text()),
                    ("strong secondary", strong_secondary_text()),
                    ("secondary", secondary_text()),
                    ("muted", muted_text()),
                    ("error", status_text(true)),
                ] {
                    assert_text_contrast(color, backdrop, blend_space, label);
                }
            }
        }
    }

    #[test]
    fn control_text_and_boundaries_meet_wcag_contrast() {
        let theme = Theme::Dark;
        for blend_space in [BlendSpace::LinearSrgb, BlendSpace::EncodedSrgb] {
            for backdrop in contrast_backdrops(blend_space) {
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
                    ] {
                        let surface = composite(
                            background_color(style.background.expect("control material")),
                            backdrop,
                            blend_space,
                        );
                        assert_text_contrast(style.text_color, surface, blend_space, label);
                        assert_boundary_contrast(
                            style.border,
                            backdrop,
                            surface,
                            backdrop,
                            blend_space,
                            label,
                        );
                    }
                }

                for status in [
                    pick_list::Status::Active,
                    pick_list::Status::Hovered,
                    pick_list::Status::Opened,
                ] {
                    let style = selector(&theme, status, false);
                    let surface =
                        composite(background_color(style.background), backdrop, blend_space);
                    assert_text_contrast(style.text_color, surface, blend_space, "selector text");
                    assert_text_contrast(
                        style.placeholder_color,
                        surface,
                        blend_space,
                        "selector placeholder",
                    );
                    assert_boundary_contrast(
                        Border {
                            color: style.handle_color,
                            width: 1.0,
                            ..Border::default()
                        },
                        surface,
                        surface,
                        surface,
                        blend_space,
                        "selector handle",
                    );
                    assert_boundary_contrast(
                        style.border,
                        backdrop,
                        surface,
                        backdrop,
                        blend_space,
                        "selector boundary",
                    );
                }

                let input_style = input(&theme, text_input::Status::Active);
                let surface = composite(
                    background_color(input_style.background),
                    backdrop,
                    blend_space,
                );
                assert_text_contrast(input_style.value, surface, blend_space, "input value");
                assert_text_contrast(
                    input_style.placeholder,
                    surface,
                    blend_space,
                    "input placeholder",
                );
                assert_text_contrast(
                    input_style.value,
                    input_style.selection,
                    blend_space,
                    "selected input value",
                );
                assert_boundary_contrast(
                    input_style.border,
                    backdrop,
                    surface,
                    backdrop,
                    blend_space,
                    "input boundary",
                );
                let avatar_style = avatar(50.0);
                let avatar_surface = composite(
                    background_color(avatar_style.background.expect("avatar material")),
                    backdrop,
                    blend_space,
                );
                assert_boundary_contrast(
                    avatar_style.border,
                    backdrop,
                    avatar_surface,
                    backdrop,
                    blend_space,
                    "avatar boundary",
                );

                for (label, style) in [
                    ("inactive control", inactive_control(&theme)),
                    ("preview badge", preview_badge(&theme)),
                ] {
                    let surface = composite(
                        background_color(style.background.expect("container material")),
                        backdrop,
                        blend_space,
                    );
                    assert_text_contrast(
                        style.text_color.unwrap_or_else(primary_text),
                        surface,
                        blend_space,
                        label,
                    );
                    assert_boundary_contrast(
                        style.border,
                        backdrop,
                        surface,
                        backdrop,
                        blend_space,
                        label,
                    );
                }

                let focused_input = input(&theme, text_input::Status::Focused);
                let focused_account = account_tile(&theme, button::Status::Active, true);
                let focused_primary = primary_button(&theme, button::Status::Active, true);
                let focused_selector = selector(&theme, pick_list::Status::Active, true);
                for (border, background) in [
                    (focused_input.border, focused_input.background),
                    (
                        focused_account.border,
                        focused_account.background.expect("account material"),
                    ),
                    (
                        focused_primary.border,
                        focused_primary.background.expect("primary material"),
                    ),
                    (focused_selector.border, focused_selector.background),
                ] {
                    let surface = composite(background_color(background), backdrop, blend_space);
                    assert_boundary_contrast(
                        border,
                        backdrop,
                        surface,
                        backdrop,
                        blend_space,
                        "focus ring",
                    );
                }
                assert_overlay_contrast(&theme, backdrop, blend_space);
            }
        }
    }

    #[test]
    fn overlays_meet_contrast_over_bright_page_content() {
        let theme = Theme::Dark;
        for blend_space in [BlendSpace::LinearSrgb, BlendSpace::EncodedSrgb] {
            assert_overlay_contrast(&theme, Color::WHITE, blend_space);
        }
    }

    #[test]
    fn scrollbar_thumbs_meet_component_contrast_in_every_state() {
        let theme = Theme::Dark;
        for status in [
            scrollable::Status::Active,
            scrollable::Status::Hovered {
                is_horizontal_scrollbar_hovered: true,
                is_vertical_scrollbar_hovered: true,
            },
            scrollable::Status::Dragged {
                is_horizontal_scrollbar_dragged: true,
                is_vertical_scrollbar_dragged: true,
            },
        ] {
            let style = scrollbar(&theme, status);
            for rail in [style.horizontal_rail, style.vertical_rail] {
                let track = background_color(rail.background.expect("scrollbar track"));
                assert!(
                    contrast(rail.scroller.color, track) >= NON_TEXT_CONTRAST,
                    "scrollbar thumb contrast was {}",
                    contrast(rail.scroller.color, track)
                );
            }
        }
    }

    #[test]
    fn contrast_tested_quads_avoid_the_wgpu_shadow_alpha_path() {
        let theme = Theme::Dark;
        for shadow in [
            dialog(&theme).shadow,
            avatar(50.0).shadow,
            account_tile(&theme, button::Status::Active, false).shadow,
            primary_button(&theme, button::Status::Active, false).shadow,
            secondary_button(&theme, button::Status::Active, false).shadow,
        ] {
            assert_eq!(shadow.color.a, 0.0);
        }
    }

    #[test]
    fn account_tile_states_keep_a_distinct_visual_hierarchy() {
        let theme = Theme::Dark;
        let active = account_tile(&theme, button::Status::Active, false);
        let disabled = account_tile(&theme, button::Status::Disabled, false);

        assert!(active.border.color.a > disabled.border.color.a);
        assert_ne!(active.background, disabled.background);
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
