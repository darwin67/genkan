mod accounts;
mod app;
mod background;
mod conversation;
mod locker;
mod power;
mod sessions;
mod theme;
mod wallpaper;

use std::path::PathBuf;

use app::{App, Config, PreviewFixture};
use clap::{Args, Parser, Subcommand, ValueEnum};
use iced::{window, Theme};

#[derive(Debug, Parser)]
#[command(version, about, arg_required_else_help = true)]
struct Arguments {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the greetd login frontend.
    Login(LoginArguments),
    /// Securely cover and lock the current Wayland session.
    Lock(LockArguments),
}

#[derive(Debug, Args)]
#[command(
    after_help = "Wallpaper playback failures restore the selected static poster. If the poster is unavailable, Genkan uses its generated background."
)]
struct LoginArguments {
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

#[derive(Debug, Args)]
#[command(
    after_help = "The lock is ready only after the compositor confirms ext-session-lock-v1 ownership. Unlock authentication uses the host's genkan-lock PAM service for the invoking real-UID account."
)]
struct LockArguments {
    /// Descriptor that receives `READY` after compositor lock confirmation.
    #[arg(long, value_parser = parse_ready_fd)]
    ready_fd: Option<std::os::fd::RawFd>,
    /// Select one of the packaged animated wallpapers.
    #[arg(long, value_enum, default_value = "tahoe-beach")]
    wallpaper: wallpaper::Catalog,
    /// Replace the selected catalog entry's video with an absolute local MOV file.
    #[arg(long, value_parser = parse_wallpaper_file)]
    wallpaper_file: Option<PathBuf>,
    /// Show the selected poster without starting the video decoder.
    #[arg(long, visible_alias = "static-wallpaper")]
    reduce_motion: bool,
    #[cfg(feature = "lock-test")]
    #[arg(long, hide = true)]
    test_unlock_after_ready: bool,
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

fn parse_ready_fd(value: &str) -> Result<std::os::fd::RawFd, String> {
    let fd = value
        .parse::<std::os::fd::RawFd>()
        .map_err(|_| "ready descriptor must be a non-negative integer".to_owned())?;
    if fd <= 2 {
        Err("ready descriptor must not replace stdin, stdout, or stderr".into())
    } else {
        Ok(fd)
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

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    match Arguments::parse().command {
        Command::Login(arguments) => run_login(arguments)?,
        Command::Lock(arguments) => run_lock(arguments)?,
    }
    Ok(())
}

fn run_login(arguments: LoginArguments) -> iced::Result {
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

    iced::application(move || App::new(config.clone()), App::update, App::view)
        .title("Genkan")
        .subscription(App::subscription)
        .theme(|_: &App| Theme::Dark)
        .window(window::Settings {
            size: window_size,
            decorations: windowed,
            ..Default::default()
        })
        .exit_on_close_request(false)
        .antialiasing(true)
        .run()
}

fn lock_config(arguments: LockArguments) -> locker::Config {
    locker::Config {
        wallpaper: wallpaper::Settings {
            catalog: arguments.wallpaper,
            override_path: arguments.wallpaper_file,
            animate: !arguments.reduce_motion,
        },
        ready_fd: arguments.ready_fd,
        #[cfg(feature = "lock-test")]
        test_unlock_after_ready: arguments.test_unlock_after_ready,
    }
}

fn run_lock(arguments: LockArguments) -> Result<(), Box<dyn std::error::Error>> {
    locker::run(lock_config(arguments))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{error::ErrorKind, CommandFactory};

    fn try_parse_login<const N: usize>(
        arguments: [&str; N],
    ) -> Result<LoginArguments, clap::Error> {
        let arguments = ["genkan", "login"]
            .into_iter()
            .chain(arguments.into_iter().skip(1));
        let parsed = Arguments::try_parse_from(arguments)?;
        match parsed.command {
            Command::Login(arguments) => Ok(arguments),
            Command::Lock(_) => unreachable!("the helper always selects login"),
        }
    }

    fn try_parse_lock<const N: usize>(arguments: [&str; N]) -> Result<LockArguments, clap::Error> {
        let arguments = ["genkan", "lock"]
            .into_iter()
            .chain(arguments.into_iter().skip(1));
        let parsed = Arguments::try_parse_from(arguments)?;
        match parsed.command {
            Command::Lock(arguments) => Ok(arguments),
            Command::Login(_) => unreachable!("the helper always selects lock"),
        }
    }

    #[test]
    fn command_is_required_and_login_options_are_scoped() {
        assert_eq!(
            Arguments::try_parse_from(["genkan"]).unwrap_err().kind(),
            ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
        );
        assert!(Arguments::try_parse_from(["genkan", "--windowed"]).is_err());
        assert!(Arguments::try_parse_from(["genkan", "login", "--windowed"]).is_ok());
        assert!(Arguments::try_parse_from(["genkan", "lock"]).is_ok());
        assert!(Arguments::try_parse_from(["genkan", "lock", "--username", "alice"]).is_err());
    }

    #[test]
    fn lock_help_describes_real_uid_pam_authentication() {
        use clap::CommandFactory;

        let mut command = Arguments::command();
        let lock = command
            .find_subcommand_mut("lock")
            .expect("lock subcommand");
        let mut help = Vec::new();
        lock.write_long_help(&mut help).unwrap();
        let help = String::from_utf8(help).unwrap();

        assert!(help.contains("genkan-lock PAM service"));
        assert!(help.contains("real-UID account"));
        assert!(!help.contains("does not yet include production authentication"));
    }

    #[test]
    fn lock_options_are_typed_and_scoped_to_the_lock_command() {
        let arguments = try_parse_lock(["genkan", "--ready-fd", "7", "--reduce-motion"])
            .expect("valid lock configuration");
        assert_eq!(arguments.ready_fd, Some(7));
        assert!(arguments.reduce_motion);
        assert!(try_parse_lock(["genkan", "--ready-fd", "1"]).is_err());
        assert!(try_parse_lock(["genkan", "--ready-fd", "not-an-fd"]).is_err());
        assert!(Arguments::try_parse_from(["genkan", "login", "--ready-fd", "7"]).is_err());
        #[cfg(not(feature = "lock-test"))]
        assert!(
            Arguments::try_parse_from(["genkan", "lock", "--test-unlock-after-ready"]).is_err()
        );
    }

    #[test]
    fn identity_overrides_are_optional() {
        let arguments = try_parse_login(["genkan"]).unwrap();
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
        assert!(try_parse_login(["genkan", "--preview"]).is_err());
        let arguments = try_parse_login(["genkan", "--windowed", "--preview"])
            .expect("preview has a safe default fixture");
        assert_eq!(arguments.preview, Some(PreviewFixture::Selected));
        assert!(
            try_parse_login(["genkan", "--windowed", "--preview", "--username", "preview",])
                .is_ok()
        );
    }

    #[test]
    fn preview_accepts_named_fixtures() {
        let arguments = try_parse_login(["genkan", "--windowed", "--preview", "users"]).unwrap();
        assert_eq!(arguments.preview, Some(PreviewFixture::Users));
        assert!(try_parse_login(["genkan", "--windowed", "--preview", "unknown",]).is_err());
    }

    #[test]
    fn fixture_listing_is_exclusive_and_exhaustive() {
        let arguments = try_parse_login(["genkan", "--list-preview-fixtures"]).unwrap();
        assert!(arguments.list_preview_fixtures);
        assert!(try_parse_login(["genkan", "--list-preview-fixtures", "--windowed",]).is_err());

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
        assert!(try_parse_login(["genkan", "--width", "480", "--height", "600"]).is_err());
        assert!(try_parse_login(["genkan", "--windowed", "--width", "480"]).is_err());
        let arguments = try_parse_login([
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
        assert!(
            try_parse_login(["genkan", "--windowed", "--width", "200", "--height", "600",])
                .is_err()
        );
    }

    #[test]
    fn display_name_override_requires_username() {
        assert!(try_parse_login(["genkan", "--display-name", "Operator"]).is_err());
    }

    #[test]
    fn identity_overrides_reject_empty_or_malformed_values() {
        assert!(try_parse_login(["genkan", "--username", ""]).is_err());
        assert!(try_parse_login(["genkan", "--username", "two users"]).is_err());
        assert!(
            try_parse_login(["genkan", "--username", "operator", "--display-name", " \n ",])
                .is_err()
        );
        assert!(try_parse_login([
            "genkan",
            "--username",
            "operator",
            "--display-name",
            "\u{0600}\u{202e}\u{200b}",
        ])
        .is_err());
        assert!(try_parse_login(["genkan", "--username", "user\u{0600}"]).is_err());
    }

    #[test]
    fn wallpaper_catalog_and_motion_flags_are_bounded() {
        for name in [
            "tahoe-beach",
            "sequoia-sunrise",
            "sequoia-morning",
            "sequoia-night",
        ] {
            assert!(try_parse_login(["genkan", "--wallpaper", name]).is_ok());
        }
        assert!(try_parse_login(["genkan", "--wallpaper", "unknown"]).is_err());
        assert!(try_parse_login(["genkan", "--animated-preview"]).is_err());
        assert!(try_parse_login([
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
    fn command_help_describes_wallpaper_fallbacks() {
        let help = Arguments::command()
            .find_subcommand_mut("login")
            .expect("login subcommand")
            .render_long_help()
            .to_string();
        assert!(help.contains("playback failures restore the selected static poster"));
        assert!(help.contains("poster is unavailable"));
        assert!(help.contains("generated background"));
    }

    #[test]
    fn wallpaper_override_accepts_only_an_existing_absolute_mov() {
        let path =
            std::env::temp_dir().join(format!("genkan-wallpaper-{}.mov", std::process::id()));
        std::fs::write(&path, []).unwrap();
        let parsed = try_parse_login(["genkan", "--wallpaper-file", path.to_str().unwrap()]);
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
                try_parse_login(["genkan", "--wallpaper-file", invalid]).is_err(),
                "accepted {invalid}"
            );
        }
    }
}
