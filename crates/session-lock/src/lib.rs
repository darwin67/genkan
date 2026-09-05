mod runtime;

use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
#[cfg(feature = "lock-test")]
use std::time::Duration;

use bytes::Bytes;

pub use runtime::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    uid: u32,
    username: String,
    display_name: String,
}

impl Identity {
    pub fn new(uid: u32, username: String, display_name: String) -> Self {
        Self {
            uid,
            username,
            display_name,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbaFrame {
    width: u32,
    height: u32,
    pixels: Bytes,
}

impl RgbaFrame {
    pub fn new(width: u32, height: u32, pixels: Bytes) -> Option<Self> {
        let expected = width.checked_mul(height)?.checked_mul(4)? as usize;
        (width > 0 && height > 0 && pixels.len() == expected).then_some(Self {
            width,
            height,
            pixels,
        })
    }

    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresentationFrame {
    width: u32,
    height: u32,
    background: Option<RgbaFrame>,
    overlay: RgbaFrame,
    overlay_x: u32,
    overlay_y: u32,
}

impl PresentationFrame {
    pub fn new(
        width: u32,
        height: u32,
        background: Option<RgbaFrame>,
        overlay: RgbaFrame,
        overlay_x: u32,
        overlay_y: u32,
    ) -> Option<Self> {
        if width == 0
            || height == 0
            || background
                .as_ref()
                .is_some_and(|frame| frame.dimensions() != (width, height))
            || overlay_x.checked_add(overlay.width)? > width
            || overlay_y.checked_add(overlay.height)? > height
        {
            return None;
        }
        Some(Self {
            width,
            height,
            background,
            overlay,
            overlay_x,
            overlay_y,
        })
    }

    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refresh {
    Unchanged,
    Frame,
    Failed,
}

#[derive(PartialEq, Eq)]
pub enum Input {
    Text(String),
    Backspace,
    Submit,
    Cancel,
}

pub trait Presentation {
    fn receive_latest(&mut self) -> Refresh;
    fn frame(&self) -> Option<PresentationFrame>;
    fn lock_confirmed(&mut self) {}
    fn input(&mut self, _input: Input) -> bool {
        false
    }
    fn take_authorization(&mut self) -> bool {
        false
    }
}

pub struct Config {
    wayland: UnixStream,
    identity: Identity,
    presentation: Box<dyn Presentation>,
    ready_fds: Vec<OwnedFd>,
    #[cfg(feature = "lock-test")]
    test_unlock_after_ready: bool,
    #[cfg(feature = "lock-test")]
    test_observer: Option<OwnedFd>,
    #[cfg(feature = "lock-test")]
    test_panic_after_ready: bool,
    #[cfg(feature = "lock-test")]
    test_renderer_failure_after_ready: bool,
    #[cfg(feature = "lock-test")]
    test_ready_delay: Duration,
}

impl Config {
    pub fn new(
        wayland: UnixStream,
        identity: Identity,
        presentation: impl Presentation + 'static,
        ready_fd: Option<OwnedFd>,
    ) -> Self {
        Self {
            wayland,
            identity,
            presentation: Box::new(presentation),
            ready_fds: ready_fd.into_iter().collect(),
            #[cfg(feature = "lock-test")]
            test_unlock_after_ready: false,
            #[cfg(feature = "lock-test")]
            test_observer: None,
            #[cfg(feature = "lock-test")]
            test_panic_after_ready: false,
            #[cfg(feature = "lock-test")]
            test_renderer_failure_after_ready: false,
            #[cfg(feature = "lock-test")]
            test_ready_delay: Duration::ZERO,
        }
    }

    pub fn with_additional_ready_fd(mut self, ready_fd: OwnedFd) -> Self {
        self.ready_fds.push(ready_fd);
        self
    }

    #[cfg(feature = "lock-test")]
    pub fn with_test_unlock_after_ready(mut self, enabled: bool) -> Self {
        self.test_unlock_after_ready = enabled;
        self
    }

    #[cfg(feature = "lock-test")]
    pub fn with_test_observer(mut self, observer: Option<OwnedFd>) -> Self {
        self.test_observer = observer;
        self
    }

    #[cfg(feature = "lock-test")]
    pub fn with_test_panic_after_ready(mut self, enabled: bool) -> Self {
        self.test_panic_after_ready = enabled;
        self
    }

    #[cfg(feature = "lock-test")]
    pub fn with_test_renderer_failure_after_ready(mut self, enabled: bool) -> Self {
        self.test_renderer_failure_after_ready = enabled;
        self
    }

    #[cfg(feature = "lock-test")]
    pub fn with_test_ready_delay(mut self, delay: Duration) -> Self {
        self.test_ready_delay = delay;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lifecycle {
    Requesting,
    Locked,
    Denied,
    Aborted,
    Unlocking,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Event {
    LockConfirmed,
    LockFinished,
    RuntimeFailed,
    PresentationFailed,
    #[cfg(test)]
    CloseRequested,
    #[cfg(test)]
    EscapePressed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    None,
    ReportReady,
    Abort,
    UnlockAndSynchronize,
}

#[derive(Debug)]
struct State {
    identity: Identity,
    lifecycle: Lifecycle,
    presentation_failed: bool,
}

impl State {
    fn new(identity: Identity) -> Self {
        Self {
            identity,
            lifecycle: Lifecycle::Requesting,
            presentation_failed: false,
        }
    }

    fn update(&mut self, event: Event) -> Action {
        match event {
            Event::LockConfirmed if self.lifecycle == Lifecycle::Requesting => {
                self.lifecycle = Lifecycle::Locked;
                Action::ReportReady
            }
            Event::LockFinished if self.lifecycle == Lifecycle::Requesting => {
                self.lifecycle = Lifecycle::Denied;
                Action::Abort
            }
            Event::LockFinished | Event::RuntimeFailed => {
                self.lifecycle = Lifecycle::Aborted;
                Action::Abort
            }
            Event::PresentationFailed => {
                self.presentation_failed = true;
                Action::None
            }
            #[cfg(test)]
            Event::CloseRequested | Event::EscapePressed => Action::None,
            Event::LockConfirmed => Action::None,
        }
    }

    fn authorize_unlock(&mut self, _authorization: UnlockAuthorization) -> Action {
        if self.lifecycle != Lifecycle::Locked {
            return Action::None;
        }
        self.lifecycle = Lifecycle::Unlocking;
        Action::UnlockAndSynchronize
    }
}

#[derive(Debug, Clone, Copy)]
struct UnlockAuthorization(());

impl UnlockAuthorization {
    fn authenticated() -> Self {
        Self(())
    }

    #[cfg(any(test, feature = "lock-test"))]
    fn test_source() -> Self {
        Self(())
    }
}

pub fn run(config: Config) -> Result<(), Error> {
    runtime::run(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> State {
        State::new(Identity::new(1000, "alice".into(), "Alice".into()))
    }

    #[test]
    fn rejects_malformed_frames_at_the_crate_boundary() {
        assert!(RgbaFrame::new(0, 1, Bytes::new()).is_none());
        assert!(RgbaFrame::new(1, 1, Bytes::from_static(&[0; 3])).is_none());
        assert!(RgbaFrame::new(u32::MAX, u32::MAX, Bytes::new()).is_none());
        assert!(RgbaFrame::new(1, 1, Bytes::from_static(&[0; 4])).is_some());
    }

    #[test]
    fn presentation_frames_require_bounded_overlays_and_matching_backgrounds() {
        let background = RgbaFrame::new(2, 2, Bytes::from_static(&[0; 16])).unwrap();
        let background_pixels = background.pixels().as_ptr();
        let overlay = RgbaFrame::new(1, 1, Bytes::from_static(&[0; 4])).unwrap();

        let frame =
            PresentationFrame::new(2, 2, Some(background.clone()), overlay.clone(), 1, 1).unwrap();
        assert_eq!(
            frame.background.as_ref().unwrap().pixels().as_ptr(),
            background_pixels,
            "sharing a presentation must not copy the background pixels"
        );
        assert!(PresentationFrame::new(3, 2, Some(background), overlay.clone(), 1, 1).is_none());
        assert!(PresentationFrame::new(2, 2, None, overlay, 2, 1).is_none());
    }

    #[test]
    fn compositor_confirmation_is_the_only_readiness_boundary() {
        let mut state = state();

        assert_eq!(state.update(Event::PresentationFailed), Action::None);
        assert_eq!(state.update(Event::CloseRequested), Action::None);
        assert_eq!(state.update(Event::EscapePressed), Action::None);
        assert_eq!(state.lifecycle, Lifecycle::Requesting);
        assert_eq!(state.update(Event::LockConfirmed), Action::ReportReady);
        assert_eq!(state.lifecycle, Lifecycle::Locked);
        assert_eq!(state.update(Event::LockConfirmed), Action::None);
    }

    #[test]
    fn finished_and_runtime_failures_abort_without_unlocking() {
        for event in [Event::LockFinished, Event::RuntimeFailed] {
            let mut pending = state();
            assert_eq!(pending.update(event), Action::Abort);
            assert!(matches!(
                pending.lifecycle,
                Lifecycle::Denied | Lifecycle::Aborted
            ));
            assert_eq!(pending.update(Event::LockConfirmed), Action::None);

            let mut locked = state();
            assert_eq!(locked.update(Event::LockConfirmed), Action::ReportReady);
            assert_eq!(locked.update(event), Action::Abort);
            assert_eq!(locked.lifecycle, Lifecycle::Aborted);
        }
    }

    #[test]
    fn presentation_failure_keeps_lock_ownership() {
        let mut state = state();
        assert_eq!(state.update(Event::LockConfirmed), Action::ReportReady);

        assert_eq!(state.update(Event::PresentationFailed), Action::None);
        assert_eq!(state.lifecycle, Lifecycle::Locked);
        assert!(state.presentation_failed);
    }

    #[test]
    fn only_the_test_authorization_source_constructs_unlock() {
        let mut state = state();
        assert_eq!(
            state.authorize_unlock(UnlockAuthorization::test_source()),
            Action::None
        );
        assert_eq!(state.update(Event::LockConfirmed), Action::ReportReady);
        assert_eq!(
            state.authorize_unlock(UnlockAuthorization::test_source()),
            Action::UnlockAndSynchronize
        );
        assert_eq!(state.lifecycle, Lifecycle::Unlocking);
    }

    #[test]
    fn authenticated_authorization_cannot_unlock_before_confirmation() {
        let mut state = state();
        assert_eq!(
            state.authorize_unlock(UnlockAuthorization::authenticated()),
            Action::None
        );
        assert_eq!(state.update(Event::LockConfirmed), Action::ReportReady);
        assert_eq!(
            state.authorize_unlock(UnlockAuthorization::authenticated()),
            Action::UnlockAndSynchronize
        );
    }
}
