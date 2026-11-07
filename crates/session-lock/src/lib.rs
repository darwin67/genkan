mod runtime;
pub mod scaling;

use std::collections::TryReserveError;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
#[cfg(feature = "lock-test")]
use std::time::Duration;

pub mod keyboard;
pub mod material;

use bytes::Bytes;
use zeroize::Zeroizing;

pub use runtime::Error;

#[derive(Debug, thiserror::Error)]
pub enum PreviewError {
    #[error("preview dimensions exceed the renderer resource budget")]
    InvalidDimensions,
    #[error("could not allocate the preview pixel buffer")]
    Allocation(#[from] TryReserveError),
}

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

    pub fn into_pixels(self) -> Bytes {
        self.pixels
    }
}

/// The largest overlay offset, margin or gap, in logical points.
const MAX_LOGICAL_LENGTH: u32 = 16_384;

/// How the runtime places a presentation's overlays on the authentication
/// output.
///
/// Overlays are rendered at `density` pixels per logical point and scaled by
/// the output's buffer scale divided by `density`. Lengths are logical points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverlayLayout {
    density: u32,
    gap: u32,
    margin: u32,
    min_top_scale_percent: u32,
}

impl OverlayLayout {
    /// The largest supported overlay density.
    pub const MAX_DENSITY: u32 = 16;

    /// A layout for overlays rendered at `density` pixels per logical point.
    pub fn new(density: u32) -> Option<Self> {
        (1..=Self::MAX_DENSITY).contains(&density).then_some(Self {
            density,
            gap: 0,
            margin: 0,
            min_top_scale_percent: 100,
        })
    }

    /// Lets the top overlay shrink, together with its offset, so that it keeps
    /// `gap` above the bottom overlay and `margin` from the left and right
    /// edges, down to `min_scale_percent` of its natural size.
    ///
    /// The bottom overlay never shrinks unless it is larger than the output.
    pub fn with_top_fit(mut self, gap: u32, margin: u32, min_scale_percent: u32) -> Self {
        self.gap = gap.min(MAX_LOGICAL_LENGTH);
        self.margin = margin.min(MAX_LOGICAL_LENGTH);
        self.min_top_scale_percent = min_scale_percent.min(100);
        self
    }
}

/// An overlay centered horizontally and `offset` logical points from the top
/// or bottom edge of the authentication output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnchoredOverlay {
    overlay: RgbaFrame,
    offset: u32,
    backdrop: Option<material::Backdrop>,
}

impl AnchoredOverlay {
    pub fn new(overlay: RgbaFrame, offset: u32) -> Option<Self> {
        (offset <= MAX_LOGICAL_LENGTH).then_some(Self {
            overlay,
            offset,
            backdrop: None,
        })
    }

    pub fn with_backdrop(mut self, backdrop: material::Backdrop) -> Self {
        self.backdrop = Some(backdrop);
        self
    }
}

/// A factor applied to the wallpaper behind the authentication overlays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dimming {
    numerator: u16,
    denominator: u16,
}

