use bytes::Bytes;
use clap::ValueEnum;
use cosmic_text::{FontSystem, SwashCache};
use genkan_session_lock::{PresentationFrame, RgbaFrame};
use iced::widget::{container, image, Image};
use iced::{Element, Fill, Task, Theme};

use crate::conversation::{Conversation, Status};
use crate::wallpaper;

use super::{authentication_panel_dimensions, render_authentication_overlay};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum Fixture {
    Securing,
    #[default]
    Prompt,
    Failure,
}

struct Preview {
    image: image::Handle,
}

impl Preview {
    fn new(image: image::Handle) -> (Self, Task<()>) {
        (Self { image }, Task::none())
    }

    fn update(&mut self, (): ()) {}

    fn view(&self) -> Element<'_, ()> {
        container(
            Image::new(self.image.clone())
                .width(Fill)
                .height(Fill)
                .content_fit(iced::ContentFit::Fill),
        )
        .width(Fill)
        .height(Fill)
        .into()
    }
}

pub(crate) fn run(
    settings: wallpaper::Settings,
    fixture: Fixture,
    width: u32,
    height: u32,
) -> iced::Result {
    let frame = frame(settings, fixture, width, height);
    let (frame_width, frame_height) = frame.dimensions();
    let image = image::Handle::from_rgba(
        frame_width,
        frame_height,
        Bytes::copy_from_slice(frame.pixels()),
    );

    iced::application(
        move || Preview::new(image.clone()),
        Preview::update,
        Preview::view,
    )
    .title("Genkan Lock Preview")
    .theme(|_: &Preview| Theme::Dark)
    .window(iced::window::Settings {
        size: iced::Size::new(width as f32, height as f32),
        decorations: true,
        ..Default::default()
    })
    .antialiasing(true)
    .run()
}

fn frame(settings: wallpaper::Settings, fixture: Fixture, width: u32, height: u32) -> RgbaFrame {
    let wallpaper = wallpaper::State::start(settings).rgba_frame();
    let (canvas_width, canvas_height) = wallpaper
        .as_ref()
        .map_or((1280, 800), RgbaFrame::dimensions);
    let (overlay_width, overlay_height) =
        authentication_panel_dimensions(canvas_width, canvas_height);
    let mut pixels = vec![0; (overlay_width * overlay_height * 4) as usize];
    let identity = super::identity::Identity {
        uid: 1000,
        username: "preview".into(),
        display_name: "Preview User".into(),
    };
    let (conversation, confirmed) = fixture.conversation();
    render_authentication_overlay(
        &mut pixels,
        overlay_width,
        overlay_height,
        &identity,
        &conversation,
        confirmed,
        &mut FontSystem::new(),
        &mut SwashCache::new(),
    );
    let overlay = RgbaFrame::new(overlay_width, overlay_height, pixels.into())
        .expect("preview overlay has valid dimensions");
    let presentation = PresentationFrame::new(
        canvas_width,
        canvas_height,
        wallpaper,
        overlay,
        (canvas_width - overlay_width) / 2,
        (canvas_height - overlay_height) / 2,
    )
    .expect("preview presentation has valid dimensions");
    genkan_session_lock::render_preview(&presentation, width, height)
        .expect("CLI dimensions fit the renderer budget")
}

impl Fixture {
    fn conversation(self) -> (Conversation, bool) {
        match self {
            Self::Securing => (Conversation::new(), false),
            Self::Prompt => (
                Conversation::for_preview(
                    "preview".into(),
                    "Password".into(),
                    None,
                    false,
                    true,
                    Status::Waiting,
                ),
                true,
            ),
            Self::Failure => (
                Conversation::for_preview(
                    String::new(),
                    "Password".into(),
                    Some("Authentication failed".into()),
                    true,
                    true,
                    Status::Failed,
                ),
                true,
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixtures_are_deterministic_without_authentication_or_lock_services() {
        for fixture in [Fixture::Securing, Fixture::Prompt, Fixture::Failure] {
            let (first, first_confirmed) = fixture.conversation();
            let (second, second_confirmed) = fixture.conversation();
            assert_eq!(first_confirmed, second_confirmed, "fixture {fixture:?}");
            assert_eq!(first.input(), second.input(), "fixture {fixture:?}");
            assert_eq!(first.prompt(), second.prompt(), "fixture {fixture:?}");
            assert_eq!(first.notice(), second.notice(), "fixture {fixture:?}");
            assert_eq!(first.status(), second.status(), "fixture {fixture:?}");
        }
    }
}
