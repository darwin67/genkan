use std::fs::File;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::time::{Duration, Instant};

use calloop::generic::Generic;
use calloop::{EventLoop, Interest, Mode, PostAction};
use calloop_wayland_source::WaylandSource;
use chrono::{Datelike, Local, Offset, Timelike};
use genkan::dynamic_wallpaper::heic::{Document, RgbaFrame};
use genkan::dynamic_wallpaper::playback::{
    DecodeOutcome, DecodeRequest, Playback, SynchronizeOutcome,
};
use genkan::dynamic_wallpaper::solar;
use genkan::dynamic_wallpaper::{
    AppearancePreference, CivilDate, CivilTime, ClockSnapshot, ImageReference, Location, Metadata,
    Schedule, TimePoint,
};

use crate::geoclue::{self, GeoClueError, GeoLocation};
use rustix::time::{
    clock_gettime, timerfd_create, timerfd_settime, ClockId, Itimerspec, TimerfdClockId,
    TimerfdFlags, TimerfdTimerFlags, Timespec,
};
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::output::{OutputHandler, OutputInfo, OutputState};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::registry_handlers;
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
    LayerSurfaceConfigure,
};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shm::{
    slot::{Buffer, SlotPool},
    Shm, ShmHandler,
};
use thiserror::Error;
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::{wl_callback, wl_output, wl_region, wl_shm, wl_surface};
use wayland_client::{Connection, Proxy, QueueHandle};

const BUFFER_COUNT: usize = 2;
const MAX_BUFFER_BYTES: usize = 256 * 1024 * 1024;
const MAX_SURFACE_DIMENSION: u32 = 16_384;
const WORKER_POLL_INTERVAL: Duration = Duration::from_millis(20);
const SUSPEND_DETECTION_THRESHOLD: Duration = Duration::from_millis(10);
const FALLBACK_RGB: [u8; 3] = [5, 9, 24];
const LOCATION_TIMEOUT: Duration = Duration::from_secs(10);
const LOCATION_REFRESH: Duration = Duration::from_secs(6 * 60 * 60);
const LOCATION_RETRY: Duration = Duration::from_secs(10 * 60);
const MEANINGFUL_LOCATION_CHANGE_METERS: f64 = 5_000.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SurfaceAction {
    Render,
    Rearm,
    Idle,
}

fn surface_action(redraw: bool, transitioning: bool) -> SurfaceAction {
    if redraw {
        SurfaceAction::Render
    } else if transitioning {
        SurfaceAction::Rearm
    } else {
        SurfaceAction::Idle
    }
}

const fn render_eligible(
    redraw: bool,
    geometry_redraw: bool,
    frame_pending: bool,
    configured: bool,
) -> bool {
    redraw && (!frame_pending || geometry_redraw) && configured
}

fn request_frame(transitioning: bool, frame_pending: bool) -> bool {
    transitioning && !frame_pending
}

