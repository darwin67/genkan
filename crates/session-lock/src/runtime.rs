use std::fs::File;
use std::io::Write;
use std::os::fd::OwnedFd;
use std::time::Duration;

use calloop::EventLoop;
use calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::registry_handlers;
use smithay_client_toolkit::session_lock::{
    SessionLock, SessionLockHandler, SessionLockState, SessionLockSurface,
    SessionLockSurfaceConfigure,
};
use smithay_client_toolkit::shm::{raw::RawPool, Shm, ShmHandler};
use thiserror::Error;
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::{wl_buffer, wl_output, wl_region, wl_shm, wl_surface};
use wayland_client::{Connection, Proxy, QueueHandle};

use super::{Action, Config, Event, Presentation, Refresh, RgbaFrame, State};

const FRAME_INTERVAL: Duration = Duration::from_millis(16);
const FALLBACK_RGB: [u8; 3] = [5, 9, 24];
const DIM_NUMERATOR: u16 = 4;
const DIM_DENOMINATOR: u16 = 5;
const MAX_SURFACE_PIXELS: usize = 16_384 * 16_384;

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
}

struct Runtime {
    #[cfg(feature = "lock-test")]
    conn: Connection,
    compositor: CompositorState,
    output_state: OutputState,
    registry_state: RegistryState,
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
}

pub(super) fn run(config: Config) -> Result<(), Error> {
    let ready = ReadySignal::new(config.ready_fd);
    let conn = Connection::connect_to_env().map_err(|error| Error::Connect(error.to_string()))?;
    let (globals, mut event_queue) =
        registry_queue_init(&conn).map_err(|error| Error::Runtime(error.to_string()))?;
    let qh = event_queue.handle();
    let compositor = CompositorState::bind(&globals, &qh)
        .map_err(|error| Error::MissingProtocol(format!("wl_compositor ({error})")))?;
    let shm = Shm::bind(&globals, &qh)
        .map_err(|error| Error::MissingProtocol(format!("wl_shm ({error})")))?;

    let mut runtime = Runtime {
        #[cfg(feature = "lock-test")]
        conn: conn.clone(),
        compositor,
        output_state: OutputState::new(&globals, &qh),
        registry_state: RegistryState::new(&globals),
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
        match refresh_presentation(runtime.presentation.as_mut(), &mut runtime.state) {
            Refresh::Unchanged => {}
            Refresh::Frame => runtime.redraw_all(&qh),
            Refresh::Failed => {
                eprintln!("genkan lock: presentation failed; retaining opaque fallback");
                runtime.redraw_all(&qh);
            }
        }
    }

    runtime.failure.map_or(Ok(()), Err)
}

