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
        color: Color::from_rgba8(255, 255, 255, if emphasized { 0.95 } else { alpha }),
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

pub fn selector(_theme: &Theme, status: pick_list::Status) -> pick_list::Style {
    let hovered = matches!(status, pick_list::Status::Hovered);
    let opened = matches!(status, pick_list::Status::Opened);
    pick_list::Style {
        text_color: primary_text(),
        placeholder_color: Color::from_rgba8(255, 255, 255, 0.55),
        handle_color: Color::from_rgba8(255, 255, 255, 0.8),
        background: Background::Color(Color::from_rgba8(
            255,
            255,
            255,
            if hovered || opened { 0.18 } else { 0.12 },
        )),
        border: outline(if hovered { 0.5 } else { 0.28 }, opened),
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

pub fn primary_button(_theme: &Theme, status: button::Status) -> button::Style {
    let alpha = match status {
        button::Status::Hovered => 0.34,
        button::Status::Pressed => 0.42,
        button::Status::Disabled => 0.14,
        button::Status::Active => 0.27,
    };
    button::Style {
        background: Some(Background::Color(Color::from_rgba8(255, 255, 255, alpha))),
        text_color: primary_text(),
        border: outline(0.34, false),
        shadow: elevation(),
    }
}

pub fn dialog_button(
    theme: &Theme,
    status: button::Status,
    focused: bool,
    destructive: bool,
) -> button::Style {
    let mut style = primary_button(theme, status);
    if destructive {
        style.background = Some(Background::Color(Color::from_rgba8(170, 42, 52, 0.72)));
    }
    if focused {
        style.border = outline(0.34, true);
    }
    style
}

pub fn secondary_button(_theme: &Theme, status: button::Status) -> button::Style {
    let alpha = match status {
        button::Status::Hovered => 0.24,
        button::Status::Pressed => 0.3,
        _ => 0.13,
    };
    button::Style {
        background: Some(Background::Color(Color::from_rgba8(255, 255, 255, alpha))),
        text_color: primary_text(),
        border: outline(0.28, false),
        shadow: elevation(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controls_share_corner_and_focus_treatment() {
        let theme = Theme::Dark;
        let active_input = input(&theme, text_input::Status::Active);
        let focused_input = input(&theme, text_input::Status::Focused);
        let focused_account = account_tile(&theme, button::Status::Active, true);
        let focused_dialog = dialog_button(&theme, button::Status::Active, true, false);
        let opened_selector = selector(&theme, pick_list::Status::Opened);
        let dialog = dialog(&theme);
        let selector_menu = selector_menu(&theme);

        assert_eq!(active_input.border.radius, CONTROL_RADIUS.into());
        assert_eq!(
            primary_button(&theme, button::Status::Active).border.radius,
            active_input.border.radius
        );
        assert_eq!(
            secondary_button(&theme, button::Status::Active)
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
        let closed = selector(&theme, pick_list::Status::Active);
        let opened = selector(&theme, pick_list::Status::Opened);

        assert_eq!(closed.border.width, 1.0);
        assert_eq!(opened.border.width, EMPHASIS_WIDTH);
        assert_eq!(opened.border.color, Color::from_rgba8(255, 255, 255, 0.95));
    }

    #[test]
    fn noninteractive_preview_badge_is_opaque() {
        assert_eq!(
            preview_badge(&Theme::Dark).background,
            Some(Background::Color(Color::from_rgb8(22, 27, 56)))
        );
    }
}
