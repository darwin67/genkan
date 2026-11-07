use std::fs::File;
use std::io::Write;
use std::os::fd::OwnedFd;
use std::time::{Duration, Instant};

use bytes::Bytes;
use calloop::EventLoop;
use calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::presentation_time::{
    PresentTime, PresentationTimeHandler, PresentationTimeState,
};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::registry_handlers;
use smithay_client_toolkit::seat::keyboard::{
    KeyEvent, KeyboardHandler, Keymap, Keysym, Modifiers, RawModifiers,
};
#[cfg(feature = "lock-test")]
use smithay_client_toolkit::seat::pointer::{PointerEvent, PointerHandler};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::session_lock::{
    SessionLock, SessionLockHandler, SessionLockState, SessionLockSurface,
    SessionLockSurfaceConfigure,
};
use smithay_client_toolkit::shm::{
    slot::{Buffer, SlotPool},
    Shm, ShmHandler,
};
use thiserror::Error;
use wayland_client::globals::registry_queue_init;
#[cfg(feature = "lock-test")]
use wayland_client::protocol::wl_pointer;
use wayland_client::protocol::{wl_keyboard, wl_output, wl_region, wl_seat, wl_shm, wl_surface};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};
use wayland_protocols::wp::presentation_time::client::wp_presentation_feedback;

use super::{
    Action, AnchoredOverlay, Config, Dimming, Event, Input, OverlayLayout, Presentation,
    PresentationFrame, PreviewError, Refresh, RgbaFrame, State, UnlockAuthorization,
};
use crate::scaling::{
    wp_fractional_scale_manager_v1, wp_fractional_scale_v1, wp_viewport, wp_viewporter, Scaling,
    SurfaceScale,
};

const FRAME_INTERVAL: Duration = Duration::from_millis(16);
const FALLBACK_RGB: [u8; 3] = [5, 9, 24];
const MAX_SURFACE_PIXELS: usize = 16_384 * 16_384;
const MAX_SURFACE_DIMENSION: u32 = 16_384;
const BUFFER_COUNT: usize = 2;
const MAX_BUFFER_BYTES: usize = 256 * 1024 * 1024;
const AUTHENTICATION_RETIREMENT_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Debug, Error)]
pub enum Error {
    #[error("could not connect to the Wayland compositor: {0}")]
    Connect(String),
    #[error("required Wayland protocol is unavailable: {0}")]
    MissingProtocol(String),
    #[error("could not initialize the lock runtime: {0}")]
    Runtime(String),
    #[error("the compositor denied or terminated the session lock")]
    LockFinished,
}

struct Surface {
    output: wl_output::WlOutput,
    output_name: Option<String>,
    lock_surface: SessionLockSurface,
    size: Option<(u32, u32)>,
    scale: SurfaceScale,
    geometry_generation: u64,
    role_generation: u64,
    buffers: Vec<SurfaceBuffer>,
    /// The top and bottom overlays scaled to their places on this surface.
    overlay_cache: [Option<CachedOverlay>; 3],
    authentication: bool,
    redraw: RedrawState,
    first_presented: bool,
}

struct CachedOverlay {
    source: RgbaFrame,
    size: (u32, u32),
    frame: RgbaFrame,
}

struct SurfaceBuffer {
    size: (u32, u32),
    geometry_generation: u64,
    role_generation: u64,
    background_generation: u64,
    buffer: Buffer,
    pool: SlotPool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthenticationRetirement {
    AwaitingCommit { deadline: Instant },
    AwaitingPresentation { deadline: Instant },
}

impl AuthenticationRetirement {
    fn begin(now: Instant) -> Self {
        Self::AwaitingCommit {
            deadline: now + AUTHENTICATION_RETIREMENT_TIMEOUT,
        }
    }

    fn committed(&mut self) {
        if let Self::AwaitingCommit { deadline } = *self {
            *self = Self::AwaitingPresentation { deadline };
        }
    }

    fn discarded(&mut self) {
        if let Self::AwaitingPresentation { deadline } = *self {
            *self = Self::AwaitingCommit { deadline };
        }
    }

    fn accepts_presentation(self) -> bool {
        matches!(self, Self::AwaitingPresentation { .. })
    }

    fn expired(self, now: Instant) -> bool {
        match self {
            Self::AwaitingCommit { deadline } | Self::AwaitingPresentation { deadline } => {
                now >= deadline
            }
        }
    }
}

/// An overlay scaled to its place on one surface.
#[derive(Debug, Clone)]
struct PlacedOverlay {
    region: Region,
    /// Exactly `region.width` by `region.height` pixels.
    pixels: RgbaFrame,
    backdrop: Option<(super::material::Backdrop, (u32, u32))>,
}

impl Surface {
    fn buffer_size(&self, logical: (u32, u32)) -> Result<(u32, u32), Error> {
        let (width, height) = self
            .scale
            .buffer_size(logical)
            .ok_or_else(|| Error::Runtime("compositor lock-surface dimensions overflow".into()))?;
        buffer_size(width, height, 1, wl_output::Transform::Normal)
    }