fn retire_pending<T>(pending: &mut Option<T>, retire: impl FnOnce(T)) {
    if let Some(value) = pending.take() {
        retire(value);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PresentationState {
    redraw: bool,
    geometry_redraw: bool,
    frame_pending: bool,
    frame_ready: bool,
    disabled: bool,
}

impl PresentationState {
    const fn new() -> Self {
        Self {
            redraw: true,
            geometry_redraw: false,
            frame_pending: false,
            frame_ready: false,
            disabled: false,
        }
    }

    fn geometry_changed(&mut self) {
        self.redraw = true;
        self.geometry_redraw = true;
        self.disabled = false;
    }

    fn playback_changed(&mut self) {
        if !self.disabled {
            self.redraw = true;
        }
    }

    fn frame_arrived(&mut self) {
        self.frame_pending = false;
        self.frame_ready = !self.disabled;
    }

    fn take_frame_action(&mut self, transitioning: bool) -> SurfaceAction {
        if !std::mem::take(&mut self.frame_ready) || self.disabled {
            return SurfaceAction::Idle;
        }
        surface_action(self.redraw, transitioning)
    }

    fn next_action(&mut self, transitioning: bool, configured: bool) -> SurfaceAction {
        if self.take_frame_action(transitioning) == SurfaceAction::Rearm {
            SurfaceAction::Rearm
        } else if self.render_eligible(configured) {
            SurfaceAction::Render
        } else {
            SurfaceAction::Idle
        }
    }

    const fn render_eligible(self, configured: bool) -> bool {
        !self.disabled
            && render_eligible(
                self.redraw,
                self.geometry_redraw,
                self.frame_pending,
                configured,
            )
    }

    fn rearmed(&mut self) {
        self.frame_pending = true;
    }

    fn rendered(&mut self, requested_frame: bool) {
        self.redraw = false;
        self.geometry_redraw = false;
        self.frame_pending |= requested_frame;
        self.disabled = false;
    }

    fn disable(&mut self) {
        self.redraw = false;
        self.geometry_redraw = false;
        self.frame_ready = false;
        self.disabled = true;
    }
}

#[derive(Clone, Copy, Debug)]
struct SuspendDetector {
    offset: Option<Duration>,
}

impl SuspendDetector {
    const fn new(offset: Option<Duration>) -> Self {
        Self { offset }
    }

    fn observe(&mut self, offset: Duration) -> bool {
        let Some(previous) = self.offset else {
            self.offset = Some(offset);
            return false;
        };
        if offset <= previous {
            return false;
        }
        self.offset = Some(offset);
        offset - previous > SUSPEND_DETECTION_THRESHOLD
    }

    fn sample(&mut self) -> bool {
        suspend_clock_offset().is_some_and(|offset| self.observe(offset))
    }
}

fn playback_deadline(
    playback: &Playback,
    clock: ClockSnapshot,
    monotonic: Duration,
) -> Option<Duration> {
    playback
        .next_wake(clock, monotonic)
        .and_then(|delay| monotonic.checked_add(delay))
}

fn earlier_deadline(left: Option<Duration>, right: Option<Duration>) -> Option<Duration> {
    left.into_iter().chain(right).min()
}

fn resolve_configure_size(
    suggested: (u32, u32),
    previous: Option<(u32, u32)>,
    output: Option<(u32, u32)>,
) -> Option<(u32, u32)> {
    let fallback = output.or(previous);
    let width = (suggested.0 != 0)
        .then_some(suggested.0)
        .or_else(|| fallback.map(|size| size.0))?;
    let height = (suggested.1 != 0)
        .then_some(suggested.1)
        .or_else(|| fallback.map(|size| size.1))?;
    Some((width, height))
}

fn output_logical_size(info: &OutputInfo) -> Option<(u32, u32)> {
    if let Some((width, height)) = info.logical_size {
        if width > 0 && height > 0 {
            return Some((width as u32, height as u32));
        }
    }
    let dimensions = info
        .modes
        .iter()
        .find(|mode| mode.current)
        .map(|mode| mode.dimensions)?;
    mode_logical_size(dimensions, info.scale_factor, info.transform)
}

fn mode_logical_size(
    (mut width, mut height): (i32, i32),
    scale: i32,
    transform: wl_output::Transform,
) -> Option<(u32, u32)> {
    if matches!(
        transform,
        wl_output::Transform::_90
            | wl_output::Transform::_270
            | wl_output::Transform::Flipped90
            | wl_output::Transform::Flipped270
    ) {
        std::mem::swap(&mut width, &mut height);
    }
    let scale = scale.max(1);
    (width > 0 && height > 0).then_some((
        u32::try_from(width / scale).ok()?.max(1),
        u32::try_from(height / scale).ok()?.max(1),
    ))
}

struct Scheduler {
    playback: Playback,
    next_synchronize: Option<Duration>,
}

impl Scheduler {
    fn new(playback: Playback, clock: ClockSnapshot, monotonic: Duration) -> Self {
        let next_synchronize = playback_deadline(&playback, clock, monotonic);
        Self {
            playback,
            next_synchronize,
        }
    }

    fn synchronize_if_due(
        &mut self,
        clock: ClockSnapshot,
        monotonic: Duration,
    ) -> Option<SynchronizeOutcome> {
        if self
            .next_synchronize
            .is_none_or(|deadline| monotonic < deadline)
        {
            return None;
        }
        let outcome = self.playback.synchronize(clock, monotonic);
        self.next_synchronize = playback_deadline(&self.playback, clock, monotonic);
        Some(outcome)
    }

    fn prepare_presentation(
        &mut self,
        clock: ClockSnapshot,
        monotonic: Duration,
        discontinuity: bool,
    ) -> bool {
        let presentation_changed = self.synchronize_now(clock, monotonic, discontinuity);
        let transition_changed = self.playback.advance_transition(monotonic);
        presentation_changed || transition_changed
    }

    fn synchronize_now(
        &mut self,
        clock: ClockSnapshot,
        monotonic: Duration,
        discontinuity: bool,
    ) -> bool {
        let deadline_due = self
            .next_synchronize
            .is_some_and(|deadline| monotonic >= deadline);
        let outcome = if discontinuity {
            self.playback
                .resynchronize_after_discontinuity(clock, monotonic)
        } else {
            self.playback.synchronize(clock, monotonic)
        };
        if deadline_due || discontinuity || outcome.selection_changed || outcome.discontinuity {
            self.next_synchronize = playback_deadline(&self.playback, clock, monotonic);
        }
        outcome.presentation_changed
    }

    fn synchronize_and_complete_decode(
        &mut self,
        request: DecodeRequest,
        result: Result<RgbaFrame, ()>,
        clock: ClockSnapshot,
        monotonic: Duration,
        discontinuity: bool,
    ) -> (DecodeOutcome, bool) {
        let presentation_changed = self.prepare_presentation(clock, monotonic, discontinuity);
        let outcome = self.complete_decode(request, result, clock, monotonic);
        (outcome, presentation_changed)
    }

    fn complete_decode(
        &mut self,
        request: DecodeRequest,
        result: Result<RgbaFrame, ()>,
        clock: ClockSnapshot,
        monotonic: Duration,
    ) -> DecodeOutcome {
        let outcome = self.playback.complete_decode(request, result, monotonic);
        self.next_synchronize = earlier_deadline(
            self.next_synchronize,
            playback_deadline(&self.playback, clock, monotonic),
        );
        outcome
    }

    fn apply_solar_schedule(
        &mut self,
        schedule: &Schedule<TimePoint>,
        clock: ClockSnapshot,
        monotonic: Duration,
    ) -> SynchronizeOutcome {
        let outcome = self.playback.set_solar_schedule(schedule, clock, monotonic);
        self.next_synchronize = playback_deadline(&self.playback, clock, monotonic);
        outcome
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BufferGeometry {
    width: u32,
    height: u32,
    bytes: usize,
}

fn buffer_geometry(size: (u32, u32), scale: i32) -> Result<BufferGeometry, &'static str> {
    let scale = scale.max(1) as u32;
    let width = size
        .0
        .checked_mul(scale)
        .filter(|width| *width <= MAX_SURFACE_DIMENSION)
        .ok_or("unsafe wallpaper surface width")?;
    let height = size
        .1
        .checked_mul(scale)
        .filter(|height| *height <= MAX_SURFACE_DIMENSION)
        .ok_or("unsafe wallpaper surface height")?;
    let bytes = usize::try_from(width)
        .ok()
        .and_then(|width| width.checked_mul(height as usize))
        .and_then(|pixels| pixels.checked_mul(4))
        .filter(|bytes| *bytes <= MAX_BUFFER_BYTES)
        .ok_or("wallpaper buffer exceeds resource limit")?;
    Ok(BufferGeometry {
        width,
        height,
        bytes,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BufferDecision {
    Reuse(usize),
    Allocate,
    Wait,
}

fn buffer_decision(released: &[bool], count: usize) -> BufferDecision {
    released
        .iter()
        .position(|released| *released)
        .map(BufferDecision::Reuse)
        .unwrap_or_else(|| {
            if count < BUFFER_COUNT {
                BufferDecision::Allocate
            } else {
                BufferDecision::Wait
            }
        })
}

pub struct Config {
    pub file: PathBuf,
    pub appearance: AppearancePreference,
    pub reduced_motion: bool,
    pub solar: bool,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("could not open dynamic wallpaper: {0}")]
    Wallpaper(#[from] genkan::dynamic_wallpaper::heic::Error),
    #[error("could not connect to the Wayland compositor: {0}")]
    Connect(String),
    #[error("required Wayland protocol is unavailable: {0}")]
    MissingProtocol(String),
    #[error("desktop wallpaper runtime failed: {0}")]
    Runtime(String),
}

struct DecodeResult {
    request: DecodeRequest,
    frame: Result<RgbaFrame, ()>,
}

enum WorkerResult {
    Initialized {
        metadata: Metadata,
        primary: ImageReference,
    },
    InitializationFailed,
    Decoded(DecodeResult),
}

struct Decoder {
    requests: SyncSender<DecodeRequest>,
    results: Receiver<WorkerResult>,
    initializing: bool,
    in_flight: bool,
}

impl Decoder {
    fn spawn(path: PathBuf) -> Result<Self, Error> {
        let (requests, request_receiver) = mpsc::sync_channel::<DecodeRequest>(1);
        let (result_sender, results) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("genkan-heic-decode".into())
            .spawn(move || {
                let document = match Document::open(&path) {
                    Ok(document) => document,
                    Err(_) => {
                        let _ = result_sender.send(WorkerResult::InitializationFailed);
                        return;
                    }
                };
                if result_sender
                    .send(WorkerResult::Initialized {
                        metadata: document.metadata().clone(),
                        primary: document.primary_image(),
                    })
                    .is_err()
                {
                    return;
                }
                while let Ok(request) = request_receiver.recv() {
                    let frame = document.decode(request.image).map_err(|_| ());
                    if result_sender
                        .send(WorkerResult::Decoded(DecodeResult { request, frame }))
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .map_err(|error| {
                Error::Runtime(format!("could not create HEIC decoder worker: {error}"))
            })?;
        Ok(Self {
            requests,
            results,
            initializing: true,
            in_flight: false,
        })
    }

    fn dispatch(&mut self, request: DecodeRequest) -> Result<(), Error> {
        match self.requests.try_send(request) {
            Ok(()) => {
                self.in_flight = true;
                Ok(())
            }
            Err(TrySendError::Full(_)) => Err(Error::Runtime(
                "bounded HEIC decode request queue is unexpectedly full".into(),
            )),
            Err(TrySendError::Disconnected(_)) => {
                Err(Error::Runtime("HEIC decoder worker stopped".into()))
            }
        }
    }

    fn receive(&mut self) -> Result<Option<WorkerResult>, Error> {
        match self.results.try_recv() {
            Ok(result) => {
                match &result {
                    WorkerResult::Initialized { .. } | WorkerResult::InitializationFailed => {
                        self.initializing = false
                    }
                    WorkerResult::Decoded(_) => self.in_flight = false,
                }
                Ok(Some(result))
            }
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) if self.initializing || self.in_flight => {
                Err(Error::Runtime("HEIC decoder worker stopped".into()))
            }
            Err(TryRecvError::Disconnected) => Ok(None),
        }
    }
}

enum SolarResult {
    Located(GeoLocation),
    Failed(GeoClueError),
}

struct SolarResolver {
    commands: SyncSender<()>,
    results: Receiver<SolarResult>,
    in_flight: bool,
}

impl SolarResolver {
    fn spawn() -> Result<Self, Error> {
        let (commands, command_receiver) = mpsc::sync_channel::<()>(1);
        let (result_sender, results) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("genkan-geoclue".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(_) => {
                        let _ = result_sender.send(SolarResult::Failed(GeoClueError::Unavailable));
                        return;
                    }
                };
                while command_receiver.recv().is_ok() {
                    let outcome =
                        match runtime.block_on(geoclue::request_city_location(LOCATION_TIMEOUT)) {
                            Ok(location) => SolarResult::Located(location),
                            Err(error) => SolarResult::Failed(error),
                        };
                    if result_sender.send(outcome).is_err() {
                        break;
                    }
                }
            })
            .map_err(|_| Error::Runtime("could not create GeoClue worker".into()))?;
        Ok(Self {
            commands,
            results,
            in_flight: false,
        })
    }

    fn dispatch(&mut self) -> Result<(), Error> {
        match self.commands.try_send(()) {
            Ok(()) => {
                self.in_flight = true;
                Ok(())
            }
            Err(TrySendError::Full(_)) => Ok(()),
            Err(TrySendError::Disconnected(_)) => {
                Err(Error::Runtime("GeoClue worker stopped".into()))
            }
        }
    }

    fn receive(&mut self) -> Option<SolarResult> {
        match self.results.try_recv() {
            Ok(result) => {
                self.in_flight = false;
                Some(result)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.in_flight = false;
                None
            }
        }
    }
}

/// True when two city-level fixes differ enough to change trajectory selection.
fn meaningful_location_change(previous: GeoLocation, next: GeoLocation) -> bool {
    let mut delta_longitude = next.longitude_degrees() - previous.longitude_degrees();
    if delta_longitude > 180.0 {
        delta_longitude -= 360.0;
    } else if delta_longitude < -180.0 {
        delta_longitude += 360.0;
    }
    let mean_latitude =
        ((previous.latitude_degrees() + next.latitude_degrees()) / 2.0).to_radians();
    let east = delta_longitude.to_radians() * mean_latitude.cos();
    let north = (next.latitude_degrees() - previous.latitude_degrees()).to_radians();
    (east * east + north * north).sqrt() * 6_371_000.0 > MEANINGFUL_LOCATION_CHANGE_METERS
}

struct SurfaceBuffer {
    size: (u32, u32),
    generation: u64,
    buffer: Buffer,
    pool: SlotPool,
}

struct Surface {
    output: wl_output::WlOutput,
    layer: LayerSurface,
    configure_hints: Option<(u32, u32)>,
    size: Option<(u32, u32)>,
    scale: i32,
    geometry_generation: u64,
    buffers: Vec<SurfaceBuffer>,
    presentation: PresentationState,
    frame_callback: Option<wl_callback::WlCallback>,
    last_error: Option<String>,
}

impl Surface {
    fn cancel_frame_callback(&mut self) {
        retire_pending(&mut self.frame_callback, |callback| {
            if let Some(backend) = callback.backend().upgrade() {
                let _ = backend.destroy_object(&callback.id());
            }
        });
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        self.cancel_frame_callback();
    }
}

struct Runtime {
    compositor: CompositorState,
    layer_shell: LayerShell,
    output_state: OutputState,
    registry_state: RegistryState,
    shm: Shm,
    surfaces: Vec<Surface>,
    scheduler: Option<Scheduler>,
    decoder: Option<Decoder>,
    appearance: AppearancePreference,
    reduced_motion: bool,
    started_at: Instant,
    suspend_detector: SuspendDetector,
    check_clock: bool,
    failure: Option<Error>,
    solar_enabled: bool,
    solar_resolver: Option<SolarResolver>,
    solar_requested: bool,
    solar_terminal: bool,
    metadata: Option<Metadata>,
    location: Option<GeoLocation>,
    next_location_refresh: Option<Instant>,
    mapped_date: Option<CivilDate>,
}

pub fn run(config: Config) -> Result<(), Error> {
    let conn = Connection::connect_to_env().map_err(|error| Error::Connect(error.to_string()))?;
    let (globals, mut event_queue) =
        registry_queue_init(&conn).map_err(|error| Error::Runtime(error.to_string()))?;
    let qh = event_queue.handle();
    let compositor = CompositorState::bind(&globals, &qh)
        .map_err(|error| Error::MissingProtocol(format!("wl_compositor ({error})")))?;
    if compositor.wl_compositor().version() < 4 {
        return Err(Error::MissingProtocol("wl_compositor version 4".into()));
    }
    let layer_shell = LayerShell::bind(&globals, &qh)
        .map_err(|error| Error::MissingProtocol(format!("zwlr_layer_shell_v1 ({error})")))?;
    let shm = Shm::bind(&globals, &qh)
        .map_err(|error| Error::MissingProtocol(format!("wl_shm ({error})")))?;
    let mut runtime = Runtime {
        compositor,
        layer_shell,
        output_state: OutputState::new(&globals, &qh),
        registry_state: RegistryState::new(&globals),
        shm,
        surfaces: Vec::new(),
        scheduler: None,
        decoder: None,
        appearance: config.appearance,
        reduced_motion: config.reduced_motion,
        started_at: Instant::now(),
        suspend_detector: SuspendDetector::new(suspend_clock_offset()),
        check_clock: false,
        failure: None,
        solar_enabled: config.solar,
        solar_resolver: if config.solar {
            Some(SolarResolver::spawn()?)
        } else {
            None
        },
        solar_requested: false,
        solar_terminal: false,
        metadata: None,
        location: None,
        next_location_refresh: None,
        mapped_date: None,
    };
    event_queue
        .roundtrip(&mut runtime)
        .map_err(|error| Error::Runtime(error.to_string()))?;
    if !runtime.shm.formats().contains(&wl_shm::Format::Argb8888) {
        return Err(Error::MissingProtocol("wl_shm ARGB8888 format".into()));
    }
    let outputs = runtime.output_state.outputs().collect::<Vec<_>>();
    for output in outputs {
        runtime.add_surface(output, &qh);
    }
    runtime.decoder = Some(Decoder::spawn(config.file)?);
    let mut event_loop: EventLoop<'static, Runtime> =
        EventLoop::try_new().map_err(|error| Error::Runtime(error.to_string()))?;
    WaylandSource::new(conn, event_queue)
        .insert(event_loop.handle())
        .map_err(|error| Error::Runtime(error.error.to_string()))?;
    let resume_timer = resume_timer()?;
    event_loop
        .handle()
        .insert_source(
            Generic::new(resume_timer, Interest::READ, Mode::Level),
            |_, timer, runtime| {
                let mut expirations = [0_u8; 8];
                let bytes = rustix::io::read(&**timer, &mut expirations)?;
                if bytes != expirations.len() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "short resume timer read",
                    ));
                }
                runtime.check_clock = true;
                Ok(PostAction::Continue)
            },
        )
        .map_err(|error| Error::Runtime(error.error.to_string()))?;

    while runtime.failure.is_none() {
        let timeout = runtime.dispatch_timeout();
        if let Err(error) = event_loop.dispatch(timeout, &mut runtime) {
            runtime.failure = Some(Error::Runtime(error.to_string()));
            break;
        }
        runtime.maintain(&qh)?;
    }
    Err(runtime
        .failure
        .unwrap_or_else(|| Error::Runtime("desktop wallpaper stopped".into())))
}

impl Runtime {
    fn add_surface(&mut self, output: wl_output::WlOutput, qh: &QueueHandle<Self>) {
        if let Some(index) = self
            .surfaces
            .iter()
            .position(|surface| surface.output == output)
        {
            self.refresh_surface_geometry(index, true);
            return;
        }
        let wl_surface = self.compositor.create_surface(qh);
        let layer = self.layer_shell.create_layer_surface(
            qh,
            wl_surface,
            Layer::Background,
            Some("genkan-wallpaper"),
            Some(&output),
        );
        layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.set_size(0, 0);
        let input = self.compositor.wl_compositor().create_region(qh, ());
        layer.wl_surface().set_input_region(Some(&input));
        input.destroy();
        layer.commit();
        self.surfaces.push(Surface {
            output,
            layer,
            configure_hints: None,
            size: None,
            scale: 1,
            geometry_generation: 0,
            buffers: Vec::with_capacity(BUFFER_COUNT),
            presentation: PresentationState::new(),
            frame_callback: None,
            last_error: None,
        });
    }

    fn refresh_surface_geometry(&mut self, index: usize, retry: bool) {
        let hints = self.surfaces[index].configure_hints;
        let output_size = self
            .output_state
            .info(&self.surfaces[index].output)
            .as_ref()
            .and_then(output_logical_size);
        let Some(hints) = hints else {
            return;
        };
        let Some(size) = resolve_configure_size(hints, self.surfaces[index].size, output_size)
        else {
            return;
        };
        let surface = &mut self.surfaces[index];
        if surface.size != Some(size) {
            surface.size = Some(size);
            surface.geometry_generation = surface.geometry_generation.wrapping_add(1);
        }
        if retry {
            surface.presentation.geometry_changed();
            surface.last_error = None;
        }
    }

    fn maintain(&mut self, qh: &QueueHandle<Self>) -> Result<(), Error> {
        if std::mem::take(&mut self.check_clock) {
            let discontinuity = self.suspend_detector.sample();
            self.synchronize_now(discontinuity)?;
        } else {
            self.synchronize_due()?;
        }
        if let Some(result) = self
            .decoder
            .as_mut()
            .expect("decoder is installed before event dispatch")
            .receive()?
        {
            match result {
                WorkerResult::Initialized { metadata, primary } => {
                    let clock = current_clock()?;
                    let monotonic = self.started_at.elapsed();
                    let mapped = self
                        .location
                        .and_then(|location| map_solar(&metadata, location, clock));
                    self.mapped_date = mapped.as_ref().map(|_| clock.date());
                    let playback = Playback::with_solar(
                        &metadata,
                        primary,
                        self.appearance,
                        clock,
                        monotonic,
                        self.reduced_motion,
                        mapped.as_ref(),
                    );
                    self.metadata = Some(metadata);
                    self.scheduler = Some(Scheduler::new(playback, clock, monotonic));
                    self.dispatch_decode()?;
                }
                WorkerResult::InitializationFailed => {
                    eprintln!(
                        "genkan wallpaper: retaining the generated background after HEIC initialization failure"
                    );
                }
                WorkerResult::Decoded(result) => {
                    let clock = current_clock()?;
                    let monotonic = self.started_at.elapsed();
                    let discontinuity = self.suspend_detector.sample();
                    let (outcome, presentation_changed) = self
                        .scheduler
                        .as_mut()
                        .expect("decode results follow worker initialization")
                        .synchronize_and_complete_decode(
                            result.request,
                            result.frame,
                            clock,
                            monotonic,
                            discontinuity,
                        );
                    if presentation_changed {
                        self.redraw_all();
                    }
                    match outcome {
                        DecodeOutcome::Presented | DecodeOutcome::Transitioning => {
                            self.redraw_all()
                        }
                        DecodeOutcome::Failed | DecodeOutcome::Rejected => eprintln!(
                            "genkan wallpaper: retaining the last valid frame after decode failure"
                        ),
                        DecodeOutcome::Ignored => {}
                    }
                    self.dispatch_decode()?;
                }
            }
        }

        self.maintain_solar()?;

        let has_frame_callbacks = self
            .surfaces
            .iter()
            .any(|surface| surface.presentation.frame_ready);
        let has_renderable_surface = self
            .surfaces
            .iter()
            .any(|surface| surface.presentation.render_eligible(surface.size.is_some()));
        if self.scheduler.is_some() && (has_frame_callbacks || has_renderable_surface) {
            let clock = current_clock()?;
            let monotonic = self.started_at.elapsed();
            let discontinuity = self.suspend_detector.sample();
            let pixels_changed = self
                .scheduler
                .as_mut()
                .expect("scheduler presence was checked")
                .prepare_presentation(clock, monotonic, discontinuity);
            if pixels_changed {
                self.redraw_all();
            }
            self.dispatch_decode()?;
        }
        let transitioning = self
            .scheduler
            .as_ref()
            .is_some_and(|scheduler| scheduler.playback.is_transitioning());
        let actions = self
            .surfaces
            .iter_mut()
            .map(|surface| {
                surface
                    .presentation
                    .next_action(transitioning, surface.size.is_some())
            })
            .collect::<Vec<_>>();
        for (index, action) in actions.into_iter().enumerate() {
            match action {
                SurfaceAction::Render => {
                    if let Err(error) = self.render(index, qh) {
                        if self.surfaces[index].last_error.as_deref() != Some(&error) {
                            eprintln!("genkan wallpaper: output presentation disabled: {error}");
                            self.surfaces[index].last_error = Some(error);
                        }
                        self.surfaces[index].presentation.disable();
                    }
                }
                SurfaceAction::Rearm => {
                    let surface = &mut self.surfaces[index];
                    let wl_surface = surface.layer.wl_surface();
                    surface.frame_callback = Some(wl_surface.frame(qh, wl_surface.clone()));
                    wl_surface.commit();
                    surface.presentation.rearmed();
                }
                SurfaceAction::Idle => {}
            }
        }
        Ok(())
    }

    fn synchronize_due(&mut self) -> Result<(), Error> {
        let monotonic = self.started_at.elapsed();
        let due = self
            .scheduler
            .as_ref()
            .and_then(|scheduler| scheduler.next_synchronize)
            .is_some_and(|deadline| monotonic >= deadline);
        if !due {
            return Ok(());
        }
        let clock = current_clock()?;
        if self
            .scheduler
            .as_mut()
            .and_then(|scheduler| scheduler.synchronize_if_due(clock, monotonic))
            .is_some_and(|outcome| outcome.presentation_changed)
        {
            self.redraw_all();
        }
        self.dispatch_decode()
    }

    fn synchronize_now(&mut self, discontinuity: bool) -> Result<(), Error> {
        let clock = current_clock()?;
        let monotonic = self.started_at.elapsed();
        let presentation_changed = self
            .scheduler
            .as_mut()
            .is_some_and(|scheduler| scheduler.synchronize_now(clock, monotonic, discontinuity));
        if presentation_changed {
            self.redraw_all();
        }
        self.dispatch_decode()?;
        Ok(())
    }

    fn dispatch_decode(&mut self) -> Result<(), Error> {
        let decoder = self
            .decoder
            .as_mut()
            .expect("decoder is installed before event dispatch");
        if decoder.in_flight {
            return Ok(());
        }
        if let Some(request) = self
            .scheduler
            .as_mut()
            .and_then(|scheduler| scheduler.playback.take_decode_request())
        {
            decoder.dispatch(request)?;
        }
        Ok(())
    }

    fn dispatch_timeout(&self) -> Option<Duration> {
        let worker = self
            .decoder
            .as_ref()
            .is_some_and(|decoder| decoder.initializing || decoder.in_flight)
            .then_some(WORKER_POLL_INTERVAL);
        let solar = self
            .solar_resolver
            .as_ref()
            .is_some_and(|resolver| resolver.in_flight)
            .then_some(WORKER_POLL_INTERVAL);
        let synchronization = self
            .scheduler
            .as_ref()
            .and_then(|scheduler| scheduler.next_synchronize)
            .map(|deadline| deadline.saturating_sub(self.started_at.elapsed()));
        worker.into_iter().chain(solar).chain(synchronization).min()
    }

    fn frame_available(&self) -> bool {
        self.scheduler
            .as_ref()
            .is_some_and(|scheduler| scheduler.playback.frame().is_some())
    }

    fn dispatch_solar(&mut self) {
        if let Some(resolver) = self.solar_resolver.as_mut() {
            if resolver.dispatch().is_err() {
                self.solar_terminal = true;
                eprintln!(
                    "genkan wallpaper: solar location worker stopped; retaining the h24 schedule or static fallback"
                );
            }
        }
    }

    fn install_solar_mapping(&mut self) -> Result<(), Error> {
        let (Some(location), Some(metadata)) = (self.location, self.metadata.as_ref()) else {
            return Ok(());
        };
        let clock = current_clock()?;
        let monotonic = self.started_at.elapsed();
        let Some(mapped) = map_solar(metadata, location, clock) else {
            return Ok(());
        };
        self.mapped_date = Some(clock.date());
        if let Some(scheduler) = self.scheduler.as_mut() {
            let outcome = scheduler.apply_solar_schedule(&mapped, clock, monotonic);
            if outcome.selection_changed || outcome.presentation_changed {
                self.redraw_all();
            }
        }
        self.dispatch_decode()
    }

    fn maintain_solar(&mut self) -> Result<(), Error> {
        if !self.solar_enabled {
            return Ok(());
        }
        while let Some(result) = self
            .solar_resolver
            .as_mut()
            .and_then(|resolver| resolver.receive())
        {
            match result {
                SolarResult::Located(location) => {
                    let changed = self
                        .location
                        .is_none_or(|previous| meaningful_location_change(previous, location));
                    self.location = Some(location);
                    self.next_location_refresh = Some(Instant::now() + LOCATION_REFRESH);
                    if changed {
                        self.install_solar_mapping()?;
                    }
                }
                SolarResult::Failed(error) => {
                    eprintln!(
                        "genkan wallpaper: solar location {}; retaining the h24 schedule or static fallback",
                        error.category()
                    );
                    match error {
                        GeoClueError::Unavailable | GeoClueError::Timeout => {
                            self.solar_requested = false;
                            self.next_location_refresh = Some(Instant::now() + LOCATION_RETRY);
                        }
                        GeoClueError::Denied | GeoClueError::Coarse | GeoClueError::Invalid => {
                            self.solar_terminal = true;
                        }
                    }
                }
            }
        }
        if self.solar_terminal {
            return Ok(());
        }
        let idle = !self
            .solar_resolver
            .as_ref()
            .is_some_and(|resolver| resolver.in_flight);
        let due = self
            .next_location_refresh
            .is_none_or(|deadline| Instant::now() >= deadline);
        let ready = self.location.is_some() || (!self.solar_requested && self.frame_available());
        if idle && due && ready {
            self.solar_requested = true;
            self.dispatch_solar();
        }
        let needs_mapping = self.location.is_some()
            && self
                .metadata
                .as_ref()
                .is_some_and(|metadata| metadata.solar().is_some())
            && self.mapped_date != Some(current_clock()?.date());
        if needs_mapping {
            self.install_solar_mapping()?;
        }
        Ok(())
    }

    fn redraw_all(&mut self) {
        for surface in &mut self.surfaces {
            surface.presentation.playback_changed();
        }
    }

    fn render(&mut self, index: usize, qh: &QueueHandle<Self>) -> Result<(), String> {
        let frame = self
            .scheduler
            .as_ref()
            .and_then(|scheduler| scheduler.playback.frame());
        let surface = &mut self.surfaces[index];
        let (width, height) = surface
            .size
            .ok_or_else(|| "attempted to render an unconfigured surface".to_owned())?;
        let geometry = buffer_geometry((width, height), surface.scale).map_err(str::to_owned)?;
        let buffer_width = geometry.width;
        let buffer_height = geometry.height;
        let bytes = geometry.bytes;
        let scale = surface.scale.max(1) as u32;
        let size = (buffer_width, buffer_height);
        surface.buffers.retain_mut(|buffer| {
            (buffer.size == size && buffer.generation == surface.geometry_generation)
                || buffer.pool.canvas(&buffer.buffer).is_none()
        });
        let released = surface
            .buffers
            .iter_mut()
            .map(|buffer| buffer.pool.canvas(&buffer.buffer).is_some())
            .collect::<Vec<_>>();
        let buffer_index = match buffer_decision(&released, surface.buffers.len()) {
            BufferDecision::Reuse(index) => {
                let buffer = &mut surface.buffers[index];
                let canvas = buffer
                    .pool
                    .canvas(&buffer.buffer)
                    .expect("buffer release state changed without dispatch");
                render_cover_argb(canvas, buffer_width, buffer_height, frame);
                index
            }
            BufferDecision::Wait => return Ok(()),
            BufferDecision::Allocate => {
                let capacity = bytes
                    .checked_add(63)
                    .map(|capacity| capacity & !63)
                    .ok_or_else(|| "wallpaper buffer capacity overflow".to_owned())?;
                let mut pool = SlotPool::new(capacity, &self.shm)
                    .map_err(|error| format!("could not allocate wallpaper pool: {error}"))?;
                let (buffer, canvas) = pool
                    .create_buffer(
                        buffer_width as i32,
                        buffer_height as i32,
                        (buffer_width * 4) as i32,
                        wl_shm::Format::Argb8888,
                    )
                    .map_err(|error| format!("could not allocate wallpaper buffer: {error}"))?;
                render_cover_argb(canvas, buffer_width, buffer_height, frame);
                surface.buffers.push(SurfaceBuffer {
                    size,
                    generation: surface.geometry_generation,
                    buffer,
                    pool,
                });
                surface.buffers.len() - 1
            }
        };
        let wl_surface = surface.layer.wl_surface();
        surface.buffers[buffer_index]
            .buffer
            .attach_to(wl_surface)
            .map_err(|error| format!("could not attach wallpaper buffer: {error}"))?;
        let opaque = self.compositor.wl_compositor().create_region(qh, ());
        opaque.add(0, 0, width as i32, height as i32);
        wl_surface.set_opaque_region(Some(&opaque));
        opaque.destroy();
        wl_surface.set_buffer_scale(scale as i32);
        wl_surface.set_buffer_transform(wl_output::Transform::Normal);
        wl_surface.damage_buffer(0, 0, buffer_width as i32, buffer_height as i32);
        let transitioning = self
            .scheduler
            .as_ref()
            .is_some_and(|scheduler| scheduler.playback.is_transitioning());
        let request_next_frame = request_frame(transitioning, surface.presentation.frame_pending);
        if request_next_frame {
            surface.frame_callback = Some(wl_surface.frame(qh, wl_surface.clone()));
        }
        wl_surface.commit();
        surface.presentation.rendered(request_next_frame);
        surface.last_error = None;
        Ok(())
    }
}

impl LayerShellHandler for Runtime {
    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, layer: &LayerSurface) {
        self.surfaces.retain(|surface| surface.layer != *layer);
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        let Some(index) = self
            .surfaces
            .iter()
            .position(|surface| surface.layer == *layer)
        else {
            self.failure = Some(Error::Runtime(
                "configured an unknown wallpaper surface".into(),
            ));
            return;
        };
        self.surfaces[index].configure_hints = Some(configure.new_size);
        self.refresh_surface_geometry(index, true);
        if self.surfaces[index].size.is_none() {
            eprintln!("genkan wallpaper: waiting for output geometry after zero-sized configure");
        }
    }
}

impl CompositorHandler for Runtime {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        wl_surface: &wl_surface::WlSurface,
        scale: i32,
    ) {
        if let Some(surface) = self
            .surfaces
            .iter_mut()
            .find(|surface| surface.layer.wl_surface() == wl_surface)
        {
            let scale = scale.max(1);
            if surface.scale != scale {
                surface.scale = scale;
                surface.geometry_generation = surface.geometry_generation.wrapping_add(1);
                surface.presentation.geometry_changed();
                surface.last_error = None;
            }
        }
    }

    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        wl_surface: &wl_surface::WlSurface,
        _transform: wl_output::Transform,
    ) {
        if let Some(surface) = self
            .surfaces
            .iter_mut()
            .find(|surface| surface.layer.wl_surface() == wl_surface)
        {
            surface.geometry_generation = surface.geometry_generation.wrapping_add(1);
            surface.presentation.geometry_changed();
            surface.last_error = None;
        }
    }

    fn frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        wl_surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
        let Some(index) = self
            .surfaces
            .iter()
            .position(|surface| surface.layer.wl_surface() == wl_surface)
        else {
            return;
        };
        self.surfaces[index].frame_callback = None;
        self.surfaces[index].presentation.frame_arrived();
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
        self.add_surface(output, qh);
    }

    fn update_output(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        self.add_surface(output, qh);
    }

    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        self.surfaces.retain(|surface| surface.output != output);
    }
}

