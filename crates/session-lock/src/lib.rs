mod runtime;

use std::os::fd::RawFd;

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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refresh {
    Unchanged,
    Frame,
    Failed,
}

pub trait Presentation {
    fn receive_latest(&mut self) -> Refresh;
    fn frame(&self) -> Option<RgbaFrame>;
}

pub struct Config {
    identity: Identity,
    presentation: Box<dyn Presentation>,
    ready_fd: Option<RawFd>,
    #[cfg(feature = "lock-test")]
    test_unlock_after_ready: bool,
}

impl Config {
    pub fn new(
        identity: Identity,
        presentation: impl Presentation + 'static,
        ready_fd: Option<RawFd>,
    ) -> Self {
        Self {
            identity,
            presentation: Box::new(presentation),
            ready_fd,
            #[cfg(feature = "lock-test")]
            test_unlock_after_ready: false,
        }
    }

    #[cfg(feature = "lock-test")]
    pub fn with_test_unlock_after_ready(mut self, enabled: bool) -> Self {
        self.test_unlock_after_ready = enabled;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lifecycle {
    Requesting,
    Locked,
    Denied,
    Aborted,
    #[cfg(any(test, feature = "lock-test"))]
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
    #[cfg(any(test, feature = "lock-test"))]
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

    #[cfg(any(test, feature = "lock-test"))]
    fn authorize_unlock(&mut self, _authorization: UnlockAuthorization) -> Action {
        if self.lifecycle != Lifecycle::Locked {
            return Action::None;
        }
        self.lifecycle = Lifecycle::Unlocking;
        Action::UnlockAndSynchronize
    }
}

#[cfg(any(test, feature = "lock-test"))]
#[derive(Debug, Clone, Copy)]
struct UnlockAuthorization(());

#[cfg(any(test, feature = "lock-test"))]
impl UnlockAuthorization {
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
}