    fn placed_overlays(
        &mut self,
        frame: Option<&PresentationFrame>,
        width: u32,
        height: u32,
        scale: u32,
    ) -> [Option<PlacedOverlay>; 3] {
        if !self.authentication {
            self.overlay_cache = [None, None, None];
            return [None, None, None];
        }
        let Some(frame) = frame else {
            return [None, None, None];
        };
        let regions = overlay_regions_at_scale(frame, width, height, scale);
        let overlays = frame.overlays();
        let caches = &mut self.overlay_cache;
        std::array::from_fn(|index| {
            let (region, anchored) = (regions[index]?, overlays[index]?);
            let size = (region.width, region.height);
            let cache = &mut caches[index];
            if cache
                .as_ref()
                .is_none_or(|cache| !cache.matches(&anchored.overlay, size))
            {
                *cache = Some(CachedOverlay {
                    source: anchored.overlay.clone(),
                    size,
                    frame: scale_overlay(&anchored.overlay, region.width, region.height),
                });
            }
            cache.as_ref().map(|cache| PlacedOverlay {
                region,
                pixels: cache.frame.clone(),
                backdrop: anchored
                    .backdrop
                    .map(|backdrop| (backdrop, anchored.overlay.dimensions())),
            })
        })
    }
}

impl CachedOverlay {
    fn matches(&self, source: &RgbaFrame, size: (u32, u32)) -> bool {
        self.size == size
            && self.source.dimensions() == source.dimensions()
            && self.source.pixels.len() == source.pixels.len()
            && self.source.pixels.as_ptr() == source.pixels.as_ptr()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RedrawKind {
    Overlay,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OverlayProgress {
    None,
    Partial,
    Full,
    Blocked,
}

/// Everything about a presentation frame that decides where its overlays sit
/// and how its wallpaper is drawn, but not the overlay pixels themselves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FrameGeometry {
    layout: OverlayLayout,
    dimming: Option<Dimming>,
    /// Width, height and edge offset of the top and bottom overlays.
    overlays: [Option<(u32, u32, u32)>; 3],
}

#[derive(Default)]
struct RedrawState {
    frame_pending: bool,
    redraw_pending: Option<RedrawKind>,
    geometry_pending: bool,
    role_pending: bool,
}

impl RedrawState {
    fn request(&mut self, kind: RedrawKind) -> bool {
        if self.redraw_pending != Some(RedrawKind::Full) {
            self.redraw_pending = Some(kind);
        }
        !self.frame_pending
    }

    fn request_geometry(&mut self) {
        self.redraw_pending = Some(RedrawKind::Full);
        self.geometry_pending = true;
    }

    fn request_role_change(&mut self) {
        self.redraw_pending = Some(RedrawKind::Full);
        self.geometry_pending = true;
        self.role_pending = true;
    }

    fn request_media(&mut self) -> bool {
        if self.redraw_pending == Some(RedrawKind::Overlay) {
            false
        } else {
            self.request(RedrawKind::Full)
        }
    }

    fn should_render(&self, current_buffer_count: usize, reusable: bool) -> bool {
        self.redraw_pending.is_some()
            && (self.geometry_pending
                || (!self.frame_pending && redraw_can_progress(current_buffer_count, reusable)))
    }

    fn request_frame_callback(&self) -> bool {
        !self.frame_pending
    }

    fn committed(&mut self, requested_frame_callback: bool) {
        self.redraw_pending = None;
        self.geometry_pending = false;
        self.role_pending = false;
        self.frame_pending |= requested_frame_callback;
    }

    fn frame_done(&mut self) {
        self.frame_pending = false;
    }
}

fn overlay_progress(
    redraw: &RedrawState,
    buffer_count: usize,
    reusable: bool,
    current_background_reusable: bool,
    include_pending_full: bool,
) -> OverlayProgress {
    match redraw.redraw_pending {
        Some(RedrawKind::Overlay) => {}
        Some(RedrawKind::Full) if include_pending_full => {
            return if redraw.should_render(buffer_count, reusable) {
                OverlayProgress::Full
            } else {
                OverlayProgress::Blocked
            };
        }
        Some(RedrawKind::Full) | None => return OverlayProgress::None,
    }
    if !redraw.should_render(buffer_count, reusable) {
        return OverlayProgress::Blocked;
    }
    if redraw.geometry_pending || !current_background_reusable {
        OverlayProgress::Full
    } else {
        OverlayProgress::Partial
    }
}

#[derive(Default)]
struct OverlayProgressSummary {
    partial: bool,
    full: bool,
    blocked: bool,
    unblocked: bool,
}

fn should_poll_deferred(priority: Refresh, progress: &OverlayProgressSummary) -> bool {
    match priority {
        Refresh::Unchanged => {
            if progress.blocked {
                progress.unblocked
            } else {
                !progress.partial || progress.full
            }
        }
        Refresh::Overlay => progress.full || (progress.blocked && progress.unblocked),
        Refresh::Frame | Refresh::Failed => false,
    }
}

fn should_allow_overlay_full(poll_deferred: bool, progress: &OverlayProgressSummary) -> bool {
    poll_deferred || progress.full
}

fn select_reusable_buffer(
    requested: RedrawKind,
    background_generation: u64,
    reusable: &[(usize, u64)],
) -> Option<usize> {
    if requested == RedrawKind::Overlay {
        reusable
            .iter()
            .find_map(|(index, generation)| {
                (*generation == background_generation).then_some(*index)
            })
            .or_else(|| reusable.first().map(|(index, _)| *index))
    } else {
        reusable.first().map(|(index, _)| *index)
    }
}

struct Runtime {
    conn: Connection,
    compositor: CompositorState,
    scaling: Scaling,
    output_state: OutputState,
    output_names: genkan_output_selection::names::OutputNames,
    presentation_time_state: PresentationTimeState,
    registry_state: RegistryState,
    seat_state: SeatState,
    keyboards: Vec<(wl_seat::WlSeat, wl_keyboard::WlKeyboard)>,
    keyboard_layouts: std::collections::HashMap<wayland_client::backend::ObjectId, Vec<String>>,
    keyboard_states: super::keyboard::KeyboardStates<wayland_client::backend::ObjectId>,
    #[cfg(feature = "lock-test")]
    pointers: Vec<(wl_seat::WlSeat, wl_pointer::WlPointer)>,
    shm: Shm,
    session_lock_state: SessionLockState,
    session_lock: Option<SessionLock>,
    surfaces: Vec<Surface>,
    state: State,
    presentation: Box<dyn Presentation>,
    presentation_geometry: Option<FrameGeometry>,
    background_generation: u64,
    authentication_output: Option<String>,
    retiring_authentication: Option<wl_output::WlOutput>,
    authentication_retirement: Option<AuthenticationRetirement>,
    authentication_retirement_feedback: Option<wp_presentation_feedback::WpPresentationFeedback>,
    ready: ReadySignal,
    failure: Option<Error>,
    terminate: bool,
    #[cfg(feature = "lock-test")]
    test_unlock_after_ready: bool,
    #[cfg(feature = "lock-test")]
    test_unlock_at: Option<Instant>,
    #[cfg(feature = "lock-test")]
    test_unlock_delay: Duration,
    #[cfg(feature = "lock-test")]
    test_observer: TestObserver,
    #[cfg(feature = "lock-test")]
    test_panic_after_ready: bool,
    #[cfg(feature = "lock-test")]
    test_renderer_failure_after_ready: bool,
    #[cfg(feature = "lock-test")]
    test_ready_delay: Duration,
}

pub(super) fn run(config: Config) -> Result<(), Error> {
    let ready = ReadySignal::new(config.ready_fds);
    let conn = Connection::from_socket(config.wayland)
        .map_err(|error| Error::Connect(error.to_string()))?;
    let (globals, mut event_queue) =
        registry_queue_init(&conn).map_err(|error| Error::Runtime(error.to_string()))?;
    let qh = event_queue.handle();
    let compositor = CompositorState::bind(&globals, &qh)
        .map_err(|error| Error::MissingProtocol(format!("wl_compositor ({error})")))?;
    let shm = Shm::bind(&globals, &qh)
        .map_err(|error| Error::MissingProtocol(format!("wl_shm ({error})")))?;
    let presentation_geometry = frame_geometry(config.presentation.frame().as_ref());
    let mut runtime = Runtime {
        conn: conn.clone(),
        compositor,
        scaling: Scaling::bind(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        output_names: genkan_output_selection::names::OutputNames::new(
            &conn,
            &globals,
            config.authentication_output.is_some(),
        ),
        presentation_time_state: PresentationTimeState::bind(&globals, &qh),
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        keyboards: Vec::new(),
        keyboard_layouts: std::collections::HashMap::new(),
        keyboard_states: Default::default(),
        #[cfg(feature = "lock-test")]
        pointers: Vec::new(),
        shm,
        session_lock_state: SessionLockState::new(&globals, &qh),
        session_lock: None,
        surfaces: Vec::new(),
        state: State::new(config.identity),
        presentation: config.presentation,
        presentation_geometry,
        background_generation: 1,
        authentication_output: config.authentication_output,
        retiring_authentication: None,
        authentication_retirement: None,
        authentication_retirement_feedback: None,
        ready,
        failure: None,
        terminate: false,
        #[cfg(feature = "lock-test")]
        test_unlock_after_ready: config.test_unlock_after_ready,
        #[cfg(feature = "lock-test")]
        test_unlock_at: None,
        #[cfg(feature = "lock-test")]
        test_unlock_delay: config.test_unlock_delay,
        #[cfg(feature = "lock-test")]
        test_observer: TestObserver::new(config.test_observer),
        #[cfg(feature = "lock-test")]
        test_panic_after_ready: config.test_panic_after_ready,
        #[cfg(feature = "lock-test")]
        test_renderer_failure_after_ready: config.test_renderer_failure_after_ready,
        #[cfg(feature = "lock-test")]
        test_ready_delay: config.test_ready_delay,
    };
    eprintln!(
        "genkan lock: requesting compositor lock for uid {} ({}; {})",
        runtime.state.identity.uid,
        runtime.state.identity.username,
        runtime.state.identity.display_name
    );

    event_queue
        .roundtrip(&mut runtime)
        .map_err(|error| Error::Runtime(error.to_string()))?;
    if !runtime.shm.formats().contains(&wl_shm::Format::Argb8888) {
        return Err(Error::MissingProtocol("wl_shm ARGB8888 format".into()));
    }
    let outputs = runtime.output_state.outputs().collect::<Vec<_>>();
    if outputs.is_empty() {
        return Err(Error::MissingProtocol("wl_output".into()));
    }
    let lock = runtime.session_lock_state.lock(&qh).map_err(|error| {
        Error::MissingProtocol(format!("ext_session_lock_manager_v1 ({error})"))
    })?;
    runtime.session_lock = Some(lock);
    for output in outputs {
        runtime.add_surface(output, &qh)?;
    }

    let mut event_loop: EventLoop<'static, Runtime> =
        EventLoop::try_new().map_err(|error| Error::Runtime(error.to_string()))?;
    WaylandSource::new(conn, event_queue)
        .insert(event_loop.handle())
        .map_err(|error| Error::Runtime(error.error.to_string()))?;

    while !runtime.terminate {
        if let Err(error) = event_loop.dispatch(FRAME_INTERVAL, &mut runtime) {
            runtime.fail(Error::Runtime(error.to_string()));
            break;
        }
        if runtime.terminate {
            break;
        }
        if let Err(error) = runtime.reconcile_outputs(&qh) {
            runtime.fail(error);
            break;
        }
        runtime.expire_authentication_retirement();
        let priority = refresh_priority(runtime.presentation.as_mut(), &mut runtime.state);
        if runtime.presentation.take_authorization() {
            let action = runtime
                .state
                .authorize_unlock(UnlockAuthorization::authenticated());
            runtime.apply(action);
        }
        if runtime.terminate {
            break;
        }
        runtime.apply_refresh(priority);
        let progress = match runtime.commit_partial_overlays(&qh) {
            Ok(progress) => progress,
            Err(error) => {
                runtime.fail(error);
                break;
            }
        };
        // A media frame may be adopted once every input update can either be
        // included in one coalesced full repaint or has already been committed
        // as a partial repaint. A callback- or buffer-blocked output retains
        // overlay priority across dispatch iterations.
        let poll_deferred = should_poll_deferred(priority, &progress);
        if poll_deferred {
            let refresh = refresh_presentation(runtime.presentation.as_mut(), &mut runtime.state);
            runtime.apply_refresh(refresh);
        }
        if let Err(error) =
            runtime.maintain_surfaces(&qh, should_allow_overlay_full(poll_deferred, &progress))
        {
            runtime.fail(error);
            break;
        }
        #[cfg(feature = "lock-test")]
        runtime.advance_test_unlock();
        if runtime.terminate {
            break;
        }
    }

    runtime.failure.map_or(Ok(()), Err)
}

impl Runtime {
    fn reconcile_outputs(&mut self, qh: &QueueHandle<Self>) -> Result<(), Error> {
        if self.terminate {
            return Ok(());
        }
        if self.output_names.update() {
            self.refresh_authentication_output();
        }
        let outputs = self.output_state.outputs().collect::<Vec<_>>();
        for output in outputs {
            self.add_surface(output, qh)?;
        }
        Ok(())
    }

    fn add_surface(
        &mut self,
        output: wl_output::WlOutput,
        qh: &QueueHandle<Self>,
    ) -> Result<(), Error> {
        if self.surfaces.iter().any(|surface| surface.output == output) {
            return Ok(());
        }
        let lock = self
            .session_lock
            .as_ref()
            .ok_or_else(|| Error::Runtime("lock surface requested without a lock".into()))?;
        let surface = self.compositor.create_surface(qh);
        let scale = self.scaling.create(&surface, qh);
        let lock_surface = lock.create_lock_surface(surface, &output, qh);
        eprintln!(
            "genkan lock: created lock surface for output {}",
            output.id().protocol_id()
        );
        let output_name = self.output_state.info(&output).and_then(|info| info.name);
        self.surfaces.push(Surface {
            output,
            output_name,
            lock_surface,
            size: None,
            scale,
            geometry_generation: 0,
            role_generation: 0,
            buffers: Vec::with_capacity(BUFFER_COUNT),
            overlay_cache: [None, None, None],
            authentication: false,
            redraw: RedrawState {
                redraw_pending: Some(RedrawKind::Full),
                ..RedrawState::default()
            },
            first_presented: false,
        });
        self.refresh_authentication_output();
        #[cfg(feature = "lock-test")]
        self.test_observer.record(TestEvent::OutputAdded);
        Ok(())
    }

    fn redraw_media(&mut self) {
        if self.terminate {
            return;
        }
        for surface in &mut self.surfaces {
            if surface.size.is_some() {
                surface.redraw.request_media();
            }
        }
    }

    fn maintain_surfaces(
        &mut self,
        qh: &QueueHandle<Self>,
        allow_overlay_full: bool,
    ) -> Result<(), Error> {
        let mut ready_to_redraw = Vec::new();
        for (index, surface) in self.surfaces.iter_mut().enumerate() {
            let Some((width, height)) = surface.size else {
                continue;
            };
            let configured_size = surface.buffer_size((width, height))?;
            surface.buffers.retain(|item| {
                item.size == configured_size
                    && item.geometry_generation == surface.geometry_generation
                    && item.role_generation == surface.role_generation
            });
            let mut reusable = false;
            let mut current = false;
            for buffer in &mut surface.buffers {
                if buffer.size == configured_size && buffer.pool.canvas(&buffer.buffer).is_some() {
                    reusable = true;
                    current |= buffer.background_generation == self.background_generation;
                }
            }
            let progress = overlay_progress(
                &surface.redraw,
                surface.buffers.len(),
                reusable,
                current,
                false,
            );
            if surface
                .redraw
                .should_render(surface.buffers.len(), reusable)
                && (allow_overlay_full || progress != OverlayProgress::Full)
            {
                ready_to_redraw.push(index);
            }
        }
        ready_to_redraw.sort_by_key(|index| self.surfaces[*index].authentication);
        for index in ready_to_redraw {
            self.render(index, qh)?;
        }
        Ok(())
    }

    fn commit_partial_overlays(
        &mut self,
        qh: &QueueHandle<Self>,
    ) -> Result<OverlayProgressSummary, Error> {
        let mut partial = Vec::new();
        let mut summary = OverlayProgressSummary::default();
        for (index, surface) in self.surfaces.iter_mut().enumerate() {
            let Some((width, height)) = surface.size else {
                continue;
            };
            let configured_size = surface.buffer_size((width, height))?;
            surface.buffers.retain(|buffer| {
                buffer.size == configured_size
                    && buffer.geometry_generation == surface.geometry_generation
                    && buffer.role_generation == surface.role_generation
            });
            let mut reusable = false;
            let mut current = false;
            for buffer in &mut surface.buffers {
                if buffer.pool.canvas(&buffer.buffer).is_some() {
                    reusable = true;
                    current |= buffer.background_generation == self.background_generation;
                }
            }
            match overlay_progress(
                &surface.redraw,
                surface.buffers.len(),
                reusable,
                current,
                true,
            ) {
                OverlayProgress::None => summary.unblocked = true,
                OverlayProgress::Partial => {
                    summary.unblocked = true;
                    summary.partial = true;
                    partial.push(index);
                }
                OverlayProgress::Full => {
                    summary.unblocked = true;
                    summary.full = true;
                }
                OverlayProgress::Blocked => summary.blocked = true,
            }
        }
        for index in partial {
            self.render(index, qh)?;
        }
        Ok(summary)
    }

    fn apply_refresh(&mut self, refresh: Refresh) {
        match refresh {
            Refresh::Unchanged => {}
            Refresh::Frame => {
                record_full_frame(
                    &mut self.presentation_geometry,
                    &mut self.background_generation,
                    frame_geometry(self.presentation.frame().as_ref()),
                );
                self.redraw_media();
            }
            Refresh::Overlay => {
                let kind = self.classify_overlay_redraw();
                self.redraw_all_surfaces(kind);
            }
            Refresh::Failed => {
                eprintln!("genkan lock: presentation failed; retaining opaque fallback");
                record_full_frame(
                    &mut self.presentation_geometry,
                    &mut self.background_generation,
                    frame_geometry(self.presentation.frame().as_ref()),
                );
                self.redraw_media();
            }
        }
    }

    fn render(&mut self, index: usize, qh: &QueueHandle<Self>) -> Result<(), Error> {
        let surface = &self.surfaces[index];
        let (width, height) = surface
            .size
            .ok_or_else(|| Error::Runtime("attempted to render an unconfigured surface".into()))?;
        let scale = surface.scale.units();
        let (buffer_width, buffer_height) = surface.buffer_size((width, height))?;
        let bytes = buffer_width
            .checked_mul(buffer_height)
            .and_then(|pixels| usize::try_from(pixels).ok())
            .filter(|pixels| *pixels <= MAX_SURFACE_PIXELS)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or_else(|| {
                Error::Runtime("compositor requested an unsafe lock-surface size".into())
            })?;
        if bytes > MAX_BUFFER_BYTES {
            return Err(Error::Runtime(
                "compositor requested a lock buffer larger than the resource budget".into(),
            ));
        }
        let geometry_generation = surface.geometry_generation;
        let role_generation = surface.role_generation;
        let requested_redraw = surface.redraw.redraw_pending.unwrap_or(RedrawKind::Full);
        let wl_surface = surface.lock_surface.wl_surface().clone();
        let output = surface.output.clone();
        let output_id = surface.output.id().protocol_id();
        let output_name = surface.output_name.clone();
        let frame = self.presentation.frame();
        let background_generation = self.background_generation;
        let surface = &mut self.surfaces[index];
        let authentication = surface.authentication;
        let role_change = surface.redraw.role_pending;
        let retiring = role_change
            && !authentication
            && self.retiring_authentication.as_ref() == Some(&output);
        let placed = surface.placed_overlays(frame.as_ref(), buffer_width, buffer_height, scale);
        surface.buffers.retain(|item| {
            item.size == (buffer_width, buffer_height)
                && item.geometry_generation == geometry_generation
                && item.role_generation == role_generation
        });
        let reusable = surface
            .buffers
            .iter_mut()
            .enumerate()
            .filter_map(|(index, item)| {
                (item.size == (buffer_width, buffer_height)
                    && item.pool.canvas(&item.buffer).is_some())
                .then_some((index, item.background_generation))
            })
            .collect::<Vec<_>>();
        let reusable = select_reusable_buffer(requested_redraw, background_generation, &reusable);
        let mut damaged = vec![Region::full(buffer_width, buffer_height)];
        let buffer_index = if let Some(buffer_index) = reusable {
            let SurfaceBuffer {
                background_generation: rendered_background,
                buffer,
                pool,
                ..
            } = &mut surface.buffers[buffer_index];
            let canvas = pool
                .canvas(buffer)
                .expect("buffer release state changed without dispatch");
            let full_redraw = requested_redraw == RedrawKind::Full
                || *rendered_background != background_generation;
            damaged = redraw_regions(
                requested_redraw,
                *rendered_background,
                background_generation,
                &placed,
                buffer_width,
                buffer_height,
            );
            if !full_redraw {
                for region in &damaged {
                    draw_opaque_region(
                        canvas,
                        buffer_width,
                        buffer_height,
                        frame.as_ref(),
                        &placed,
                        authentication,
                        *region,
                    );
                }
            } else {
                draw_opaque_placed(
                    canvas,
                    buffer_width,
                    buffer_height,
                    frame.as_ref(),
                    &placed,
                    authentication,
                );
                *rendered_background = background_generation;
            }
            buffer_index
        } else {
            if surface.buffers.len() >= BUFFER_COUNT {
                return Ok(());
            }
            let capacity = aligned_buffer_capacity(bytes)?;
            let mut pool = SlotPool::new(capacity, &self.shm).map_err(|error| {
                Error::Runtime(format!("could not allocate lock buffer pool: {error}"))
            })?;
            let (buffer, canvas) = pool
                .create_buffer(
                    buffer_width as i32,
                    buffer_height as i32,
                    (buffer_width * 4) as i32,
                    wl_shm::Format::Argb8888,
                )
                .map_err(|error| {
                    Error::Runtime(format!("could not allocate lock buffer: {error}"))
                })?;
            draw_opaque_placed(
                canvas,
                buffer_width,
                buffer_height,
                frame.as_ref(),
                &placed,
                authentication,
            );
            surface.buffers.push(SurfaceBuffer {
                size: (buffer_width, buffer_height),
                geometry_generation,
                role_generation,
                background_generation,
                buffer,
                pool,
            });
            surface.buffers.len() - 1
        };
        if retiring {
            match self.presentation_time_state.feedback(&wl_surface, qh) {
                Ok(feedback) => self.authentication_retirement_feedback = Some(feedback),
                Err(error) => {
                    eprintln!(
                        "genkan lock: cannot safely migrate authentication without presentation feedback: {error}"
                    );
                    surface.authentication = true;
                    surface.overlay_cache = [None, None, None];
                    surface.role_generation = surface.role_generation.wrapping_add(1);
                    surface.redraw.request_role_change();
                    self.retiring_authentication = None;
                    self.authentication_retirement = None;
                    self.authentication_retirement_feedback = None;
                    return Ok(());
                }
            }
        }
        surface.buffers[buffer_index]
            .buffer
            .attach_to(&wl_surface)
            .map_err(|error| Error::Runtime(format!("could not attach lock buffer: {error}")))?;
        let region = self.compositor.wl_compositor().create_region(qh, ());
        region.add(0, 0, width as i32, height as i32);
        wl_surface.set_opaque_region(Some(&region));
        region.destroy();
        surface.scale.apply(&wl_surface, (width, height));
        wl_surface.set_buffer_transform(wl_output::Transform::Normal);
        for region in &damaged {
            wl_surface.damage_buffer(
                region.x as i32,
                region.y as i32,
                region.width as i32,
                region.height as i32,
            );
        }
        let request_frame_callback = surface.redraw.request_frame_callback();
        if request_frame_callback {
            wl_surface.frame(qh, wl_surface.clone());
        }
        wl_surface.commit();
        surface.redraw.committed(request_frame_callback);
        if retiring {
            if let Some(retirement) = &mut self.authentication_retirement {
                retirement.committed();
            }
        }
        if role_change {
            eprintln!(
                "genkan lock: committed {} role for output {}",
                if authentication {
                    "authentication"
                } else {
                    "wallpaper"
                },
                output_name.as_deref().unwrap_or("<unnamed>")
            );
        }
        if !surface.first_presented {
            eprintln!("genkan lock: committed first opaque buffer for output {output_id}");
            surface.first_presented = true;
        }
        Ok(())
    }

    fn apply(&mut self, action: Action) {
        match action {
            Action::None => {}
            Action::ReportReady => {
                eprintln!("genkan lock: compositor confirmed lock");
                #[cfg(feature = "lock-test")]
                self.test_observer.record(TestEvent::Locked);
                #[cfg(feature = "lock-test")]
                std::thread::sleep(self.test_ready_delay);
                if let Err(error) = self.ready.apply(action) {
                    self.fail(Error::Runtime(format!(
                        "could not report lock readiness: {error}"
                    )));
                }
                if !self.terminate {
                    self.presentation.lock_confirmed();
                    record_full_frame(
                        &mut self.presentation_geometry,
                        &mut self.background_generation,
                        frame_geometry(self.presentation.frame().as_ref()),
                    );
                    self.redraw_all_surfaces(RedrawKind::Full);
                }
                #[cfg(feature = "lock-test")]
                if self.test_unlock_after_ready && !self.terminate {
                    self.test_unlock_at = Some(Instant::now() + self.test_unlock_delay);
                }
                #[cfg(feature = "lock-test")]
                if self.test_renderer_failure_after_ready && !self.terminate {
                    self.fail(Error::Runtime("injected renderer failure".into()));
                }
                #[cfg(feature = "lock-test")]
                if self.test_panic_after_ready && !self.terminate {
                    panic!("injected session-lock panic");
                }
            }
            Action::Abort => {
                self.fail(Error::LockFinished);
            }
            Action::UnlockAndSynchronize => {
                eprintln!("genkan lock: authentication accepted; unlocking");
                if let Some(lock) = self.session_lock.take() {
                    lock.unlock();
                    if let Err(error) = self.conn.roundtrip() {
                        self.fail(Error::Runtime(format!(
                            "could not synchronize authorized unlock: {error}"
                        )));
                    }
                }
                self.terminate = true;
            }
        }
    }

    fn fail(&mut self, error: Error) {
        #[cfg(feature = "lock-test")]
        self.test_observer.record(TestEvent::Failed);
        let _ = self.state.update(Event::RuntimeFailed);
        self.failure.get_or_insert(error);
        self.terminate = true;
    }

    fn redraw_all_surfaces(&mut self, kind: RedrawKind) {
        for surface in &mut self.surfaces {
            if surface.size.is_some() && (kind == RedrawKind::Full || surface.authentication) {
                surface.redraw.request(kind);
            }
        }
    }

    fn keyboard_status(&mut self, status: super::keyboard::KeyboardStatus) {
        if self.presentation.input(Input::Keyboard(status)) {
            let kind = self.classify_overlay_redraw();
            self.redraw_all_surfaces(kind);
        }
    }

    fn refresh_authentication_output(&mut self) {
        let candidates = self
            .surfaces
            .iter()
            .map(|surface| {
                (
                    surface.output_name.as_deref(),
                    surface.size.is_some(),
                    surface.authentication,
                )
            })
            .collect::<Vec<_>>();
        let requested = self
            .output_names
            .resolve(self.authentication_output.as_deref());
        let selected = select_authentication_index(&candidates, requested.as_deref());
        if self.retiring_authentication.is_some() {
            return;
        }
        let current = self
            .surfaces
            .iter()
            .position(|surface| surface.authentication);
        if current == selected {
            return;
        }
        if let Some(current) = current {
            self.retiring_authentication = Some(self.surfaces[current].output.clone());
            self.authentication_retirement = Some(AuthenticationRetirement::begin(Instant::now()));
            self.authentication_retirement_feedback = None;
            self.set_authentication(current, false);
        } else if let Some(selected) = selected {
            self.set_authentication(selected, true);
        }
    }

    fn expire_authentication_retirement(&mut self) {
        if !self
            .authentication_retirement
            .is_some_and(|retirement| retirement.expired(Instant::now()))
        {
            return;
        }
        eprintln!("genkan lock: authentication handoff timed out; restoring the previous output");
        self.authentication_retirement_feedback = None;
        self.authentication_retirement = None;
        let retiring = self.retiring_authentication.take();
        if let Some(index) = retiring.and_then(|output| {
            self.surfaces
                .iter()
                .position(|surface| surface.output == output && surface.size.is_some())
        }) {
            self.set_authentication(index, true);
        } else {
            self.refresh_authentication_output();
        }
    }

    fn set_authentication(&mut self, index: usize, authentication: bool) {
        let surface = &mut self.surfaces[index];
        if surface.authentication == authentication {
            return;
        }
        surface.authentication = authentication;
        surface.overlay_cache = [None, None, None];
        surface.role_generation = surface.role_generation.wrapping_add(1);
        if surface.size.is_some() {
            surface.redraw.request_role_change();
        }
    }

    fn classify_overlay_redraw(&mut self) -> RedrawKind {
        let current = frame_geometry(self.presentation.frame().as_ref());
        let kind = overlay_redraw_kind(self.presentation_geometry, current);
        if kind == RedrawKind::Full {
            self.presentation_geometry = current;
            self.background_generation = self.background_generation.wrapping_add(1);
        }
        kind
    }

    fn handle_key(&mut self, keyboard: &wl_keyboard::WlKeyboard, event: KeyEvent) {
        self.keyboard_states.select(&keyboard.id());
        self.keyboard_status(self.keyboard_states.status().cloned().unwrap_or_default());
        #[cfg(feature = "lock-test")]
        self.test_observer.record(TestEvent::Keyboard);
        let input = match event.keysym {
            Keysym::BackSpace => Some(Input::Backspace),
            Keysym::Return | Keysym::KP_Enter => Some(Input::Submit),
            Keysym::Escape => Some(Input::Cancel),
            Keysym::Tab => Some(Input::NextPage),
            _ => event
                .utf8
                .map(zeroize::Zeroizing::new)
                .filter(|text| !text.chars().any(char::is_control))
                .map(Input::Text),
        };
        if input.is_some_and(|input| self.presentation.input(input)) {
            let kind = self.classify_overlay_redraw();
            self.redraw_all_surfaces(kind);
        }
    }

    #[cfg(feature = "lock-test")]
    fn advance_test_unlock(&mut self) {
        if self.test_unlock_at.is_some_and(|at| Instant::now() >= at) {
            self.test_unlock_at = None;
            let action = self
                .state
                .authorize_unlock(super::UnlockAuthorization::test_source());
            self.apply(action);
        }
    }
}

fn select_authentication_index(
    outputs: &[(Option<&str>, bool, bool)],
    requested: Option<&str>,
) -> Option<usize> {
    let select = |candidates: &[(Option<&str>, bool, bool)]| {
        genkan_output_selection::select(
            candidates
                .iter()
                .enumerate()
                .map(|(index, (name, _, _))| (index, *name)),
            requested,
        )
    };
    let desired = select(outputs);
    if desired.is_some_and(|index| outputs[index].1) {
        return desired;
    }
    outputs
        .iter()
        .position(|(_, configured, authentication)| *configured && *authentication)
        .or_else(|| {
            let configured = outputs
                .iter()
                .map(|(name, configured, authentication)| (*name, *configured, *authentication))
                .collect::<Vec<_>>();
            let selected = genkan_output_selection::select(
                configured
                    .iter()
                    .enumerate()
                    .filter(|(_, (_, configured, _))| *configured)
                    .map(|(index, (name, _, _))| (index, *name)),
                requested,
            );
            selected
        })
}

impl SessionLockHandler for Runtime {
    fn locked(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _lock: SessionLock) {
        let action =
            lock_confirmation_action(&mut self.state, self.failure.is_some() || self.terminate);
        self.apply(action);
    }

    fn finished(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _lock: SessionLock) {
        eprintln!("genkan lock: compositor denied or terminated lock");
        #[cfg(feature = "lock-test")]
        self.test_observer.record(TestEvent::Finished);
        let action = self.state.update(Event::LockFinished);
        self.apply(action);
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        lock_surface: SessionLockSurface,
        configure: SessionLockSurfaceConfigure,
        _serial: u32,
    ) {
        let Some(index) = self
            .surfaces
            .iter()
            .position(|surface| surface.lock_surface.wl_surface() == lock_surface.wl_surface())
        else {
            self.fail(Error::Runtime("configured an unknown lock surface".into()));
            return;
        };
        if self.surfaces[index].size != Some(configure.new_size) {
            let surface = &mut self.surfaces[index];
            surface.size = Some(configure.new_size);
            surface.geometry_generation = surface.geometry_generation.wrapping_add(1);
            surface.redraw.request_geometry();
            #[cfg(feature = "lock-test")]
            self.test_observer.record(TestEvent::Geometry);
        }
        self.refresh_authentication_output();
    }
}

impl CompositorHandler for Runtime {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        scale: i32,
    ) {
        if let Some(index) = self
            .surfaces
            .iter()
            .position(|item| item.lock_surface.wl_surface() == surface)
        {
            let surface = &mut self.surfaces[index];
            if surface.scale.set_integer(scale) {
                surface.geometry_generation = surface.geometry_generation.wrapping_add(1);
                surface.redraw.request_geometry();
                #[cfg(feature = "lock-test")]
                self.test_observer.record(TestEvent::Geometry);
            }
        }
    }

    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _transform: wl_output::Transform,
    ) {
    }

    fn frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
        if self.terminate {
            return;
        }
        let Some(index) = self
            .surfaces
            .iter()
            .position(|item| item.lock_surface.wl_surface() == surface)
        else {
            return;
        };
        self.surfaces[index].redraw.frame_done();
    }

    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for Runtime {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        self.output_names.invalidate();
        if self.session_lock.is_some() && !self.terminate {
            if let Some(surface) = self
                .surfaces
                .iter_mut()
                .find(|surface| surface.output == output)
            {
                surface.output_name = self.output_state.info(&output).and_then(|info| info.name);
                self.refresh_authentication_output();
                return;
            }
            if let Err(error) = self.add_surface(output, qh) {
                self.fail(error);
            }
        }
    }

