mod accounts;
mod app;
mod background;
mod power;
mod sessions;
mod theme;
mod wallpaper;

use std::path::PathBuf;

use app::{App, Config, PreviewFixture};
use clap::{Parser, ValueEnum};
use iced::{window, Theme};

#[derive(Debug, Parser)]
#[command(version, about)]
struct Arguments {
    #[arg(long, exclusive = true)]
    list_preview_fixtures: bool,
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
    #[arg(long, requires_all = ["windowed", "height"], value_parser = parse_dimension)]
    width: Option<u32>,
    #[arg(long, requires_all = ["windowed", "width"], value_parser = parse_dimension)]
    height: Option<u32>,
    /// Select one of the packaged animated wallpapers.
    #[arg(long, value_enum, default_value = "tahoe-beach")]
    wallpaper: wallpaper::Catalog,
    /// Replace the selected catalog entry's video with an absolute local MOV file.
    #[arg(long, value_parser = parse_wallpaper_file)]
    wallpaper_file: Option<PathBuf>,
    /// Show the selected poster without starting the video decoder.
    #[arg(
        long,
        visible_alias = "static-wallpaper",
        conflicts_with = "animated_preview"
    )]
    reduce_motion: bool,
    /// Enable real wallpaper playback while keeping preview services simulated.
    #[arg(long, requires = "preview", conflicts_with = "reduce_motion")]
    animated_preview: bool,
}

const DEFAULT_WINDOW_WIDTH: u32 = 1280;
const DEFAULT_WINDOW_HEIGHT: u32 = 800;

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

fn parse_dimension(value: &str) -> Result<u32, String> {
    let dimension = value
        .parse::<u32>()
        .map_err(|_| "window dimensions must be whole numbers".to_owned())?;
    if !(320..=16_384).contains(&dimension) {
        Err("window dimensions must be between 320 and 16384 pixels".into())
    } else {
        Ok(dimension)
    }
}

fn parse_wallpaper_file(value: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        return Err("wallpaper file must be an absolute local path, not a URI or pipeline".into());
    }
    if !path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("mov"))
    {
        return Err("wallpaper file must be a MOV file, not a playlist or pipeline".into());
    }
    if !path.is_file() {
        return Err("wallpaper file must name an existing regular file".into());
    }
    Ok(path)
}

fn animate_wallpaper(preview: bool, reduce_motion: bool, animated_preview: bool) -> bool {
    !reduce_motion && (!preview || animated_preview)
}

pub fn main() -> iced::Result {
    let arguments = Arguments::parse();
    if arguments.list_preview_fixtures {
        for fixture in PreviewFixture::value_variants() {
            println!(
                "{}",
                fixture
                    .to_possible_value()
                    .expect("preview fixture has a clap value")
                    .get_name()
            );
        }
        return Ok(());
    }
    let windowed = arguments.windowed;
    let animate_wallpaper = animate_wallpaper(
        arguments.preview.is_some(),
        arguments.reduce_motion,
        arguments.animated_preview,
    );
    let window_size = iced::Size::new(
        arguments.width.unwrap_or(DEFAULT_WINDOW_WIDTH) as f32,
        arguments.height.unwrap_or(DEFAULT_WINDOW_HEIGHT) as f32,
    );
    let config = Config {
        username: arguments.username,
        display_name: arguments.display_name,
        preview: arguments.preview,
        wallpaper: wallpaper::Settings {
            catalog: arguments.wallpaper,
            override_path: arguments.wallpaper_file,
            animate: animate_wallpaper,
        },
    };

    iced::application("Genkan", App::update, App::view)
        .subscription(App::subscription)
        .theme(|_| Theme::Dark)
        .window(window::Settings {
            size: window_size,
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
        assert!(!arguments.list_preview_fixtures);
        assert_eq!(arguments.username, None);
        assert_eq!(arguments.display_name, None);
        assert_eq!(arguments.preview, None);
        assert_eq!(arguments.width, None);
        assert_eq!(arguments.height, None);
        assert_eq!(arguments.wallpaper, wallpaper::Catalog::TahoeBeach);
        assert_eq!(arguments.wallpaper_file, None);
        assert!(!arguments.reduce_motion);
        assert!(!arguments.animated_preview);
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
    fn fixture_listing_is_exclusive_and_exhaustive() {
        let arguments = Arguments::try_parse_from(["genkan", "--list-preview-fixtures"]).unwrap();
        assert!(arguments.list_preview_fixtures);
        assert!(
            Arguments::try_parse_from(["genkan", "--list-preview-fixtures", "--windowed",])
                .is_err()
        );

        let names = PreviewFixture::value_variants()
            .iter()
            .map(|fixture| {
                fixture
                    .to_possible_value()
                    .expect("preview fixture has a clap value")
                    .get_name()
                    .to_owned()
            })
            .collect::<Vec<_>>();
        assert_eq!(names.len(), 17);
        assert!(names.contains(&"selected".to_owned()));
        assert!(names.contains(&"power-confirmation".to_owned()));
    }

    #[test]
    fn window_dimensions_are_paired_bounded_and_windowed() {
        assert!(
            Arguments::try_parse_from(["genkan", "--width", "480", "--height", "600"]).is_err()
        );
        assert!(Arguments::try_parse_from(["genkan", "--windowed", "--width", "480"]).is_err());
        let arguments = Arguments::try_parse_from([
            "genkan",
            "--windowed",
            "--preview",
            "long-authentication",
            "--width",
            "480",
            "--height",
            "600",
        ])
        .unwrap();
        assert_eq!(arguments.width, Some(480));
        assert_eq!(arguments.height, Some(600));
        assert!(Arguments::try_parse_from([
            "genkan",
            "--windowed",
            "--width",
            "200",
            "--height",
            "600",
        ])
        .is_err());
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

    #[test]
    fn wallpaper_catalog_and_motion_flags_are_bounded() {
        for name in [
            "tahoe-beach",
            "sequoia-sunrise",
            "sequoia-morning",
            "sequoia-night",
        ] {
            assert!(Arguments::try_parse_from(["genkan", "--wallpaper", name]).is_ok());
        }
        assert!(Arguments::try_parse_from(["genkan", "--wallpaper", "unknown"]).is_err());
        assert!(Arguments::try_parse_from(["genkan", "--animated-preview"]).is_err());
        assert!(Arguments::try_parse_from([
            "genkan",
            "--windowed",
            "--preview",
            "--animated-preview",
            "--reduce-motion",
        ])
        .is_err());
        assert!(animate_wallpaper(false, false, false));
        assert!(!animate_wallpaper(true, false, false));
        assert!(animate_wallpaper(true, false, true));
        assert!(!animate_wallpaper(false, true, false));
    }

    #[test]
    fn wallpaper_override_accepts_only_an_existing_absolute_mov() {
        let path =
            std::env::temp_dir().join(format!("genkan-wallpaper-{}.mov", std::process::id()));
        std::fs::write(&path, []).unwrap();
        let parsed =
            Arguments::try_parse_from(["genkan", "--wallpaper-file", path.to_str().unwrap()]);
        std::fs::remove_file(&path).unwrap();

        assert_eq!(parsed.unwrap().wallpaper_file, Some(path));
        for invalid in [
            "wallpaper.mov",
            "https://example.test/wallpaper.mov",
            "/tmp/wallpaper.m3u8",
            "videotestsrc ! appsink",
            "/does/not/exist.mov",
        ] {
            assert!(
                Arguments::try_parse_from(["genkan", "--wallpaper-file", invalid]).is_err(),
                "accepted {invalid}"
            );
        }
    }
}
