use std::fs::File;
use std::io::Write;
use std::os::fd::OwnedFd;
use std::time::Duration;
#[cfg(feature = "lock-test")]
use std::time::Instant;

use calloop::EventLoop;
use calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
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
use wayland_client::{Connection, Proxy, QueueHandle};

use super::{
    Action, Config, Event, Input, Presentation, Refresh, RgbaFrame, State, UnlockAuthorization,
};

const FRAME_INTERVAL: Duration = Duration::from_millis(16);
const FALLBACK_RGB: [u8; 3] = [5, 9, 24];
const DIM_NUMERATOR: u16 = 4;
const DIM_DENOMINATOR: u16 = 5;
const MAX_SURFACE_PIXELS: usize = 16_384 * 16_384;
const MAX_SURFACE_DIMENSION: u32 = 16_384;
const BUFFER_COUNT: usize = 2;
const MAX_BUFFER_BYTES: usize = 256 * 1024 * 1024;
#[cfg(feature = "lock-test")]
const TEST_UNLOCK_DELAY: Duration = Duration::from_secs(5);

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
    lock_surface: SessionLockSurface,
    size: Option<(u32, u32)>,
    scale: i32,
    transform: wl_output::Transform,
    geometry_generation: u64,
    buffers: Vec<SurfaceBuffer>,
    redraw: RedrawState,
    first_presented: bool,
}

struct SurfaceBuffer {
    size: (u32, u32),
    geometry_generation: u64,
    buffer: Buffer,
    pool: SlotPool,
}

#[derive(Default)]
struct RedrawState {
    frame_pending: bool,
    redraw_pending: bool,
    geometry_pending: bool,
}

impl RedrawState {
    fn request(&mut self) -> bool {
        self.redraw_pending = true;
        !self.frame_pending
    }

    fn request_geometry(&mut self) {
        self.redraw_pending = true;
        self.geometry_pending = true;
    }

    fn should_render(&self, current_buffer_count: usize, reusable: bool) -> bool {
        self.redraw_pending
            && (self.geometry_pending
                || (!self.frame_pending && redraw_can_progress(current_buffer_count, reusable)))
    }

    fn request_frame_callback(&self) -> bool {
        !self.frame_pending
    }

    fn committed(&mut self, requested_frame_callback: bool) {
        self.redraw_pending = false;
        self.geometry_pending = false;
        self.frame_pending |= requested_frame_callback;
    }

    fn frame_done(&mut self) {
        self.frame_pending = false;
    }
}