impl ShmHandler for Runtime {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

smithay_client_toolkit::delegate_compositor!(Runtime);
smithay_client_toolkit::delegate_layer!(Runtime);
smithay_client_toolkit::delegate_output!(Runtime);
smithay_client_toolkit::delegate_registry!(Runtime);
smithay_client_toolkit::delegate_shm!(Runtime);
wayland_client::delegate_noop!(Runtime: ignore wl_region::WlRegion);

impl ProvidesRegistryState for Runtime {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }

    registry_handlers![OutputState];
}

fn resume_timer() -> Result<File, Error> {
    let timer = timerfd_create(
        TimerfdClockId::Boottime,
        TimerfdFlags::CLOEXEC | TimerfdFlags::NONBLOCK,
    )
    .map_err(|error| Error::Runtime(format!("could not create resume timer: {error}")))?;
    let interval = Timespec {
        // CLOCK_BOOTTIME advances through suspend, making the descriptor
        // readable immediately after resume without polling the compositor.
        tv_sec: 1,
        tv_nsec: 0,
    };
    timerfd_settime(
        &timer,
        TimerfdTimerFlags::empty(),
        &Itimerspec {
            it_interval: interval,
            it_value: interval,
        },
    )
    .map_err(|error| Error::Runtime(format!("could not arm resume timer: {error}")))?;
    Ok(File::from(timer))
}

