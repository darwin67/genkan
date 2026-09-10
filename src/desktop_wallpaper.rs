use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::time::{Duration, Instant};

use calloop::EventLoop;
use calloop_wayland_source::WaylandSource;
use chrono::{Datelike, Local, Offset, Timelike};
use genkan::dynamic_wallpaper::heic::{Document, RgbaFrame};
use genkan::dynamic_wallpaper::playback::{DecodeOutcome, DecodeRequest, Playback};
use genkan::dynamic_wallpaper::{AppearancePreference, CivilDate, CivilTime, ClockSnapshot};
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
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
use wayland_client::protocol::{wl_output, wl_region, wl_shm, wl_surface};
use wayland_client::{Connection, QueueHandle};

const BUFFER_COUNT: usize = 2;
const MAX_BUFFER_BYTES: usize = 256 * 1024 * 1024;
const MAX_SURFACE_DIMENSION: u32 = 16_384;
const WORKER_POLL_INTERVAL: Duration = Duration::from_millis(20);
const FRAME_INTERVAL: Duration = Duration::from_millis(16);
const FALLBACK_RGB: [u8; 3] = [5, 9, 24];

pub struct Config {
    pub file: PathBuf,
    pub appearance: AppearancePreference,
    pub reduced_motion: bool,
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

struct Decoder {
    requests: SyncSender<DecodeRequest>,
    results: Receiver<DecodeResult>,
    in_flight: bool,
}

impl Decoder {
    fn spawn(document: Document) -> Result<Self, Error> {
        let (requests, request_receiver) = mpsc::sync_channel::<DecodeRequest>(1);
        let (result_sender, results) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("genkan-heic-decode".into())
            .spawn(move || {
                while let Ok(request) = request_receiver.recv() {
                    let frame = document.decode(request.image).map_err(|_| ());
                    if result_sender.send(DecodeResult { request, frame }).is_err() {
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

    fn receive(&mut self) -> Result<Option<DecodeResult>, Error> {
        match self.results.try_recv() {
            Ok(result) => {
                self.in_flight = false;
                Ok(Some(result))
            }
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => {
                Err(Error::Runtime("HEIC decoder worker stopped".into()))
            }
        }
    }
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
    size: Option<(u32, u32)>,
    scale: i32,
    geometry_generation: u64,
    buffers: Vec<SurfaceBuffer>,
    redraw: bool,
    frame_pending: bool,
}

struct Runtime {
    compositor: CompositorState,
    layer_shell: LayerShell,
    output_state: OutputState,
    registry_state: RegistryState,
    shm: Shm,
    surfaces: Vec<Surface>,
    playback: Playback,
    decoder: Decoder,
    started_at: Instant,
    next_synchronize: Option<Duration>,
    failure: Option<Error>,
}

pub fn run(config: Config) -> Result<(), Error> {
    let document = Document::open(&config.file)?;
    let clock = current_clock()?;
    let playback = Playback::new(
        document.metadata(),
        document.primary_image(),
        config.appearance,
        clock,
        Duration::ZERO,
        config.reduced_motion,
    );
    let conn = Connection::connect_to_env().map_err(|error| Error::Connect(error.to_string()))?;
    let (globals, mut event_queue) =
        registry_queue_init(&conn).map_err(|error| Error::Runtime(error.to_string()))?;
    let qh = event_queue.handle();
    let compositor = CompositorState::bind(&globals, &qh)
        .map_err(|error| Error::MissingProtocol(format!("wl_compositor ({error})")))?;
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
        playback,
        decoder: Decoder::spawn(document)?,
        started_at: Instant::now(),
        next_synchronize: Some(Duration::ZERO),
        failure: None,
    };
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
    for output in outputs {
        runtime.add_surface(output, &qh);
    }
    runtime.dispatch_decode()?;
    runtime.schedule_next_synchronization(clock);

    let mut event_loop: EventLoop<'static, Runtime> =
        EventLoop::try_new().map_err(|error| Error::Runtime(error.to_string()))?;
    WaylandSource::new(conn, event_queue)
        .insert(event_loop.handle())
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
        if self.surfaces.iter().any(|surface| surface.output == output) {
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
            size: None,
            scale: 1,
            geometry_generation: 0,
            buffers: Vec::with_capacity(BUFFER_COUNT),
            redraw: true,
            frame_pending: false,
        });
    }

    fn maintain(&mut self, qh: &QueueHandle<Self>) -> Result<(), Error> {
        let monotonic = self.started_at.elapsed();
        if let Some(result) = self.decoder.receive()? {
            match self
                .playback
                .complete_decode(result.request, result.frame, monotonic)
            {
                DecodeOutcome::Presented | DecodeOutcome::Transitioning => self.redraw_all(),
                DecodeOutcome::Failed | DecodeOutcome::Rejected => {
                    eprintln!(
                        "genkan wallpaper: retaining the last valid frame after decode failure"
                    )
                }
                DecodeOutcome::Ignored => {}
            }
            self.dispatch_decode()?;
        }
        if self
            .next_synchronize
            .is_some_and(|deadline| monotonic >= deadline)
        {
            let clock = current_clock()?;
            self.playback.synchronize(clock, monotonic);
            self.dispatch_decode()?;
            self.schedule_next_synchronization(clock);
        }
        let ready = self
            .surfaces
            .iter()
            .enumerate()
            .filter_map(|(index, surface)| {
                (surface.redraw && !surface.frame_pending && surface.size.is_some())
                    .then_some(index)
            })
            .collect::<Vec<_>>();
        for index in ready {
            self.render(index, qh)?;
        }
        Ok(())
    }

    fn dispatch_decode(&mut self) -> Result<(), Error> {
        if self.decoder.in_flight {
            return Ok(());
        }
        if let Some(request) = self.playback.take_decode_request() {
            self.decoder.dispatch(request)?;
        }
        Ok(())
    }

    fn schedule_next_synchronization(&mut self, clock: ClockSnapshot) {
        let monotonic = self.started_at.elapsed();
        self.next_synchronize = self
            .playback
            .next_wake(clock, monotonic)
            .and_then(|delay| monotonic.checked_add(delay));
    }

    fn dispatch_timeout(&self) -> Option<Duration> {
        if self.decoder.in_flight {
            Some(WORKER_POLL_INTERVAL)
        } else if self.playback.is_transitioning() {
            Some(FRAME_INTERVAL)
        } else {
            self.next_synchronize
                .map(|deadline| deadline.saturating_sub(self.started_at.elapsed()))
        }
    }

    fn redraw_all(&mut self) {
        for surface in &mut self.surfaces {
            surface.redraw = true;
        }
    }

    fn render(&mut self, index: usize, qh: &QueueHandle<Self>) -> Result<(), Error> {
        let surface = &mut self.surfaces[index];
        let (width, height) = surface
            .size
            .ok_or_else(|| Error::Runtime("attempted to render an unconfigured surface".into()))?;
        let scale = surface.scale.max(1) as u32;
        let buffer_width = width
            .checked_mul(scale)
            .filter(|width| *width <= MAX_SURFACE_DIMENSION)
            .ok_or_else(|| Error::Runtime("unsafe wallpaper surface width".into()))?;
        let buffer_height = height
            .checked_mul(scale)
            .filter(|height| *height <= MAX_SURFACE_DIMENSION)
            .ok_or_else(|| Error::Runtime("unsafe wallpaper surface height".into()))?;
        let bytes = usize::try_from(buffer_width)
            .ok()
            .and_then(|width| width.checked_mul(buffer_height as usize))
            .and_then(|pixels| pixels.checked_mul(4))
            .filter(|bytes| *bytes <= MAX_BUFFER_BYTES)
            .ok_or_else(|| Error::Runtime("wallpaper buffer exceeds resource limit".into()))?;
        let size = (buffer_width, buffer_height);
        surface.buffers.retain_mut(|buffer| {
            (buffer.size == size && buffer.generation == surface.geometry_generation)
                || buffer.pool.canvas(&buffer.buffer).is_none()
        });
        let reusable = surface
            .buffers
            .iter_mut()
            .position(|buffer| buffer.pool.canvas(&buffer.buffer).is_some());
        let buffer_index = if let Some(index) = reusable {
            let buffer = &mut surface.buffers[index];
            let canvas = buffer
                .pool
                .canvas(&buffer.buffer)
                .expect("buffer release state changed without dispatch");
            render_cover_argb(canvas, buffer_width, buffer_height, self.playback.frame());
            index
        } else {
            if surface.buffers.len() >= BUFFER_COUNT {
                return Ok(());
            }
            let capacity = bytes
                .checked_add(63)
                .map(|capacity| capacity & !63)
                .ok_or_else(|| Error::Runtime("wallpaper buffer capacity overflow".into()))?;
            let mut pool = SlotPool::new(capacity, &self.shm).map_err(|error| {
                Error::Runtime(format!("could not allocate wallpaper pool: {error}"))
            })?;
            let (buffer, canvas) = pool
                .create_buffer(
                    buffer_width as i32,
                    buffer_height as i32,
                    (buffer_width * 4) as i32,
                    wl_shm::Format::Argb8888,
                )
                .map_err(|error| {
                    Error::Runtime(format!("could not allocate wallpaper buffer: {error}"))
                })?;
            render_cover_argb(canvas, buffer_width, buffer_height, self.playback.frame());
            surface.buffers.push(SurfaceBuffer {
                size,
                generation: surface.geometry_generation,
                buffer,
                pool,
            });
            surface.buffers.len() - 1
        };
        let wl_surface = surface.layer.wl_surface();
        surface.buffers[buffer_index]
            .buffer
            .attach_to(wl_surface)
            .map_err(|error| {
                Error::Runtime(format!("could not attach wallpaper buffer: {error}"))
            })?;
        let opaque = self.compositor.wl_compositor().create_region(qh, ());
        opaque.add(0, 0, width as i32, height as i32);
        wl_surface.set_opaque_region(Some(&opaque));
        opaque.destroy();
        wl_surface.set_buffer_scale(scale as i32);
        wl_surface.set_buffer_transform(wl_output::Transform::Normal);
        wl_surface.damage_buffer(0, 0, buffer_width as i32, buffer_height as i32);
        let transitioning = self.playback.is_transitioning();
        if transitioning {
            wl_surface.frame(qh, wl_surface.clone());
        }
        wl_surface.commit();
        surface.redraw = false;
        surface.frame_pending = transitioning;
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
        let Some(surface) = self
            .surfaces
            .iter_mut()
            .find(|surface| surface.layer == *layer)
        else {
            self.failure = Some(Error::Runtime(
                "configured an unknown wallpaper surface".into(),
            ));
            return;
        };
        if configure.new_size.0 == 0 || configure.new_size.1 == 0 {
            self.failure = Some(Error::Runtime(
                "compositor configured an empty wallpaper surface".into(),
            ));
            return;
        }
        if surface.size != Some(configure.new_size) {
            surface.size = Some(configure.new_size);
            surface.geometry_generation = surface.geometry_generation.wrapping_add(1);
            surface.redraw = true;
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
                surface.redraw = true;
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
            surface.redraw = true;
        }
    }

    fn frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        wl_surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
        if let Some(surface) = self
            .surfaces
            .iter_mut()
            .find(|surface| surface.layer.wl_surface() == wl_surface)
        {
            surface.frame_pending = false;
        }
        if self.playback.advance_transition(self.started_at.elapsed()) {
            self.redraw_all();
        }
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
}
