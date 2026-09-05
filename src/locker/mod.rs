mod auth;
mod coordination;
mod identity;
mod launcher;

use std::ffi::CString;
#[cfg(feature = "lock-test")]
use std::fs::File;
#[cfg(feature = "lock-test")]
use std::io::Write;
use std::os::fd::{FromRawFd, OwnedFd, RawFd};
use std::path::Path;

use bytes::Bytes;
use cosmic_text::{Align, Attrs, Buffer, Color, FontSystem, Metrics, Shaping, SwashCache};
use genkan_session_lock::{Input, Presentation, PresentationFrame, Refresh, RgbaFrame};
use thiserror::Error;
use zeroize::Zeroize;

use crate::conversation::{Conversation, Effect, Response, Status};
use crate::wallpaper;

const MAX_AUTH_EVENTS_PER_REFRESH: usize = 32;
#[cfg(test)]
static PROCESS_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[derive(Debug, Clone)]
pub(crate) struct Config {
    pub(crate) wallpaper: wallpaper::Settings,
    pub(crate) ready_fd: Option<RawFd>,
    #[cfg(feature = "lock-test")]
    pub(crate) test_unlock_after_ready: bool,
    #[cfg(feature = "lock-test")]
    pub(crate) test_observer_fd: Option<RawFd>,
    #[cfg(feature = "lock-test")]
    pub(crate) test_panic_after_ready: bool,
    #[cfg(feature = "lock-test")]
    pub(crate) test_renderer_failure_after_ready: bool,
    #[cfg(feature = "lock-test")]
    pub(crate) test_worker_failure_after_ready: bool,
    #[cfg(feature = "lock-test")]
    pub(crate) test_ready_delay_ms: Option<u64>,
}

