use clap::ValueEnum;
use genkan_session_lock::PresentationFrame;
use iced::widget::{container, image, Image};
use iced::{Element, Fill, Subscription, Task, Theme};
use std::sync::{Arc, Mutex};
use thiserror::Error;

use crate::conversation::{Conversation, Status};
use crate::wallpaper;

use super::render;

const MAX_PREVIEW_PIXELS: u32 = 4096 * 4096;
type ExitStatus = Arc<Mutex<ExitState>>;

#[derive(Default)]
struct ExitState {
    failure: Option<Error>,
    close_requested: bool,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum Fixture {
    Securing,
    #[default]
    Prompt,
    CapsLock,
    /// The prompt with the built-in Pine picture, decoded by the real
    /// sandboxed decoder (RFD 7).
    AccountPicture,
    Challenge,
    Submitting,
    Failure,
}

struct Preview {
    presentation: PresentationFrame,
    logical_size: iced::Size,
    scale: f32,
    image: image::Handle,
    exit_status: ExitStatus,
}

#[derive(Debug, Clone)]
enum Message {
    Window(iced::window::Event),
}

#[derive(Debug, Error)]
pub(crate) enum Error {
    #[error("lock preview dimensions exceed the renderer resource budget")]
    Dimensions,
    #[error(transparent)]
    Render(#[from] genkan_session_lock::PreviewError),
    #[error(transparent)]
    Iced(#[from] iced::Error),
    #[error("lock preview backend terminated unexpectedly")]
    BackendTerminated,
}

impl Preview {
    fn new(
        presentation: PresentationFrame,
        width: u32,
        height: u32,
        image: image::Handle,
        exit_status: ExitStatus,
    ) -> (Self, Task<Message>) {
        (
            Self {
                presentation,
                logical_size: iced::Size::new(width as f32, height as f32),
                scale: 1.0,
                image,
                exit_status,
            },
            Task::none(),
        )
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        let Message::Window(event) = message;
        if event == iced::window::Event::CloseRequested {
            record_close(&self.exit_status);
            return iced::exit();
        }
        if !apply_window_geometry(&mut self.logical_size, &mut self.scale, &event) {
            return Task::none();
        }
        let Some((width, height)) = physical_dimensions(self.logical_size, self.scale) else {
            record_failure(&self.exit_status, Error::Dimensions);
            return iced::exit();
        };
        match render_frame(&self.presentation, width, height, buffer_scale(self.scale)) {
            Ok(frame) => {
                let (width, height) = frame.dimensions();
                self.image = image::Handle::from_rgba(width, height, frame.into_pixels());
                Task::none()
            }
            Err(error) => {
                record_failure(&self.exit_status, error);
                iced::exit()
            }
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        iced::window::events().map(|(_, event)| Message::Window(event))
    }

    fn view(&self) -> Element<'_, Message> {
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
) -> Result<(), Error> {
    if !valid_dimensions(width, height) {
        return Err(Error::Dimensions);
    }
    let presentation = presentation(settings, fixture);
    let frame = render_frame(&presentation, width, height, 1)?;
    let (frame_width, frame_height) = frame.dimensions();
    let image = image::Handle::from_rgba(frame_width, frame_height, frame.into_pixels());
    let exit_status = Arc::new(Mutex::new(ExitState::default()));
    let application_exit_status = exit_status.clone();

    iced::application(
        move || {
            Preview::new(
                presentation.clone(),
                width,
                height,
                image.clone(),
                application_exit_status.clone(),
            )
        },
        Preview::update,
        Preview::view,
    )
    .title("Genkan Lock Preview")
    .theme(|_: &Preview| Theme::Dark)
    .window(iced::window::Settings {
        size: iced::Size::new(width as f32, height as f32),
        decorations: true,
        resizable: false,
        ..Default::default()
    })
    .exit_on_close_request(false)
    .subscription(Preview::subscription)
    .antialiasing(true)
    .run()
    .map_err(Error::Iced)?;

    finish(exit_status)
}

fn apply_window_geometry(
    logical_size: &mut iced::Size,
    scale: &mut f32,
    event: &iced::window::Event,
) -> bool {
    match event {
        iced::window::Event::Opened { size, .. } | iced::window::Event::Resized(size) => {
            *logical_size = *size;
            true
        }
        iced::window::Event::Rescaled(new_scale) => {
            *scale = *new_scale;
            false
        }
        _ => false,
    }
}

fn record_failure(exit_status: &ExitStatus, error: Error) {
    eprintln!("genkan lock preview: {error}");
    let mut status = exit_status
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if status.failure.is_none() {
        status.failure = Some(error);
    }
}

fn record_close(exit_status: &ExitStatus) {
    exit_status
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .close_requested = true;
}

fn finish(exit_status: ExitStatus) -> Result<(), Error> {
    let mut status = exit_status
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(error) = status.failure.take() {
        Err(error)
    } else if status.close_requested {
        Ok(())
    } else {
        Err(Error::BackendTerminated)
    }
}

fn presentation(settings: wallpaper::Settings, fixture: Fixture) -> PresentationFrame {
    let wallpaper = wallpaper::State::start(settings).rgba_frame();
    let identity = super::identity::Identity {
        uid: 1000,
        username: "preview".into(),
        display_name: "Preview User".into(),
    };
    let (conversation, confirmed) = fixture.conversation();
    let mut renderer = render::Renderer::new();
    if fixture == Fixture::AccountPicture {
        renderer.picture = builtin_picture("pine");
    }
    if fixture == Fixture::CapsLock {
        renderer.keyboard = genkan_session_lock::keyboard::KeyboardStatus {
            caps_lock: true,
            layout: Some("English (US)".into()),
        };
    }
    // The login preview's fixed time keeps both previews deterministic.
    let clock = renderer.clock(crate::app::preview::preview_now());
    let overlay = renderer.identity(&identity, &conversation, confirmed, 0);
    render::frame(wallpaper, Some(clock), Some(overlay), 0.0).with_status(
        renderer
            .status(None)
            .and_then(|frame| genkan_session_lock::AnchoredOverlay::new(frame, 8)),
    )
}

/// A built-in picture decoded as the lock decodes the user's own: from
/// `GENKAN_AVATAR_DIR` in the dev shell, otherwise the packaged set.
fn builtin_picture(name: &str) -> Option<tiny_skia::Pixmap> {
    let executable = std::env::current_exe().ok()?;
    let directory = std::env::var_os("GENKAN_AVATAR_DIR")
        .map(std::path::PathBuf::from)
        .or_else(|| genkan::avatar::builtin_directory(&executable))?;
    let picture =
        genkan::avatar::decode_file(&executable, &directory.join(format!("{name}.png"))).ok()?;
    crate::design::picture::pixmap(&picture)
}

/// Renders the presentation as an authentication output `width`×`height`
/// pixels at the integer buffer `scale` shows it.
fn render_frame(
    presentation: &PresentationFrame,
    width: u32,
    height: u32,
    scale: u32,
) -> Result<genkan_session_lock::RgbaFrame, Error> {
    if !valid_dimensions(width, height) {
        return Err(Error::Dimensions);
    }
    genkan_session_lock::render_preview(presentation, width, height, scale).map_err(Error::Render)
}

/// The integer buffer scale nearest a window's scale factor, as a session
/// lock surface would use.
fn buffer_scale(scale: f32) -> u32 {
    if scale.is_finite() {
        scale.round().clamp(1.0, 16.0) as u32
    } else {
        1
    }
}

fn physical_dimensions(size: iced::Size, scale: f32) -> Option<(u32, u32)> {
    if !scale.is_finite() || scale <= 0.0 {
        return None;
    }
    let width = (size.width * scale).round();
    let height = (size.height * scale).round();
    if width < 1.0 || height < 1.0 || width > u32::MAX as f32 || height > u32::MAX as f32 {
        return None;
    }
    let dimensions = (width as u32, height as u32);
    valid_dimensions(dimensions.0, dimensions.1).then_some(dimensions)
}

fn valid_dimensions(width: u32, height: u32) -> bool {
    width > 0
        && height > 0
        && width
            .checked_mul(height)
            .is_some_and(|pixels| pixels <= MAX_PREVIEW_PIXELS)
}

impl Fixture {
    fn conversation(self) -> (Conversation, bool) {
        match self {
            Self::Securing => (Conversation::new(), false),
            Self::Prompt | Self::CapsLock | Self::AccountPicture => (
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
            Self::Challenge => (
                Conversation::for_preview(
                    "7".into(),
                    format!(
                        "{}\nEnter the following challenge response: 793146",
                        "Step\n".repeat(16)
                    ),
                    None,
                    false,
                    false,
                    Status::Waiting,
                ),
                true,
            ),
            Self::Submitting => (
                Conversation::for_preview(
                    String::new(),
                    "Password".into(),
                    None,
                    false,
                    true,
                    Status::Submitting,
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
        for fixture in [
            Fixture::Securing,
            Fixture::Prompt,
            Fixture::Challenge,
            Fixture::Submitting,
            Fixture::Failure,
        ] {
            let (first, first_confirmed) = fixture.conversation();
            let (second, second_confirmed) = fixture.conversation();
            assert_eq!(first_confirmed, second_confirmed, "fixture {fixture:?}");
            assert_eq!(first.input(), second.input(), "fixture {fixture:?}");
            assert_eq!(first.prompt(), second.prompt(), "fixture {fixture:?}");
            assert_eq!(first.notice(), second.notice(), "fixture {fixture:?}");
            assert_eq!(first.status(), second.status(), "fixture {fixture:?}");
        }
    }

    #[test]
    fn oversized_dimension_pairs_return_an_error_instead_of_panicking() {
        let overlay = genkan_session_lock::RgbaFrame::new(1, 1, vec![0; 4].into()).unwrap();
        let presentation = render::frame(None, None, Some(overlay), 0.0);

        assert!(matches!(
            render_frame(&presentation, 16_384, 16_384, 1),
            Err(Error::Dimensions)
        ));

        assert!(valid_dimensions(4096, 4096));
        assert!(!valid_dimensions(4097, 4096));
        assert_eq!(
            physical_dimensions(iced::Size::new(1280.0, 800.0), 2.0),
            Some((2560, 1600))
        );
        let hidpi = render_frame(&presentation, 4, 2, buffer_scale(2.0)).unwrap();
        assert_eq!(hidpi.dimensions(), (4, 2));
        assert_eq!(buffer_scale(1.25), 1);
        assert_eq!(buffer_scale(f32::NAN), 1);
        assert_eq!(
            physical_dimensions(iced::Size::new(4096.0, 4096.0), 2.0),
            None
        );
    }

    #[test]
    fn scale_change_waits_for_the_matching_logical_resize_before_rasterizing() {
        let mut logical_size = iced::Size::new(3840.0, 2160.0);
        let mut scale = 1.0;

        assert!(!apply_window_geometry(
            &mut logical_size,
            &mut scale,
            &iced::window::Event::Rescaled(2.0)
        ));
        assert_eq!(logical_size, iced::Size::new(3840.0, 2160.0));
        assert_eq!(scale, 2.0);

        assert!(apply_window_geometry(
            &mut logical_size,
            &mut scale,
            &iced::window::Event::Resized(iced::Size::new(1920.0, 1080.0))
        ));
        assert_eq!(physical_dimensions(logical_size, scale), Some((3840, 2160)));
    }

    #[test]
    fn post_open_failures_are_returned_after_the_preview_exits() {
        let exit_status = Arc::new(Mutex::new(ExitState::default()));

        record_failure(&exit_status, Error::Dimensions);

        assert!(matches!(finish(exit_status), Err(Error::Dimensions)));
    }

    #[test]
    fn only_an_explicit_close_is_a_successful_preview_exit() {
        let closed = Arc::new(Mutex::new(ExitState::default()));
        record_close(&closed);
        assert!(finish(closed).is_ok());

        let disconnected = Arc::new(Mutex::new(ExitState::default()));
        assert!(matches!(
            finish(disconnected),
            Err(Error::BackendTerminated)
        ));
    }
}