impl Runtime {
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
            "genkan lock: created opaque surface for output {}",
            output.id().protocol_id()
        );
        self.surfaces.push(Surface {
            output,
            lock_surface,
            size: None,
            scale: 1,
            transform: wl_output::Transform::Normal,
        });
        Ok(())
    }

    fn redraw_all(&mut self, qh: &QueueHandle<Self>) {
        let configured = self
            .surfaces
            .iter()
            .enumerate()
            .filter_map(|(index, surface)| surface.size.map(|_| index))
            .collect::<Vec<_>>();
        for index in configured {
            if let Err(error) = self.render(index, qh) {
                self.fail(error);
                return;
            }
        }
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
        let mut pool = RawPool::new(bytes, &self.shm)
            .map_err(|error| Error::Runtime(format!("could not allocate lock buffer: {error}")))?;
        draw_opaque(
            pool.mmap(),
            buffer_width,
            buffer_height,
            self.presentation.frame().as_ref(),
        );
        let buffer = pool.create_buffer(
            0,
            buffer_width as i32,
            buffer_height as i32,
            (buffer_width * 4) as i32,
            wl_shm::Format::Argb8888,
            (),
            qh,
        );
        let wl_surface = surface.lock_surface.wl_surface();
        let region = self.compositor.wl_compositor().create_region(qh, ());
        region.add(0, 0, width as i32, height as i32);
        wl_surface.set_opaque_region(Some(&region));
        region.destroy();
        wl_surface.set_buffer_scale(scale as i32);
        wl_surface.set_buffer_transform(surface.transform);
        wl_surface.attach(Some(&buffer), 0, 0);
        wl_surface.damage_buffer(0, 0, buffer_width as i32, buffer_height as i32);
        wl_surface.commit();
        buffer.destroy();
        Ok(())
    }

    fn apply(&mut self, action: Action) {
        match action {
            Action::None => {}
            Action::ReportReady => {
                eprintln!("genkan lock: compositor confirmed lock");
                if let Err(error) = self.ready.apply(action) {
                    self.fail(Error::Runtime(format!(
                        "could not report lock readiness: {error}"
                    )));
                }
                #[cfg(feature = "lock-test")]
                if self.test_unlock_after_ready && !self.terminate {
                    let action = self
                        .state
                        .authorize_unlock(super::UnlockAuthorization::test_source());
                    self.apply(action);
                }
            }
            Action::Abort => {
                self.fail(Error::LockFinished);
            }
            #[cfg(any(test, feature = "lock-test"))]
            Action::UnlockAndSynchronize => {
                #[cfg(feature = "lock-test")]
                {
                    eprintln!("genkan lock: test authorization accepted; unlocking");
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
    }

    fn fail(&mut self, error: Error) {
        let _ = self.state.update(Event::RuntimeFailed);
        self.failure.get_or_insert(error);
        self.terminate = true;
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
        let action = self.state.update(Event::LockFinished);
        self.apply(action);
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
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
        self.surfaces[index].size = Some(configure.new_size);
        if let Err(error) = self.render(index, qh) {
            self.fail(error);
        }
    }
}

impl CompositorHandler for Runtime {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        scale: i32,
    ) {
        if let Some(index) = self
            .surfaces
            .iter()
            .position(|item| item.lock_surface.wl_surface() == surface)
        {
            self.surfaces[index].scale = scale.max(1);
            if self.surfaces[index].size.is_some() {
                if let Err(error) = self.render(index, qh) {
                    self.fail(error);
                }
            }
        }
    }

    fn transform_changed(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        transform: wl_output::Transform,
    ) {
        if let Some(index) = self
            .surfaces
            .iter()
            .position(|item| item.lock_surface.wl_surface() == surface)
        {
            self.surfaces[index].transform = transform;
            if self.surfaces[index].size.is_some() {
                if let Err(error) = self.render(index, qh) {
                    self.fail(error);
                }
            }
        }
    }

    fn frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
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
        if self.session_lock.is_some() {
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

    registry_handlers![OutputState];
}

impl ShmHandler for Runtime {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

smithay_client_toolkit::delegate_compositor!(Runtime);
smithay_client_toolkit::delegate_output!(Runtime);
smithay_client_toolkit::delegate_registry!(Runtime);
smithay_client_toolkit::delegate_session_lock!(Runtime);
smithay_client_toolkit::delegate_shm!(Runtime);
wayland_client::delegate_noop!(Runtime: ignore wl_buffer::WlBuffer);
wayland_client::delegate_noop!(Runtime: ignore wl_region::WlRegion);

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
    width
        .checked_mul(scale)
        .zip(height.checked_mul(scale))
        .ok_or_else(|| Error::Runtime("compositor lock-surface dimensions overflow".into()))
}

fn draw_opaque(target: &mut [u8], width: u32, height: u32, frame: Option<&RgbaFrame>) {
    for (index, pixel) in target.chunks_exact_mut(4).enumerate() {
        let x = index as u32 % width;
        let y = index as u32 / width;
        let [red, green, blue] = frame
            .filter(|frame| frame.width > 0 && frame.height > 0)
            .map(|frame| sample_cover(frame, width, height, x, y))
            .unwrap_or(FALLBACK_RGB);
        pixel[0] = dim(blue);
        pixel[1] = dim(green);
        pixel[2] = dim(red);
        pixel[3] = u8::MAX;
    }
}

fn sample_cover(frame: &RgbaFrame, width: u32, height: u32, x: u32, y: u32) -> [u8; 3] {
    let source_wider =
        u64::from(frame.width) * u64::from(height) > u64::from(width) * u64::from(frame.height);
    let (scaled_width, scaled_height) = if source_wider {
        (
            u64::from(frame.width) * u64::from(height) / u64::from(frame.height),
            u64::from(height),
        )
    } else {
        (
            u64::from(width),
            u64::from(frame.height) * u64::from(width) / u64::from(frame.width),
        )
    };
    let crop_x = scaled_width.saturating_sub(u64::from(width)) / 2;
    let crop_y = scaled_height.saturating_sub(u64::from(height)) / 2;
    let source_x = ((u64::from(x) + crop_x) * u64::from(frame.width) / scaled_width)
        .min(u64::from(frame.width - 1));
    let source_y = ((u64::from(y) + crop_y) * u64::from(frame.height) / scaled_height)
        .min(u64::from(frame.height - 1));
    let offset = ((source_y * u64::from(frame.width) + source_x) * 4) as usize;
    [
        frame.pixels[offset],
        frame.pixels[offset + 1],
        frame.pixels[offset + 2],
    ]
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

struct ReadySignal(Option<File>);

impl ReadySignal {
    fn new(fd: Option<OwnedFd>) -> Self {
        Self(fd.map(File::from))
    }

    fn apply(&mut self, action: Action) -> std::io::Result<()> {
        if action == Action::ReportReady {
            if let Some(mut ready) = self.0.take() {
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
    fn valid_readiness_descriptor_reports_exact_protocol_message() {
        let (mut reader, writer) = UnixStream::pair().unwrap();
        let mut ready = ReadySignal::new(Some(writer.into()));

        ready.apply(Action::ReportReady).unwrap();

        let mut message = String::new();
        reader.read_to_string(&mut message).unwrap();
        assert_eq!(message, "READY\n");
        assert!(ready.0.is_none());
    }

    #[test]
    fn readiness_write_failures_are_reported() {
        let read_only = File::open("/dev/null").unwrap();
        let mut ready = ReadySignal::new(Some(read_only.into()));

        assert!(ready.apply(Action::ReportReady).is_err());
        assert!(ready.0.is_none());
    }

    #[test]
    fn fatal_error_before_confirmation_cannot_report_readiness() {
        let (mut reader, writer) = UnixStream::pair().unwrap();
        let mut ready = ReadySignal::new(Some(writer.into()));
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