    fn update_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        self.output_names.invalidate();
        let name = self.output_state.info(&output).and_then(|info| info.name);
        if let Some(surface) = self
            .surfaces
            .iter_mut()
            .find(|surface| surface.output == output)
        {
            surface.output_name = name;
        }
        self.refresh_authentication_output();
    }

    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        self.output_names.invalidate();
        if self.retiring_authentication.as_ref() == Some(&output) {
            self.retiring_authentication = None;
            self.authentication_retirement = None;
            self.authentication_retirement_feedback = None;
        }
        self.surfaces.retain(|surface| surface.output != output);
        self.refresh_authentication_output();
        #[cfg(feature = "lock-test")]
        self.test_observer.record(TestEvent::OutputRemoved);
        eprintln!(
            "genkan lock: removed surface for output {}",
            output.id().protocol_id()
        );
    }
}

impl PresentationTimeHandler for Runtime {
    fn presentation_time_state(&mut self) -> &mut PresentationTimeState {
        &mut self.presentation_time_state
    }

    fn presented(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        feedback: &wp_presentation_feedback::WpPresentationFeedback,
        _surface: &wl_surface::WlSurface,
        _outputs: Vec<wl_output::WlOutput>,
        _time: PresentTime,
        _refresh: u32,
        _seq: u64,
        _flags: WEnum<wp_presentation_feedback::Kind>,
    ) {
        if self.authentication_retirement_feedback.as_ref() != Some(feedback)
            || !self
                .authentication_retirement
                .is_some_and(AuthenticationRetirement::accepts_presentation)
        {
            return;
        }
        self.authentication_retirement_feedback = None;
        self.authentication_retirement = None;
        self.retiring_authentication = None;
        self.refresh_authentication_output();
    }