fn suspend_clock_offset() -> Option<Duration> {
    for _ in 0..3 {
        let before = timespec_duration(clock_gettime(ClockId::Monotonic));
        let boottime = timespec_duration(clock_gettime(ClockId::Boottime));
        let after = timespec_duration(clock_gettime(ClockId::Monotonic));
        let sampling = after.saturating_sub(before);
        if sampling <= SUSPEND_DETECTION_THRESHOLD {
            let midpoint = before.checked_add(sampling / 2)?;
            return Some(boottime.saturating_sub(midpoint));
        }
    }
    None
}

fn timespec_duration(value: Timespec) -> Duration {
    Duration::new(value.tv_sec.max(0) as u64, value.tv_nsec.max(0) as u32)
}

fn current_clock() -> Result<ClockSnapshot, Error> {
    let now = Local::now();
    ClockSnapshot::new_with_nanosecond(
        CivilDate::new(now.year(), now.month() as u8, now.day() as u8)
            .map_err(|error| Error::Runtime(error.to_string()))?,
        CivilTime::new(now.hour() as u8, now.minute() as u8, now.second() as u8)
            .map_err(|error| Error::Runtime(error.to_string()))?,
        now.nanosecond(),
        now.offset().fix().local_minus_utc(),
    )
    .map_err(|error| Error::Runtime(error.to_string()))
}

