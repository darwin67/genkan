use iced::overlay::menu;
use iced::widget::{button, container, pick_list, text_input};
use iced::{Background, Border, Color, Shadow, Theme};

pub fn panel(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(Color::from_rgba8(7, 10, 24, 0.58))),
        border: Border {
            color: Color::from_rgba8(255, 255, 255, 0.16),
            width: 1.0,
            radius: 32.0.into(),
        },
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
        background: Background::Color(Color::from_rgba8(255, 255, 255, 0.12)),
        border: Border {
            color: if focused {
                Color::from_rgba8(255, 255, 255, 0.7)
            } else {
                Color::from_rgba8(255, 255, 255, 0.28)
            },
            width: if focused { 2.0 } else { 1.0 },
            radius: 22.0.into(),
        },
        icon: Color::WHITE,
        placeholder: Color::from_rgba8(255, 255, 255, 0.55),
        value: Color::WHITE,
        selection: Color::from_rgb8(65, 105, 225),
    }
}

pub fn selector(_theme: &Theme, status: pick_list::Status) -> pick_list::Style {
    let highlighted = matches!(
        status,
        pick_list::Status::Hovered | pick_list::Status::Opened
    );
    pick_list::Style {
        text_color: Color::WHITE,
        placeholder_color: Color::from_rgba8(255, 255, 255, 0.55),
        handle_color: Color::from_rgba8(255, 255, 255, 0.8),
        background: Background::Color(Color::from_rgba8(
            255,
            255,
            255,
            if highlighted { 0.18 } else { 0.12 },
        )),
        border: Border {
            color: Color::from_rgba8(255, 255, 255, if highlighted { 0.5 } else { 0.28 }),
            width: 1.0,
            radius: 18.0.into(),
        },
    }
}

pub fn selector_menu(_theme: &Theme) -> menu::Style {
    menu::Style {
        background: Background::Color(Color::from_rgba8(22, 27, 56, 0.98)),
        border: Border {
            color: Color::from_rgba8(255, 255, 255, 0.28),
            width: 1.0,
            radius: 14.0.into(),
        },
        text_color: Color::WHITE,
        selected_text_color: Color::WHITE,
        selected_background: Background::Color(Color::from_rgba8(255, 255, 255, 0.18)),
    }
}

pub fn selection(_theme: &Theme) -> container::Style {
    container::Style {
        text_color: Some(Color::WHITE),
        background: Some(Background::Color(Color::from_rgba8(255, 255, 255, 0.12))),
        border: Border {
            color: Color::from_rgba8(255, 255, 255, 0.28),
            width: 1.0,
            radius: 18.0.into(),
        },
        ..Default::default()
    }
}

pub fn translucent_button(_theme: &Theme, status: button::Status) -> button::Style {
    let alpha = match status {
        button::Status::Hovered => 0.24,
        button::Status::Pressed => 0.3,
        _ => 0.13,
    };
    button::Style {
        background: Some(Background::Color(Color::from_rgba8(255, 255, 255, alpha))),
        text_color: Color::WHITE,
        border: Border {
            radius: 18.0.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}