impl Dimming {
    /// Multiplies each wallpaper channel by `numerator / denominator`.
    pub fn new(numerator: u16, denominator: u16) -> Option<Self> {
        (denominator > 0 && numerator <= denominator).then_some(Self {
            numerator,
            denominator,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresentationFrame {
    background: Option<RgbaFrame>,
    layout: OverlayLayout,
    /// Anchored to the top edge; drawn first.
    top: Option<AnchoredOverlay>,
    /// Anchored to the bottom edge; drawn over the top overlay.
    bottom: Option<AnchoredOverlay>,
    status: Option<AnchoredOverlay>,
    dimming: Option<Dimming>,
}

impl PresentationFrame {
    /// A wallpaper with up to two overlays for the authentication output.
    /// Every other output shows the wallpaper only. The wallpaper is not
    /// dimmed unless [`Self::with_dimming`] supplies a factor.
    pub fn new(
        background: Option<RgbaFrame>,
        layout: OverlayLayout,
        top: Option<AnchoredOverlay>,
        bottom: Option<AnchoredOverlay>,
    ) -> Self {
        Self {
            background,
            layout,
            top,
            bottom,
            status: None,
            dimming: None,
        }
    }

    /// Dims the wallpaper of the authentication output by `dimming`.
    pub fn with_dimming(mut self, dimming: Option<Dimming>) -> Self {
        self.dimming = dimming;
        self
    }

    pub fn with_status(mut self, status: Option<AnchoredOverlay>) -> Self {
        self.status = status;
        self
    }

    fn overlays(&self) -> [Option<&AnchoredOverlay>; 3] {
        [
            self.top.as_ref(),
            self.bottom.as_ref(),
            self.status.as_ref(),
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refresh {
    Unchanged,
    /// The background or presentation geometry changed.
    Frame,
    /// Only overlay pixels changed. The runtime promotes this to a full redraw
    /// if the canvas or overlay bounds differ from the preceding frame.
    Overlay,
    Failed,
}

#[derive(PartialEq, Eq)]
pub enum Input {
    Keyboard(keyboard::KeyboardStatus),
    Text(Zeroizing<String>),
    Backspace,
    Submit,
    Cancel,
    NextPage,
}

pub trait Presentation {
    /// Receives latency-sensitive presentation changes. This is polled every
    /// dispatch and must own authentication progress.
    fn receive_latest(&mut self) -> Refresh;
    /// Receives media updates that may be deferred while a single output's
    /// latency-sensitive redraw is blocked.
    fn receive_deferred(&mut self) -> Refresh {
        Refresh::Unchanged
    }
    fn frame(&self) -> Option<PresentationFrame>;
    fn lock_confirmed(&mut self) {}
    /// Applies user input and reports whether overlay pixels changed.
    /// Geometry changes are detected and promoted to full redraws by the
    /// runtime.
    fn input(&mut self, _input: Input) -> bool {
        false
    }
    fn take_authorization(&mut self) -> bool {
        false
    }
}

/// Render a presentation as the authentication output would show it, into
/// `width`×`height` RGBA pixels at an integer buffer `scale`, without
/// acquiring a session lock.
///
/// This is intended for deterministic visual previews. Production lock
/// surfaces are rendered directly into compositor-owned shared-memory buffers.
pub fn render_preview(
    frame: &PresentationFrame,
    width: u32,
    height: u32,
    scale: u32,
) -> Result<RgbaFrame, PreviewError> {
    runtime::render_preview(frame, width, height, scale)
}

pub struct Config {
    wayland: UnixStream,
    identity: Identity,
    presentation: Box<dyn Presentation>,
    ready_fds: Vec<OwnedFd>,
    authentication_output: Option<String>,
    #[cfg(feature = "lock-test")]
    test_unlock_after_ready: bool,
    #[cfg(feature = "lock-test")]
    test_unlock_delay: Duration,
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
            authentication_output: None,
            #[cfg(feature = "lock-test")]
            test_unlock_after_ready: false,
            #[cfg(feature = "lock-test")]
            test_unlock_delay: Duration::from_secs(5),
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

    pub fn with_authentication_output(mut self, output: Option<String>) -> Self {
        self.authentication_output = output;
        self
    }

    #[cfg(feature = "lock-test")]
    pub fn with_test_unlock_after_ready(mut self, enabled: bool) -> Self {
        self.test_unlock_after_ready = enabled;
        self
    }

    #[cfg(feature = "lock-test")]
    pub fn with_test_unlock_delay(mut self, delay: Duration) -> Self {
        self.test_unlock_delay = delay;
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
    fn presentation_frames_share_backgrounds_and_validate_layout() {
        let background = RgbaFrame::new(2, 2, Bytes::from_static(&[0; 16])).unwrap();
        let background_pixels = background.pixels().as_ptr();
        let overlay = RgbaFrame::new(1, 1, Bytes::from_static(&[0; 4])).unwrap();
        let layout = OverlayLayout::new(1).unwrap();

        let frame = PresentationFrame::new(
            Some(background.clone()),
            layout,
            AnchoredOverlay::new(overlay.clone(), 3),
            None,
        );
        assert_eq!(
            frame.background.as_ref().unwrap().pixels().as_ptr(),
            background_pixels,
            "sharing a presentation must not copy the background pixels"
        );
        assert_eq!(frame.dimming, None, "wallpaper is undimmed by default");
        assert!(OverlayLayout::new(0).is_none());
        assert!(OverlayLayout::new(OverlayLayout::MAX_DENSITY + 1).is_none());
        assert!(AnchoredOverlay::new(overlay, MAX_LOGICAL_LENGTH + 1).is_none());
        assert!(Dimming::new(1, 0).is_none());
        assert!(Dimming::new(6, 5).is_none());
        let fit = layout.with_top_fit(u32::MAX, u32::MAX, 500);
        assert_eq!(
            (fit.gap, fit.margin, fit.min_top_scale_percent),
            (MAX_LOGICAL_LENGTH, MAX_LOGICAL_LENGTH, 100)
        );
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