    fn discarded(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        feedback: &wp_presentation_feedback::WpPresentationFeedback,
        surface: &wl_surface::WlSurface,
    ) {
        if self.authentication_retirement_feedback.as_ref() != Some(feedback) {
            return;
        }
        self.authentication_retirement_feedback = None;
        if let Some(retirement) = &mut self.authentication_retirement {
            retirement.discarded();
        }
        if let Some(surface) = self
            .surfaces
            .iter_mut()
            .find(|candidate| candidate.lock_surface.wl_surface() == surface)
        {
            surface.redraw.request_role_change();
        }
    }
}

impl ProvidesRegistryState for Runtime {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }

    registry_handlers![OutputState, SeatState];
}

impl SeatHandler for Runtime {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _seat: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard
            && !self.keyboards.iter().any(|(known, _)| known == &seat)
        {
            match self.seat_state.get_keyboard(qh, &seat, None) {
                Ok(keyboard) => {
                    #[cfg(feature = "lock-test")]
                    self.test_observer
                        .record(TestEvent::KeyboardAcquired(keyboard.id().protocol_id()));
                    self.keyboards.push((seat.clone(), keyboard));
                }
                Err(error) => self.fail(Error::Runtime(format!(
                    "could not acquire lock keyboard: {error}"
                ))),
            }
        }
        #[cfg(feature = "lock-test")]
        if capability == Capability::Pointer
            && !self.pointers.iter().any(|(known, _)| known == &seat)
        {
            match self.seat_state.get_pointer(qh, &seat) {
                Ok(pointer) => self.pointers.push((seat, pointer)),
                Err(error) => self.fail(Error::Runtime(format!(
                    "could not acquire lock pointer: {error}"
                ))),
            }
        }
    }

    fn remove_capability(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard {
            let mut retained = Vec::new();
            for (known, keyboard) in self.keyboards.drain(..) {
                if known != seat {
                    retained.push((known, keyboard));
                    continue;
                }
                #[cfg(feature = "lock-test")]
                self.test_observer
                    .record(TestEvent::KeyboardReleased(keyboard.id().protocol_id()));
                self.keyboard_layouts.remove(&keyboard.id());
                self.keyboard_states.leave(&keyboard.id());
                keyboard.release();
            }
            self.keyboards = retained;
            self.keyboard_status(self.keyboard_states.status().cloned().unwrap_or_default());
        }
        #[cfg(feature = "lock-test")]
        if capability == Capability::Pointer {
            let mut retained = Vec::new();
            for (known, pointer) in self.pointers.drain(..) {
                if known != seat {
                    retained.push((known, pointer));
                    continue;
                }
                pointer.release();
            }
            self.pointers = retained;
        }
    }

    fn remove_seat(&mut self, conn: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat) {
        self.remove_capability(conn, qh, seat, Capability::Keyboard);
    }
}

impl KeyboardHandler for Runtime {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
        #[cfg(feature = "lock-test")]
        self.test_observer
            .record(TestEvent::KeyboardEnter(_keyboard.id().protocol_id()));
        self.keyboard_states.enter(_keyboard.id());
        self.keyboard_status(self.keyboard_states.status().cloned().unwrap_or_default());
    }
    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
    ) {
        #[cfg(feature = "lock-test")]
        self.test_observer
            .record(TestEvent::KeyboardLeave(_keyboard.id().protocol_id()));
        self.keyboard_states.leave(&_keyboard.id());
        self.keyboard_status(self.keyboard_states.status().cloned().unwrap_or_default());
    }
    fn press_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        keyboard: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        self.handle_key(keyboard, event);
    }
    fn repeat_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        keyboard: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        self.handle_key(keyboard, event);
    }
    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: KeyEvent,
    ) {
    }
    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        keyboard: &wl_keyboard::WlKeyboard,
        _: u32,
        modifiers: Modifiers,
        _: RawModifiers,
        group: u32,
    ) {
        let status = super::keyboard::KeyboardStatus {
            caps_lock: modifiers.caps_lock,
            layout: self
                .keyboard_layouts
                .get(&keyboard.id())
                .and_then(|names| names.get(group as usize))
                .filter(|name| !name.trim().is_empty())
                .cloned(),
        };
        self.keyboard_states.update(&keyboard.id(), Some(status));
        self.keyboard_status(self.keyboard_states.status().cloned().unwrap_or_default());
    }
    fn update_keymap(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        keymap: Keymap<'_>,
    ) {
        self.keyboard_layouts.insert(
            _keyboard.id(),
            super::keyboard::layout_names(keymap.as_string()),
        );
        self.keyboard_states.update(&_keyboard.id(), None);
        self.keyboard_status(self.keyboard_states.status().cloned().unwrap_or_default());
        // SCTK only calls this after it compiled a usable xkb state for the
        // keyboard. A resource that is acquired but never keymapped silently
        // drops every key event, so the fixture needs this to tell the two
        // apart.
        #[cfg(feature = "lock-test")]
        self.test_observer
            .record(TestEvent::KeyboardKeymap(_keyboard.id().protocol_id()));
    }
}

#[cfg(feature = "lock-test")]
impl PointerHandler for Runtime {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        if events.iter().any(|event| {
            matches!(
                event.kind,
                smithay_client_toolkit::seat::pointer::PointerEventKind::Enter { .. }
                    | smithay_client_toolkit::seat::pointer::PointerEventKind::Motion { .. }
                    | smithay_client_toolkit::seat::pointer::PointerEventKind::Press { .. }
                    | smithay_client_toolkit::seat::pointer::PointerEventKind::Release { .. }
                    | smithay_client_toolkit::seat::pointer::PointerEventKind::Axis { .. }
            )
        }) {
            self.test_observer.record(TestEvent::Pointer);
        }
    }
}

impl ShmHandler for Runtime {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

smithay_client_toolkit::delegate_compositor!(Runtime);
smithay_client_toolkit::delegate_output!(Runtime);
smithay_client_toolkit::delegate_presentation_time!(Runtime);
smithay_client_toolkit::delegate_registry!(Runtime);
smithay_client_toolkit::delegate_seat!(Runtime);
smithay_client_toolkit::delegate_keyboard!(Runtime);
#[cfg(feature = "lock-test")]
smithay_client_toolkit::delegate_pointer!(Runtime);
smithay_client_toolkit::delegate_session_lock!(Runtime);
smithay_client_toolkit::delegate_shm!(Runtime);
wayland_client::delegate_noop!(Runtime: ignore wl_region::WlRegion);
wayland_client::delegate_noop!(Runtime: ignore wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1);
wayland_client::delegate_noop!(Runtime: ignore wp_viewporter::WpViewporter);
wayland_client::delegate_noop!(Runtime: ignore wp_viewport::WpViewport);

impl Dispatch<wp_fractional_scale_v1::WpFractionalScaleV1, wl_surface::WlSurface> for Runtime {
    fn event(
        state: &mut Self,
        _: &wp_fractional_scale_v1::WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        surface: &wl_surface::WlSurface,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            if scale == 0 {
                state.fail(Error::Runtime("invalid fractional output scale".into()));
                return;
            }
            if let Some(surface) = state
                .surfaces
                .iter_mut()
                .find(|item| item.lock_surface.wl_surface() == surface)
            {
                if surface.scale.set_preferred(scale) {
                    surface.geometry_generation = surface.geometry_generation.wrapping_add(1);
                    surface.redraw.request_geometry();
                    #[cfg(feature = "lock-test")]
                    state.test_observer.record(TestEvent::Geometry);
                }
            }
        }
    }
}

fn redraw_can_progress(current_buffer_count: usize, reusable: bool) -> bool {
    reusable || current_buffer_count < BUFFER_COUNT
}

