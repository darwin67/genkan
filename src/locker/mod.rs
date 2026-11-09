mod auth;
mod coordination;
mod identity;
mod launcher;
mod preview;
mod render;

pub(crate) use preview::{run as run_preview, Fixture as PreviewFixture};

use std::ffi::CString;
#[cfg(feature = "lock-test")]
use std::fs::File;
#[cfg(feature = "lock-test")]
use std::io::Write;
use std::os::fd::{FromRawFd, OwnedFd, RawFd};
use std::path::Path;

use chrono::{DateTime, Local};
use genkan_session_lock::{Input, Presentation, PresentationFrame, Refresh, RgbaFrame};
use thiserror::Error;

use crate::conversation::{Conversation, Effect, Response, Status};
use crate::wallpaper;

const MAX_AUTH_EVENTS_PER_REFRESH: usize = 32;
#[cfg(test)]
static PROCESS_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[derive(Debug, Clone)]
pub(crate) struct Config {
    pub(crate) authentication_output: Option<String>,
    pub(crate) wallpaper: wallpaper::Settings,
    pub(crate) ready_fd: Option<RawFd>,
    #[cfg(feature = "lock-test")]
    pub(crate) test_unlock_after_ready: bool,
    #[cfg(feature = "lock-test")]
    pub(crate) test_unlock_delay_ms: Option<u64>,
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
    /// The identity column; the frame exists once this does.
    identity_overlay: Option<RgbaFrame>,
    clock_overlay: Option<RgbaFrame>,
    /// The minute since the Unix epoch the clock overlay shows.
    clock_minute: Option<i64>,
    frame: Option<PresentationFrame>,
    renderer: render::Renderer,
    instruction_page: usize,
    reduced_motion: bool,
    shake_started: Option<std::time::Instant>,
    status_checked: std::time::Instant,
    network: Option<&'static str>,
    /// The locked user's picture, decoded on a background thread (RFD 7).
    /// Locking never waits for it; the avatar shows initials until it arrives.
    picture: Option<std::sync::mpsc::Receiver<tiny_skia::Pixmap>>,
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
        let reduced_motion = wallpaper.reduced_motion;
        let identity_uid = identity.uid;
        let mut presentation = Self {
            wallpaper: wallpaper::State::start(wallpaper),
            identity,
            conversation,
            auth: Some((attempt, auth)),
            prompt_id: None,
            confirmed: false,
            authorized: false,
            background: None,
            identity_overlay: None,
            clock_overlay: None,
            clock_minute: None,
            frame: None,
            renderer: render::Renderer::new(),
            instruction_page: 0,
            reduced_motion,
            shake_started: None,
            status_checked: std::time::Instant::now(),
            network: None,
            picture: load_picture(identity_uid),
            #[cfg(feature = "lock-test")]
            fail_worker_after_ready,
            #[cfg(feature = "lock-test")]
            test_observer: test_observer.map(File::from),
        };
        presentation.rebuild();
        presentation
    }

    /// A presentation without the authentication worker, for wallpaper
    /// integration tests that do not exercise PAM.
    ///
    /// `rebuild` is deliberately skipped: the wallpaper path under test does
    /// not depend on the overlays, and shaping them would only slow it down.
    #[cfg(test)]
    fn for_test(identity: identity::Identity, wallpaper: wallpaper::Settings) -> Self {
        let reduced_motion = wallpaper.reduced_motion;
        Self {
            wallpaper: wallpaper::State::start(wallpaper),
            identity,
            conversation: Conversation::new(),
            auth: None,
            prompt_id: None,
            confirmed: false,
            authorized: false,
            background: None,
            identity_overlay: None,
            clock_overlay: None,
            clock_minute: None,
            frame: None,
            renderer: render::Renderer::new(),
            instruction_page: 0,
            reduced_motion,
            shake_started: None,
            status_checked: std::time::Instant::now(),
            network: None,
            picture: None,
            #[cfg(feature = "lock-test")]
            fail_worker_after_ready: false,
            #[cfg(feature = "lock-test")]
            test_observer: None,
        }
    }

    fn receive_auth(&mut self) -> (bool, bool) {
        if !self.confirmed {
            return (false, false);
        }
        let mut changed = false;
        let mut rejected = false;
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
                    rejected = true;
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
        (changed, rejected)
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
        self.refresh_clock(Local::now());
        self.rebuild_overlay();
    }

    /// Redraws the identity column and the frame that carries it.
    fn rebuild_overlay(&mut self) {
        self.identity_overlay = Some(self.renderer.identity(
            &self.identity,
            &self.conversation,
            self.confirmed,
            self.instruction_page,
        ));
        self.rebuild_frame();
    }

    /// Redraws the clock when the minute it shows has passed, and reports
    /// whether it did. The frame is rebuilt by the caller.
    fn refresh_clock(&mut self, now: DateTime<Local>) -> bool {
        let minute = now.timestamp().div_euclid(60);
        if self.clock_minute == Some(minute) {
            return false;
        }
        self.clock_minute = Some(minute);
        self.clock_overlay = Some(self.renderer.clock(now));
        true
    }

    fn rebuild_frame(&mut self) {
        let status = self.renderer.status(self.network);
        self.frame = self.identity_overlay.clone().map(|identity| {
            render::frame(
                self.background.clone(),
                self.clock_overlay.clone(),
                Some(identity),
                self.renderer.shake,
            )
            .with_status(status.and_then(|frame| {
                genkan_session_lock::AnchoredOverlay::new(
                    frame,
                    crate::design::tokens::spacing::BAR_INSET_TOP as u32,
                )
            }))
        });
    }

    #[cfg(feature = "lock-test")]
    fn record_test_event(&mut self, event: AuthTestEvent) {
        if let Some(observer) = self.test_observer.as_mut() {
            let _ = writeln!(observer, "{}", event.name());
            let _ = observer.flush();
        }
    }
}

