mod accounts;
mod app;
mod background;
mod power;
mod sessions;
mod theme;

use app::{App, Config, PreviewFixture};
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
    #[arg(
        long,
        value_enum,
        requires = "windowed",
        num_args = 0..=1,
        default_missing_value = "selected"
    )]
    preview: Option<PreviewFixture>,
}

fn parse_username(value: &str) -> Result<String, String> {
    if !accounts::valid_username(value) {
        Err("username must be 1–256 characters without whitespace or control characters".into())
    } else {
        Ok(value.into())
    }
}

fn parse_display_name(value: &str) -> Result<String, String> {
    accounts::presentation_label(value)
        .ok_or_else(|| "display name must contain visible characters".into())
}

pub fn main() -> iced::Result {
    let arguments = Arguments::parse();
    let windowed = arguments.windowed;
    let config = Config {
        username: arguments.username,
        display_name: arguments.display_name,
        preview: arguments.preview,
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
        assert_eq!(arguments.preview, None);
    }

    #[test]
    fn preview_requires_a_window() {
        assert!(Arguments::try_parse_from(["genkan", "--preview"]).is_err());
        let arguments = Arguments::try_parse_from(["genkan", "--windowed", "--preview"])
            .expect("preview has a safe default fixture");
        assert_eq!(arguments.preview, Some(PreviewFixture::Selected));
        assert!(Arguments::try_parse_from([
            "genkan",
            "--windowed",
            "--preview",
            "--username",
            "preview",
        ])
        .is_ok());
    }

    #[test]
    fn preview_accepts_named_fixtures() {
        let arguments =
            Arguments::try_parse_from(["genkan", "--windowed", "--preview", "users"]).unwrap();
        assert_eq!(arguments.preview, Some(PreviewFixture::Users));
        assert!(
            Arguments::try_parse_from(["genkan", "--windowed", "--preview", "unknown",]).is_err()
        );
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
        assert!(Arguments::try_parse_from([
            "genkan",
            "--username",
            "operator",
            "--display-name",
            "\u{0600}\u{202e}\u{200b}",
        ])
        .is_err());
        assert!(Arguments::try_parse_from(["genkan", "--username", "user\u{0600}"]).is_err());
    }
}
