mod identity;
mod runtime;

use std::os::fd::RawFd;

use crate::wallpaper;

pub(crate) use identity::Identity;

#[derive(Debug, Clone)]
pub(crate) struct Config {
    pub(crate) wallpaper: wallpaper::Settings,
    pub(crate) ready_fd: Option<RawFd>,
    #[cfg(feature = "lock-test")]
    pub(crate) test_unlock_after_ready: bool,
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
    WallpaperFailed,
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
    wallpaper_failed: bool,
}

impl State {
    fn new(identity: Identity) -> Self {
        Self {
            identity,
            lifecycle: Lifecycle::Requesting,
            wallpaper_failed: false,
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
            Event::WallpaperFailed => {
                self.wallpaper_failed = true;
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

pub(crate) fn run(config: Config) -> Result<(), runtime::Error> {
    runtime::run(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> State {
        State::new(Identity::fixture())
    }

    #[test]
    fn compositor_confirmation_is_the_only_readiness_boundary() {
        let mut state = state();

        assert_eq!(state.update(Event::WallpaperFailed), Action::None);
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

        assert_eq!(state.update(Event::WallpaperFailed), Action::None);
        assert_eq!(state.lifecycle, Lifecycle::Locked);
        assert!(state.wallpaper_failed);
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