/// Looks up and decodes the locked user's AccountsService picture on a
/// background thread, in the confined decoder. Any failure leaves the
/// initials in place.
fn load_picture(uid: u32) -> Option<std::sync::mpsc::Receiver<tiny_skia::Pixmap>> {
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("genkan-lock-picture".into())
        .spawn(move || -> Option<()> {
            let path = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .ok()?
                .block_on(crate::accounts::picture_for_uid(uid))
                .ok()??;
            let executable = std::env::current_exe().ok()?;
            let picture = genkan::avatar::decode_file(&executable, &path).ok()?;
            sender.send(crate::design::picture::pixmap(&picture)?).ok()
        })
        .ok()?;
    Some(receiver)
}

impl Presentation for LockerPresentation {
    fn receive_latest(&mut self) -> Refresh {
        let (mut authentication, rejected) = self.receive_auth();
        if rejected {
            self.shake_started = None;
            self.renderer.shake = 0.0;
        }
        if let Some(started) = self.shake_started {
            let elapsed = started.elapsed();
            if elapsed >= crate::design::tokens::motion::SHAKE_DURATION {
                self.shake_started = None;
            }
            // Runtime dispatch can happen faster than its 16ms frame cadence.
            // Do not reshape the identity overlay on every keyboard event.
            let elapsed = std::time::Duration::from_millis(elapsed.as_millis() as u64 / 16 * 16);
            let offset = self
                .shake_started
                .map_or(0.0, |_| crate::app::shake_offset(elapsed));
            authentication |= self.renderer.shake != offset;
            self.renderer.shake = offset;
        }
        if self.status_checked.elapsed() >= std::time::Duration::from_secs(5) {
            self.status_checked = std::time::Instant::now();
            let network = crate::status::network();
            authentication |= self.network != network;
            self.network = network;
        }
        if let Some(receiver) = &self.picture {
            match receiver.try_recv() {
                Ok(picture) => {
                    self.renderer.picture = Some(picture);
                    self.picture = None;
                    authentication = true;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.picture = None,
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        if authentication {
            self.rebuild_overlay();
        }
        if self.refresh_clock(Local::now()) {
            self.rebuild_frame();
        } else if !authentication {
            return Refresh::Unchanged;
        }
        // Cold failure text and clock rasterization can exceed the entire
        // animation on a software renderer. Arm it only after preparing that
        // first failed-state frame, not while its pixels are still being built.
        if rejected && !self.reduced_motion {
            self.shake_started = Some(std::time::Instant::now());
        }
        Refresh::Overlay
    }

    fn receive_deferred(&mut self) -> Refresh {
        let wallpaper = self.wallpaper.receive_latest();
        if wallpaper != wallpaper::Refresh::Unchanged {
            self.background = self.wallpaper.rgba_frame();
            self.rebuild_frame();
        }
        match wallpaper {
            wallpaper::Refresh::Failed => Refresh::Failed,
            wallpaper::Refresh::Frame => Refresh::Frame,
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
        if !authentication_input_enabled(self.confirmed) && !matches!(input, Input::Keyboard(_)) {
            return false;
        }
        let changed = match input {
            Input::Keyboard(status) => {
                if self.renderer.keyboard == status {
                    false
                } else {
                    self.renderer.keyboard = status;
                    true
                }
            }
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
                if self
                    .renderer
                    .prompt_above(&self.conversation, self.confirmed)
                    .is_none()
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
            .with_authentication_output(config.authentication_output)
            .with_additional_ready_fd(coordination_ready);
    #[cfg(feature = "lock-test")]
    let runtime = runtime
        .with_test_unlock_after_ready(config.test_unlock_after_ready)
        .with_test_unlock_delay(std::time::Duration::from_millis(
            config.test_unlock_delay_ms.unwrap_or(5_000),
        ))
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

    /// Identity of the file a descriptor refers to.
    fn descriptor_identity(fd: RawFd) -> (u64, u64) {
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: `stat` points to storage for one `stat` structure.
        assert_eq!(unsafe { libc::fstat(fd, &mut stat) }, 0);
        (stat.st_dev, stat.st_ino)
    }

    /// Asserts that `fd` was closed, tolerating the descriptor number being
    /// reused by another test thread between the close and this probe.
    ///
    /// Tests in this binary run in parallel and open descriptors freely, so a
    /// bare `F_GETFD` probe cannot distinguish a leaked descriptor from a number
    /// that was handed to an unrelated open in the meantime. Comparing the file
    /// identity tells the two apart, which is why this test opens a unique file
    /// rather than `/dev/null`.
    fn assert_descriptor_released(fd: RawFd, identity: (u64, u64)) {
        // SAFETY: F_GETFD only inspects the descriptor integer.
        if unsafe { libc::fcntl(fd, libc::F_GETFD) } == -1 {
            return;
        }
        assert_ne!(
            descriptor_identity(fd),
            identity,
            "descriptor {fd} is still open"
        );
    }

    #[test]
    fn read_only_readiness_descriptors_are_rejected_and_closed() {
        let _guard = PROCESS_TEST_LOCK.lock().unwrap();
        let path = std::env::temp_dir().join(format!(
            "genkan-readiness-descriptor-{}",
            std::process::id()
        ));
        std::fs::write(&path, b"readiness").unwrap();
        let c_path = CString::new(path.to_str().unwrap()).unwrap();

        let original = File::open(&path).unwrap().into_raw_fd();
        let identity = descriptor_identity(original);
        assert!(matches!(adopt_ready_fd(original), Err(Error::ReadyFd(_))));
        assert_descriptor_released(original, identity);

        let mut pipe = [0; 2];
        // SAFETY: `pipe` points to storage for both returned descriptors.
        assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
        // SAFETY: the write end is not transferred to the function under test.
        unsafe { libc::close(pipe[1]) };
        let identity = descriptor_identity(pipe[0]);
        assert!(matches!(adopt_ready_fd(pipe[0]), Err(Error::ReadyFd(_))));
        assert_descriptor_released(pipe[0], identity);

        // SAFETY: `c_path` is a valid, NUL-terminated path and open returns a
        // descriptor owned by this test on success.
        let original = unsafe { libc::open(c_path.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
        assert!(original >= 0);
        let identity = descriptor_identity(original);
        assert!(matches!(adopt_ready_fd(original), Err(Error::ReadyFd(_))));
        assert_descriptor_released(original, identity);

        // Linux accepts access mode 3, but it permits neither reads nor writes.
        // SAFETY: `c_path` remains a valid NUL-terminated path.
        let original = unsafe { libc::open(c_path.as_ptr(), libc::O_ACCMODE | libc::O_CLOEXEC) };
        assert!(original >= 0);
        let identity = descriptor_identity(original);
        assert!(matches!(adopt_ready_fd(original), Err(Error::ReadyFd(_))));
        assert_descriptor_released(original, identity);

        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn challenge_prompts_stay_above_the_pill_while_entering_a_response() {
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
        let mut renderer = render::Renderer::new();

        assert_eq!(
            renderer.prompt_above(&conversation, true),
            Some(challenge.as_str())
        );
        assert!(conversation.push_input("7"));
        assert_eq!(
            renderer.prompt_above(&conversation, true),
            Some(challenge.as_str())
        );
        assert_eq!(renderer.prompt_above(&conversation, false), None);
        let password = Conversation::for_preview(
            String::new(),
            "Password".into(),
            None,
            false,
            true,
            Status::Waiting,
        );
        assert_eq!(renderer.prompt_above(&password, true), None);
    }

    #[test]
    fn the_clock_is_redrawn_only_when_its_minute_passes() {
        let identity = identity::Identity {
            uid: 1000,
            username: "preview".into(),
            display_name: "Preview User".into(),
        };
        let settings = wallpaper::Settings {
            catalog: wallpaper::Catalog::TahoeBeach,
            override_path: Some(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/dynamic-heic/synthetic-all-properties.heic"),
            ),
            animate: false,
            reduced_motion: true,
            appearance: genkan::dynamic_wallpaper::AppearancePreference::Automatic,
        };
        let mut presentation = LockerPresentation::for_test(identity, settings);
        let now = crate::app::preview::preview_now();

        assert!(presentation.refresh_clock(now));
        let shown = presentation.clock_overlay.clone();
        assert!(!presentation.refresh_clock(now + chrono::Duration::seconds(59)));
        assert_eq!(presentation.clock_overlay, shown);
        assert!(presentation.refresh_clock(now + chrono::Duration::seconds(60)));
    }

    #[test]
    fn authentication_input_is_disabled_before_compositor_confirmation() {
        assert!(!authentication_input_enabled(false));
        assert!(authentication_input_enabled(true));
    }

    #[test]
    fn metadata_and_shake_refreshes_never_authorize_unlock_and_stop_at_rest() {
        let identity = identity::Identity {
            uid: 1000,
            username: "preview".into(),
            display_name: "Preview User".into(),
        };
        let settings = wallpaper::Settings {
            catalog: wallpaper::Catalog::TahoeBeach,
            override_path: Some(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/dynamic-heic/synthetic-all-properties.heic"),
            ),
            animate: false,
            reduced_motion: false,
            appearance: genkan::dynamic_wallpaper::AppearancePreference::Automatic,
        };
        let mut presentation = LockerPresentation::for_test(identity, settings);
        assert!(!presentation.input(Input::Text(zeroize::Zeroizing::new(
            "not accepted before confirmation".into()
        ))));
        assert!(presentation.input(Input::Keyboard(
            genkan_session_lock::keyboard::KeyboardStatus {
                caps_lock: true,
                layout: Some("French".into()),
            }
        )));
        presentation.confirmed = true;
        presentation.conversation = Conversation::for_preview(
            String::new(),
            "Password".into(),
            None,
            false,
            true,
            Status::Waiting,
        );
        presentation.rebuild();
        let rest = presentation.frame();
        // `receive_latest` reads the host's real network status every five
        // seconds. A slow test run would otherwise cross that interval and
        // draw the CI runner's link into a frame compared against `rest`.
        let receive_latest = |presentation: &mut LockerPresentation| {
            presentation.status_checked = std::time::Instant::now();
            presentation.receive_latest()
        };
        presentation.shake_started =
            Some(std::time::Instant::now() - std::time::Duration::from_millis(80));
        assert_eq!(receive_latest(&mut presentation), Refresh::Overlay);
        assert_ne!(presentation.frame(), rest);
        assert!(!presentation.authorized);
        presentation.shake_started =
            Some(std::time::Instant::now() - crate::design::tokens::motion::SHAKE_DURATION);
        assert_eq!(receive_latest(&mut presentation), Refresh::Overlay);
        assert_eq!(presentation.frame(), rest);
        assert_eq!(receive_latest(&mut presentation), Refresh::Unchanged);
        assert!(!presentation.authorized);
    }

    #[test]
    fn heic_lock_presentation_adopts_scheduled_frames() {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/dynamic-heic/synthetic-all-properties.heic");
        let settings = wallpaper::Settings {
            catalog: wallpaper::Catalog::TahoeBeach,
            override_path: Some(fixture),
            animate: true,
            reduced_motion: true,
            appearance: genkan::dynamic_wallpaper::AppearancePreference::Automatic,
        };
        let identity = identity::Identity {
            uid: 1000,
            username: "preview".into(),
            display_name: "Preview User".into(),
        };
        let mut presentation = LockerPresentation::for_test(identity, settings);
        // Install a transparent overlay before adoption so automatic rebuilding
        // in `receive_deferred` produces the frame. A regression that stopped
        // rebuilding would leave `frame()` empty and fail here.
        presentation.identity_overlay = Some(
            RgbaFrame::new(1, 1, vec![0, 0, 0, 0].into())
                .expect("synthetic overlay has valid dimensions"),
        );

        let mut adopted = false;
        for _ in 0..1000 {
            if presentation.receive_deferred() == Refresh::Frame {
                adopted = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(adopted, "the locker never adopted a scheduled HEIC frame");

        let frame = presentation
            .frame()
            .expect("automatic rebuilding produced a presentation frame");
        let rendered =
            genkan_session_lock::render_preview(&frame, 4, 4, 1).expect("render presentation");
        let background = presentation
            .background
            .as_ref()
            .expect("adopted a scheduled background");
        assert_eq!(background.dimensions(), (8, 8));
        assert_eq!(rendered.dimensions(), (4, 4));
        assert_eq!(rendered.pixels()[3], 255);
        // The wallpaper is not dimmed: away from the overlays the render
        // carries the fixture's own top-left pixel rather than a fallback.
        assert_eq!(rendered.pixels()[..3], background.pixels()[..3]);
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