struct Runtime {
    conn: Connection,
    compositor: CompositorState,
    output_state: OutputState,
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
    ready: ReadySignal,
    failure: Option<Error>,
    terminate: bool,
    #[cfg(feature = "lock-test")]
    test_unlock_after_ready: bool,
    #[cfg(feature = "lock-test")]
    test_unlock_at: Option<Instant>,
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
    let mut runtime = Runtime {
        conn: conn.clone(),
        compositor,
        output_state: OutputState::new(&globals, &qh),
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
        ready,
        failure: None,
        terminate: false,
        #[cfg(feature = "lock-test")]
        test_unlock_after_ready: config.test_unlock_after_ready,
        #[cfg(feature = "lock-test")]
        test_unlock_at: None,
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
        let refresh = refresh_presentation(runtime.presentation.as_mut(), &mut runtime.state);
        if runtime.presentation.take_authorization() {
            let action = runtime
                .state
                .authorize_unlock(UnlockAuthorization::authenticated());
            runtime.apply(action);
        }
        if runtime.terminate {
            break;
        }
        match refresh {
            Refresh::Unchanged => {}
            Refresh::Frame => runtime.redraw_all(&qh),
            Refresh::Failed => {
                eprintln!("genkan lock: presentation failed; retaining opaque fallback");
                runtime.redraw_all(&qh);
            }
        }
        if let Err(error) = runtime.maintain_surfaces(&qh) {
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
        self.surfaces.push(Surface {
            output,
            lock_surface,
            size: None,
            scale: 1,
            transform: wl_output::Transform::Normal,
            geometry_generation: 0,
            buffers: Vec::with_capacity(BUFFER_COUNT),
            redraw: RedrawState {
                redraw_pending: true,
                ..RedrawState::default()
            },
            first_presented: false,
        });
        #[cfg(feature = "lock-test")]
        self.test_observer.record(TestEvent::OutputAdded);
        Ok(())
    }

    fn redraw_all(&mut self, qh: &QueueHandle<Self>) {
        if self.terminate {
            return;
        }
        let configured = self
            .surfaces
            .iter_mut()
            .enumerate()
            .filter_map(|(index, surface)| {
                (surface.size.is_some() && surface.redraw.request()).then_some(index)
            })
            .collect::<Vec<_>>();
        for index in configured {
            if let Err(error) = self.render(index, qh) {
                self.fail(error);
                return;
            }
        }
    }

    fn maintain_surfaces(&mut self, qh: &QueueHandle<Self>) -> Result<(), Error> {
        let mut ready_to_redraw = Vec::new();
        for (index, surface) in self.surfaces.iter_mut().enumerate() {
            let Some((width, height)) = surface.size else {
                continue;
            };
            let configured_size = buffer_size(
                width,
                height,
                surface.scale.max(1) as u32,
                surface.transform,
            )?;
            surface.buffers.retain(|item| {
                item.size == configured_size
                    && item.geometry_generation == surface.geometry_generation
            });
            let reusable = surface.buffers.iter_mut().any(|item| {
                item.size == configured_size && item.pool.canvas(&item.buffer).is_some()
            });
            if surface
                .redraw
                .should_render(surface.buffers.len(), reusable)
            {
                ready_to_redraw.push(index);
            }
        }
        for index in ready_to_redraw {
            self.render(index, qh)?;
        }
        Ok(())
    }

    fn render(&mut self, index: usize, qh: &QueueHandle<Self>) -> Result<(), Error> {
        let surface = &self.surfaces[index];
        let (width, height) = surface
            .size
            .ok_or_else(|| Error::Runtime("attempted to render an unconfigured surface".into()))?;
        let scale = surface.scale.max(1) as u32;
        let (buffer_width, buffer_height) = buffer_size(width, height, scale, surface.transform)?;
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
        let transform = surface.transform;
        let geometry_generation = surface.geometry_generation;
        let wl_surface = surface.lock_surface.wl_surface().clone();
        let output_id = surface.output.id().protocol_id();
        let frame = self.presentation.frame();
        let surface = &mut self.surfaces[index];
        surface.buffers.retain(|item| {
            item.size == (buffer_width, buffer_height)
                && item.geometry_generation == geometry_generation
        });
        let reusable = surface.buffers.iter_mut().position(|item| {
            item.size == (buffer_width, buffer_height) && item.pool.canvas(&item.buffer).is_some()
        });
        let buffer_index = if let Some(buffer_index) = reusable {
            let SurfaceBuffer { buffer, pool, .. } = &mut surface.buffers[buffer_index];
            let canvas = pool
                .canvas(buffer)
                .expect("buffer release state changed without dispatch");
            draw_opaque(canvas, buffer_width, buffer_height, frame.as_ref());
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
            draw_opaque(canvas, buffer_width, buffer_height, frame.as_ref());
            surface.buffers.push(SurfaceBuffer {
                size: (buffer_width, buffer_height),
                geometry_generation,
                buffer,
                pool,
            });
            surface.buffers.len() - 1
        };
        surface.buffers[buffer_index]
            .buffer
            .attach_to(&wl_surface)
            .map_err(|error| Error::Runtime(format!("could not attach lock buffer: {error}")))?;
        let region = self.compositor.wl_compositor().create_region(qh, ());
        region.add(0, 0, width as i32, height as i32);
        wl_surface.set_opaque_region(Some(&region));
        region.destroy();
        wl_surface.set_buffer_scale(scale as i32);
        wl_surface.set_buffer_transform(transform);
        wl_surface.damage_buffer(0, 0, buffer_width as i32, buffer_height as i32);
        let request_frame_callback = surface.redraw.request_frame_callback();
        if request_frame_callback {
            wl_surface.frame(qh, wl_surface.clone());
        }
        wl_surface.commit();
        surface.redraw.committed(request_frame_callback);
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
                    self.redraw_all_surfaces();
                }
                #[cfg(feature = "lock-test")]
                if self.test_unlock_after_ready && !self.terminate {
                    self.test_unlock_at = Some(Instant::now() + TEST_UNLOCK_DELAY);
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

    fn redraw_all_surfaces(&mut self) {
        for surface in &mut self.surfaces {
            if surface.size.is_some() {
                surface.redraw.request();
            }
        }
    }

    fn handle_key(&mut self, event: KeyEvent) {
        #[cfg(feature = "lock-test")]
        self.test_observer.record(TestEvent::Keyboard);
        let input = match event.keysym {
            Keysym::BackSpace => Some(Input::Backspace),
            Keysym::Return | Keysym::KP_Enter => Some(Input::Submit),
            Keysym::Escape => Some(Input::Cancel),
            _ => event
                .utf8
                .filter(|text| !text.chars().any(char::is_control))
                .map(Input::Text),
        };
        if input.is_some_and(|input| self.presentation.input(input)) {
            self.redraw_all_surfaces();
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
        let surface = &mut self.surfaces[index];
        if surface.size != Some(configure.new_size) {
            surface.size = Some(configure.new_size);
            surface.geometry_generation = surface.geometry_generation.wrapping_add(1);
            surface.redraw.request_geometry();
            #[cfg(feature = "lock-test")]
            self.test_observer.record(TestEvent::Geometry);
        }
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
        surface: &wl_surface::WlSurface,
        transform: wl_output::Transform,
    ) {
        if let Some(index) = self
            .surfaces
            .iter()
            .position(|item| item.lock_surface.wl_surface() == surface)
        {
            let surface = &mut self.surfaces[index];
            if surface.transform != transform {
                surface.transform = transform;
                surface.geometry_generation = surface.geometry_generation.wrapping_add(1);
                surface.redraw.request_geometry();
                #[cfg(feature = "lock-test")]
                self.test_observer.record(TestEvent::Geometry);
            }
        }
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
            if let Err(error) = self.add_surface(output, qh) {
                self.fail(error);
            }
        }
    }

    fn update_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }

    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        self.surfaces.retain(|surface| surface.output != output);
        #[cfg(feature = "lock-test")]
        self.test_observer.record(TestEvent::OutputRemoved);
        eprintln!(
            "genkan lock: removed surface for output {}",
            output.id().protocol_id()
        );
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

fn buffer_size(
    width: u32,
    height: u32,
    scale: u32,
    transform: wl_output::Transform,
) -> Result<(u32, u32), Error> {
    let rotated = matches!(
        transform,
        wl_output::Transform::_90
            | wl_output::Transform::_270
            | wl_output::Transform::Flipped90
            | wl_output::Transform::Flipped270
    );
    let (width, height) = if rotated {
        (height, width)
    } else {
        (width, height)
    };
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

fn draw_opaque(target: &mut [u8], width: u32, height: u32, frame: Option<&RgbaFrame>) {
    let coordinates = frame.and_then(|frame| cover_coordinates(frame, width, height));
    for (index, pixel) in target.chunks_exact_mut(4).enumerate() {
        let color = coordinates.as_ref().and_then(|(source_x, source_y)| {
            let x = source_x.get(index % width as usize)?;
            let y = source_y.get(index / width as usize)?;
            frame_pixel(frame?, *x, *y)
        });
        let [red, green, blue] = color.unwrap_or(FALLBACK_RGB);
        pixel.copy_from_slice(&[dim(blue), dim(green), dim(red), u8::MAX]);
    }
}

#[cfg(test)]
fn sample_cover(frame: &RgbaFrame, width: u32, height: u32, x: u32, y: u32) -> Option<[u8; 3]> {
    let geometry = cover_geometry(frame, width, height)?;
    frame_pixel(
        frame,
        source_coordinate(x, geometry.0, geometry.2, frame.width),
        source_coordinate(y, geometry.1, geometry.3, frame.height),
    )
}

fn cover_coordinates(frame: &RgbaFrame, width: u32, height: u32) -> Option<(Vec<u32>, Vec<u32>)> {
    let geometry = cover_geometry(frame, width, height)?;
    let source_x = (0..width)
        .map(|x| source_coordinate(x, geometry.0, geometry.2, frame.width))
        .collect();
    let source_y = (0..height)
        .map(|y| source_coordinate(y, geometry.1, geometry.3, frame.height))
        .collect();
    Some((source_x, source_y))
}

fn cover_geometry(frame: &RgbaFrame, width: u32, height: u32) -> Option<(u128, u128, u128, u128)> {
    if frame.width == 0 || frame.height == 0 || width == 0 || height == 0 {
        return None;
    }
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
    Some((crop_x, crop_y, scaled_width, scaled_height))
}

fn source_coordinate(position: u32, crop: u128, scaled: u128, source: u32) -> u32 {
    (((u128::from(position) + crop) * u128::from(source) / scaled).min(u128::from(source - 1)))
        as u32
}

fn frame_pixel(frame: &RgbaFrame, source_x: u32, source_y: u32) -> Option<[u8; 3]> {
    let offset = u64::from(source_y)
        .checked_mul(u64::from(frame.width))?
        .checked_add(u64::from(source_x))?
        .checked_mul(4)
        .and_then(|offset| usize::try_from(offset).ok())?;
    let pixel = frame.pixels.get(offset..offset.checked_add(3)?)?;
    Some([pixel[0], pixel[1], pixel[2]])
}

fn dim(value: u8) -> u8 {
    ((u16::from(value) * DIM_NUMERATOR) / DIM_DENOMINATOR) as u8
}

fn refresh_presentation(presentation: &mut dyn Presentation, state: &mut State) -> Refresh {
    let refresh = presentation.receive_latest();
    if refresh == Refresh::Failed {
        let _ = state.update(Event::PresentationFailed);
    }
    refresh
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
        frame: Option<RgbaFrame>,
    }

    impl Presentation for FakePresentation {
        fn receive_latest(&mut self) -> Refresh {
            self.refresh
        }

        fn frame(&self) -> Option<RgbaFrame> {
            self.frame.clone()
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
    fn transformed_scaled_buffers_cover_the_configured_surface() {
        assert_eq!(
            buffer_size(1920, 1080, 2, wl_output::Transform::Normal).unwrap(),
            (3840, 2160)
        );
        assert_eq!(
            buffer_size(1920, 1080, 2, wl_output::Transform::_90).unwrap(),
            (2160, 3840)
        );
        assert!(buffer_size(u32::MAX, 1, 2, wl_output::Transform::Normal).is_err());
        assert!(buffer_size(16_385, 1, 1, wl_output::Transform::Normal).is_err());
        assert!(buffer_size(0, 1, 1, wl_output::Transform::Normal).is_err());
    }

    #[test]
    fn redraws_are_coalesced_until_the_compositor_finishes_a_frame() {
        let mut redraw = RedrawState::default();

        assert!(redraw.request());
        redraw.committed(true);
        assert!(!redraw.request());
        assert!(!redraw.request());
        redraw.frame_done();
        assert!(redraw.should_render(2, true));
        redraw.committed(true);
        redraw.frame_done();
        assert!(!redraw.should_render(2, true));
    }

    #[test]
    fn geometry_redraw_bypasses_an_obsolete_frame_callback_without_adding_one() {
        let mut redraw = RedrawState::default();
        redraw.request();
        redraw.committed(true);

        redraw.request_geometry();

        assert!(redraw.should_render(0, false));
        assert!(!redraw.request_frame_callback());
        redraw.committed(false);
        assert!(redraw.frame_pending);
        assert!(!redraw.geometry_pending);
        assert!(!redraw.redraw_pending);
    }

    #[test]
    fn frame_callback_defers_latest_geometry_to_post_dispatch_maintenance() {
        let mut redraw = RedrawState::default();
        redraw.request();
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
    fn rendering_forces_opaque_argb_and_dims_wallpaper() {
        let frame = RgbaFrame {
            width: 1,
            height: 1,
            pixels: Bytes::from_static(&[100, 150, 200, 0]),
        };
        let mut target = [0; 8];
        draw_opaque(&mut target, 2, 1, Some(&frame));
        assert_eq!(target, [160, 120, 80, 255, 160, 120, 80, 255]);
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

            assert_eq!(refresh_presentation(&mut presentation, &mut state), refresh);
            let mut target = [0; 4];
            draw_opaque(&mut target, 1, 1, presentation.frame().as_ref());

            assert_eq!(target, [19, 7, 4, 255]);
            assert_eq!(state.lifecycle, super::super::Lifecycle::Locked);
            assert_eq!(state.presentation_failed, refresh == Refresh::Failed);
        }
    }
}