/// Maps the file's authored solar points onto the current day's trajectory.
///
/// Returns `None` when the file has no solar schedule or when a retained
/// location cannot be represented, so callers fall back to `h24` and then to
/// static appearance or the primary image.
fn map_solar(
    metadata: &Metadata,
    location: GeoLocation,
    clock: ClockSnapshot,
) -> Option<Schedule<TimePoint>> {
    let schedule = metadata.solar()?;
    let location = Location::new(location.latitude_degrees(), location.longitude_degrees()).ok()?;
    solar::map_schedule(schedule, location, clock).ok()
}

fn render_cover_argb(target: &mut [u8], width: u32, height: u32, frame: Option<&RgbaFrame>) {
    let Some(frame) = frame else {
        for pixel in target.chunks_exact_mut(4) {
            pixel.copy_from_slice(&[FALLBACK_RGB[2], FALLBACK_RGB[1], FALLBACK_RGB[0], 255]);
        }
        return;
    };
    let source_wider =
        u128::from(frame.width) * u128::from(height) > u128::from(width) * u128::from(frame.height);
    let (scaled_width, scaled_height) = if source_wider {
        (
            u128::from(frame.width) * u128::from(height) / u128::from(frame.height),
            u128::from(height),
        )
    } else {
        (
            u128::from(width),
            u128::from(frame.height) * u128::from(width) / u128::from(frame.width),
        )
    };
    let crop_x = scaled_width.saturating_sub(u128::from(width)) / 2;
    let crop_y = scaled_height.saturating_sub(u128::from(height)) / 2;
    for y in 0..height {
        let source_y = (((u128::from(y) + crop_y) * u128::from(frame.height) / scaled_height)
            .min(u128::from(frame.height - 1))) as usize;
        for x in 0..width {
            let source_x = (((u128::from(x) + crop_x) * u128::from(frame.width) / scaled_width)
                .min(u128::from(frame.width - 1))) as usize;
            let source = (source_y * frame.width as usize + source_x) * 4;
            let target_offset = (y as usize * width as usize + x as usize) * 4;
            target[target_offset..target_offset + 4].copy_from_slice(&[
                frame.pixels[source + 2],
                frame.pixels[source + 1],
                frame.pixels[source],
                255,
            ]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use genkan::dynamic_wallpaper::{
        Appearance, AppleProperty, NormalizedTime, PropertyValue, Schedule, TimePoint,
    };

    fn image(position: usize) -> ImageReference {
        ImageReference::from_position(position)
    }

    fn clock(hour: u8, minute: u8, offset: i32) -> ClockSnapshot {
        ClockSnapshot::new(
            CivilDate::new(2026, 9, 10).unwrap(),
            CivilTime::new(hour, minute, 0).unwrap(),
            offset,
        )
        .unwrap()
    }

    fn frame(value: u8) -> RgbaFrame {
        RgbaFrame {
            width: 1,
            height: 1,
            pixels: vec![value, value, value, 255],
        }
    }

    fn time_metadata() -> Metadata {
        let mut metadata = Metadata::default();
        metadata
            .insert(
                AppleProperty::Time,
                PropertyValue::Time(
                    Schedule::new(
                        vec![
                            TimePoint {
                                image: image(0),
                                time: NormalizedTime::new(0.0).unwrap(),
                            },
                            TimePoint {
                                image: image(1),
                                time: NormalizedTime::new(60.0 / 86_400.0).unwrap(),
                            },
                            TimePoint {
                                image: image(2),
                                time: NormalizedTime::new(120.0 / 86_400.0).unwrap(),
                            },
                        ],
                        None,
                    )
                    .unwrap(),
                ),
            )
            .unwrap();
        metadata
    }

    fn complete_initial(playback: &mut Playback) {
        let request = playback.take_decode_request().unwrap();
        assert_eq!(
            playback.complete_decode(request, Ok(frame(0)), Duration::ZERO),
            DecodeOutcome::Presented
        );
    }

    fn transitioning_scheduler() -> (Scheduler, ClockSnapshot) {
        let initial = clock(0, 0, 0);
        let playback = Playback::new(
            &time_metadata(),
            image(0),
            AppearancePreference::Automatic,
            initial,
            Duration::ZERO,
            false,
        );
        let mut scheduler = Scheduler::new(playback, initial, Duration::ZERO);
        complete_initial(&mut scheduler.playback);
        let boundary = clock(0, 1, 0);
        scheduler.synchronize_if_due(boundary, Duration::from_secs(60));
        let request = scheduler.playback.take_decode_request().unwrap();
        assert_eq!(
            scheduler.complete_decode(request, Ok(frame(255)), boundary, Duration::from_secs(60)),
            DecodeOutcome::Transitioning
        );
        (scheduler, boundary)
    }

    #[test]
    fn active_transition_rearms_callbacks_even_when_pixels_are_unchanged() {
        let playback = Playback::new(
            &time_metadata(),
            image(0),
            AppearancePreference::Automatic,
            clock(0, 0, 0),
            Duration::ZERO,
            false,
        );
        let mut scheduler = Scheduler::new(playback, clock(0, 0, 0), Duration::ZERO);
        complete_initial(&mut scheduler.playback);
        let boundary = ClockSnapshot::new(
            CivilDate::new(2026, 9, 10).unwrap(),
            CivilTime::new(0, 1, 0).unwrap(),
            0,
        )
        .unwrap();
        scheduler.synchronize_if_due(boundary, Duration::from_secs(60));
        let request = scheduler.playback.take_decode_request().unwrap();
        assert_eq!(
            scheduler.complete_decode(request, Ok(frame(255)), boundary, Duration::from_secs(60)),
            DecodeOutcome::Transitioning
        );

        let changed = scheduler
            .playback
            .advance_transition(Duration::from_secs(60));
        assert!(!changed);
        assert!(scheduler.playback.is_transitioning());
        assert_eq!(
            surface_action(false, scheduler.playback.is_transitioning()),
            SurfaceAction::Rearm
        );
        assert_eq!(
            surface_action(true, scheduler.playback.is_transitioning()),
            SurfaceAction::Render
        );
        assert!(!render_eligible(true, false, true, true));
        assert!(render_eligible(true, true, true, true));
        assert!(!request_frame(true, true));
        assert!(request_frame(true, false));
    }

    #[test]
    fn clock_resynchronization_requests_redraw_when_it_finishes_a_transition() {
        let playback = Playback::new(
            &time_metadata(),
            image(0),
            AppearancePreference::Automatic,
            clock(0, 0, 0),
            Duration::ZERO,
            false,
        );
        let mut scheduler = Scheduler::new(playback, clock(0, 0, 0), Duration::ZERO);
        complete_initial(&mut scheduler.playback);
        let boundary = ClockSnapshot::new(
            CivilDate::new(2026, 9, 10).unwrap(),
            CivilTime::new(0, 1, 0).unwrap(),
            0,
        )
        .unwrap();
        scheduler.synchronize_if_due(boundary, Duration::from_secs(60));
        let request = scheduler.playback.take_decode_request().unwrap();
        scheduler.complete_decode(request, Ok(frame(255)), boundary, Duration::from_secs(60));
        assert!(scheduler
            .playback
            .advance_transition(Duration::from_secs(61)));

        assert_eq!(scheduler.next_synchronize, Some(Duration::from_secs(120)));
        let changed = scheduler.prepare_presentation(
            ClockSnapshot::new(
                CivilDate::new(2026, 9, 10).unwrap(),
                CivilTime::new(0, 1, 1).unwrap(),
                3_600,
            )
            .unwrap(),
            Duration::from_secs(61),
            false,
        );
        assert!(changed);
        assert!(!scheduler.playback.is_transitioning());
    }

    #[test]
    fn presentation_samples_an_expired_transition_without_a_frame_callback() {
        let (mut scheduler, _) = transitioning_scheduler();
        let after_dissolve = ClockSnapshot::new(
            CivilDate::new(2026, 9, 10).unwrap(),
            CivilTime::new(0, 1, 3).unwrap(),
            0,
        )
        .unwrap();
        let mut hotplugged = PresentationState::new();

        assert_eq!(hotplugged.next_action(true, true), SurfaceAction::Render);
        assert!(scheduler.prepare_presentation(after_dissolve, Duration::from_secs(63), false));
        assert!(!scheduler.playback.is_transitioning());
        assert_eq!(scheduler.playback.frame(), Some(&frame(255)));
        hotplugged.rendered(false);
        assert_eq!(hotplugged.next_action(false, true), SurfaceAction::Idle);
    }

    #[test]
    fn subsecond_suspend_snaps_callback_and_decode_paths() {
        let mut detector = SuspendDetector::new(Some(Duration::from_secs(5)));
        assert!(detector.observe(Duration::from_millis(5_800)));

        let (mut callback_scheduler, boundary) = transitioning_scheduler();
        assert_eq!(
            callback_scheduler.next_synchronize,
            Some(Duration::from_secs(120))
        );
        assert!(callback_scheduler.prepare_presentation(
            boundary,
            Duration::from_millis(60_800),
            true,
        ));
        assert!(!callback_scheduler.playback.is_transitioning());

        let initial = clock(0, 0, 0);
        let playback = Playback::new(
            &time_metadata(),
            image(0),
            AppearancePreference::Automatic,
            initial,
            Duration::ZERO,
            false,
        );
        let mut decode_scheduler = Scheduler::new(playback, initial, Duration::ZERO);
        complete_initial(&mut decode_scheduler.playback);
        decode_scheduler.synchronize_if_due(boundary, Duration::from_secs(60));
        let stale = decode_scheduler.playback.take_decode_request().unwrap();
        let (outcome, _) = decode_scheduler.synchronize_and_complete_decode(
            stale,
            Ok(frame(255)),
            boundary,
            Duration::from_millis(60_800),
            true,
        );
        assert_eq!(outcome, DecodeOutcome::Ignored);
        assert!(decode_scheduler.playback.take_decode_request().is_some());
    }

    #[test]
    fn suspend_detector_ignores_lower_jitter_without_lowering_its_baseline() {
        let mut detector = SuspendDetector::new(Some(Duration::from_secs(5)));
        assert!(!detector.observe(Duration::from_millis(4_980)));
        assert!(!detector.observe(Duration::from_secs(5)));
        assert!(detector.observe(Duration::from_millis(5_800)));
    }

    #[test]
    fn static_decode_failure_schedules_a_retry_after_fallback_failure() {
        let mut metadata = Metadata::default();
        metadata
            .insert(
                AppleProperty::Appearance,
                PropertyValue::Appearance(Appearance {
                    light: image(1),
                    dark: image(2),
                }),
            )
            .unwrap();
        let now = clock(12, 0, 0);
        let playback = Playback::new(
            &metadata,
            image(0),
            AppearancePreference::Dark,
            now,
            Duration::ZERO,
            false,
        );
        let mut scheduler = Scheduler::new(playback, now, Duration::ZERO);
        let selected = scheduler.playback.take_decode_request().unwrap();
        assert_eq!(
            scheduler.complete_decode(selected, Err(()), now, Duration::ZERO),
            DecodeOutcome::Failed
        );
        let fallback = scheduler.playback.take_decode_request().unwrap();
        assert_eq!(
            scheduler.complete_decode(fallback, Err(()), now, Duration::ZERO),
            DecodeOutcome::Failed
        );
        assert_eq!(scheduler.next_synchronize, Some(Duration::from_secs(60)));
    }

    #[test]
    fn decode_completion_preserves_an_earlier_clock_synchronization() {
        let playback = Playback::new(
            &time_metadata(),
            image(0),
            AppearancePreference::Automatic,
            clock(0, 0, 0),
            Duration::ZERO,
            false,
        );
        let mut scheduler = Scheduler::new(playback, clock(0, 0, 0), Duration::ZERO);
        let request = scheduler.playback.take_decode_request().unwrap();
        let changed_clock = ClockSnapshot::new(
            CivilDate::new(2026, 9, 10).unwrap(),
            CivilTime::new(12, 0, 59).unwrap(),
            0,
        )
        .unwrap();
        scheduler.complete_decode(
            request,
            Ok(frame(10)),
            changed_clock,
            Duration::from_secs(59),
        );
        assert_eq!(scheduler.next_synchronize, Some(Duration::from_secs(60)));
    }

    #[test]
    fn overdue_synchronization_rejects_the_older_decode_result() {
        let playback = Playback::new(
            &time_metadata(),
            image(0),
            AppearancePreference::Automatic,
            clock(0, 0, 0),
            Duration::ZERO,
            false,
        );
        let mut scheduler = Scheduler::new(playback, clock(0, 0, 0), Duration::ZERO);
        complete_initial(&mut scheduler.playback);
        let first_boundary = ClockSnapshot::new(
            CivilDate::new(2026, 9, 10).unwrap(),
            CivilTime::new(0, 1, 0).unwrap(),
            0,
        )
        .unwrap();
        scheduler.synchronize_if_due(first_boundary, Duration::from_secs(60));
        let obsolete = scheduler.playback.take_decode_request().unwrap();
        let second_boundary = ClockSnapshot::new(
            CivilDate::new(2026, 9, 10).unwrap(),
            CivilTime::new(0, 2, 0).unwrap(),
            0,
        )
        .unwrap();
        let (outcome, _) = scheduler.synchronize_and_complete_decode(
            obsolete,
            Ok(frame(100)),
            second_boundary,
            Duration::from_secs(120),
            false,
        );
        assert_eq!(outcome, DecodeOutcome::Ignored);
        assert_eq!(scheduler.playback.selected(), image(2));
        assert_eq!(scheduler.playback.frame(), Some(&frame(0)));
    }

    #[test]
    fn presentation_state_preserves_callbacks_and_latches_failures() {
        let mut geometry = PresentationState::new();
        geometry.rendered(true);
        geometry.geometry_changed();
        assert_eq!(geometry.next_action(true, true), SurfaceAction::Render);
        geometry.rendered(false);
        assert!(geometry.frame_pending);
        geometry.frame_arrived();
        assert_eq!(geometry.next_action(false, true), SurfaceAction::Idle);
        assert!(!geometry.frame_pending);

        let mut failed = PresentationState::new();
        failed.disable();
        let mut healthy = PresentationState::new();
        healthy.rendered(true);
        healthy.frame_arrived();
        failed.playback_changed();
        healthy.playback_changed();
        assert_eq!(failed.next_action(true, true), SurfaceAction::Idle);
        assert_eq!(healthy.next_action(true, true), SurfaceAction::Render);
        failed.geometry_changed();
        assert_eq!(failed.next_action(true, true), SurfaceAction::Render);
    }

    #[test]
    fn retiring_pending_callback_releases_owned_resource_once() {
        let mut pending = Some(7);
        let mut retired = Vec::new();
        retire_pending(&mut pending, |callback| retired.push(callback));
        retire_pending(&mut pending, |callback| retired.push(callback));
        assert_eq!(retired, [7]);
        assert!(pending.is_none());
    }

    #[test]
    fn buffer_release_and_output_hotplug_reenter_presentation() {
        let mut blocked = PresentationState::new();
        assert_eq!(buffer_decision(&[false, false], 2), BufferDecision::Wait);
        assert_eq!(blocked.next_action(false, true), SurfaceAction::Render);
        assert_eq!(blocked.next_action(false, true), SurfaceAction::Render);
        assert_eq!(buffer_decision(&[true, false], 2), BufferDecision::Reuse(0));
        blocked.rendered(false);
        assert_eq!(blocked.next_action(false, true), SurfaceAction::Idle);

        let mut surfaces = Vec::<PresentationState>::new();
        assert!(surfaces.is_empty());
        surfaces.push(PresentationState::new());
        assert_eq!(surfaces[0].next_action(false, true), SurfaceAction::Render);
    }

    #[test]
    fn zero_configure_dimensions_use_output_or_previous_geometry() {
        assert_eq!(
            resolve_configure_size((0, 0), None, Some((3840, 2160))),
            Some((3840, 2160))
        );
        assert_eq!(
            resolve_configure_size((1920, 0), Some((800, 600)), None),
            Some((1920, 600))
        );
        assert_eq!(resolve_configure_size((0, 0), None, None), None);
        assert_eq!(
            mode_logical_size((3840, 2160), 2, wl_output::Transform::Normal),
            Some((1920, 1080))
        );
        assert_eq!(
            mode_logical_size((3840, 2160), 2, wl_output::Transform::_90),
            Some((1080, 1920))
        );
    }

    #[test]
    fn renderer_bounds_geometry_and_waits_for_buffer_release() {
        assert_eq!(
            buffer_geometry((3840, 2160), 2).unwrap(),
            BufferGeometry {
                width: 7680,
                height: 4320,
                bytes: 132_710_400,
            }
        );
        assert!(buffer_geometry((16_384, 16_384), 2).is_err());
        assert_eq!(buffer_decision(&[], 0), BufferDecision::Allocate);
        assert_eq!(buffer_decision(&[false], 1), BufferDecision::Allocate);
        assert_eq!(buffer_decision(&[false, false], 2), BufferDecision::Wait);
        assert_eq!(buffer_decision(&[false, true], 2), BufferDecision::Reuse(1));
    }

    #[test]
    fn document_initialization_failure_is_reported_from_the_worker() {
        let mut decoder = Decoder::spawn(PathBuf::from("/does/not/exist.heic")).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match decoder.receive().unwrap() {
                Some(WorkerResult::InitializationFailed) => break,
                Some(_) => panic!("invalid source unexpectedly initialized"),
                None if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
                None => panic!("worker did not report initialization failure"),
            }
        }
    }

    #[test]
    fn cover_is_centered_for_wide_tall_and_equal_sources() {
        let frame = RgbaFrame {
            width: 4,
            height: 1,
            pixels: [
                [255, 0, 0, 255],
                [0, 255, 0, 255],
                [0, 0, 255, 255],
                [255, 255, 255, 255],
            ]
            .concat(),
        };
        let mut target = vec![0; 2 * 2 * 4];
        render_cover_argb(&mut target, 2, 2, Some(&frame));
        assert_eq!(&target[..4], &[0, 255, 0, 255]);
        assert_eq!(&target[4..8], &[255, 0, 0, 255]);

        let tall = RgbaFrame {
            width: 1,
            height: 4,
            pixels: frame.pixels.clone(),
        };
        render_cover_argb(&mut target, 2, 2, Some(&tall));
        assert_eq!(&target[..4], &[0, 255, 0, 255]);
        assert_eq!(&target[8..12], &[255, 0, 0, 255]);
    }

    #[test]
    fn missing_frame_is_an_opaque_bounded_fallback() {
        let mut target = vec![0; 8];
        render_cover_argb(&mut target, 2, 1, None);
        assert_eq!(target, [24, 9, 5, 255, 24, 9, 5, 255]);
    }

    #[test]
    fn meaningful_location_change_ignores_jitter_and_detects_moves() {
        let base = GeoLocation::new(37.7749, -122.4194, 5_000.0).unwrap();
        let jitter = GeoLocation::new(37.7849, -122.4194, 5_000.0).unwrap();
        assert!(!meaningful_location_change(base, jitter));

        let moved = GeoLocation::new(37.8749, -122.4194, 5_000.0).unwrap();
        assert!(meaningful_location_change(base, moved));

        let east = GeoLocation::new(0.0, 179.99, 5_000.0).unwrap();
        let west = GeoLocation::new(0.0, -179.99, 5_000.0).unwrap();
        assert!(!meaningful_location_change(east, west));
    }

    #[test]
    fn map_solar_requires_a_solar_schedule_and_a_valid_location() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/dynamic-heic/synthetic-all-properties.heic");
        let document = Document::open(&path).unwrap();
        let location = GeoLocation::new(40.0, -75.0, 1_000.0).unwrap();

        let mapped = map_solar(document.metadata(), location, clock(6, 0, 0)).unwrap();
        assert_eq!(
            mapped.points().len(),
            document.metadata().solar().unwrap().points().len()
        );

        let mut without_solar = Metadata::default();
        without_solar
            .insert(
                AppleProperty::Appearance,
                PropertyValue::Appearance(Appearance {
                    light: image(0),
                    dark: image(1),
                }),
            )
            .unwrap();
        assert!(map_solar(&without_solar, location, clock(6, 0, 0)).is_none());
    }
}
