mod auth;
mod coordination;
mod identity;
mod launcher;

use std::ffi::CString;
use std::os::fd::{FromRawFd, OwnedFd, RawFd};
use std::path::Path;

use bytes::Bytes;
use cosmic_text::{Align, Attrs, Buffer, Color, FontSystem, Metrics, Shaping, SwashCache};
use genkan_session_lock::{Input, Presentation, Refresh, RgbaFrame};
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
}

#[derive(Debug, Error)]
pub(crate) enum Error {
    #[error("could not resolve lock identity: {0}")]
    Identity(#[from] identity::Error),
    #[error("could not adopt readiness descriptor: {0}")]
    ReadyFd(#[source] std::io::Error),
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
    frame: Option<RgbaFrame>,
    fonts: FontSystem,
    glyphs: SwashCache,
}

impl LockerPresentation {
    fn new(
        identity: identity::Identity,
        auth: auth::Client,
        wallpaper: wallpaper::Settings,
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
            frame: None,
            fonts: FontSystem::new(),
            glyphs: SwashCache::new(),
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
                    }
                }
                auth::Event::Failure => {
                    self.prompt_id = None;
                    let _ = self
                        .conversation
                        .receive(attempt, Response::Failure("Authentication failed".into()));
                }
            }
        }
        changed
    }

    fn retry(&mut self) -> bool {
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
                    self.conversation
                        .fail("Authentication worker unavailable".into());
                }
            },
            Err(_) => self
                .conversation
                .fail("Authentication worker unavailable".into()),
        }
    }

    fn rebuild(&mut self) {
        const WIDTH: u32 = 1280;
        const HEIGHT: u32 = 800;
        let wallpaper = self.wallpaper.rgba_frame();
        let (width, height, mut pixels) = wallpaper.as_ref().map_or_else(
            || {
                (
                    WIDTH,
                    HEIGHT,
                    [5_u8, 9, 24, 255].repeat((WIDTH * HEIGHT) as usize),
                )
            },
            |frame| {
                let (width, height) = frame.dimensions();
                (width, height, frame.pixels().to_vec())
            },
        );
        render_authentication(
            &mut pixels,
            width,
            height,
            &self.identity,
            &self.conversation,
            self.confirmed,
            &mut self.fonts,
            &mut self.glyphs,
        );
        self.frame = RgbaFrame::new(width, height, Bytes::from(pixels));
    }
}

impl Presentation for LockerPresentation {
    fn receive_latest(&mut self) -> Refresh {
        let auth_changed = self.receive_auth();
        let wallpaper = self.wallpaper.receive_latest();
        if auth_changed || wallpaper != wallpaper::Refresh::Unchanged {
            self.rebuild();
        }
        match wallpaper {
            wallpaper::Refresh::Failed => Refresh::Failed,
            wallpaper::Refresh::Frame => Refresh::Frame,
            wallpaper::Refresh::Unchanged if auth_changed => Refresh::Frame,
            wallpaper::Refresh::Unchanged => Refresh::Unchanged,
        }
    }

    fn frame(&self) -> Option<RgbaFrame> {
        self.frame.clone()
    }

    fn lock_confirmed(&mut self) {
        self.confirmed = true;
        if self
            .auth
            .as_mut()
            .is_none_or(|(_, client)| client.begin().is_err())
        {
            self.conversation
                .fail("Authentication worker unavailable".into());
        }
        self.rebuild();
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
                    self.conversation
                        .fail("Authentication worker unavailable".into());
                }
                true
            }
            Input::Cancel => self.cancel_attempt(),
        };
        if changed {
            self.rebuild();
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
    let presentation = LockerPresentation::new(identity, authentication, config.wallpaper);
    let runtime =
        genkan_session_lock::Config::new(wayland, runtime_identity, presentation, ready_fd)
            .with_additional_ready_fd(coordination_ready);
    #[cfg(feature = "lock-test")]
    let runtime = runtime.with_test_unlock_after_ready(config.test_unlock_after_ready);
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
fn render_authentication(
    pixels: &mut [u8],
    width: u32,
    height: u32,
    identity: &identity::Identity,
    conversation: &Conversation,
    confirmed: bool,
    fonts: &mut FontSystem,
    glyphs: &mut SwashCache,
) {
    let panel_width = width.min(620);
    let panel_height = height.min(340);
    let left = (width - panel_width) / 2;
    let top = (height - panel_height) / 2;
    blend_rect(
        pixels,
        width,
        left,
        top,
        panel_width,
        panel_height,
        [7, 10, 24, 210],
    );
    draw_text(
        pixels,
        width,
        height,
        fonts,
        glyphs,
        &identity.display_name,
        30.0,
        top as i32 + 48,
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
        top as i32 + 88,
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
        top as i32 + 140,
        [255, 255, 255, 255],
    );
    if confirmed {
        // Treat visible PAM responses as credentials too. Keeping all mutable
        // responses out of the text renderer guarantees its internal scratch
        // buffers never retain application-owned response text.
        let input = "•".repeat(conversation.input().chars().count());
        let (input_margin, input_width) = input_layout(panel_width);
        blend_rect(
            pixels,
            width,
            left + input_margin,
            top + 172,
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
            top as i32 + 185,
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
                top as i32 + 250,
                if conversation.notice_is_error() {
                    [255, 171, 171, 255]
                } else {
                    [220, 224, 240, 255]
                },
            );
        }
    }
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
    let alpha = source[3] as u16;
    for channel in 0..3 {
        target[channel] =
            ((source[channel] as u16 * alpha + target[channel] as u16 * (255 - alpha)) / 255) as u8;
    }
    target[3] = 255;
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
    fn authentication_input_is_disabled_before_compositor_confirmation() {
        assert!(!authentication_input_enabled(false));
        assert!(authentication_input_enabled(true));
    }
}
