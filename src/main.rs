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
    #[arg(long, default_value = "darwin")]
    username: String,
    #[arg(long, default_value = "Darwin")]
    display_name: String,
    #[arg(long)]
    windowed: bool,
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