fn aligned_buffer_capacity(bytes: usize) -> Result<usize, Error> {
    bytes
        .checked_add(63)
        .map(|capacity| capacity & !63)
        .ok_or_else(|| Error::Runtime("lock buffer capacity overflow".into()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Region {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

impl Region {
    fn full(width: u32, height: u32) -> Self {
        Self {
            x: 0,
            y: 0,
            width,
            height,
        }
    }
}

fn frame_geometry(frame: Option<&PresentationFrame>) -> Option<FrameGeometry> {
    let frame = frame?;
    Some(FrameGeometry {
        layout: frame.layout,
        dimming: frame.dimming,
        overlays: frame.overlays().map(|anchored| {
            anchored.map(|anchored| {
                let (width, height) = anchored.overlay.dimensions();
                (width, height, anchored.offset)
            })
        }),
    })
}

fn overlay_redraw_kind(
    previous: Option<FrameGeometry>,
    current: Option<FrameGeometry>,
) -> RedrawKind {
    if previous == current {
        RedrawKind::Overlay
    } else {
        RedrawKind::Full
    }
}

fn buffer_size(
    width: u32,
    height: u32,
    scale: u32,
    _transform: wl_output::Transform,
) -> Result<(u32, u32), Error> {
    let (width, height) = width
        .checked_mul(scale)
        .zip(height.checked_mul(scale))
        .ok_or_else(|| Error::Runtime("compositor lock-surface dimensions overflow".into()))?;
    if width == 0 || height == 0 {
        return Err(Error::Runtime(
            "compositor requested an empty lock surface".into(),
        ));
    }
    if width > MAX_SURFACE_DIMENSION || height > MAX_SURFACE_DIMENSION {
        return Err(Error::Runtime(
            "compositor lock-surface dimensions exceed the supported limit".into(),
        ));
    }
    Ok((width, height))
}

/// Draws `frame` as an output `width`×`height` buffer pixels at buffer
/// `scale` shows it when it owns authentication, scaling the overlays here
/// instead of reusing a surface's cache.
fn draw_opaque(
    target: &mut [u8],
    width: u32,
    height: u32,
    scale: u32,
    frame: Option<&PresentationFrame>,
) {
    let placed = place_overlays(frame, width, height, scale);
    draw_opaque_placed(target, width, height, frame, &placed, true);
}

fn place_overlays(
    frame: Option<&PresentationFrame>,
    width: u32,
    height: u32,
    scale: u32,
) -> [Option<PlacedOverlay>; 3] {
    let Some(frame) = frame else {
        return [None, None, None];
    };
    let regions = overlay_regions(frame, width, height, scale);
    let overlays = frame.overlays();
    std::array::from_fn(|index| {
        let (region, anchored) = (regions[index]?, overlays[index]?);
        Some(PlacedOverlay {
            region,
            pixels: scale_overlay(&anchored.overlay, region.width, region.height),
            backdrop: anchored
                .backdrop
                .map(|backdrop| (backdrop, anchored.overlay.dimensions())),
        })
    })
}

fn draw_opaque_placed(
    target: &mut [u8],
    width: u32,
    height: u32,
    frame: Option<&PresentationFrame>,
    placed: &[Option<PlacedOverlay>; 3],
    authentication: bool,
) {
    draw_opaque_region(
        target,
        width,
        height,
        frame,
        placed,
        authentication,
        Region::full(width, height),
    );
}

/// Repaints `region`: the wallpaper, dimmed only on the authentication output
/// and only when the frame supplies a factor, then the placed overlays.
fn draw_opaque_region(
    target: &mut [u8],
    width: u32,
    height: u32,
    frame: Option<&PresentationFrame>,
    placed: &[Option<PlacedOverlay>; 3],
    authentication: bool,
    region: Region,
) {
    let dimming = frame
        .filter(|_| authentication)
        .and_then(|frame| frame.dimming);
    let background = BackgroundSampler::new(
        frame.and_then(|frame| frame.background.as_ref()),
        width,
        height,
    );
    let end_x = region.x.saturating_add(region.width).min(width);
    let end_y = region.y.saturating_add(region.height).min(height);
    for y in region.y..end_y {
        let source_row = background.source_row(y);
        for x in region.x..end_x {
            let [red, green, blue] = background
                .pixel(source_row, x)
                .map(|value| dim(value, dimming));
            let offset = ((y as usize * width as usize) + x as usize) * 4;
            target[offset..offset + 4].copy_from_slice(&[blue, green, red, u8::MAX]);
        }
    }
    if !authentication {
        return;
    }
    let blur = frame
        .and_then(|frame| frame.background.as_ref())
        .filter(|_| placed.iter().flatten().any(|p| p.backdrop.is_some()))
        .map(super::material::blurred);
    let blurred = BackgroundSampler::with_aspect(
        blur.as_ref(),
        width,
        height,
        frame
            .and_then(|frame| frame.background.as_ref())
            .map(RgbaFrame::dimensions),
    );
    // The bottom overlay is drawn last, over the top overlay where they meet.
    for overlay in placed.iter().flatten() {
        let Some(area) = intersect(region, overlay.region) else {
            continue;
        };
        let stride = overlay.pixels.width as usize;
        for y in area.y..area.y + area.height {
            let overlay_y = (y - overlay.region.y) as usize;
            let blur_row = blurred.source_row(y);
            for x in area.x..area.x + area.width {
                let overlay_x = (x - overlay.region.x) as usize;
                let offset = ((y as usize * width as usize) + x as usize) * 4;
                if blur.is_some()
                    && overlay.backdrop.is_some_and(|(backdrop, (sw, sh))| {
                        backdrop.contains(
                            (overlay_x as f32 + 0.5) * sw as f32 / overlay.region.width as f32,
                            (overlay_y as f32 + 0.5) * sh as f32 / overlay.region.height as f32,
                        )
                    })
                {
                    let [red, green, blue] = blurred.pixel(blur_row, x);
                    target[offset..offset + 4].copy_from_slice(&[blue, green, red, 255]);
                }
                let source = (overlay_y * stride + overlay_x) * 4;
                let pixel =
                    <[u8; 4]>::try_from(&overlay.pixels.pixels[source..source + 4]).unwrap();
                if pixel[3] == 0 {
                    continue;
                }
                let offset = ((y as usize * width as usize) + x as usize) * 4;
                let current = [target[offset + 2], target[offset + 1], target[offset]];
                let [red, green, blue] = blend_rgb(current, pixel);
                target[offset..offset + 4].copy_from_slice(&[blue, green, red, u8::MAX]);
            }
        }
    }
}

struct BackgroundSampler<'a> {
    frame: Option<&'a RgbaFrame>,
    columns: Vec<usize>,
    crop_y: u128,
    scaled_height: u128,
}

impl<'a> BackgroundSampler<'a> {
    fn new(frame: Option<&'a RgbaFrame>, width: u32, height: u32) -> Self {
        Self::with_aspect(frame, width, height, frame.map(RgbaFrame::dimensions))
    }

    fn with_aspect(
        frame: Option<&'a RgbaFrame>,
        width: u32,
        height: u32,
        dimensions: Option<(u32, u32)>,
    ) -> Self {
        let Some(frame) = frame else {
            return Self {
                frame: None,
                columns: Vec::new(),
                crop_y: 0,
                scaled_height: 1,
            };
        };
        let (source_width, source_height) = dimensions.unwrap_or(frame.dimensions());
        let Some((crop_x, crop_y, scaled_width, scaled_height)) =
            cover_geometry(source_width, source_height, width, height)
        else {
            return Self {
                frame: None,
                columns: Vec::new(),
                crop_y: 0,
                scaled_height: 1,
            };
        };
        let columns = (0..width)
            .map(|x| {
                usize::try_from(source_coordinate(x, crop_x, scaled_width, frame.width)).unwrap()
                    * 4
            })
            .collect();
        Self {
            frame: Some(frame),
            columns,
            crop_y,
            scaled_height,
        }
    }

    fn source_row(&self, y: u32) -> Option<usize> {
        let frame = self.frame?;
        Some(
            usize::try_from(source_coordinate(
                y,
                self.crop_y,
                self.scaled_height,
                frame.height,
            ))
            .unwrap()
                * frame.width as usize
                * 4,
        )
    }

    fn pixel(&self, source_row: Option<usize>, x: u32) -> [u8; 3] {
        let Some((frame, source_row)) = self.frame.zip(source_row) else {
            return FALLBACK_RGB;
        };
        let offset = source_row + self.columns[x as usize];
        [
            frame.pixels[offset],
            frame.pixels[offset + 1],
            frame.pixels[offset + 2],
        ]
    }
}

const FILTER_SCALE: u64 = 65_536;

#[derive(Clone, Copy)]
struct WeightedSample {
    index: u32,
    weight: u64,
}

struct FilteredAxis {
    offsets: Vec<usize>,
    samples: Vec<WeightedSample>,
}

impl FilteredAxis {
    fn coordinate(&self, position: usize) -> &[WeightedSample] {
        &self.samples[self.offsets[position]..self.offsets[position + 1]]
    }
}

fn filtered_axis(source: u32, target: u32) -> FilteredAxis {
    let mut offsets = Vec::with_capacity(target as usize + 1);
    let mut samples = Vec::new();
    for position in 0..target {
        offsets.push(samples.len());
        if source <= target {
            let denominator = u64::from(target) * 2;
            let centered = (u64::from(position) * 2 + 1)
                .saturating_mul(u64::from(source))
                .saturating_sub(u64::from(target));
            let lower = (centered / denominator).min(u64::from(source - 1)) as u32;
            let upper = lower.saturating_add(1).min(source - 1);
            let upper_weight = if lower == upper {
                0
            } else {
                (centered % denominator) * FILTER_SCALE / denominator
            };
            samples.push(WeightedSample {
                index: lower,
                weight: FILTER_SCALE - upper_weight,
            });
            if upper != lower && upper_weight != 0 {
                samples.push(WeightedSample {
                    index: upper,
                    weight: upper_weight,
                });
            }
        } else {
            let start = u64::from(position) * u64::from(source) * FILTER_SCALE / u64::from(target);
            let end =
                u64::from(position + 1) * u64::from(source) * FILTER_SCALE / u64::from(target);
            let span = end - start;
            let first = start / FILTER_SCALE;
            let last = (end - 1) / FILTER_SCALE;
            for index in first..=last {
                let sample_start = index * FILTER_SCALE;
                let overlap_start = start.max(sample_start);
                let overlap_end = end.min(sample_start + FILTER_SCALE);
                let normalized_start = ((overlap_start - start) * FILTER_SCALE + span / 2) / span;
                let normalized_end = ((overlap_end - start) * FILTER_SCALE + span / 2) / span;
                let weight = normalized_end - normalized_start;
                if weight != 0 {
                    samples.push(WeightedSample {
                        index: index as u32,
                        weight,
                    });
                }
            }
        }
    }
    offsets.push(samples.len());
    FilteredAxis { offsets, samples }
}

fn filtered_pixel(
    frame: &RgbaFrame,
    horizontal: &[WeightedSample],
    vertical: &[WeightedSample],
) -> [u8; 4] {
    let mut alpha_weight = 0_u64;
    let mut channels = [0_u64; 3];
    for y in vertical {
        for x in horizontal {
            let offset = (y.index as usize * frame.width as usize + x.index as usize) * 4;
            let weight = x.weight * y.weight;
            let alpha = u64::from(frame.pixels[offset + 3]);
            alpha_weight += alpha * weight;
            for (channel, result) in channels.iter_mut().enumerate() {
                *result += u64::from(frame.pixels[offset + channel]) * alpha * weight;
            }
        }
    }
    if alpha_weight == 0 {
        return [0; 4];
    }
    let total_weight = FILTER_SCALE * FILTER_SCALE;
    [
        (channels[0] / alpha_weight) as u8,
        (channels[1] / alpha_weight) as u8,
        (channels[2] / alpha_weight) as u8,
        ((alpha_weight + total_weight / 2) / total_weight) as u8,
    ]
}

fn scale_overlay(frame: &RgbaFrame, width: u32, height: u32) -> RgbaFrame {
    if frame.dimensions() == (width, height) {
        return frame.clone();
    }
    let horizontal = filtered_axis(frame.width, width);
    let vertical = filtered_axis(frame.height, height);
    let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height as usize {
        let vertical = vertical.coordinate(y);
        for x in 0..width as usize {
            pixels.extend_from_slice(&filtered_pixel(frame, horizontal.coordinate(x), vertical));
        }
    }
    RgbaFrame::new(width, height, Bytes::from(pixels)).expect("scaled overlay has valid dimensions")
}

fn intersect(first: Region, second: Region) -> Option<Region> {
    let left = first.x.max(second.x);
    let top = first.y.max(second.y);
    let right = first
        .x
        .saturating_add(first.width)
        .min(second.x.saturating_add(second.width));
    let bottom = first
        .y
        .saturating_add(first.height)
        .min(second.y.saturating_add(second.height));
    (left < right && top < bottom).then(|| Region {
        x: left,
        y: top,
        width: right - left,
        height: bottom - top,
    })
}

/// Fixed-point one for overlay fit factors.
const FIT_ONE: u128 = 1 << 16;

/// Where the top and bottom overlays sit on a `width`×`height` buffer at
/// buffer `scale`, in buffer pixels.
///
/// Overlays keep their logical size (overlay pixels divided by the density)
/// and their logical edge offsets. The top overlay then shrinks, with its
/// offset, to keep the layout's gap above the bottom overlay and its margin
/// from the sides, but not below its minimum scale. Either overlay shrinks
/// further only when it would not otherwise fit on the surface, so every
/// returned region lies inside the buffer.
fn overlay_regions(
    frame: &PresentationFrame,
    width: u32,
    height: u32,
    scale: u32,
) -> [Option<Region>; 3] {
    overlay_regions_at_scale(frame, width, height, scale.saturating_mul(120))
}

fn overlay_regions_at_scale(
    frame: &PresentationFrame,
    width: u32,
    height: u32,
    scale: u32,
) -> [Option<Region>; 3] {
    let layout = frame.layout;
    let density = u128::from(layout.density);
    let scale = u128::from(scale.max(1));
    let (surface_width, surface_height) = (u128::from(width), u128::from(height));
    let extent = |anchored: &AnchoredOverlay| {
        let (overlay_width, overlay_height) = anchored.overlay.dimensions();
        (
            u128::from(overlay_width) * scale,
            u128::from(overlay_height) * scale + u128::from(anchored.offset) * scale * density,
        )
    };
    // Extents are in buffer pixels times the density, so a fit of `FIT_ONE`
    // is the overlay's natural logical size.
    let fit_within = |anchored: &AnchoredOverlay, available_width: u128, available_height: u128| {
        let (extent_width, extent_height) = extent(anchored);
        FIT_ONE
            .min(available_width * density * 120 * FIT_ONE / extent_width)
            .min(available_height * density * 120 * FIT_ONE / extent_height)
    };
    // Lengths round to the nearest pixel and are then clamped to the surface,
    // which absorbs the half pixel rounding can add to a fit at its limit.
    let place = |anchored: &AnchoredOverlay, fit: u128, from_bottom: bool| -> Option<Region> {
        let (overlay_width, overlay_height) = anchored.overlay.dimensions();
        let round = |length: u128, denominator: u128| {
            let denominator = denominator * 120;
            u32::try_from((length * scale * fit + denominator / 2) / denominator).ok()
        };
        let offset = round(u128::from(anchored.offset), FIT_ONE)?.min(height);
        let region_width = round(u128::from(overlay_width), density * FIT_ONE)?.min(width);
        let region_height =
            round(u128::from(overlay_height), density * FIT_ONE)?.min(height - offset);
        if region_width == 0 || region_height == 0 {
            return None;
        }
        Some(Region {
            x: (width - region_width) / 2,
            y: if from_bottom {
                height - offset - region_height
            } else {
                offset
            },
            width: region_width,
            height: region_height,
        })
    };

    let bottom = frame.bottom.as_ref().and_then(|anchored| {
        place(
            anchored,
            fit_within(anchored, surface_width, surface_height),
            true,
        )
    });
    let top = frame.top.as_ref().and_then(|anchored| {
        let above_bottom = u128::from(bottom.map_or(height, |region| region.y))
            .saturating_sub((u128::from(layout.gap) * scale).div_ceil(120));
        let inside_margins =
            surface_width.saturating_sub((2 * u128::from(layout.margin) * scale).div_ceil(120));
        let minimum = (FIT_ONE * u128::from(layout.min_top_scale_percent)).div_ceil(100);
        let fit = fit_within(anchored, inside_margins, above_bottom)
            .max(minimum)
            .min(fit_within(anchored, surface_width, surface_height));
        place(anchored, fit, false)
    });
    let status = frame.status.as_ref().and_then(|anchored| {
        let mut region = place(
            anchored,
            fit_within(anchored, surface_width, surface_height),
            false,
        )?;
        let margin = (u128::from(layout.margin) * scale)
            .div_ceil(120)
            .min(u128::from(width)) as u32;
        region.x = width.saturating_sub(region.width).saturating_sub(margin);
        Some(region)
    });
    [top, bottom, status]
}

/// The buffer regions an overlay-only redraw repaints: every placed overlay,
/// or the whole buffer when the buffer holds an older background.
fn redraw_regions(
    kind: RedrawKind,
    rendered_background: u64,
    current_background: u64,
    placed: &[Option<PlacedOverlay>; 3],
    width: u32,
    height: u32,
) -> Vec<Region> {
    let regions = placed
        .iter()
        .flatten()
        .map(|overlay| overlay.region)
        .collect::<Vec<_>>();
    if kind == RedrawKind::Overlay
        && rendered_background == current_background
        && !regions.is_empty()
    {
        regions
    } else {
        vec![Region::full(width, height)]
    }
}

fn record_full_frame(
    presentation_geometry: &mut Option<FrameGeometry>,
    background_generation: &mut u64,
    current_geometry: Option<FrameGeometry>,
) {
    *presentation_geometry = current_geometry;
    *background_generation = background_generation.wrapping_add(1);
}

pub(super) fn render_preview(
    frame: &PresentationFrame,
    width: u32,
    height: u32,
    scale: u32,
) -> Result<RgbaFrame, PreviewError> {
    if width == 0 || height == 0 || width > MAX_SURFACE_DIMENSION || height > MAX_SURFACE_DIMENSION
    {
        return Err(PreviewError::InvalidDimensions);
    }
    let bytes = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .and_then(|bytes| usize::try_from(bytes).ok())
        .ok_or(PreviewError::InvalidDimensions)?;
    if bytes > MAX_BUFFER_BYTES {
        return Err(PreviewError::InvalidDimensions);
    }
    let mut pixels = allocate_preview_pixels(bytes)?;
    draw_opaque(&mut pixels, width, height, scale, Some(frame));
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    RgbaFrame::new(width, height, pixels.into()).ok_or(PreviewError::InvalidDimensions)
}

fn allocate_preview_pixels(bytes: usize) -> Result<Vec<u8>, PreviewError> {
    let mut pixels = Vec::new();
    pixels.try_reserve_exact(bytes)?;
    pixels.resize(bytes, 0);
    Ok(pixels)
}

#[cfg(test)]
fn sample_cover(frame: &RgbaFrame, width: u32, height: u32, x: u32, y: u32) -> Option<[u8; 3]> {
    let geometry = cover_geometry(frame.width, frame.height, width, height)?;
    frame_pixel(
        frame,
        source_coordinate(x, geometry.0, geometry.2, frame.width),
        source_coordinate(y, geometry.1, geometry.3, frame.height),
    )
}

fn cover_geometry(
    source_width: u32,
    source_height: u32,
    width: u32,
    height: u32,
) -> Option<(u128, u128, u128, u128)> {
    if source_width == 0 || source_height == 0 || width == 0 || height == 0 {
        return None;
    }
    let source_wider = u128::from(source_width) * u128::from(height)
        > u128::from(width) * u128::from(source_height);
    let (scaled_width, scaled_height) = if source_wider {
        (
            u128::from(source_width) * u128::from(height) / u128::from(source_height),
            u128::from(height),
        )
    } else {
        (
            u128::from(width),
            u128::from(source_height) * u128::from(width) / u128::from(source_width),
        )
    };
    let crop_x = scaled_width.saturating_sub(u128::from(width)) / 2;
    let crop_y = scaled_height.saturating_sub(u128::from(height)) / 2;
    Some((crop_x, crop_y, scaled_width, scaled_height))
}

fn source_coordinate(position: u32, crop: u128, scaled: u128, source: u32) -> u32 {
    (((u128::from(position) + crop) * u128::from(source) / scaled).min(u128::from(source - 1)))
        as u32
}

#[cfg(test)]
fn frame_pixel(frame: &RgbaFrame, source_x: u32, source_y: u32) -> Option<[u8; 3]> {
    if source_x >= frame.width || source_y >= frame.height {
        return None;
    }
    let offset = u64::from(source_y)
        .checked_mul(u64::from(frame.width))?
        .checked_add(u64::from(source_x))?
        .checked_mul(4)
        .and_then(|offset| usize::try_from(offset).ok())?;
    let pixel = frame.pixels.get(offset..offset.checked_add(3)?)?;
    Some([pixel[0], pixel[1], pixel[2]])
}

fn blend_rgb(background: [u8; 3], foreground: [u8; 4]) -> [u8; 3] {
    let alpha = u16::from(foreground[3]);
    std::array::from_fn(|channel| {
        ((u16::from(foreground[channel]) * alpha + u16::from(background[channel]) * (255 - alpha))
            / 255) as u8
    })
}

fn dim(value: u8, dimming: Option<Dimming>) -> u8 {
    match dimming {
        Some(Dimming {
            numerator,
            denominator,
        }) => (u32::from(value) * u32::from(numerator) / u32::from(denominator)) as u8,
        None => value,
    }
}

fn refresh_priority(presentation: &mut dyn Presentation, state: &mut State) -> Refresh {
    let refresh = presentation.receive_latest();
    record_presentation_failure(refresh, state);
    refresh
}

fn refresh_presentation(presentation: &mut dyn Presentation, state: &mut State) -> Refresh {
    let refresh = presentation.receive_deferred();
    record_presentation_failure(refresh, state);
    refresh
}

fn record_presentation_failure(refresh: Refresh, state: &mut State) {
    if refresh == Refresh::Failed {
        let _ = state.update(Event::PresentationFailed);
    }
}

fn lock_confirmation_action(state: &mut State, blocked: bool) -> Action {
    if blocked {
        Action::None
    } else {
        state.update(Event::LockConfirmed)
    }
}

struct ReadySignal(Vec<File>);

#[cfg(feature = "lock-test")]
struct TestObserver(Option<File>);

#[cfg(feature = "lock-test")]
#[derive(Clone, Copy)]
enum TestEvent {
    Locked,
    Finished,
    Failed,
    OutputAdded,
    OutputRemoved,
    Keyboard,
    Pointer,
    Geometry,
    /// Wayland keyboard lifecycle, correlated by the keyboard object's
    /// protocol id. The fixture uses these to attribute a keypress that never
    /// arrived to a specific keyboard resource, and to tell a resource that
    /// was never keymapped from one that was. They are fixed, non-secret
    /// names: only the object id varies.
    KeyboardAcquired(u32),
    KeyboardKeymap(u32),
    KeyboardEnter(u32),
    KeyboardLeave(u32),
    KeyboardReleased(u32),
}

#[cfg(feature = "lock-test")]
impl TestObserver {
    fn new(fd: Option<OwnedFd>) -> Self {
        Self(fd.map(File::from))
    }

    fn record(&mut self, event: TestEvent) {
        let Some(output) = self.0.as_mut() else {
            return;
        };
        let _ = match event {
            TestEvent::Locked => writeln!(output, "LOCKED"),
            TestEvent::Finished => writeln!(output, "FINISHED"),
            TestEvent::Failed => writeln!(output, "FAILED"),
            TestEvent::OutputAdded => writeln!(output, "OUTPUT_ADDED"),
            TestEvent::OutputRemoved => writeln!(output, "OUTPUT_REMOVED"),
            TestEvent::Keyboard => writeln!(output, "KEYBOARD"),
            TestEvent::Pointer => writeln!(output, "POINTER"),
            TestEvent::Geometry => writeln!(output, "GEOMETRY"),
            TestEvent::KeyboardAcquired(id) => writeln!(output, "KBD_ACQUIRED {id}"),
            TestEvent::KeyboardKeymap(id) => writeln!(output, "KBD_KEYMAP {id}"),
            TestEvent::KeyboardEnter(id) => writeln!(output, "KBD_ENTER {id}"),
            TestEvent::KeyboardLeave(id) => writeln!(output, "KBD_LEAVE {id}"),
            TestEvent::KeyboardReleased(id) => writeln!(output, "KBD_RELEASED {id}"),
        };
        let _ = output.flush();
    }
}

impl ReadySignal {
    fn new(fds: Vec<OwnedFd>) -> Self {
        Self(fds.into_iter().map(File::from).collect())
    }

    fn apply(&mut self, action: Action) -> std::io::Result<()> {
        if action == Action::ReportReady {
            for mut ready in self.0.drain(..) {
                ready.write_all(b"READY\n")?;
                ready.flush()?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod properties;

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use std::io::Read;
    use std::os::unix::net::UnixStream;

    struct FakePresentation {
        refresh: Refresh,
        frame: Option<PresentationFrame>,
    }

    impl Presentation for FakePresentation {
        fn receive_latest(&mut self) -> Refresh {
            self.refresh
        }

        fn frame(&self) -> Option<PresentationFrame> {
            self.frame.clone()
        }
    }

    struct PollingPresentation {
        latest_calls: usize,
        deferred_calls: usize,
    }

    impl Presentation for PollingPresentation {
        fn receive_latest(&mut self) -> Refresh {
            self.latest_calls += 1;
            Refresh::Overlay
        }

        fn receive_deferred(&mut self) -> Refresh {
            self.deferred_calls += 1;
            Refresh::Frame
        }

        fn frame(&self) -> Option<PresentationFrame> {
            None
        }
    }

    fn state() -> State {
        State::new(super::super::Identity::new(
            1000,
            "alice".into(),
            "Alice".into(),
        ))
    }

    #[test]
    fn authentication_handoff_waits_for_the_preferred_surface_to_be_configured() {
        let waiting = [(Some("eDP-1"), true, true), (Some("DP-1"), false, false)];
        assert_eq!(select_authentication_index(&waiting, None), Some(0));

        let ready = [(Some("eDP-1"), true, true), (Some("DP-1"), true, false)];
        assert_eq!(select_authentication_index(&ready, None), Some(1));
    }

    #[test]
    fn authentication_selection_recovers_when_the_owner_disappears() {
        let outputs = [(Some("DP-1"), false, false), (Some("eDP-1"), true, false)];

        assert_eq!(select_authentication_index(&outputs, None), Some(1));
        assert_eq!(select_authentication_index(&outputs, Some("DP-1")), Some(1));
    }

    #[test]
    fn role_change_repaint_bypasses_an_outstanding_frame_callback() {
        let mut redraw = RedrawState::default();
        redraw.request(RedrawKind::Overlay);
        redraw.committed(true);

        redraw.request_role_change();

        assert!(redraw.should_render(BUFFER_COUNT, false));
        assert_eq!(redraw.redraw_pending, Some(RedrawKind::Full));
        assert!(!redraw.request_frame_callback());
    }

    #[test]
    fn authentication_handoff_requires_presented_wallpaper_feedback() {
        let started = Instant::now();
        let deadline = started + AUTHENTICATION_RETIREMENT_TIMEOUT;
        let mut retirement = AuthenticationRetirement::begin(started);

        assert!(!retirement.accepts_presentation());
        retirement.committed();
        assert!(retirement.accepts_presentation());
        assert!(!retirement.expired(deadline - Duration::from_millis(1)));
        assert!(retirement.expired(deadline));
        retirement.discarded();
        assert!(
            !retirement.accepts_presentation(),
            "a discarded wallpaper commit must keep promotion blocked"
        );
        assert!(
            retirement.expired(deadline),
            "discarded feedback must retain the original handoff deadline"
        );
        for _ in 0..3 {
            retirement.committed();
            assert!(retirement.accepts_presentation());
            assert!(
                retirement.expired(deadline),
                "a retry must not extend the original handoff deadline"
            );
            retirement.discarded();
            assert!(retirement.expired(deadline));
        }
    }

    #[test]
    fn frame_callbacks_never_complete_an_authentication_handoff() {
        let started = Instant::now();
        let mut retirement = AuthenticationRetirement::begin(started);

        retirement.committed();
        for _ in 0..3 {
            assert!(retirement.accepts_presentation());
            assert!(!retirement.expired(started));
        }

        retirement.discarded();
        assert!(!retirement.accepts_presentation());
        assert!(retirement.expired(started + AUTHENTICATION_RETIREMENT_TIMEOUT));
    }

    #[test]
    fn latency_sensitive_polling_remains_independent_from_deferred_media() {
        let mut presentation = PollingPresentation {
            latest_calls: 0,
            deferred_calls: 0,
        };
        let mut state = state();

        assert_eq!(
            refresh_priority(&mut presentation, &mut state),
            Refresh::Overlay
        );
        assert_eq!(presentation.latest_calls, 1);
        assert_eq!(presentation.deferred_calls, 0);

        assert_eq!(
            refresh_presentation(&mut presentation, &mut state),
            Refresh::Frame
        );
        assert_eq!(presentation.latest_calls, 1);
        assert_eq!(presentation.deferred_calls, 1);
    }

    #[test]
    fn preferred_output_transforms_use_upright_normal_buffers() {
        assert_eq!(
            buffer_size(1920, 1080, 2, wl_output::Transform::Normal).unwrap(),
            (3840, 2160)
        );
        assert_eq!(
            buffer_size(1920, 1080, 2, wl_output::Transform::_90).unwrap(),
            (3840, 2160)
        );
        assert_eq!(
            buffer_size(1920, 1080, 2, wl_output::Transform::Flipped270).unwrap(),
            (3840, 2160)
        );
        assert!(buffer_size(u32::MAX, 1, 2, wl_output::Transform::Normal).is_err());
        assert!(buffer_size(16_385, 1, 1, wl_output::Transform::Normal).is_err());
        assert!(buffer_size(0, 1, 1, wl_output::Transform::Normal).is_err());
    }

    #[test]
    fn redraws_are_coalesced_until_the_compositor_finishes_a_frame() {
        let mut redraw = RedrawState::default();

        assert!(redraw.request(RedrawKind::Overlay));
        redraw.committed(true);
        assert!(!redraw.request(RedrawKind::Overlay));
        assert!(!redraw.request(RedrawKind::Full));
        assert_eq!(redraw.redraw_pending, Some(RedrawKind::Full));
        redraw.frame_done();
        assert!(redraw.should_render(2, true));
        redraw.committed(true);
        redraw.frame_done();
        assert!(!redraw.should_render(2, true));
    }

    #[test]
    fn deferred_frames_wait_for_blocked_input_and_coalesce_required_full_repaints() {
        let mut callback_blocked = RedrawState::default();
        callback_blocked.request(RedrawKind::Full);
        callback_blocked.committed(true);
        callback_blocked.request(RedrawKind::Overlay);
        assert_eq!(
            overlay_progress(&callback_blocked, 2, true, true, false),
            OverlayProgress::Blocked
        );

        let mut buffer_blocked = RedrawState::default();
        buffer_blocked.request(RedrawKind::Overlay);
        assert_eq!(
            overlay_progress(&buffer_blocked, BUFFER_COUNT, false, false, false),
            OverlayProgress::Blocked
        );

        let mut ready = RedrawState::default();
        ready.request(RedrawKind::Overlay);
        assert_eq!(
            overlay_progress(&ready, 2, true, true, false),
            OverlayProgress::Partial
        );
        assert_eq!(
            overlay_progress(&ready, 2, true, false, false),
            OverlayProgress::Full
        );

        let mut pending_full = RedrawState::default();
        pending_full.request(RedrawKind::Full);
        assert_eq!(
            overlay_progress(&pending_full, 2, true, true, true),
            OverlayProgress::Full
        );
        assert_eq!(
            overlay_progress(&pending_full, 2, true, true, false),
            OverlayProgress::None
        );

        assert!(!should_poll_deferred(
            Refresh::Overlay,
            &OverlayProgressSummary {
                blocked: true,
                ..OverlayProgressSummary::default()
            }
        ));
        assert!(!should_poll_deferred(
            Refresh::Overlay,
            &OverlayProgressSummary {
                partial: true,
                ..OverlayProgressSummary::default()
            }
        ));
        assert!(should_poll_deferred(
            Refresh::Overlay,
            &OverlayProgressSummary {
                full: true,
                ..OverlayProgressSummary::default()
            }
        ));
        let mixed = OverlayProgressSummary {
            full: true,
            blocked: true,
            ..OverlayProgressSummary::default()
        };
        assert!(should_poll_deferred(Refresh::Overlay, &mixed));
        assert!(should_allow_overlay_full(false, &mixed));

        let visible_updated = OverlayProgressSummary {
            partial: true,
            blocked: true,
            unblocked: true,
            ..OverlayProgressSummary::default()
        };
        assert!(should_poll_deferred(Refresh::Unchanged, &visible_updated));
        assert!(should_poll_deferred(
            Refresh::Unchanged,
            &OverlayProgressSummary {
                full: true,
                ..OverlayProgressSummary::default()
            }
        ));
        assert!(should_poll_deferred(
            Refresh::Unchanged,
            &OverlayProgressSummary {
                blocked: true,
                unblocked: true,
                ..OverlayProgressSummary::default()
            }
        ));

        let mut media_blocked = RedrawState::default();
        media_blocked.request(RedrawKind::Overlay);
        assert!(!media_blocked.request_media());
        assert_eq!(
            media_blocked.redraw_pending,
            Some(RedrawKind::Overlay),
            "media must not overwrite a blocked output's priority update"
        );
    }

    #[test]
    fn geometry_redraw_bypasses_an_obsolete_frame_callback_without_adding_one() {
        let mut redraw = RedrawState::default();
        redraw.request(RedrawKind::Overlay);
        redraw.committed(true);

        redraw.request_geometry();

        assert!(redraw.should_render(0, false));
        assert!(!redraw.request_frame_callback());
        redraw.committed(false);
        assert!(redraw.frame_pending);
        assert!(!redraw.geometry_pending);
        assert!(redraw.redraw_pending.is_none());
    }

    #[test]
    fn frame_callback_defers_latest_geometry_to_post_dispatch_maintenance() {
        let mut redraw = RedrawState::default();
        redraw.request(RedrawKind::Overlay);
        redraw.committed(true);
        redraw.request_geometry();

        redraw.frame_done();
        redraw.request_geometry();

        assert!(redraw.geometry_pending);
        assert!(redraw.should_render(0, false));
        assert!(redraw.request_frame_callback());
    }

    #[test]
    fn double_buffer_pressure_waits_until_a_buffer_is_reusable() {
        assert!(!redraw_can_progress(2, false));
        assert!(redraw_can_progress(2, true));
        assert!(redraw_can_progress(1, false));
        assert_eq!(aligned_buffer_capacity(65).unwrap(), 128);
    }

    #[test]
    fn overlay_redraw_prefers_a_reusable_current_background() {
        let reusable = [(0, 6), (1, 7)];

        assert_eq!(
            select_reusable_buffer(RedrawKind::Overlay, 7, &reusable),
            Some(1)
        );
        assert_eq!(
            select_reusable_buffer(RedrawKind::Full, 7, &reusable),
            Some(0)
        );
        assert_eq!(
            select_reusable_buffer(RedrawKind::Overlay, 8, &reusable),
            Some(0)
        );
    }

    fn solid(width: u32, height: u32, rgba: [u8; 4]) -> RgbaFrame {
        RgbaFrame::new(
            width,
            height,
            Bytes::from(rgba.repeat((width * height) as usize)),
        )
        .unwrap()
    }

    fn anchored(width: u32, height: u32, rgba: [u8; 4], offset: u32) -> Option<AnchoredOverlay> {
        AnchoredOverlay::new(solid(width, height, rgba), offset)
    }

    /// A frame whose single overlay is anchored to the bottom edge at density 1.
    fn bottom_frame(background: RgbaFrame, overlay: RgbaFrame) -> PresentationFrame {
        PresentationFrame::new(
            Some(background),
            OverlayLayout::new(1).unwrap(),
            None,
            AnchoredOverlay::new(overlay, 0),
        )
    }

    #[test]
    fn rendering_forces_opaque_argb_and_leaves_wallpaper_undimmed() {
        let frame = bottom_frame(
            RgbaFrame::new(1, 1, Bytes::from_static(&[100, 150, 200, 0])).unwrap(),
            solid(1, 1, [0; 4]),
        );
        let mut target = [0; 8];
        draw_opaque(&mut target, 2, 1, 1, Some(&frame));
        assert_eq!(target, [200, 150, 100, 255, 200, 150, 100, 255]);
    }

    #[test]
    fn a_supplied_dimming_factor_dims_only_the_authentication_wallpaper() {
        let frame = bottom_frame(solid(1, 1, [100, 150, 200, 255]), solid(1, 1, [0; 4]))
            .with_dimming(Dimming::new(4, 5));
        let mut target = [0; 8];
        draw_opaque(&mut target, 2, 1, 1, Some(&frame));
        assert_eq!(target, [160, 120, 80, 255, 160, 120, 80, 255]);

        let placed = place_overlays(Some(&frame), 2, 1, 1);
        draw_opaque_placed(&mut target, 2, 1, Some(&frame), &placed, false);
        assert_eq!(target, [200, 150, 100, 255, 200, 150, 100, 255]);
    }

    #[test]
    fn overlay_is_blended_at_full_strength_over_the_wallpaper() {
        for (alpha, dimming, expected) in [
            (255, None, [255; 4]),
            (128, None, [227, 202, 177, 255]),
            (255, Dimming::new(4, 5), [255; 4]),
            (128, Dimming::new(4, 5), [207, 187, 167, 255]),
        ] {
            let frame = bottom_frame(
                solid(1, 1, [100, 150, 200, 255]),
                solid(1, 1, [255, 255, 255, alpha]),
            )
            .with_dimming(dimming);
            let mut target = [0; 4];
            draw_opaque(&mut target, 1, 1, 1, Some(&frame));
            assert_eq!(target, expected, "alpha {alpha}, dimming {dimming:?}");
        }
    }

    #[test]
    fn rendering_scales_wallpaper_independently_from_the_overlays() {
        let frame = bottom_frame(
            RgbaFrame::new(2, 1, Bytes::from_static(&[100, 0, 0, 255, 0, 0, 200, 255])).unwrap(),
            solid(1, 1, [0; 4]),
        );
        let mut target = [0; 8];

        draw_opaque(&mut target, 2, 1, 1, Some(&frame));

        assert_eq!(target, [0, 0, 100, 255, 200, 0, 0, 255]);
    }

    #[test]
    fn background_only_surface_keeps_wallpaper_bright_and_omits_overlays() {
        let frame = bottom_frame(solid(1, 1, [100, 150, 200, 255]), solid(1, 1, [255; 4]))
            .with_dimming(Dimming::new(1, 2));
        let placed = place_overlays(Some(&frame), 1, 1, 1);
        assert!(placed[1].is_some());
        let mut target = [0; 4];

        draw_opaque_placed(&mut target, 1, 1, Some(&frame), &placed, false);

        assert_eq!(target, [200, 150, 100, 255]);
    }

    #[test]
    fn status_overlay_keeps_its_logical_right_margin_at_each_scale() {
        let background = [5, 9, 24, 255];
        let status = [201, 67, 19, 255];
        let frame = PresentationFrame::new(
            Some(solid(1, 1, background)),
            OverlayLayout::new(1).unwrap().with_top_fit(0, 7, 100),
            None,
            None,
        )
        .with_status(anchored(3, 2, status, 4));

        for scale in [1, 2] {
            let (width, height) = (80 * scale, 40 * scale);
            let rendered = crate::render_preview(&frame, width, height, scale).unwrap();
            for y in 0..height {
                for x in 0..width {
                    let inside = (70 * scale..73 * scale).contains(&x)
                        && (4 * scale..6 * scale).contains(&y);
                    let expected = if inside { status } else { background };
                    let offset = ((y * width + x) * 4) as usize;
                    assert_eq!(
                        &rendered.pixels[offset..offset + 4],
                        &expected,
                        "pixel ({x}, {y}) at scale {scale}"
                    );
                }
            }
        }
    }

    #[test]
    fn scaled_overlays_are_filtered_instead_of_pixel_doubled() {
        let frame = bottom_frame(
            solid(1, 1, [0, 0, 0, 255]),
            RgbaFrame::new(2, 1, Bytes::from_static(&[255, 0, 0, 255, 0, 0, 255, 255])).unwrap(),
        );
        let mut target = [0; 32];

        draw_opaque(&mut target, 4, 2, 2, Some(&frame));

        assert_eq!(target[0..4], [0, 0, 255, 255]);
        assert_eq!(target[4..8], [63, 0, 191, 255]);
        assert_eq!(target[8..12], [191, 0, 63, 255]);
        assert_eq!(target[12..16], [255, 0, 0, 255]);
    }

    #[test]
    fn filtered_transparency_does_not_add_dark_edge_fringe() {
        let overlay =
            RgbaFrame::new(2, 1, Bytes::from_static(&[0, 0, 0, 0, 255, 255, 255, 255])).unwrap();
        let horizontal = filtered_axis(2, 4);
        let vertical = filtered_axis(1, 1);

        let edge = filtered_pixel(&overlay, horizontal.coordinate(1), vertical.coordinate(0));

        assert_eq!(&edge[..3], &[255, 255, 255]);
        assert!(edge[3] > 0 && edge[3] < u8::MAX);
    }

    #[test]
    fn minification_preserves_thin_feature_coverage() {
        let overlay = RgbaFrame::new(
            3,
            1,
            Bytes::from_static(&[255, 255, 255, 255, 0, 0, 0, 0, 0, 0, 0, 0]),
        )
        .unwrap();
        let horizontal = filtered_axis(3, 1);
        let vertical = filtered_axis(1, 1);

        assert_eq!(
            filtered_pixel(&overlay, horizontal.coordinate(0), vertical.coordinate(0)),
            [255, 255, 255, 85]
        );
    }

    #[test]
    fn scaled_overlay_cache_tracks_source_allocation_and_target_size() {
        let source = RgbaFrame::new(1, 1, Bytes::from(vec![1, 2, 3, 4])).unwrap();
        let same_source = source.clone();
        let replacement = RgbaFrame::new(1, 1, Bytes::from(vec![1, 2, 3, 4])).unwrap();
        let cache = CachedOverlay {
            source,
            size: (2, 2),
            frame: RgbaFrame::new(2, 2, Bytes::from(vec![0; 16])).unwrap(),
        };

        assert!(cache.matches(&same_source, (2, 2)));
        assert!(!cache.matches(&replacement, (2, 2)));
        assert!(!cache.matches(&same_source, (3, 2)));
    }

    #[test]
    fn overlay_redraw_touches_only_the_placed_overlay_regions() {
        let frame = PresentationFrame::new(
            Some(solid(1, 1, [100, 0, 0, 255])),
            OverlayLayout::new(1).unwrap(),
            anchored(2, 1, [200; 4], 0),
            anchored(2, 1, [200; 4], 0),
        );
        let placed = place_overlays(Some(&frame), 8, 4, 1);
        let regions = redraw_regions(RedrawKind::Overlay, 7, 7, &placed, 8, 4);
        let mut target = vec![17; 8 * 4 * 4];

        let moved = PresentationFrame::new(
            None,
            OverlayLayout::new(1).unwrap(),
            anchored(2, 1, [200; 4], 0),
            anchored(2, 1, [200; 4], 1),
        );
        assert_eq!(
            overlay_redraw_kind(frame_geometry(Some(&frame)), frame_geometry(Some(&moved))),
            RedrawKind::Full
        );
        assert_eq!(
            overlay_redraw_kind(frame_geometry(Some(&frame)), frame_geometry(Some(&frame))),
            RedrawKind::Overlay
        );
        let dimmed = frame.clone().with_dimming(Dimming::new(1, 2));
        assert_eq!(
            overlay_redraw_kind(frame_geometry(Some(&frame)), frame_geometry(Some(&dimmed))),
            RedrawKind::Full,
            "changing the wallpaper factor repaints the whole wallpaper"
        );

        assert_eq!(
            regions,
            [
                Region {
                    x: 3,
                    y: 0,
                    width: 2,
                    height: 1
                },
                Region {
                    x: 3,
                    y: 3,
                    width: 2,
                    height: 1
                }
            ]
        );
        assert_eq!(
            redraw_regions(RedrawKind::Overlay, 6, 7, &placed, 8, 4),
            [Region::full(8, 4)]
        );
        assert_eq!(
            redraw_regions(RedrawKind::Full, 7, 7, &placed, 8, 4),
            [Region::full(8, 4)]
        );
        assert_eq!(
            redraw_regions(RedrawKind::Overlay, 7, 7, &[None, None, None], 8, 4),
            [Region::full(8, 4)]
        );

        for region in &regions {
            draw_opaque_region(&mut target, 8, 4, Some(&frame), &placed, true, *region);
        }

        let pixel = |x: usize, y: usize| &target[(y * 8 + x) * 4..(y * 8 + x + 1) * 4];
        assert_ne!(pixel(3, 0), &[17; 4]);
        assert_ne!(pixel(4, 3), &[17; 4]);
        for (x, y) in [(0, 0), (2, 0), (5, 0), (3, 1), (4, 2), (7, 3)] {
            assert_eq!(pixel(x, y), &[17; 4], "({x}, {y}) is outside both overlays");
        }
    }

    #[test]
    fn full_redraws_record_current_geometry_and_invalidate_every_buffer_once() {
        let layout = OverlayLayout::new(1).unwrap();
        let old = PresentationFrame::new(None, layout, None, anchored(1, 1, [0; 4], 0));
        let new = PresentationFrame::new(None, layout, None, anchored(1, 1, [0; 4], 2));
        let mut geometry = frame_geometry(Some(&old));
        let mut generation = u64::MAX;

        record_full_frame(&mut geometry, &mut generation, frame_geometry(Some(&new)));

        assert_eq!(geometry, frame_geometry(Some(&new)));
        assert_eq!(generation, 0);
        assert_eq!(
            overlay_redraw_kind(geometry, frame_geometry(Some(&new))),
            RedrawKind::Overlay
        );
    }

    /// A clock 600x294 points, 183 points from the top, over an identity
    /// column 300x262 points on the bottom edge, rendered at density 3.
    fn lock_like_frame() -> PresentationFrame {
        PresentationFrame::new(
            None,
            OverlayLayout::new(3).unwrap().with_top_fit(12, 48, 20),
            anchored(1800, 882, [0; 4], 183),
            anchored(900, 786, [0; 4], 0),
        )
    }

    #[test]
    fn overlays_remain_visible_on_portrait_ultrawide_and_minimum_outputs() {
        let frame = lock_like_frame();

        for (width, height, scale) in [
            (1080, 1920, 1),
            (3840, 1080, 1),
            (320, 320, 1),
            (480, 600, 1),
            (1280, 800, 1),
            (2560, 1600, 2),
        ] {
            let [top, bottom, _] = overlay_regions(&frame, width, height, scale);
            let (top, bottom) = (top.unwrap(), bottom.unwrap());
            for region in [top, bottom] {
                assert!(region.width > 0 && region.height > 0, "{width}x{height}");
                assert!(region.x + region.width <= width, "{width}x{height}");
                assert!(region.y + region.height <= height, "{width}x{height}");
            }
            assert_eq!(
                bottom.y + bottom.height,
                height,
                "identity sits on the edge"
            );
            assert_eq!(bottom.height, 262 * scale, "identity keeps its point size");
            if height >= 600 {
                assert!(
                    top.y + top.height + 12 * scale <= bottom.y,
                    "clock overlaps identity on {width}x{height}"
                );
            }
        }
    }

    #[test]
    fn the_top_overlay_keeps_its_point_size_unless_the_output_is_too_small() {
        let frame = lock_like_frame();
        let [top, _, _] = overlay_regions(&frame, 1280, 800, 1);
        assert_eq!(
            top.unwrap(),
            Region {
                x: 340,
                y: 183,
                width: 600,
                height: 294
            }
        );
        let [top, bottom, _] = overlay_regions(&frame, 2560, 1600, 2);
        assert_eq!(
            (top.unwrap().y, top.unwrap().height, bottom.unwrap().y),
            (366, 588, 1076)
        );
        // Too narrow for the clock between the margins: it shrinks with its offset.
        let [top, _, _] = overlay_regions(&frame, 480, 600, 1);
        let top = top.unwrap();
        assert_eq!(top.width, 384);
        assert!(top.y < 183);

        let layout = OverlayLayout::new(1).unwrap().with_top_fit(5, 0, 20);
        let frame = |margin| {
            PresentationFrame::new(
                None,
                layout.with_top_fit(5, margin, 20),
                anchored(100, 50, [0; 4], 10),
                anchored(40, 20, [0; 4], 0),
            )
        };
        let regions = |width, height, margin| overlay_regions(&frame(margin), width, height, 1);
        assert_eq!(
            regions(200, 100, 0)[0],
            Some(Region {
                x: 50,
                y: 10,
                width: 100,
                height: 50
            }),
            "room to spare keeps the natural size"
        );
        // Too short: the clock and its offset shrink to keep the gap.
        assert_eq!(
            regions(200, 60, 0)[0],
            Some(Region {
                x: 71,
                y: 6,
                width: 58,
                height: 29
            })
        );
        // Never below the minimum scale, even when that means overlapping.
        assert_eq!(regions(200, 30, 0)[0].unwrap().height, 10);
        // The margin narrows the room for the top overlay only.
        assert_eq!(regions(120, 200, 20)[0].unwrap().width, 80);
        assert_eq!(regions(120, 200, 20)[1].unwrap().width, 40);
    }

    #[test]
    fn rendering_composites_only_inside_the_positioned_overlay_bounds() {
        let frame = bottom_frame(
            solid(3, 1, [100, 150, 200, 255]),
            solid(1, 1, [200, 100, 50, 128]),
        );
        let mut target = [0; 12];

        draw_opaque(&mut target, 3, 1, 1, Some(&frame));

        assert_eq!(
            target,
            [200, 150, 100, 255, 124, 124, 150, 255, 200, 150, 100, 255],
            "the wallpaper outside the overlay is exactly the source pixel"
        );
    }

    #[test]
    fn preview_rendering_converts_compositor_argb_to_rgba_and_bounds_allocations() {
        let frame = bottom_frame(solid(1, 1, [100, 150, 200, 255]), solid(1, 1, [0; 4]));

        assert_eq!(
            render_preview(&frame, 1, 1, 1).unwrap().pixels(),
            &[100, 150, 200, 255]
        );
        assert!(matches!(
            render_preview(&frame, 0, 1, 1),
            Err(PreviewError::InvalidDimensions)
        ));
        assert!(matches!(
            render_preview(&frame, MAX_SURFACE_DIMENSION + 1, 1, 1),
            Err(PreviewError::InvalidDimensions)
        ));
        assert!(matches!(
            allocate_preview_pixels(usize::MAX),
            Err(PreviewError::Allocation(_))
        ));
    }

    #[test]
    fn missing_wallpaper_uses_an_opaque_generated_fallback() {
        let mut target = [0; 4];
        draw_opaque(&mut target, 1, 1, 1, None);
        assert_eq!(target, [24, 9, 5, 255]);
    }

    #[test]
    fn cover_sampling_handles_large_valid_coordinates_without_overflow() {
        let mut pixels = vec![0; 400_000 * 4];
        pixels[(399_999 * 4)..(400_000 * 4)].copy_from_slice(&[77, 88, 99, 255]);
        let frame = RgbaFrame {
            width: 400_000,
            height: 1,
            pixels: pixels.into(),
        };

        assert_eq!(
            sample_cover(&frame, u32::MAX, 1, u32::MAX - 1, 0),
            Some([77, 88, 99])
        );
    }

    #[test]
    fn cover_sampling_rejects_zero_dimensions() {
        let frame = RgbaFrame {
            width: 0,
            height: 1,
            pixels: Bytes::new(),
        };

        assert_eq!(sample_cover(&frame, 1, 1, 0, 0), None);
        assert_eq!(sample_cover(&frame, 0, 1, 0, 0), None);
    }

    #[test]
    fn valid_readiness_descriptor_reports_exact_protocol_message() {
        let (mut reader, writer) = UnixStream::pair().unwrap();
        let mut ready = ReadySignal::new(vec![writer.into()]);

        ready.apply(Action::ReportReady).unwrap();

        let mut message = String::new();
        reader.read_to_string(&mut message).unwrap();
        assert_eq!(message, "READY\n");
        assert!(ready.0.is_empty());
    }

    #[test]
    fn compositor_confirmation_reaches_every_readiness_consumer() {
        let (mut first_reader, first_writer) = UnixStream::pair().unwrap();
        let (mut second_reader, second_writer) = UnixStream::pair().unwrap();
        let mut ready = ReadySignal::new(vec![first_writer.into(), second_writer.into()]);

        ready.apply(Action::ReportReady).unwrap();

        for reader in [&mut first_reader, &mut second_reader] {
            let mut message = String::new();
            reader.read_to_string(&mut message).unwrap();
            assert_eq!(message, "READY\n");
        }
    }

    #[cfg(feature = "lock-test")]
    #[test]
    fn test_observer_emits_only_fixed_non_secret_event_names() {
        let (mut reader, writer) = UnixStream::pair().unwrap();
        let mut observer = TestObserver::new(Some(writer.into()));

        for event in [
            TestEvent::Locked,
            TestEvent::Finished,
            TestEvent::Failed,
            TestEvent::OutputAdded,
            TestEvent::OutputRemoved,
            TestEvent::Keyboard,
            TestEvent::Pointer,
            TestEvent::Geometry,
            TestEvent::KeyboardAcquired(11),
            TestEvent::KeyboardKeymap(11),
            TestEvent::KeyboardEnter(11),
            TestEvent::KeyboardLeave(11),
            TestEvent::KeyboardReleased(11),
        ] {
            observer.record(event);
        }
        drop(observer);

        let mut events = String::new();
        reader.read_to_string(&mut events).unwrap();
        assert_eq!(
            events,
            "LOCKED\nFINISHED\nFAILED\nOUTPUT_ADDED\nOUTPUT_REMOVED\nKEYBOARD\nPOINTER\nGEOMETRY\n\
             KBD_ACQUIRED 11\nKBD_KEYMAP 11\nKBD_ENTER 11\nKBD_LEAVE 11\nKBD_RELEASED 11\n"
        );
    }

    #[test]
    fn readiness_write_failures_are_reported() {
        let read_only = File::open("/dev/null").unwrap();
        let mut ready = ReadySignal::new(vec![read_only.into()]);

        assert!(ready.apply(Action::ReportReady).is_err());
        assert!(ready.0.is_empty());
    }

    #[test]
    fn fatal_error_before_confirmation_cannot_report_readiness() {
        let (mut reader, writer) = UnixStream::pair().unwrap();
        let mut ready = ReadySignal::new(vec![writer.into()]);
        let mut state = state();

        assert_eq!(state.update(Event::RuntimeFailed), Action::Abort);
        let action = lock_confirmation_action(&mut state, true);
        assert_eq!(action, Action::None);
        ready.apply(action).unwrap();
        drop(ready);

        let mut message = String::new();
        reader.read_to_string(&mut message).unwrap();
        assert!(message.is_empty());
    }

    #[test]
    fn absent_and_failed_presentation_frames_keep_opaque_locked_fallback() {
        for refresh in [Refresh::Frame, Refresh::Failed] {
            let mut presentation = FakePresentation {
                refresh,
                frame: None,
            };
            let mut state = state();
            assert_eq!(state.update(Event::LockConfirmed), Action::ReportReady);

            assert_eq!(refresh_priority(&mut presentation, &mut state), refresh);
            let mut target = [0; 4];
            draw_opaque(&mut target, 1, 1, 1, presentation.frame().as_ref());

            assert_eq!(target, [24, 9, 5, 255]);
            assert_eq!(state.lifecycle, super::super::Lifecycle::Locked);
            assert_eq!(state.presentation_failed, refresh == Refresh::Failed);
        }
    }
}