#[derive(Debug, Error)]
pub(crate) enum Error {
    #[error("could not resolve lock identity: {0}")]
    Identity(#[from] identity::Error),
    #[error("could not adopt readiness descriptor: {0}")]
    ReadyFd(#[source] std::io::Error),
    #[cfg(feature = "lock-test")]
    #[error("could not duplicate test observer descriptor: {0}")]
    ObserverFd(#[source] std::io::Error),
    #[error(transparent)]
    Coordination(#[from] coordination::Error),
    #[error(transparent)]
    Launcher(#[from] launcher::Error),
    #[error(transparent)]
    Authentication(#[from] auth::Error),
    #[error(transparent)]
    Runtime(#[from] genkan_session_lock::Error),
}

struct LockerPresentation {
    wallpaper: wallpaper::State,
    identity: identity::Identity,
    conversation: Conversation,
    auth: Option<(crate::conversation::Attempt, auth::Client)>,
    prompt_id: Option<u64>,
    confirmed: bool,
    authorized: bool,
    background: Option<RgbaFrame>,
    frame: Option<PresentationFrame>,
    fonts: FontSystem,
    glyphs: SwashCache,
    #[cfg(feature = "lock-test")]
    fail_worker_after_ready: bool,
    #[cfg(feature = "lock-test")]
    test_observer: Option<File>,
}

#[cfg(feature = "lock-test")]
#[derive(Clone, Copy)]
enum AuthTestEvent {
    Prompt,
    Retry,
    Success,
    Failure,
}

#[cfg(feature = "lock-test")]
impl AuthTestEvent {
    fn name(self) -> &'static str {
        match self {
            Self::Prompt => "AUTH_PROMPT",
            Self::Retry => "AUTH_RETRY",
            Self::Success => "AUTH_SUCCESS",
            Self::Failure => "AUTH_FAILURE",
        }
    }
}

impl LockerPresentation {
    fn new(
        identity: identity::Identity,
        auth: auth::Client,
        wallpaper: wallpaper::Settings,
        #[cfg(feature = "lock-test")] fail_worker_after_ready: bool,
        #[cfg(feature = "lock-test")] test_observer: Option<OwnedFd>,
    ) -> Self {
        let conversation = Conversation::new();
        let attempt = conversation.attempt();
        let mut presentation = Self {
            wallpaper: wallpaper::State::start(wallpaper),
            identity,
            conversation,
            auth: Some((attempt, auth)),
            prompt_id: None,
            confirmed: false,
            authorized: false,
            background: None,
            frame: None,
            fonts: FontSystem::new(),
            glyphs: SwashCache::new(),
            #[cfg(feature = "lock-test")]
            fail_worker_after_ready,
            #[cfg(feature = "lock-test")]
            test_observer: test_observer.map(File::from),
        };
        presentation.rebuild();
        presentation
    }

    fn receive_auth(&mut self) -> bool {
        if !self.confirmed {
            return false;
        }
        let mut changed = false;
        for _ in 0..MAX_AUTH_EVENTS_PER_REFRESH {
            let event = self
                .auth
                .as_ref()
                .and_then(|(_, client)| client.try_receive());
            let Some(event) = event else { break };
            changed = true;
            let attempt = self
                .auth
                .as_ref()
                .map(|(attempt, _)| *attempt)
                .expect("active client");
            if !self.conversation.accepts(attempt) {
                continue;
            }
            match event {
                auth::Event::Prompt { id, secret, text } => {
                    self.prompt_id = Some(id);
                    let _ = self.conversation.receive(
                        attempt,
                        Response::Prompt {
                            secret,
                            message: text,
                        },
                    );
                    #[cfg(feature = "lock-test")]
                    self.record_test_event(AuthTestEvent::Prompt);
                }
                auth::Event::Notice { error, text } => {
                    let _ = self.conversation.receive(
                        attempt,
                        Response::Notice {
                            error,
                            message: text,
                        },
                    );
                }
                auth::Event::Success => {
                    self.prompt_id = None;
                    if self.conversation.receive(attempt, Response::Success)
                        == Some(Effect::Authenticated)
                    {
                        self.authorized = true;
                        #[cfg(feature = "lock-test")]
                        self.record_test_event(AuthTestEvent::Success);
                    }
                }
                auth::Event::Failure => {
                    self.prompt_id = None;
                    let _ = self
                        .conversation
                        .receive(attempt, Response::Failure("Authentication failed".into()));
                    #[cfg(feature = "lock-test")]
                    self.record_test_event(AuthTestEvent::Failure);
                }
            }
        }
        changed
    }

    fn retry(&mut self) -> bool {
        #[cfg(feature = "lock-test")]
        self.record_test_event(AuthTestEvent::Retry);
        let next = self.conversation.begin_attempt();
        if let Some((attempt, client)) = self.auth.as_mut() {
            if client.retry().is_ok() {
                *attempt = next;
                self.prompt_id = None;
                return true;
            }
        }
        self.replace_worker(next);
        self.prompt_id = None;
        true
    }

    fn cancel_attempt(&mut self) -> bool {
        let Some((_, mut client)) = self.auth.take() else {
            return false;
        };
        let next = self.conversation.begin_attempt();
        client.cancel();
        self.replace_worker(next);
        self.prompt_id = None;
        true
    }

    fn replace_worker(&mut self, attempt: crate::conversation::Attempt) {
        if let Some((_, mut client)) = self.auth.take() {
            client.cancel();
        }
        match auth::Client::start(&self.identity) {
            Ok(mut replacement) => match replacement.begin() {
                Ok(()) => self.auth = Some((attempt, replacement)),
                Err(_) => {
                    replacement.cancel();
                    self.fail_authentication();
                }
            },
            Err(_) => self.fail_authentication(),
        }
    }

    fn fail_authentication(&mut self) {
        self.conversation
            .fail("Authentication worker unavailable".into());
        #[cfg(feature = "lock-test")]
        self.record_test_event(AuthTestEvent::Failure);
    }

    fn rebuild(&mut self) {
        const WIDTH: u32 = 1280;
        const HEIGHT: u32 = 800;
        self.background = self.wallpaper.rgba_frame();
        let (width, height) = self
            .background
            .as_ref()
            .map_or((WIDTH, HEIGHT), RgbaFrame::dimensions);
        self.rebuild_overlay(width, height);
    }

    fn rebuild_overlay(&mut self, width: u32, height: u32) {
        let (panel_width, panel_height) = authentication_panel_dimensions(width, height);
        let mut pixels = vec![0; (panel_width * panel_height * 4) as usize];
        render_authentication_overlay(
            &mut pixels,
            panel_width,
            panel_height,
            &self.identity,
            &self.conversation,
            self.confirmed,
            &mut self.fonts,
            &mut self.glyphs,
        );
        let overlay = RgbaFrame::new(panel_width, panel_height, Bytes::from(pixels))
            .expect("authentication overlay has valid dimensions");
        self.frame = PresentationFrame::new(
            width,
            height,
            self.background.clone(),
            overlay,
            (width - panel_width) / 2,
            (height - panel_height) / 2,
        );
    }

    #[cfg(feature = "lock-test")]
    fn record_test_event(&mut self, event: AuthTestEvent) {
        if let Some(observer) = self.test_observer.as_mut() {
            let _ = writeln!(observer, "{}", event.name());
            let _ = observer.flush();
        }
    }
}

impl Presentation for LockerPresentation {
    fn receive_latest(&mut self) -> Refresh {
        let auth_changed = self.receive_auth();
        let wallpaper = self.wallpaper.receive_latest();
        if wallpaper != wallpaper::Refresh::Unchanged {
            self.rebuild();
        } else if auth_changed {
            let (width, height) = self
                .frame
                .as_ref()
                .map_or((1280, 800), PresentationFrame::dimensions);
            self.rebuild_overlay(width, height);
        }
        match wallpaper {
            wallpaper::Refresh::Failed => Refresh::Failed,
            wallpaper::Refresh::Frame => Refresh::Frame,
            wallpaper::Refresh::Unchanged if auth_changed => Refresh::Frame,
            wallpaper::Refresh::Unchanged => Refresh::Unchanged,
        }
    }

    fn frame(&self) -> Option<PresentationFrame> {
        self.frame.clone()
    }

    fn lock_confirmed(&mut self) {
        self.confirmed = true;
        #[cfg(feature = "lock-test")]
        if self.fail_worker_after_ready {
            self.fail_worker_after_ready = false;
            if let Some((_, client)) = self.auth.as_mut() {
                client.cancel();
            }
        }
        if self
            .auth
            .as_mut()
            .is_none_or(|(_, client)| client.begin().is_err())
        {
            self.fail_authentication();
        }
        let (width, height) = self
            .frame
            .as_ref()
            .map_or((1280, 800), PresentationFrame::dimensions);
        self.rebuild_overlay(width, height);
    }

    fn input(&mut self, input: Input) -> bool {
        if !authentication_input_enabled(self.confirmed) {
            return false;
        }
        let changed = match input {
            Input::Text(mut text) => {
                let changed = self.conversation.push_input(&text);
                text.zeroize();
                changed
            }
            Input::Backspace => self.conversation.pop_input(),
            Input::Submit if self.conversation.status() == Status::Failed => self.retry(),
            Input::Submit => {
                let Some(id) = self.prompt_id.take() else {
                    return false;
                };
                let Some((attempt, response)) = self.conversation.submit() else {
                    return false;
                };
                let Some((active_attempt, client)) = self.auth.as_mut() else {
                    return false;
                };
                if *active_attempt != attempt || client.respond(id, response).is_err() {
                    self.fail_authentication();
                }
                true
            }
            Input::Cancel => self.cancel_attempt(),
        };
        if changed {
            let (width, height) = self
                .frame
                .as_ref()
                .map_or((1280, 800), PresentationFrame::dimensions);
            self.rebuild_overlay(width, height);
        }
        changed
    }

    fn take_authorization(&mut self) -> bool {
        std::mem::take(&mut self.authorized)
    }
}

fn authentication_input_enabled(confirmed: bool) -> bool {
    confirmed
}

pub(crate) fn run(config: Config) -> Result<(), Error> {
    let ready_fd = config.ready_fd.map(adopt_ready_fd).transpose()?;
    #[cfg(feature = "lock-test")]
    let observer_fd = config.test_observer_fd.map(adopt_ready_fd).transpose()?;
    #[cfg(feature = "lock-test")]
    let presentation_observer = observer_fd
        .as_ref()
        .map(OwnedFd::try_clone)
        .transpose()
        .map_err(Error::ObserverFd)?;
    let coordination = coordination::enter()?;
    let coordination::Entry::Owner {
        coordination: owner,
        wayland,
    } = coordination
    else {
        report_joined_ready(ready_fd)?;
        return Ok(());
    };
    let identity = identity::Identity::current()?;
    // Start the worker before wallpaper decoding or Wayland can create threads.
    let authentication = auth::Client::start(&identity)?;
    let (coordination_ready, _coordination) = owner.activate()?;
    let runtime_identity = genkan_session_lock::Identity::new(
        identity.uid,
        identity.username.clone(),
        identity.display_name.clone(),
    );
    let presentation = LockerPresentation::new(
        identity,
        authentication,
        config.wallpaper,
        #[cfg(feature = "lock-test")]
        config.test_worker_failure_after_ready,
        #[cfg(feature = "lock-test")]
        presentation_observer,
    );
    let runtime =
        genkan_session_lock::Config::new(wayland, runtime_identity, presentation, ready_fd)
            .with_additional_ready_fd(coordination_ready);
    #[cfg(feature = "lock-test")]
    let runtime = runtime
        .with_test_unlock_after_ready(config.test_unlock_after_ready)
        .with_test_observer(observer_fd)
        .with_test_panic_after_ready(config.test_panic_after_ready)
        .with_test_renderer_failure_after_ready(config.test_renderer_failure_after_ready)
        .with_test_ready_delay(std::time::Duration::from_millis(
            config.test_ready_delay_ms.unwrap_or_default(),
        ));
    genkan_session_lock::run(runtime)?;
    Ok(())
}

pub(crate) fn daemonize(executable: &Path, arguments: &[CString]) -> Result<(), Error> {
    launcher::launch(executable, arguments)?;
    Ok(())
}

fn report_joined_ready(ready_fd: Option<OwnedFd>) -> Result<(), Error> {
    if let Some(ready_fd) = ready_fd {
        let mut ready = std::fs::File::from(ready_fd);
        use std::io::Write;
        ready.write_all(b"READY\n").map_err(Error::ReadyFd)?;
        ready.flush().map_err(Error::ReadyFd)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn render_authentication_overlay(
    pixels: &mut [u8],
    width: u32,
    height: u32,
    identity: &identity::Identity,
    conversation: &Conversation,
    confirmed: bool,
    fonts: &mut FontSystem,
    glyphs: &mut SwashCache,
) {
    blend_rect(pixels, width, 0, 0, width, height, [7, 10, 24, 210]);
    draw_text(
        pixels,
        width,
        height,
        fonts,
        glyphs,
        &identity.display_name,
        30.0,
        48,
        [255, 255, 255, 255],
    );
    draw_text(
        pixels,
        width,
        height,
        fonts,
        glyphs,
        &format!("@{}", identity.username),
        16.0,
        88,
        [190, 196, 220, 255],
    );
    let prompt = if confirmed {
        conversation.prompt()
    } else {
        "Securing session…"
    };
    draw_text(
        pixels,
        width,
        height,
        fonts,
        glyphs,
        prompt,
        17.0,
        140,
        [255, 255, 255, 255],
    );
    if confirmed {
        // Treat visible PAM responses as credentials too. Keeping all mutable
        // responses out of the text renderer guarantees its internal scratch
        // buffers never retain application-owned response text.
        let input = "•".repeat(conversation.input().chars().count());
        let (input_margin, input_width) = input_layout(width);
        blend_rect(
            pixels,
            width,
            input_margin,
            172,
            input_width,
            52,
            [7, 10, 24, 220],
        );
        draw_text(
            pixels,
            width,
            height,
            fonts,
            glyphs,
            &input,
            21.0,
            185,
            [255, 255, 255, 255],
        );
        if let Some(notice) = conversation.notice() {
            draw_text(
                pixels,
                width,
                height,
                fonts,
                glyphs,
                notice,
                15.0,
                250,
                if conversation.notice_is_error() {
                    [255, 171, 171, 255]
                } else {
                    [220, 224, 240, 255]
                },
            );
        }
    }
}

fn authentication_panel_dimensions(width: u32, height: u32) -> (u32, u32) {
    (width.min(620), height.min(340))
}

fn input_layout(panel_width: u32) -> (u32, u32) {
    let margin = (panel_width / 9).min(70);
    (margin, panel_width.saturating_sub(margin.saturating_mul(2)))
}

fn blend_rect(
    pixels: &mut [u8],
    stride: u32,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    color: [u8; 4],
) {
    for row in y..y.saturating_add(height) {
        for column in x..x.saturating_add(width) {
            blend_pixel(pixels, stride, column as i32, row as i32, color);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_text(
    pixels: &mut [u8],
    width: u32,
    height: u32,
    fonts: &mut FontSystem,
    glyphs: &mut SwashCache,
    text: &str,
    size: f32,
    y: i32,
    color: [u8; 4],
) {
    let mut buffer = Buffer::new(fonts, Metrics::new(size, size * 1.3));
    buffer.set_size(
        fonts,
        Some(width as f32),
        Some((height as i32 - y).max(0) as f32),
    );
    buffer.set_text(
        fonts,
        text,
        &Attrs::new(),
        Shaping::Advanced,
        Some(Align::Center),
    );
    buffer.shape_until_scroll(fonts, true);
    buffer.draw(
        fonts,
        glyphs,
        Color::rgba(color[0], color[1], color[2], color[3]),
        |x, glyph_y, _, _, glyph_color| {
            blend_pixel(pixels, width, x, y + glyph_y, glyph_color.as_rgba());
        },
    );
}

fn blend_pixel(pixels: &mut [u8], width: u32, x: i32, y: i32, source: [u8; 4]) {
    if x < 0 || y < 0 || x >= width as i32 {
        return;
    }
    let Some(index) = (y as usize)
        .checked_mul(width as usize)
        .and_then(|row| row.checked_add(x as usize))
        .and_then(|pixel| pixel.checked_mul(4))
    else {
        return;
    };
    let Some(target) = pixels.get_mut(index..index + 4) else {
        return;
    };
    let source_alpha = u32::from(source[3]);
    let target_alpha = u32::from(target[3]);
    let output_alpha = source_alpha + target_alpha * (255 - source_alpha) / 255;
    if output_alpha == 0 {
        return;
    }
    for channel in 0..3 {
        let premultiplied = u32::from(source[channel]) * source_alpha
            + u32::from(target[channel]) * target_alpha * (255 - source_alpha) / 255;
        target[channel] = (premultiplied / output_alpha) as u8;
    }
    target[3] = output_alpha as u8;
}

fn adopt_ready_fd(fd: RawFd) -> Result<OwnedFd, Error> {
    // SAFETY: `fcntl` accepts an integer descriptor and reports EBADF without
    // assuming ownership. A non-negative result is a new owned descriptor.
    let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
    if duplicate < 0 {
        return Err(Error::ReadyFd(std::io::Error::last_os_error()));
    }
    // SAFETY: `duplicate` is open and F_GETFL only inspects its status flags.
    let flags = unsafe { libc::fcntl(duplicate, libc::F_GETFL) };
    let access_mode = flags & libc::O_ACCMODE;
    if flags < 0
        || flags & libc::O_PATH != 0
        || !matches!(access_mode, libc::O_WRONLY | libc::O_RDWR)
    {
        let error = if flags < 0 {
            std::io::Error::last_os_error()
        } else {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "readiness descriptor is not writable",
            )
        };
        // The inherited descriptor was transferred to this function once its
        // successful duplication proved it was open. Close both copies.
        // SAFETY: both descriptors are open and uniquely owned here.
        unsafe {
            libc::close(fd);
            libc::close(duplicate);
        }
        return Err(Error::ReadyFd(error));
    }
    // The CLI's inherited descriptor is explicitly transferred to Genkan.
    // Close it after successful duplication so only the typed owner remains.
    // SAFETY: successful duplication proves `fd` was open at this boundary.
    let close_result = unsafe { libc::close(fd) };
    if close_result != 0 {
        // SAFETY: `duplicate` is uniquely owned and converted exactly once.
        drop(unsafe { OwnedFd::from_raw_fd(duplicate) });
        return Err(Error::ReadyFd(std::io::Error::last_os_error()));
    }
    // SAFETY: `duplicate` is a fresh descriptor uniquely owned by this call.
    Ok(unsafe { OwnedFd::from_raw_fd(duplicate) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;
    use std::fs::File;
    use std::io::{Read, Write};
    use std::os::fd::{AsRawFd, IntoRawFd};
    use std::os::unix::net::UnixStream;

    #[test]
    fn invalid_inherited_readiness_descriptor_is_rejected() {
        assert!(matches!(adopt_ready_fd(1_000_000), Err(Error::ReadyFd(_))));
    }

    #[test]
    fn inherited_readiness_descriptor_is_transferred_to_typed_ownership() {
        let _guard = PROCESS_TEST_LOCK.lock().unwrap();
        let (mut reader, writer) = UnixStream::pair().unwrap();
        let original = writer.into_raw_fd();

        let owned = adopt_ready_fd(original).unwrap();
        // SAFETY: F_GETFD only inspects the descriptor integer.
        assert_eq!(unsafe { libc::fcntl(original, libc::F_GETFD) }, -1);
        assert_ne!(owned.as_raw_fd(), original);

        let mut writer = File::from(owned);
        writer.write_all(b"READY\n").unwrap();
        drop(writer);
        let mut message = String::new();
        reader.read_to_string(&mut message).unwrap();
        assert_eq!(message, "READY\n");
    }

    #[test]
    fn read_only_readiness_descriptors_are_rejected_and_closed() {
        let _guard = PROCESS_TEST_LOCK.lock().unwrap();
        let original = File::open("/dev/null").unwrap().into_raw_fd();

        assert!(matches!(adopt_ready_fd(original), Err(Error::ReadyFd(_))));
        // SAFETY: F_GETFD only inspects the descriptor integer.
        assert_eq!(unsafe { libc::fcntl(original, libc::F_GETFD) }, -1);

        let mut pipe = [0; 2];
        // SAFETY: `pipe` points to storage for both returned descriptors.
        assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
        // SAFETY: the write end is not transferred to the function under test.
        unsafe { libc::close(pipe[1]) };
        assert!(matches!(adopt_ready_fd(pipe[0]), Err(Error::ReadyFd(_))));
        // SAFETY: F_GETFD only inspects the descriptor integer.
        assert_eq!(unsafe { libc::fcntl(pipe[0], libc::F_GETFD) }, -1);

        let path = CString::new("/dev/null").unwrap();
        // SAFETY: `path` is a valid, NUL-terminated path and open returns a
        // descriptor owned by this test on success.
        let original = unsafe { libc::open(path.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
        assert!(original >= 0);
        assert!(matches!(adopt_ready_fd(original), Err(Error::ReadyFd(_))));
        // SAFETY: F_GETFD only inspects the descriptor integer.
        assert_eq!(unsafe { libc::fcntl(original, libc::F_GETFD) }, -1);

        // Linux accepts access mode 3, but it permits neither reads nor writes.
        // SAFETY: `path` remains a valid NUL-terminated path.
        let original = unsafe { libc::open(path.as_ptr(), libc::O_ACCMODE | libc::O_CLOEXEC) };
        assert!(original >= 0);
        assert!(matches!(adopt_ready_fd(original), Err(Error::ReadyFd(_))));
        // SAFETY: F_GETFD only inspects the descriptor integer.
        assert_eq!(unsafe { libc::fcntl(original, libc::F_GETFD) }, -1);
    }

    #[test]
    fn authentication_layout_handles_tiny_frames_without_underflow() {
        assert_eq!(input_layout(1), (0, 1));
        assert_eq!(input_layout(139), (15, 109));
        assert_eq!(input_layout(620), (68, 484));
    }

    #[test]
    fn authentication_overlay_is_bounded_independently_of_wallpaper_size() {
        assert_eq!(authentication_panel_dimensions(3840, 2160), (620, 340));
        assert_eq!(authentication_panel_dimensions(320, 200), (320, 200));
        assert_eq!(620 * 340 * 4, 843_200);
    }

    #[test]
    fn alpha_blending_preserves_transparency_for_layered_rendering() {
        let mut transparent = [0, 0, 0, 0];

        blend_pixel(&mut transparent, 1, 0, 0, [7, 10, 24, 210]);
        assert_eq!(transparent, [7, 10, 24, 210]);

        blend_pixel(&mut transparent, 1, 0, 0, [255, 255, 255, 255]);
        assert_eq!(transparent, [255, 255, 255, 255]);
    }

    #[test]
    fn authentication_input_is_disabled_before_compositor_confirmation() {
        assert!(!authentication_input_enabled(false));
        assert!(authentication_input_enabled(true));
    }

    #[cfg(feature = "lock-test")]
    #[test]
    fn authentication_observer_uses_fixed_non_secret_event_names() {
        assert_eq!(
            [
                AuthTestEvent::Prompt,
                AuthTestEvent::Retry,
                AuthTestEvent::Success,
                AuthTestEvent::Failure,
            ]
            .map(AuthTestEvent::name),
            ["AUTH_PROMPT", "AUTH_RETRY", "AUTH_SUCCESS", "AUTH_FAILURE"]
        );
    }
}
