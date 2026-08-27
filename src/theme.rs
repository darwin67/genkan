use iced::widget::{button, container, text_input};
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
