mod accounts;
mod app;
mod background;
mod power;
mod sessions;
mod theme;

use app::{App, Config};
use clap::Parser;
use iced::{window, Theme};

#[derive(Debug, Parser)]
#[command(version, about)]
struct Arguments {
    #[arg(long, value_parser = parse_username)]
    username: Option<String>,
    #[arg(long, requires = "username", value_parser = parse_display_name)]
    display_name: Option<String>,
    #[arg(long)]
    windowed: bool,
}

fn parse_username(value: &str) -> Result<String, String> {
    if value.is_empty()
        || value.chars().count() > 256
        || value
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
    {
        Err("username must be 1–256 characters without whitespace or control characters".into())
    } else {
        Ok(value.into())
    }
}

fn parse_display_name(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() || value.chars().any(char::is_control) {
        Err("display name must not be empty or contain control characters".into())
    } else {
        Ok(value.into())
    }
}

pub fn main() -> iced::Result {
    let arguments = Arguments::parse();
    let windowed = arguments.windowed;
    let config = Config {
        username: arguments.username,
        display_name: arguments.display_name,
    };

    iced::application("Genkan", App::update, App::view)
        .subscription(App::subscription)
        .theme(|_| Theme::Dark)
        .window(window::Settings {
            size: iced::Size::new(1280.0, 800.0),
            decorations: windowed,
            ..Default::default()
        })
        .exit_on_close_request(false)
        .antialiasing(true)
        .run_with(|| App::new(config))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_overrides_are_optional() {
        let arguments = Arguments::try_parse_from(["genkan"]).unwrap();
        assert_eq!(arguments.username, None);
        assert_eq!(arguments.display_name, None);
    }

    #[test]
    fn display_name_override_requires_username() {
        assert!(Arguments::try_parse_from(["genkan", "--display-name", "Operator"]).is_err());
    }

    #[test]
    fn identity_overrides_reject_empty_or_malformed_values() {
        assert!(Arguments::try_parse_from(["genkan", "--username", ""]).is_err());
        assert!(Arguments::try_parse_from(["genkan", "--username", "two users"]).is_err());
        assert!(Arguments::try_parse_from([
            "genkan",
            "--username",
            "operator",
            "--display-name",
            " \n ",
        ])
        .is_err());
    }
}
