mod auth;
mod coordination;
mod identity;
mod launcher;
mod preview;

pub(crate) use preview::{run as run_preview, Fixture as PreviewFixture};

use std::ffi::CString;
#[cfg(feature = "lock-test")]
use std::fs::File;
#[cfg(feature = "lock-test")]
use std::io::Write;
use std::os::fd::{FromRawFd, OwnedFd, RawFd};
use std::path::Path;

use bytes::Bytes;
use cosmic_text::{Align, Attrs, Buffer, Color, FontSystem, Metrics, Shaping, SwashCache, Wrap};
use genkan_session_lock::{Input, Presentation, PresentationFrame, Refresh, RgbaFrame};
use thiserror::Error;

use crate::conversation::{Conversation, Effect, Response, Status};
use crate::wallpaper;

const MAX_AUTH_EVENTS_PER_REFRESH: usize = 32;
const LOCK_CANVAS_WIDTH: u32 = 1280;
const LOCK_CANVAS_HEIGHT: u32 = 800;
const AUTHENTICATION_OVERLAY_WIDTH: u32 = 500;
const AUTHENTICATION_OVERLAY_HEIGHT: u32 = 400;
const AUTHENTICATION_FIELD_X: u32 = 55;
const AUTHENTICATION_FIELD_Y: u32 = 247;
const AUTHENTICATION_FIELD_WIDTH: u32 = 390;
const AUTHENTICATION_FIELD_HEIGHT: u32 = 52;
const AUTHENTICATION_FIELD_BORDER: [u8; 4] = [255, 255, 255, 120];
const AUTHENTICATION_FIELD_GLASS: [u8; 4] = [190, 205, 230, 54];
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
    overlay: Option<RgbaFrame>,
    frame: Option<PresentationFrame>,
    fonts: FontSystem,
    glyphs: SwashCache,
    instruction_page: usize,
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
            overlay: None,
            frame: None,
            fonts: FontSystem::new(),
            glyphs: SwashCache::new(),
            instruction_page: 0,
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
        if changed {
            self.instruction_page = 0;
        }
        changed
    }

    fn retry(&mut self) -> bool {
        self.instruction_page = 0;
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
        self.instruction_page = 0;
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
        self.background = self.wallpaper.rgba_frame();
        self.rebuild_overlay();
    }

    fn rebuild_overlay(&mut self) {
        let (overlay_width, overlay_height) = authentication_overlay_dimensions();
        let mut pixels = vec![0; (overlay_width * overlay_height * 4) as usize];
        render_authentication_overlay(
            &mut pixels,
            overlay_width,
            overlay_height,
            &self.identity,
            &self.conversation,
            self.confirmed,
            self.instruction_page,
            &mut self.fonts,
            &mut self.glyphs,
        );
        self.overlay = Some(
            RgbaFrame::new(overlay_width, overlay_height, Bytes::from(pixels))
                .expect("authentication overlay has valid dimensions"),
        );
        self.rebuild_frame();
    }

    fn rebuild_frame(&mut self) {
        let Some(overlay) = self.overlay.clone() else {
            self.frame = None;
            return;
        };
        let (overlay_width, overlay_height) = overlay.dimensions();
        self.frame = PresentationFrame::new(
            LOCK_CANVAS_WIDTH,
            LOCK_CANVAS_HEIGHT,
            self.background.clone(),
            overlay,
            (LOCK_CANVAS_WIDTH - overlay_width) / 2,
            (LOCK_CANVAS_HEIGHT - overlay_height) / 2,
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
            self.background = self.wallpaper.rgba_frame();
            if auth_changed {
                self.rebuild_overlay();
            } else {
                self.rebuild_frame();
            }
        } else if auth_changed {
            self.rebuild_overlay();
        }
        match wallpaper {
            wallpaper::Refresh::Failed => Refresh::Failed,
            wallpaper::Refresh::Frame => Refresh::Frame,
            wallpaper::Refresh::Unchanged if auth_changed => Refresh::Overlay,
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
        self.rebuild_overlay();
    }

    fn input(&mut self, input: Input) -> bool {
        if !authentication_input_enabled(self.confirmed) {
            return false;
        }
        let changed = match input {
            Input::Text(text) => self.conversation.push_input(&text),
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
            Input::NextPage => {
                if persistent_prompt(self.conversation.prompt()).is_none()
                    && self.conversation.notice().is_none()
                {
                    false
                } else {
                    self.instruction_page = self.instruction_page.wrapping_add(1);
                    true
                }
            }
        };
        if changed {
            self.rebuild_overlay();
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
    instruction_page: usize,
    fonts: &mut FontSystem,
    glyphs: &mut SwashCache,
) {
    const AVATAR_X: u32 = 194;
    const AVATAR_Y: u32 = 10;
    const AVATAR_DIAMETER: u32 = 112;

    let display_name = fit_single_line(fonts, &identity.display_name, 27.0, width as f32);
    let username = fit_single_line(
        fonts,
        &format!("@{}", identity.username),
        16.0,
        width as f32,
    );
    let detailed_prompt = confirmed
        .then(|| persistent_prompt(conversation.prompt()))
        .flatten();
    if let Some(prompt) = detailed_prompt {
        draw_shadowed_text(
            pixels,
            width,
            height,
            fonts,
            glyphs,
            &display_name,
            22.0,
            8,
            [255, 255, 255, 255],
        );
        draw_shadowed_text(
            pixels,
            width,
            height,
            fonts,
            glyphs,
            &username,
            14.0,
            38,
            [255, 255, 255, 255],
        );
        let prompt_pages = draw_wrapped_text_in(
            pixels,
            width,
            fonts,
            glyphs,
            prompt,
            15.0,
            8.0,
            66,
            15,
            width - 30,
            155,
            [255, 255, 255, 255],
            instruction_page,
        );
        if prompt_pages > 1 {
            draw_page_hint(
                pixels,
                width,
                height,
                fonts,
                glyphs,
                instruction_page,
                prompt_pages,
                226,
            );
        }
    } else {
        blend_circle(
            pixels,
            width,
            AVATAR_X + AVATAR_DIAMETER / 2,
            AVATAR_Y + AVATAR_DIAMETER / 2,
            AVATAR_DIAMETER / 2,
            [255, 255, 255, 90],
        );
        blend_circle(
            pixels,
            width,
            AVATAR_X + AVATAR_DIAMETER / 2,
            AVATAR_Y + AVATAR_DIAMETER / 2,
            AVATAR_DIAMETER / 2 - 3,
            [24, 31, 46, 220],
        );
        draw_shadowed_text(
            pixels,
            width,
            height,
            fonts,
            glyphs,
            &initials(&identity.display_name),
            34.0,
            43,
            [255, 255, 255, 255],
        );
        draw_shadowed_text(
            pixels,
            width,
            height,
            fonts,
            glyphs,
            &display_name,
            27.0,
            139,
            [255, 255, 255, 255],
        );
        draw_shadowed_text(
            pixels,
            width,
            height,
            fonts,
            glyphs,
            &username,
            16.0,
            177,
            [255, 255, 255, 255],
        );
    }
    if !confirmed {
        draw_shadowed_text(
            pixels,
            width,
            height,
            fonts,
            glyphs,
            "Securing session…",
            17.0,
            235,
            [255, 255, 255, 255],
        );
        return;
    }

    draw_authentication_field(pixels, width);

    // Treat visible PAM responses as credentials too. Keeping all mutable
    // responses out of the text renderer guarantees its internal scratch
    // buffers never retain application-owned response text.
    let input = masked_response(conversation.input().chars().count());
    let submitting = conversation.status() == Status::Submitting;
    let prompt = fit_single_line(
        fonts,
        if detailed_prompt.is_some() {
            "Response"
        } else {
            conversation.prompt()
        },
        16.0,
        (AUTHENTICATION_FIELD_WIDTH - 82) as f32,
    );
    let field_text = if submitting {
        "Authenticating…"
    } else if input.is_empty() {
        &prompt
    } else {
        &input
    };
    draw_text_in(
        pixels,
        width,
        height,
        fonts,
        glyphs,
        field_text,
        if input.is_empty() { 16.0 } else { 21.0 },
        261,
        AUTHENTICATION_FIELD_X + 24,
        AUTHENTICATION_FIELD_WIDTH - 82,
        if input.is_empty() || submitting {
            [245, 246, 250, 255]
        } else {
            [255, 255, 255, 255]
        },
        false,
    );
    draw_text_in(
        pixels,
        width,
        height,
        fonts,
        glyphs,
        if submitting { "…" } else { "↵" },
        20.0,
        259,
        AUTHENTICATION_FIELD_X + 346,
        36,
        if submitting {
            [180, 184, 194, 255]
        } else {
            [245, 246, 250, 255]
        },
        false,
    );

    if let Some(notice) = conversation.notice() {
        let notice_pages = draw_wrapped_text_in(
            pixels,
            width,
            fonts,
            glyphs,
            notice,
            15.0,
            7.0,
            315,
            10,
            width - 20,
            66,
            if conversation.notice_is_error() {
                [255, 215, 215, 255]
            } else {
                [240, 242, 248, 255]
            },
            instruction_page,
        );
        if notice_pages > 1 {
            draw_page_hint(
                pixels,
                width,
                height,
                fonts,
                glyphs,
                instruction_page,
                notice_pages,
                384,
            );
        }
    }
}

fn authentication_overlay_dimensions() -> (u32, u32) {
    (AUTHENTICATION_OVERLAY_WIDTH, AUTHENTICATION_OVERLAY_HEIGHT)
}

fn initials(name: &str) -> String {
    name.split_whitespace()
        .filter_map(|word| word.chars().next())
        .take(2)
        .collect::<String>()
        .to_uppercase()
}

fn masked_response(characters: usize) -> String {
    "•".repeat(characters.min(18))
}

fn persistent_prompt(prompt: &str) -> Option<&str> {
    (!prompt.eq_ignore_ascii_case("password")).then_some(prompt)
}

fn reflow_instruction(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn fit_single_line(fonts: &mut FontSystem, text: &str, size: f32, width: f32) -> String {
    fit_single_line_by(text, width, |candidate| {
        let mut buffer = Buffer::new(fonts, Metrics::new(size, size * 1.3));
        buffer.set_size(fonts, None, Some(size * 1.4));
        buffer.set_wrap(fonts, Wrap::None);
        buffer.set_text(
            fonts,
            candidate,
            &Attrs::new(),
            Shaping::Advanced,
            Some(Align::Left),
        );
        buffer.shape_until_scroll(fonts, true);
        buffer
            .layout_runs()
            .map(|run| run.line_w)
            .fold(0.0, f32::max)
    })
}

fn fit_single_line_by(text: &str, width: f32, mut measure: impl FnMut(&str) -> f32) -> String {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if measure(&normalized) <= width {
        return normalized;
    }
    if measure("…") > width {
        return String::new();
    }
    let characters = normalized.chars().collect::<Vec<_>>();
    let (mut lower, mut upper) = (0, characters.len());
    while lower < upper {
        let midpoint = (lower + upper).div_ceil(2);
        let candidate = characters[..midpoint]
            .iter()
            .chain(std::iter::once(&'…'))
            .collect::<String>();
        if measure(&candidate) <= width {
            lower = midpoint;
        } else {
            upper = midpoint - 1;
        }
    }
    characters[..lower]
        .iter()
        .chain(std::iter::once(&'…'))
        .collect()
}

fn blend_circle(
    pixels: &mut [u8],
    stride: u32,
    center_x: u32,
    center_y: u32,
    radius: u32,
    color: [u8; 4],
) {
    let radius_squared = i64::from(radius).pow(2);
    for y in center_y.saturating_sub(radius)..center_y.saturating_add(radius) {
        for x in center_x.saturating_sub(radius)..center_x.saturating_add(radius) {
            let dx = i64::from(x) - i64::from(center_x);
            let dy = i64::from(y) - i64::from(center_y);
            if dx * dx + dy * dy <= radius_squared {
                blend_pixel(pixels, stride, x as i32, y as i32, color);
            }
        }
    }
}

fn draw_authentication_field(pixels: &mut [u8], width: u32) {
    blend_rounded_rect(
        pixels,
        width,
        AUTHENTICATION_FIELD_X,
        AUTHENTICATION_FIELD_Y,
        AUTHENTICATION_FIELD_WIDTH,
        AUTHENTICATION_FIELD_HEIGHT,
        AUTHENTICATION_FIELD_HEIGHT / 2,
        AUTHENTICATION_FIELD_GLASS,
    );
    blend_rounded_rect_outline(
        pixels,
        width,
        AUTHENTICATION_FIELD_X,
        AUTHENTICATION_FIELD_Y,
        AUTHENTICATION_FIELD_WIDTH,
        AUTHENTICATION_FIELD_HEIGHT,
        AUTHENTICATION_FIELD_HEIGHT / 2,
        AUTHENTICATION_FIELD_BORDER,
    );
}

#[allow(clippy::too_many_arguments)]
fn blend_rounded_rect(
    pixels: &mut [u8],
    stride: u32,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    radius: u32,
    color: [u8; 4],
) {
    let radius = radius.min(width / 2).min(height / 2);
    for row in y..y.saturating_add(height) {
        for column in x..x.saturating_add(width) {
            let local_x = column - x;
            let local_y = row - y;
            if rounded_rect_contains(local_x, local_y, width, height, radius) {
                blend_pixel(pixels, stride, column as i32, row as i32, color);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn blend_rounded_rect_outline(
    pixels: &mut [u8],
    stride: u32,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    radius: u32,
    color: [u8; 4],
) {
    let radius = radius.min(width / 2).min(height / 2);
    for local_y in 0..height {
        for local_x in 0..width {
            let inside_outer = rounded_rect_contains(local_x, local_y, width, height, radius);
            let inside_inner = local_x > 0
                && local_y > 0
                && local_x + 1 < width
                && local_y + 1 < height
                && rounded_rect_contains(
                    local_x - 1,
                    local_y - 1,
                    width - 2,
                    height - 2,
                    radius.saturating_sub(1),
                );
            if inside_outer && !inside_inner {
                blend_pixel(
                    pixels,
                    stride,
                    (x + local_x) as i32,
                    (y + local_y) as i32,
                    color,
                );
            }
        }
    }
}

fn rounded_rect_contains(local_x: u32, local_y: u32, width: u32, height: u32, radius: u32) -> bool {
    let dx = if local_x < radius {
        radius - 1 - local_x
    } else {
        local_x.saturating_sub(width - radius)
    };
    let dy = if local_y < radius {
        radius - 1 - local_y
    } else {
        local_y.saturating_sub(height - radius)
    };
    i64::from(dx).pow(2) + i64::from(dy).pow(2) <= i64::from(radius).pow(2)
}

#[allow(clippy::too_many_arguments)]
fn draw_wrapped_text_in(
    pixels: &mut [u8],
    width: u32,
    fonts: &mut FontSystem,
    glyphs: &mut SwashCache,
    text: &str,
    maximum_size: f32,
    minimum_size: f32,
    y: i32,
    x: u32,
    text_width: u32,
    text_height: u32,
    color: [u8; 4],
    page: usize,
) -> usize {
    let text = reflow_instruction(text);
    let mut size = maximum_size;
    let buffer = loop {
        let mut candidate = Buffer::new(fonts, Metrics::new(size, size * 1.3));
        candidate.set_size(fonts, Some(text_width as f32), None);
        candidate.set_wrap(fonts, Wrap::WordOrGlyph);
        candidate.set_text(
            fonts,
            &text,
            &Attrs::new(),
            Shaping::Advanced,
            Some(Align::Center),
        );
        candidate.shape_until_scroll(fonts, true);
        let content_height = candidate
            .layout_runs()
            .map(|run| run.line_top + run.line_height)
            .fold(0.0, f32::max);
        if content_height <= text_height as f32 || size <= minimum_size {
            break candidate;
        }
        size = (size - 1.0).max(minimum_size);
    };
    let line_height = size * 1.3;
    let line_count = buffer.layout_runs().count();
    let (page_count, first_line, lines_on_page) =
        page_layout(line_count, text_height, line_height, page);
    let runs = buffer
        .layout_runs()
        .skip(first_line)
        .take(lines_on_page)
        .collect::<Vec<_>>();
    let page_top = runs.first().map_or(0.0, |run| run.line_top).round() as i32;
    let default_color = Color::rgba(color[0], color[1], color[2], color[3]);
    for run in runs {
        for glyph in run.glyphs {
            let physical = glyph.physical((0.0, 0.0), 1.0);
            let glyph_color = glyph.color_opt.unwrap_or(default_color);
            glyphs.with_pixels(
                fonts,
                physical.cache_key,
                glyph_color,
                |offset_x, offset_y, glyph_color| {
                    let glyph_x = physical.x + offset_x;
                    let glyph_y = run.line_y as i32 + physical.y + offset_y - page_top;
                    if glyph_x < 0
                        || glyph_y < 0
                        || glyph_x >= text_width as i32
                        || glyph_y >= text_height as i32
                    {
                        return;
                    }
                    blend_pixel(
                        pixels,
                        width,
                        x as i32 + glyph_x,
                        y + glyph_y,
                        glyph_pixel(glyph_color.as_rgba(), color, false),
                    );
                },
            );
        }
    }
    page_count
}

fn page_layout(
    line_count: usize,
    text_height: u32,
    line_height: f32,
    page: usize,
) -> (usize, usize, usize) {
    let lines_per_page = ((text_height as f32 / line_height).floor() as usize).max(1);
    let page_count = line_count.max(1).div_ceil(lines_per_page);
    let page = page % page_count;
    let first_line = page * lines_per_page;
    (
        page_count,
        first_line,
        line_count.saturating_sub(first_line).min(lines_per_page),
    )
}

#[allow(clippy::too_many_arguments)]
fn draw_page_hint(
    pixels: &mut [u8],
    width: u32,
    height: u32,
    fonts: &mut FontSystem,
    glyphs: &mut SwashCache,
    page: usize,
    page_count: usize,
    y: i32,
) {
    draw_text(
        pixels,
        width,
        height,
        fonts,
        glyphs,
        &format!("{} / {page_count} · Tab for more", page % page_count + 1),
        10.0,
        y,
        [230, 232, 238, 255],
    );
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
    draw_text_in(
        pixels, width, height, fonts, glyphs, text, size, y, 0, width, color, false,
    );
}

#[allow(clippy::too_many_arguments)]
fn draw_shadowed_text(
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
    draw_text_in(
        pixels,
        width,
        height,
        fonts,
        glyphs,
        text,
        size,
        y + 2,
        0,
        width,
        [0, 0, 0, 190],
        true,
    );
    draw_text(pixels, width, height, fonts, glyphs, text, size, y, color);
}

#[allow(clippy::too_many_arguments)]
fn draw_text_in(
    pixels: &mut [u8],
    width: u32,
    height: u32,
    fonts: &mut FontSystem,
    glyphs: &mut SwashCache,
    text: &str,
    size: f32,
    y: i32,
    x: u32,
    text_width: u32,
    color: [u8; 4],
    force_rgb: bool,
) {
    let text_height = (size * 1.4).ceil().min((height as i32 - y).max(0) as f32);
    let mut buffer = Buffer::new(fonts, Metrics::new(size, size * 1.3));
    buffer.set_size(fonts, Some(text_width as f32), Some(text_height));
    buffer.set_wrap(fonts, Wrap::None);
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
        |glyph_x, glyph_y, glyph_width, glyph_height, glyph_color| {
            if glyph_x < 0
                || glyph_y < 0
                || glyph_x + glyph_width as i32 > text_width as i32
                || glyph_y + glyph_height as i32 > text_height as i32
            {
                return;
            }
            blend_pixel(
                pixels,
                width,
                x as i32 + glyph_x,
                y + glyph_y,
                glyph_pixel(glyph_color.as_rgba(), color, force_rgb),
            );
        },
    );
}

fn glyph_pixel(glyph: [u8; 4], requested: [u8; 4], force_rgb: bool) -> [u8; 4] {
    let rgb = if force_rgb {
        [requested[0], requested[1], requested[2]]
    } else {
        [glyph[0], glyph[1], glyph[2]]
    };
    [
        rgb[0],
        rgb[1],
        rgb[2],
        ((u16::from(glyph[3]) * u16::from(requested[3])) / 255) as u8,
    ]
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
    let output_alpha = source_alpha * 255 + target_alpha * (255 - source_alpha);
    if output_alpha == 0 {
        return;
    }
    for channel in 0..3 {
        let premultiplied = u32::from(source[channel]) * source_alpha * 255
            + u32::from(target[channel]) * target_alpha * (255 - source_alpha);
        target[channel] = (premultiplied / output_alpha) as u8;
    }
    target[3] = (output_alpha / 255) as u8;
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
    fn authentication_identity_uses_tahoe_style_initials() {
        assert_eq!(initials("Preview User"), "PU");
        assert_eq!(initials("alice"), "A");
        assert_eq!(initials(""), "");
    }

    #[test]
    fn authentication_text_is_bounded_to_single_line_regions() {
        assert_eq!(masked_response(512).chars().count(), 18);
        let measure = |text: &str| {
            text.chars()
                .map(|character| if character == '界' { 10.0 } else { 1.0 })
                .sum()
        };
        assert_eq!(fit_single_line_by("short", 8.0, measure), "short");
        assert_eq!(
            fit_single_line_by(&"a".repeat(512), 8.0, measure),
            "aaaaaaa…"
        );
        assert_eq!(fit_single_line_by("界界界界", 22.0, measure), "界界…");
        assert_eq!(
            fit_single_line_by("Challenge:\n Enter   token", 40.0, measure),
            "Challenge: Enter token"
        );
    }

    #[test]
    fn challenge_instructions_remain_available_while_entering_a_response() {
        let challenge = format!(
            "{}\nEnter the following challenge response: 793146",
            "Step\n".repeat(16)
        );
        let mut conversation = Conversation::for_preview(
            String::new(),
            challenge.clone(),
            None,
            false,
            false,
            Status::Waiting,
        );

        assert_eq!(
            persistent_prompt(conversation.prompt()),
            Some(challenge.as_str())
        );
        assert!(conversation.push_input("7"));
        assert_eq!(
            persistent_prompt(conversation.prompt()),
            Some(challenge.as_str())
        );
        let displayed = reflow_instruction(conversation.prompt());
        assert!(!displayed.contains('\n'));
        assert!(displayed.ends_with("Enter the following challenge response: 793146"));
        assert_eq!(persistent_prompt("Password"), None);

        assert_eq!(page_layout(17, 165, 10.4, 0), (2, 0, 15));
        assert_eq!(page_layout(17, 165, 10.4, 1), (2, 15, 2));
        assert_eq!(page_layout(10, 80, 9.1, 1), (2, 8, 2));
    }

    #[test]
    fn authentication_overlay_is_bounded_independently_of_wallpaper_size() {
        assert_eq!(authentication_overlay_dimensions(), (500, 400));
        assert_eq!(500 * 400 * 4, 800_000);
    }

    #[test]
    fn authentication_field_has_transparent_rounded_corners() {
        let mut pixels = [0; 9 * 9 * 4];

        blend_rounded_rect(&mut pixels, 9, 0, 0, 9, 9, 4, [10, 20, 30, 255]);

        assert_eq!(&pixels[0..4], &[0, 0, 0, 0]);
        assert_eq!(
            &pixels[(4 * 9 + 4) * 4..(4 * 9 + 5) * 4],
            &[10, 20, 30, 255]
        );

        let mut capsule = vec![0; 52 * 52 * 4];
        blend_rounded_rect(&mut capsule, 52, 0, 0, 52, 52, 26, [10, 20, 30, 255]);
        assert_eq!(
            &capsule[(26 * 52 + 26) * 4..(26 * 52 + 27) * 4],
            &[10, 20, 30, 255]
        );
    }

    #[test]
    fn authentication_field_uses_translucent_glass_material() {
        let mut pixels =
            vec![0; (AUTHENTICATION_OVERLAY_WIDTH * AUTHENTICATION_OVERLAY_HEIGHT * 4) as usize];
        draw_authentication_field(&mut pixels, AUTHENTICATION_OVERLAY_WIDTH);
        let pixel = |x: u32, y: u32| {
            let offset = ((y * AUTHENTICATION_OVERLAY_WIDTH + x) * 4) as usize;
            <[u8; 4]>::try_from(&pixels[offset..offset + 4]).unwrap()
        };

        assert_eq!(
            pixel(
                AUTHENTICATION_FIELD_X + 12,
                AUTHENTICATION_FIELD_Y + AUTHENTICATION_FIELD_HEIGHT / 2,
            ),
            AUTHENTICATION_FIELD_GLASS
        );
        let border = pixel(
            AUTHENTICATION_FIELD_X,
            AUTHENTICATION_FIELD_Y + AUTHENTICATION_FIELD_HEIGHT / 2,
        );
        assert!(border[3] > AUTHENTICATION_FIELD_GLASS[3]);
        assert!(border[3] < 192);
        assert!(AUTHENTICATION_FIELD_GLASS[3] < 64);
    }

    #[test]
    fn alpha_blending_preserves_transparency_for_layered_rendering() {
        let mut transparent = [0, 0, 0, 0];

        blend_pixel(&mut transparent, 1, 0, 0, [7, 10, 24, 210]);
        assert_eq!(transparent, [7, 10, 24, 210]);

        blend_pixel(&mut transparent, 1, 0, 0, [255, 255, 255, 255]);
        assert_eq!(transparent, [255, 255, 255, 255]);

        let mut translucent_white = [255, 255, 255, 128];
        blend_pixel(&mut translucent_white, 1, 0, 0, [255, 255, 255, 128]);
        assert_eq!(translucent_white, [255, 255, 255, 191]);
    }

    #[test]
    fn glyph_rendering_applies_opacity_and_forces_black_shadows() {
        assert_eq!(
            glyph_pixel([240, 240, 240, 128], [240, 240, 240, 190], false),
            [240, 240, 240, 95]
        );
        assert_eq!(
            glyph_pixel([12, 140, 220, 255], [255, 255, 255, 128], false),
            [12, 140, 220, 128]
        );
        assert_eq!(
            glyph_pixel([12, 140, 220, 255], [0, 0, 0, 190], true),
            [0, 0, 0, 190]
        );
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
