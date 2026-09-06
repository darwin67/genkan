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
    KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers,
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
use wayland_client::{Connection, Proxy, QueueHandle, WEnum};
use wayland_protocols::wp::presentation_time::client::wp_presentation_feedback;

use super::{
    Action, Config, Event, Input, Presentation, PresentationFrame, PreviewError, Refresh,
    RgbaFrame, State, UnlockAuthorization,
};

const FRAME_INTERVAL: Duration = Duration::from_millis(16);
const FALLBACK_RGB: [u8; 3] = [5, 9, 24];
const DIM_NUMERATOR: u16 = 4;
const DIM_DENOMINATOR: u16 = 5;
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
    scale: i32,
    geometry_generation: u64,
    role_generation: u64,
    buffers: Vec<SurfaceBuffer>,
    overlay_cache: Option<CachedOverlay>,
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

impl Surface {
    fn scaled_overlay(
        &mut self,
        frame: Option<&PresentationFrame>,
        width: u32,
        height: u32,
    ) -> Option<RgbaFrame> {
        if !self.authentication {
            self.overlay_cache = None;
            return None;
        }
        let frame = frame?;
        let target = overlay_geometry(frame, width, height)?.target;
        let size = (target.width, target.height);
        if self
            .overlay_cache
            .as_ref()
            .is_none_or(|cache| !cache.matches(&frame.overlay, size))
        {
            self.overlay_cache = Some(CachedOverlay {
                source: frame.overlay.clone(),
                size,
                frame: scale_overlay(&frame.overlay, target.width, target.height),
            });
        }
        self.overlay_cache.as_ref().map(|cache| cache.frame.clone())
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FrameGeometry {
    canvas: (u32, u32),
    overlay: Region,
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
    output_state: OutputState,
    presentation_time_state: PresentationTimeState,
    registry_state: RegistryState,
    seat_state: SeatState,
    keyboards: Vec<(wl_seat::WlSeat, wl_keyboard::WlKeyboard)>,
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
        output_state: OutputState::new(&globals, &qh),
        presentation_time_state: PresentationTimeState::bind(&globals, &qh),
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        keyboards: Vec::new(),
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
            scale: 1,
            geometry_generation: 0,
            role_generation: 0,
            buffers: Vec::with_capacity(BUFFER_COUNT),
            overlay_cache: None,
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
            let configured_size = buffer_size(
                width,
                height,
                surface.scale.max(1) as u32,
                wl_output::Transform::Normal,
            )?;
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
            let configured_size = buffer_size(
                width,
                height,
                surface.scale.max(1) as u32,
                wl_output::Transform::Normal,
            )?;
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
        let scale = surface.scale.max(1) as u32;
        let (buffer_width, buffer_height) =
            buffer_size(width, height, scale, wl_output::Transform::Normal)?;
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
        let scaled_overlay = surface.scaled_overlay(frame.as_ref(), buffer_width, buffer_height);
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
        let mut damaged = Region::full(buffer_width, buffer_height);
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
            damaged = redraw_region(
                requested_redraw,
                *rendered_background,
                background_generation,
                frame.as_ref(),
                buffer_width,
                buffer_height,
            );
            if !full_redraw {
                draw_opaque_region_cached(
                    canvas,
                    buffer_width,
                    buffer_height,
                    frame.as_ref(),
                    scaled_overlay.as_ref(),
                    authentication,
                    damaged,
                );
            } else {
                draw_opaque_cached(
                    canvas,
                    buffer_width,
                    buffer_height,
                    frame.as_ref(),
                    scaled_overlay.as_ref(),
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
            draw_opaque_cached(
                canvas,
                buffer_width,
                buffer_height,
                frame.as_ref(),
                scaled_overlay.as_ref(),
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
                    surface.overlay_cache = None;
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
        wl_surface.set_buffer_scale(scale as i32);
        wl_surface.set_buffer_transform(wl_output::Transform::Normal);
        wl_surface.damage_buffer(
            damaged.x as i32,
            damaged.y as i32,
            damaged.width as i32,
            damaged.height as i32,
        );
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
        let selected =
            select_authentication_index(&candidates, self.authentication_output.as_deref());
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
        surface.overlay_cache = None;
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

    fn handle_key(&mut self, event: KeyEvent) {
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
            let scale = scale.max(1);
            if surface.scale != scale {
                surface.scale = scale;
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
                Ok(keyboard) => self.keyboards.push((seat.clone(), keyboard)),
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
                keyboard.release();
            }
            self.keyboards = retained;
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

    fn remove_seat(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _seat: wl_seat::WlSeat) {
    }
}

impl KeyboardHandler for Runtime {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
    }
    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
    ) {
    }
    fn press_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        self.handle_key(event);
    }
    fn repeat_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        self.handle_key(event);
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
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: Modifiers,
        _: RawModifiers,
        _: u32,
    ) {
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
        canvas: frame.dimensions(),
        overlay: Region {
            x: frame.overlay_x,
            y: frame.overlay_y,
            width: frame.overlay.width,
            height: frame.overlay.height,
        },
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

fn draw_opaque(target: &mut [u8], width: u32, height: u32, frame: Option<&PresentationFrame>) {
    draw_opaque_cached(target, width, height, frame, None, true);
}

fn draw_opaque_cached(
    target: &mut [u8],
    width: u32,
    height: u32,
    frame: Option<&PresentationFrame>,
    scaled_overlay: Option<&RgbaFrame>,
    authentication: bool,
) {
    draw_opaque_region_cached(
        target,
        width,
        height,
        frame,
        scaled_overlay,
        authentication,
        Region::full(width, height),
    );
}

#[cfg(test)]
fn draw_opaque_region(
    target: &mut [u8],
    width: u32,
    height: u32,
    frame: Option<&PresentationFrame>,
    region: Region,
) {
    draw_opaque_region_cached(target, width, height, frame, None, true, region);
}

fn draw_opaque_region_cached(
    target: &mut [u8],
    width: u32,
    height: u32,
    frame: Option<&PresentationFrame>,
    scaled_overlay: Option<&RgbaFrame>,
    authentication: bool,
    region: Region,
) {
    let overlay_geometry = frame.and_then(|frame| overlay_geometry(frame, width, height));
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
            let [red, green, blue] = background.pixel(source_row, x);
            let offset = ((y as usize * width as usize) + x as usize) * 4;
            let pixel = if authentication {
                [dim(blue), dim(green), dim(red), u8::MAX]
            } else {
                [blue, green, red, u8::MAX]
            };
            target[offset..offset + 4].copy_from_slice(&pixel);
        }
    }

    let Some((frame, geometry)) = frame
        .filter(|_| authentication)
        .zip(overlay_geometry.as_ref())
    else {
        return;
    };
    let Some(overlay_region) = intersect(region, geometry.target) else {
        return;
    };
    if let Some(overlay) = scaled_overlay {
        for y in overlay_region.y..overlay_region.y + overlay_region.height {
            let source_row = background.source_row(y);
            let overlay_y = y - geometry.target.y;
            for x in overlay_region.x..overlay_region.x + overlay_region.width {
                let overlay_x = x - geometry.target.x;
                let offset = (overlay_y as usize * overlay.width as usize + overlay_x as usize) * 4;
                let overlay = <[u8; 4]>::try_from(&overlay.pixels[offset..offset + 4]).unwrap();
                if overlay[3] == 0 {
                    continue;
                }
                let background = background.pixel(source_row, x);
                let [red, green, blue] = blend_rgb(background, overlay);
                let offset = ((y as usize * width as usize) + x as usize) * 4;
                target[offset..offset + 4].copy_from_slice(&[
                    dim(blue),
                    dim(green),
                    dim(red),
                    u8::MAX,
                ]);
            }
        }
        return;
    }
    let horizontal = filtered_axis(frame.overlay.width, geometry.target.width);
    let vertical = filtered_axis(frame.overlay.height, geometry.target.height);
    for y in overlay_region.y..overlay_region.y + overlay_region.height {
        let source_row = background.source_row(y);
        let vertical_samples = vertical.coordinate((y - geometry.target.y) as usize);
        for x in overlay_region.x..overlay_region.x + overlay_region.width {
            let horizontal_samples = horizontal.coordinate((x - geometry.target.x) as usize);
            let overlay = filtered_pixel(&frame.overlay, horizontal_samples, vertical_samples);
            if overlay[3] == 0 {
                continue;
            }
            let background = background.pixel(source_row, x);
            let [red, green, blue] = blend_rgb(background, overlay);
            let offset = ((y as usize * width as usize) + x as usize) * 4;
            target[offset..offset + 4].copy_from_slice(&[dim(blue), dim(green), dim(red), u8::MAX]);
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
        let Some(frame) = frame else {
            return Self {
                frame: None,
                columns: Vec::new(),
                crop_y: 0,
                scaled_height: 1,
            };
        };
        let Some((crop_x, crop_y, scaled_width, scaled_height)) =
            cover_geometry(frame.width, frame.height, width, height)
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
    (left < right && top < bottom).then_some(Region {
        x: left,
        y: top,
        width: right - left,
        height: bottom - top,
    })
}

#[derive(Debug, Clone, Copy)]
struct OverlayGeometry {
    target: Region,
}

fn overlay_region(frame: Option<&PresentationFrame>, width: u32, height: u32) -> Option<Region> {
    Some(overlay_geometry(frame?, width, height)?.target)
}

fn overlay_geometry(frame: &PresentationFrame, width: u32, height: u32) -> Option<OverlayGeometry> {
    let (source_width, source_height) = frame.dimensions();
    if source_width == 0 || source_height == 0 || width == 0 || height == 0 {
        return None;
    }
    let width_limited = u128::from(width) * u128::from(source_height)
        <= u128::from(source_width) * u128::from(height);
    let (canvas_width, canvas_height) = if width_limited {
        (
            width,
            u32::try_from(u128::from(source_height) * u128::from(width) / u128::from(source_width))
                .ok()?,
        )
    } else {
        (
            u32::try_from(
                u128::from(source_width) * u128::from(height) / u128::from(source_height),
            )
            .ok()?,
            height,
        )
    };
    if canvas_width == 0 || canvas_height == 0 {
        return None;
    }
    let canvas_x = (width - canvas_width) / 2;
    let canvas_y = (height - canvas_height) / 2;
    let left = canvas_x
        + u32::try_from(
            u128::from(frame.overlay_x) * u128::from(canvas_width) / u128::from(source_width),
        )
        .ok()?;
    let top = canvas_y
        + u32::try_from(
            u128::from(frame.overlay_y) * u128::from(canvas_height) / u128::from(source_height),
        )
        .ok()?;
    let right = canvas_x
        + div_ceil_u128(
            u128::from(frame.overlay_x + frame.overlay.width) * u128::from(canvas_width),
            u128::from(source_width),
        )?;
    let bottom = canvas_y
        + div_ceil_u128(
            u128::from(frame.overlay_y + frame.overlay.height) * u128::from(canvas_height),
            u128::from(source_height),
        )?;
    Some(OverlayGeometry {
        target: Region {
            x: left,
            y: top,
            width: right.saturating_sub(left),
            height: bottom.saturating_sub(top),
        },
    })
}

fn redraw_region(
    kind: RedrawKind,
    rendered_background: u64,
    current_background: u64,
    frame: Option<&PresentationFrame>,
    width: u32,
    height: u32,
) -> Region {
    if kind == RedrawKind::Overlay && rendered_background == current_background {
        overlay_region(frame, width, height).unwrap_or_else(|| Region::full(width, height))
    } else {
        Region::full(width, height)
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

fn div_ceil_u128(numerator: u128, denominator: u128) -> Option<u32> {
    u32::try_from(numerator.checked_add(denominator.checked_sub(1)?)? / denominator).ok()
}

pub(super) fn render_preview(
    frame: &PresentationFrame,
    width: u32,
    height: u32,
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
    draw_opaque(&mut pixels, width, height, Some(frame));
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

fn dim(value: u8) -> u8 {
    ((u16::from(value) * DIM_NUMERATOR) / DIM_DENOMINATOR) as u8
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
}

#[cfg(feature = "lock-test")]
impl TestObserver {
    fn new(fd: Option<OwnedFd>) -> Self {
        Self(fd.map(File::from))
    }

    fn record(&mut self, event: TestEvent) {
        if let Some(output) = self.0.as_mut() {
            let event = match event {
                TestEvent::Locked => "LOCKED",
                TestEvent::Finished => "FINISHED",
                TestEvent::Failed => "FAILED",
                TestEvent::OutputAdded => "OUTPUT_ADDED",
                TestEvent::OutputRemoved => "OUTPUT_REMOVED",
                TestEvent::Keyboard => "KEYBOARD",
                TestEvent::Pointer => "POINTER",
                TestEvent::Geometry => "GEOMETRY",
            };
            let _ = writeln!(output, "{event}");
            let _ = output.flush();
        }
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

    #[test]
    fn rendering_forces_opaque_argb_and_dims_wallpaper() {
        let background = RgbaFrame {
            width: 1,
            height: 1,
            pixels: Bytes::from_static(&[100, 150, 200, 0]),
        };
        let overlay = RgbaFrame {
            width: 1,
            height: 1,
            pixels: Bytes::from_static(&[0, 0, 0, 0]),
        };
        let frame = PresentationFrame::new(1, 1, Some(background), overlay, 0, 0).unwrap();
        let mut target = [0; 8];
        draw_opaque(&mut target, 2, 1, Some(&frame));
        assert_eq!(target, [160, 120, 80, 255, 160, 120, 80, 255]);
    }

    #[test]
    fn rendering_scales_wallpaper_independently_from_the_overlay_canvas() {
        let background =
            RgbaFrame::new(2, 1, Bytes::from_static(&[100, 0, 0, 255, 0, 0, 200, 255])).unwrap();
        let overlay = RgbaFrame::new(1, 1, Bytes::from_static(&[0; 4])).unwrap();
        let frame = PresentationFrame::new(1, 1, Some(background), overlay, 0, 0).unwrap();
        let mut target = [0; 8];

        draw_opaque(&mut target, 2, 1, Some(&frame));

        assert_eq!(target, [0, 0, 80, 255, 160, 0, 0, 255]);
    }

    #[test]
    fn background_only_surface_keeps_wallpaper_bright_and_omits_overlay() {
        let background = RgbaFrame::new(1, 1, Bytes::from_static(&[100, 150, 200, 255])).unwrap();
        let overlay = RgbaFrame::new(1, 1, Bytes::from_static(&[255, 255, 255, 255])).unwrap();
        let frame = PresentationFrame::new(1, 1, Some(background), overlay, 0, 0).unwrap();
        let mut target = [0; 4];

        draw_opaque_cached(&mut target, 1, 1, Some(&frame), None, false);

        assert_eq!(target, [200, 150, 100, 255]);
    }

    #[test]
    fn scaled_overlays_are_filtered_instead_of_pixel_doubled() {
        let background = RgbaFrame::new(1, 1, Bytes::from_static(&[0, 0, 0, 255])).unwrap();
        let overlay =
            RgbaFrame::new(2, 1, Bytes::from_static(&[255, 0, 0, 255, 0, 0, 255, 255])).unwrap();
        let frame = PresentationFrame::new(2, 1, Some(background), overlay, 0, 0).unwrap();
        let mut target = [0; 32];

        draw_opaque(&mut target, 4, 2, Some(&frame));

        assert_eq!(target[0..4], [0, 0, 204, 255]);
        assert_eq!(target[4..8], [50, 0, 152, 255]);
        assert_eq!(target[8..12], [152, 0, 50, 255]);
        assert_eq!(target[12..16], [204, 0, 0, 255]);
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
    fn cached_scaled_overlay_matches_direct_filtered_rendering() {
        let background = RgbaFrame::new(1, 1, Bytes::from_static(&[80, 60, 40, 255])).unwrap();
        let overlay = RgbaFrame::new(
            3,
            1,
            Bytes::from_static(&[255, 255, 255, 255, 0, 0, 0, 0, 0, 0, 0, 0]),
        )
        .unwrap();
        let frame = PresentationFrame::new(3, 1, Some(background), overlay, 0, 0).unwrap();
        let scaled = scale_overlay(&frame.overlay, 1, 1);
        let mut direct = [0; 4];
        let mut cached = [0; 4];

        draw_opaque(&mut direct, 1, 1, Some(&frame));
        draw_opaque_cached(&mut cached, 1, 1, Some(&frame), Some(&scaled), true);

        assert_eq!(cached, direct);
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
    fn overlay_redraw_touches_only_the_contained_overlay_region() {
        let background = RgbaFrame::new(1, 1, Bytes::from_static(&[100, 0, 0, 255])).unwrap();
        let overlay = RgbaFrame::new(2, 2, Bytes::from_static(&[200; 16])).unwrap();
        let frame = PresentationFrame::new(4, 4, Some(background), overlay, 1, 1).unwrap();
        let region = overlay_region(Some(&frame), 8, 4).unwrap();
        let mut target = vec![17; 8 * 4 * 4];

        let moved_overlay = RgbaFrame::new(2, 2, Bytes::from_static(&[200; 16])).unwrap();
        let moved = PresentationFrame::new(4, 4, None, moved_overlay, 2, 1).unwrap();
        assert_eq!(
            overlay_redraw_kind(frame_geometry(Some(&frame)), frame_geometry(Some(&moved))),
            RedrawKind::Full
        );
        assert_eq!(
            overlay_redraw_kind(frame_geometry(Some(&frame)), frame_geometry(Some(&frame))),
            RedrawKind::Overlay
        );

        assert_eq!(
            redraw_region(RedrawKind::Overlay, 7, 7, Some(&frame), 8, 4),
            region
        );
        assert_eq!(
            redraw_region(RedrawKind::Overlay, 6, 7, Some(&frame), 8, 4),
            Region::full(8, 4)
        );
        assert_eq!(
            redraw_region(RedrawKind::Full, 7, 7, Some(&frame), 8, 4),
            Region::full(8, 4)
        );

        draw_opaque_region(&mut target, 8, 4, Some(&frame), region);

        assert_eq!(
            region,
            Region {
                x: 3,
                y: 1,
                width: 2,
                height: 2
            }
        );
        assert_eq!(&target[0..4], &[17; 4]);
        assert_ne!(&target[(8 + 3) * 4..(8 + 4) * 4], &[17; 4]);
        assert_eq!(&target[(3 * 8 + 7) * 4..(4 * 8) * 4], &[17; 4]);
    }

    #[test]
    fn full_redraws_record_current_geometry_and_invalidate_every_buffer_once() {
        let old_overlay = RgbaFrame::new(1, 1, Bytes::from_static(&[0; 4])).unwrap();
        let old = PresentationFrame::new(2, 2, None, old_overlay, 0, 0).unwrap();
        let new_overlay = RgbaFrame::new(1, 1, Bytes::from_static(&[0; 4])).unwrap();
        let new = PresentationFrame::new(4, 4, None, new_overlay, 2, 2).unwrap();
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

    #[test]
    fn overlay_remains_visible_on_portrait_ultrawide_and_minimum_outputs() {
        let overlay = RgbaFrame::new(500, 400, Bytes::from(vec![0; 500 * 400 * 4])).unwrap();
        let frame = PresentationFrame::new(1280, 800, None, overlay, 390, 200).unwrap();

        for (width, height) in [(1080, 1920), (3840, 1080), (320, 320)] {
            let region = overlay_region(Some(&frame), width, height).unwrap();
            assert!(region.width > 0 && region.height > 0, "{width}x{height}");
            assert!(region.x + region.width <= width, "{width}x{height}");
            assert!(region.y + region.height <= height, "{width}x{height}");
        }
    }

    #[test]
    fn rendering_composites_only_inside_the_positioned_overlay_bounds() {
        let background = RgbaFrame::new(
            3,
            1,
            Bytes::from_static(&[100, 150, 200, 255, 100, 150, 200, 255, 100, 150, 200, 255]),
        )
        .unwrap();
        let overlay = RgbaFrame::new(1, 1, Bytes::from_static(&[200, 100, 50, 128])).unwrap();
        let frame = PresentationFrame::new(3, 1, Some(background), overlay, 1, 0).unwrap();
        let mut target = [0; 12];

        draw_opaque(&mut target, 3, 1, Some(&frame));

        assert_eq!(
            target,
            [160, 120, 80, 255, 99, 99, 120, 255, 160, 120, 80, 255,]
        );
    }

    #[test]
    fn preview_rendering_converts_compositor_argb_to_rgba_and_bounds_allocations() {
        let background = RgbaFrame::new(1, 1, Bytes::from_static(&[100, 150, 200, 255])).unwrap();
        let overlay = RgbaFrame::new(1, 1, Bytes::from_static(&[0; 4])).unwrap();
        let frame = PresentationFrame::new(1, 1, Some(background), overlay, 0, 0).unwrap();

        assert_eq!(
            render_preview(&frame, 1, 1).unwrap().pixels(),
            &[80, 120, 160, 255]
        );
        assert!(matches!(
            render_preview(&frame, 0, 1),
            Err(PreviewError::InvalidDimensions)
        ));
        assert!(matches!(
            render_preview(&frame, MAX_SURFACE_DIMENSION + 1, 1),
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
        draw_opaque(&mut target, 1, 1, None);
        assert_eq!(target, [19, 7, 4, 255]);
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
        ] {
            observer.record(event);
        }
        drop(observer);

        let mut events = String::new();
        reader.read_to_string(&mut events).unwrap();
        assert_eq!(
            events,
            "LOCKED\nFINISHED\nFAILED\nOUTPUT_ADDED\nOUTPUT_REMOVED\nKEYBOARD\nPOINTER\nGEOMETRY\n"
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
            draw_opaque(&mut target, 1, 1, presentation.frame().as_ref());

            assert_eq!(target, [19, 7, 4, 255]);
            assert_eq!(state.lifecycle, super::super::Lifecycle::Locked);
            assert_eq!(state.presentation_failed, refresh == Refresh::Failed);
        }
    }
}
