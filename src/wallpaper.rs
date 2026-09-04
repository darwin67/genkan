use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::{Seek, SeekFrom};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use ::image as image_rs;
use bytes::Bytes;
use clap::ValueEnum;
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use iced::futures::stream;
use iced::widget::{image, Image};
use iced::{ContentFit, Element, Fill, Subscription};
use iced_runtime::image::{Allocation, Error as AllocationError};
use rustix::fs::{open, Mode, OFlags};
use tokio::sync::watch;

const OUTPUT_FRAMES_PER_SECOND: i32 = 30;
const MAX_DIAGNOSTIC_CHARS: usize = 240;
const LOOP_LEAD: Duration = Duration::from_millis(50);
const LOOP_MESSAGE: &str = "genkan-wallpaper-loop";
const AUTOMATIC_STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const SOFTWARE_STARTUP_TIMEOUT: Duration = Duration::from_secs(60);
const FRAME_STALL_TIMEOUT: Duration = Duration::from_secs(10);
const SEEK_STALL_TIMEOUT: Duration = Duration::from_secs(10);
static POSTERS: [OnceLock<Result<image::Handle, String>>; 4] = [const { OnceLock::new() }; 4];

#[derive(Debug, Clone, Copy)]
struct PlaybackSpec {
    install_name: &'static str,
    poster_name: &'static str,
    duration: Duration,
    crossfade: Duration,
}

const WALLPAPERS: [PlaybackSpec; 4] = [
    PlaybackSpec {
        install_name: "tahoe-beach.mov",
        poster_name: "tahoe-beach-poster.jpg",
        duration: Duration::from_micros(120_004_167),
        crossfade: Duration::from_millis(2_000),
    },
    PlaybackSpec {
        install_name: "sequoia-sunrise.mov",
        poster_name: "sequoia-sunrise-poster.jpg",
        duration: Duration::from_micros(120_008_333),
        crossfade: Duration::from_millis(1_000),
    },
    PlaybackSpec {
        install_name: "sequoia-morning.mov",
        poster_name: "sequoia-morning-poster.jpg",
        duration: Duration::from_micros(243_336_667),
        crossfade: Duration::from_millis(1_000),
    },
    PlaybackSpec {
        install_name: "sequoia-night.mov",
        poster_name: "sequoia-night-poster.jpg",
        duration: Duration::from_micros(291_603_333),
        crossfade: Duration::from_millis(2_000),
    },
];

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum Catalog {
    #[default]
    TahoeBeach,
    SequoiaSunrise,
    SequoiaMorning,
    SequoiaNight,
}

impl Catalog {
    fn index(self) -> usize {
        match self {
            Self::TahoeBeach => 0,
            Self::SequoiaSunrise => 1,
            Self::SequoiaMorning => 2,
            Self::SequoiaNight => 3,
        }
    }

    fn spec(self) -> &'static PlaybackSpec {
        &WALLPAPERS[self.index()]
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Settings {
    pub(crate) catalog: Catalog,
    pub(crate) override_path: Option<PathBuf>,
    pub(crate) animate: bool,
}

#[derive(Debug)]
pub(crate) struct State {
    player: Option<Player>,
    poster: Option<image::Handle>,
    frame: Option<image::Handle>,
    allocation: Option<Allocation>,
    allocation_pending: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refresh {
    Unchanged,
    Frame,
    Failed,
}

impl State {
    pub(crate) fn start(settings: Settings) -> Self {
        let spec = settings.catalog.spec();
        let poster = load_poster(settings.catalog)
            .map_err(|error| diagnostic(&error))
            .ok();
        if !settings.animate {
            return Self {
                player: None,
                frame: poster.clone(),
                poster,
                allocation: None,
                allocation_pending: false,
            };
        }

        let result = settings
            .override_path
            .map_or_else(|| packaged_wallpaper_path(spec.install_name), Ok)
            .and_then(|path| Player::start(&path, spec.duration, spec.crossfade));
        match result {
            Ok(player) => Self {
                player: Some(player),
                frame: poster.clone(),
                poster,
                allocation: None,
                allocation_pending: false,
            },
            Err(error) => {
                diagnostic(&error);
                Self {
                    player: None,
                    frame: poster.clone(),
                    poster,
                    allocation: None,
                    allocation_pending: false,
                }
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn disabled() -> Self {
        Self {
            player: None,
            poster: None,
            frame: None,
            allocation: None,
            allocation_pending: false,
        }
    }

    pub(crate) fn subscription(&self) -> Subscription<()> {
        self.player
            .as_ref()
            .map_or_else(Subscription::none, Player::subscription)
    }

    pub(crate) fn receive_latest(&mut self) -> Refresh {
        let Some(update) = self.player.as_ref().and_then(Player::take_latest) else {
            return Refresh::Unchanged;
        };

        match update {
            Update::Frame(frame) => {
                self.allocation = None;
                self.frame = Some(image::Handle::from_rgba(
                    frame.width,
                    frame.height,
                    frame.pixels,
                ));
                Refresh::Frame
            }
            Update::Failed => {
                self.stop_playback();
                Refresh::Failed
            }
        }
    }

    pub(crate) fn prepare_latest(&mut self) -> Option<image::Handle> {
        if self.allocation_pending {
            return None;
        }
        let update = self.player.as_ref().and_then(Player::take_latest)?;

        match update {
            Update::Frame(frame) => {
                self.allocation_pending = true;
                Some(image::Handle::from_rgba(
                    frame.width,
                    frame.height,
                    frame.pixels,
                ))
            }
            Update::Failed => {
                self.stop_playback();
                None
            }
        }
    }

    pub(crate) fn finish_allocation(
        &mut self,
        result: Result<Allocation, AllocationError>,
    ) -> Refresh {
        if !self.allocation_pending {
            return Refresh::Unchanged;
        }
        self.allocation_pending = false;
        if self.stop_after_terminal_failure() {
            return Refresh::Failed;
        }

        match result {
            Ok(allocation) => {
                self.frame = Some(allocation.handle().clone());
                self.allocation = Some(allocation);
                Refresh::Frame
            }
            Err(error) => {
                diagnostic(&pipeline_error(&format!(
                    "wallpaper frame could not be allocated: {error}"
                )));
                self.stop_playback();
                Refresh::Failed
            }
        }
    }

    fn stop_after_terminal_failure(&mut self) -> bool {
        if !self.player.as_ref().is_some_and(Player::has_failed) {
            return false;
        }
        self.stop_playback();
        true
    }

    fn stop_playback(&mut self) {
        self.allocation_pending = false;
        if self.frame.is_none() {
            self.frame.clone_from(&self.poster);
        }
        self.player.take();
    }

    pub(crate) fn rgba_frame(&self) -> Option<genkan_session_lock::RgbaFrame> {
        let image::Handle::Rgba {
            width,
            height,
            pixels,
            ..
        } = self.frame.as_ref()?
        else {
            return None;
        };
        genkan_session_lock::RgbaFrame::new(*width, *height, pixels.clone())
    }

    pub(crate) fn view<Message: 'static>(&self) -> Option<Element<'static, Message>> {
        self.frame.clone().map(|frame| {
            Image::new(frame)
                .width(Fill)
                .height(Fill)
                .content_fit(ContentFit::Cover)
                .into()
        })
    }

    pub(crate) fn has_frame(&self) -> bool {
        self.frame.is_some()
    }

    #[cfg(test)]
    pub(crate) fn decoder_is_stopped(&self) -> bool {
        self.player.is_none()
    }
}

struct Player {
    shared: Arc<Shared>,
    signal: watch::Receiver<u64>,
    stopping: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl std::fmt::Debug for Player {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Player").finish_non_exhaustive()
    }
}

impl Player {
    fn start(path: &Path, duration: Duration, crossfade: Duration) -> Result<Self, String> {
        let file = open_wallpaper(path)?;

        gst::init().map_err(|_| pipeline_error("GStreamer initialization failed"))?;

        let (signal_sender, signal) = watch::channel(0);
        let shared = Arc::new(Shared::default());
        let stopping = Arc::new(AtomicBool::new(false));
        let worker_shared = Arc::clone(&shared);
        let worker_signal = signal_sender;
        let worker_stopping = Arc::clone(&stopping);
        let worker = thread::Builder::new()
            .name("wallpaper-events".into())
            .spawn(move || {
                run_playback(
                    file,
                    duration,
                    crossfade,
                    &worker_shared,
                    &worker_signal,
                    &worker_stopping,
                )
            })
            .map_err(|_| pipeline_error("could not start the wallpaper event worker"))?;

        Ok(Self {
            shared,
            signal,
            stopping,
            worker: Some(worker),
        })
    }

    fn subscription(&self) -> Subscription<()> {
        Subscription::run_with(
            FrameSignal {
                player: Arc::as_ptr(&self.shared) as usize,
                receiver: self.signal.clone(),
            },
            |signal| {
                stream::unfold(signal.receiver.clone(), |mut receiver| async move {
                    receiver.changed().await.ok().map(|()| ((), receiver))
                })
            },
        )
    }

    fn take_latest(&self) -> Option<Update> {
        lock(&self.shared.state).pending.take()
    }

    fn has_failed(&self) -> bool {
        self.shared.failed.load(Ordering::Acquire)
    }
}

struct FrameSignal {
    player: usize,
    receiver: watch::Receiver<u64>,
}

impl Hash for FrameSignal {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.player.hash(state);
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[derive(Default)]
struct Shared {
    state: Mutex<SharedState>,
    sequence: AtomicU64,
    failed: AtomicBool,
    loop_requested: AtomicBool,
}

#[derive(Default)]
struct SharedState {
    pending: Option<Update>,
    last_frame: Option<Frame>,
    transition: Option<LoopTransition>,
    last_frame_at: Option<Instant>,
    awaiting_opening_since: Option<Instant>,
}

struct LoopTransition {
    held: Frame,
    started_at: Instant,
    duration: Duration,
}

enum Update {
    Frame(Frame),
    Failed,
}

#[derive(Clone)]
struct Frame {
    width: u32,
    height: u32,
    pixels: Bytes,
    pts: Option<Duration>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DecoderMode {
    Automatic,
    Software,
}

impl DecoderMode {
    fn startup_timeout(self) -> Duration {
        match self {
            Self::Automatic => AUTOMATIC_STARTUP_TIMEOUT,
            Self::Software => SOFTWARE_STARTUP_TIMEOUT,
        }
    }
}

struct PlaybackPipeline {
    pipeline: gst::Pipeline,
    bus: gst::Bus,
    faulted: Arc<AtomicBool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlaybackOutcome {
    Stopped,
    Failed(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stall {
    Startup,
    Frame,
    Seek,
}

fn element(factory: &str) -> Result<gst::Element, String> {
    gst::ElementFactory::make(factory).build().map_err(|_| {
        pipeline_error(&format!(
            "required GStreamer element {factory} is unavailable"
        ))
    })
}

fn open_wallpaper(path: &Path) -> Result<File, String> {
    let bound = open(path, OFlags::PATH | OFlags::CLOEXEC, Mode::empty())
        .map(File::from)
        .map_err(|_| pipeline_error("wallpaper file is unavailable"))?;
    if !bound
        .metadata()
        .map_err(|_| pipeline_error("wallpaper file metadata is unavailable"))?
        .is_file()
    {
        return Err(pipeline_error("wallpaper path is not a regular file"));
    }
    open(
        format!("/proc/self/fd/{}", bound.as_raw_fd()),
        OFlags::RDONLY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(|_| pipeline_error("wallpaper file could not be opened for playback"))
}

fn build_pipeline(
    file: &File,
    duration: Duration,
    mode: DecoderMode,
    shared: &Arc<Shared>,
    signal: &watch::Sender<u64>,
) -> Result<PlaybackPipeline, String> {
    let pipeline = gst::Pipeline::new();
    let source = element("fdsrc")?;
    source.set_property("fd", file.as_raw_fd());
    let demux = element("qtdemux")?;
    let parser = element("h265parse")?;
    let decoder = element(match mode {
        DecoderMode::Automatic => "decodebin",
        DecoderMode::Software => "avdec_h265",
    })?;
    let convert = element("videoconvert")?;
    let rate = element("videorate")?;
    let caps = gst::Caps::builder("video/x-raw")
        .field("format", "RGBA")
        .field("framerate", gst::Fraction::new(OUTPUT_FRAMES_PER_SECOND, 1))
        .build();
    let appsink = gst_app::AppSink::builder()
        .caps(&caps)
        .max_buffers(1)
        .drop(true)
        .enable_last_sample(false)
        .sync(true)
        .build();

    pipeline
        .add_many([
            &source,
            &demux,
            &parser,
            &decoder,
            &convert,
            &rate,
            appsink.upcast_ref(),
        ])
        .map_err(|_| pipeline_error("could not assemble the decode pipeline"))?;
    source
        .link(&demux)
        .map_err(|_| pipeline_error("could not link the wallpaper source"))?;
    parser
        .link(&decoder)
        .map_err(|_| pipeline_error("could not link the wallpaper decoder"))?;
    gst::Element::link_many([&rate, &convert, appsink.upcast_ref()])
        .map_err(|_| pipeline_error("could not link the wallpaper decoder"))?;
    let bus = pipeline
        .bus()
        .ok_or_else(|| pipeline_error("the decode pipeline has no event bus"))?;
    let faulted = Arc::new(AtomicBool::new(false));

    let parser_sink = parser
        .static_pad("sink")
        .ok_or_else(|| pipeline_error("the HEVC parser has no input"))?;
    let demux_faulted = Arc::clone(&faulted);
    demux.connect_pad_added(move |_demux, source_pad| {
        if parser_sink.is_linked() {
            return;
        }
        let caps = source_pad
            .current_caps()
            .unwrap_or_else(|| source_pad.query_caps(None));
        let is_hevc = caps
            .structure(0)
            .is_some_and(|structure| structure.name() == "video/x-h265");
        if is_hevc && source_pad.link(&parser_sink).is_err() {
            demux_faulted.store(true, Ordering::Release);
        }
    });

    let rate_sink = rate
        .static_pad("sink")
        .ok_or_else(|| pipeline_error("the frame-rate converter has no input"))?;
    if mode == DecoderMode::Automatic {
        let decode_faulted = Arc::clone(&faulted);
        decoder.connect_pad_added(move |_decoder, source_pad| {
            if rate_sink.is_linked() {
                return;
            }
            let caps = source_pad
                .current_caps()
                .unwrap_or_else(|| source_pad.query_caps(None));
            let is_video = caps
                .structure(0)
                .is_some_and(|structure| structure.name().starts_with("video/x-raw"));
            if is_video && source_pad.link(&rate_sink).is_err() {
                decode_faulted.store(true, Ordering::Release);
            }
        });
    } else {
        decoder
            .link(&rate)
            .map_err(|_| pipeline_error("could not link the software decoder"))?;
    }

    let sample_shared = Arc::clone(shared);
    let sample_signal = signal.clone();
    let sample_bus = bus.clone();
    let sample_faulted = Arc::clone(&faulted);
    appsink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                if sample_shared.failed.load(Ordering::Acquire) {
                    return Err(gst::FlowError::Flushing);
                }
                let result = sink
                    .pull_sample()
                    .map_err(|_| gst::FlowError::Eos)
                    .and_then(|sample| decoded_frame(&sample))
                    .map(
                        |frame| match loop_frame_action(&sample_shared, &frame, duration) {
                            LoopFrameAction::Publish => {
                                publish_frame(&sample_shared, &sample_signal, frame);
                            }
                            LoopFrameAction::Request => {
                                if publish_frame(&sample_shared, &sample_signal, frame)
                                    && !request_loop_before_eos(&sample_bus)
                                {
                                    sample_faulted.store(true, Ordering::Release);
                                }
                            }
                            LoopFrameAction::Drop => {}
                        },
                    );
                if result.is_err() {
                    sample_faulted.store(true, Ordering::Release);
                }
                result.map(|_| gst::FlowSuccess::Ok)
            })
            .build(),
    );

    Ok(PlaybackPipeline {
        pipeline,
        bus,
        faulted,
    })
}

fn decoded_frame(sample: &gst::Sample) -> Result<Frame, gst::FlowError> {
    let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
    let caps = sample.caps().ok_or(gst::FlowError::NotNegotiated)?;
    let info = gst_video::VideoInfo::from_caps(caps).map_err(|_| gst::FlowError::NotNegotiated)?;
    let mapped = gst_video::VideoFrameRef::from_buffer_ref_readable(buffer, &info)
        .map_err(|_| gst::FlowError::Error)?;
    let stride = *info.stride().first().ok_or(gst::FlowError::Error)?;
    let plane = mapped.plane_data(0).map_err(|_| gst::FlowError::Error)?;
    let pixels =
        pack_rgba(plane, info.width(), info.height(), stride).ok_or(gst::FlowError::Error)?;
    let pts = buffer.pts().map(|pts| Duration::from_nanos(pts.nseconds()));

    Ok(Frame {
        width: info.width(),
        height: info.height(),
        pixels,
        pts,
    })
}

fn pack_rgba(source: &[u8], width: u32, height: u32, stride: i32) -> Option<Bytes> {
    let row_bytes = usize::try_from(width).ok()?.checked_mul(4)?;
    let height = usize::try_from(height).ok()?;
    let stride = usize::try_from(stride).ok()?;
    if stride < row_bytes || source.len() < stride.checked_mul(height)? {
        return None;
    }

    if stride == row_bytes {
        return Some(Bytes::copy_from_slice(
            &source[..row_bytes.checked_mul(height)?],
        ));
    }

    let mut packed = Vec::with_capacity(row_bytes.checked_mul(height)?);
    for row in source.chunks_exact(stride).take(height) {
        packed.extend_from_slice(&row[..row_bytes]);
    }
    Some(packed.into())
}

fn publish_frame(shared: &Shared, signal: &watch::Sender<u64>, frame: Frame) -> bool {
    let transition = {
        let mut state = lock(&shared.state);
        if shared.failed.load(Ordering::Acquire) {
            return false;
        }
        state.transition.take()
    };
    let (frame, transition) = transition.map_or((frame.clone(), None), |transition| {
        let progress = transition_progress(&transition, frame.pts);
        if progress < 256 && same_dimensions(&transition.held, &frame) {
            let frame = blend(&transition.held, frame, progress);
            (frame, Some(transition))
        } else {
            (frame, None)
        }
    });
    let mut state = lock(&shared.state);
    if shared.failed.load(Ordering::Acquire) {
        return false;
    }
    if state.transition.is_none() {
        state.transition = transition;
    }
    state.last_frame = Some(frame.clone());
    state.last_frame_at = Some(Instant::now());
    state.awaiting_opening_since = None;
    state.pending = Some(Update::Frame(frame));
    drop(state);
    notify(shared, signal);
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoopFrameAction {
    Publish,
    Request,
    Drop,
}

fn loop_frame_action(shared: &Shared, frame: &Frame, duration: Duration) -> LoopFrameAction {
    let Some(pts) = frame.pts else {
        return if shared.loop_requested.load(Ordering::Acquire) {
            LoopFrameAction::Drop
        } else {
            LoopFrameAction::Publish
        };
    };
    let loop_at = duration.saturating_sub(LOOP_LEAD);
    if pts < loop_at {
        shared.loop_requested.store(false, Ordering::Release);
        return LoopFrameAction::Publish;
    }
    if shared.loop_requested.swap(true, Ordering::AcqRel) {
        LoopFrameAction::Drop
    } else {
        LoopFrameAction::Request
    }
}

fn request_loop_before_eos(bus: &gst::Bus) -> bool {
    let message = gst::message::Application::new(gst::Structure::new_empty(LOOP_MESSAGE));
    bus.post(message).is_ok()
}

fn transition_progress(transition: &LoopTransition, pts: Option<Duration>) -> u16 {
    let elapsed = pts.unwrap_or_else(|| transition.started_at.elapsed());
    let denominator = transition.duration.as_nanos().max(1);
    ((elapsed.as_nanos().saturating_mul(256) / denominator).min(256)) as u16
}

fn same_dimensions(left: &Frame, right: &Frame) -> bool {
    left.width == right.width
        && left.height == right.height
        && left.pixels.len() == right.pixels.len()
}

fn blend(held: &Frame, opening: Frame, opening_weight: u16) -> Frame {
    let held_weight = 256 - opening_weight;
    let pixels = held
        .pixels
        .iter()
        .zip(opening.pixels.iter())
        .map(|(held, opening)| {
            ((u16::from(*held) * held_weight + u16::from(*opening) * opening_weight + 128) / 256)
                as u8
        })
        .collect::<Vec<_>>()
        .into();
    Frame { pixels, ..opening }
}

fn begin_loop(shared: &Shared, crossfade: Duration) {
    begin_loop_at(shared, crossfade, Instant::now());
}

fn begin_loop_at(shared: &Shared, crossfade: Duration, now: Instant) {
    let mut state = lock(&shared.state);
    state.transition = state.last_frame.clone().map(|held| LoopTransition {
        held,
        started_at: now,
        duration: crossfade,
    });
    state.awaiting_opening_since = Some(now);
}

fn run_playback(
    mut file: File,
    duration: Duration,
    crossfade: Duration,
    shared: &Arc<Shared>,
    signal: &watch::Sender<u64>,
    stopping: &AtomicBool,
) {
    let mut mode = DecoderMode::Automatic;
    loop {
        if stopping.load(Ordering::Acquire) {
            return;
        }
        shared.loop_requested.store(false, Ordering::Release);
        let outcome = run_attempt(
            &mut file, duration, crossfade, mode, shared, signal, stopping,
        );
        if outcome == PlaybackOutcome::Stopped || stopping.load(Ordering::Acquire) {
            return;
        }
        if should_retry_with_software(mode, lock(&shared.state).last_frame.is_some()) {
            mode = DecoderMode::Software;
            continue;
        }
        let PlaybackOutcome::Failed(message) = outcome else {
            unreachable!("stopped playback returned above");
        };
        fail_once(shared, signal, &pipeline_error(message));
        return;
    }
}

fn should_retry_with_software(mode: DecoderMode, has_frame: bool) -> bool {
    mode == DecoderMode::Automatic && !has_frame
}

fn run_attempt(
    file: &mut File,
    duration: Duration,
    crossfade: Duration,
    mode: DecoderMode,
    shared: &Arc<Shared>,
    signal: &watch::Sender<u64>,
    stopping: &AtomicBool,
) -> PlaybackOutcome {
    if file.seek(SeekFrom::Start(0)).is_err() {
        return PlaybackOutcome::Failed("wallpaper file could not be rewound");
    }
    let playback = match build_pipeline(file, duration, mode, shared, signal) {
        Ok(playback) => playback,
        Err(_) => {
            return PlaybackOutcome::Failed("wallpaper decode pipeline could not be built");
        }
    };
    if playback.pipeline.set_state(gst::State::Playing).is_err() {
        let _ = playback.pipeline.set_state(gst::State::Null);
        return PlaybackOutcome::Failed("wallpaper decode pipeline did not start");
    }
    let started_at = Instant::now();
    let outcome = monitor_pipeline(&playback, mode, started_at, crossfade, shared, stopping);
    let _ = playback.pipeline.set_state(gst::State::Null);
    outcome
}

fn monitor_pipeline(
    playback: &PlaybackPipeline,
    mode: DecoderMode,
    started_at: Instant,
    crossfade: Duration,
    shared: &Shared,
    stopping: &AtomicBool,
) -> PlaybackOutcome {
    while !stopping.load(Ordering::Acquire) {
        if shared.failed.load(Ordering::Acquire) {
            return PlaybackOutcome::Stopped;
        }
        if playback.faulted.load(Ordering::Acquire) {
            return PlaybackOutcome::Failed("wallpaper frame decoding failed");
        }
        if let Some(stall) = playback_stall(
            &lock(&shared.state),
            started_at,
            mode.startup_timeout(),
            Instant::now(),
        ) {
            return PlaybackOutcome::Failed(match stall {
                Stall::Startup => "wallpaper did not produce its first frame in time",
                Stall::Frame => "wallpaper playback stopped producing frames",
                Stall::Seek => "wallpaper loop did not resume after seeking",
            });
        }
        let Some(message) = playback.bus.timed_pop(gst::ClockTime::from_mseconds(100)) else {
            continue;
        };
        match message.view() {
            gst::MessageView::Application(message)
                if message
                    .structure()
                    .is_some_and(|structure| structure.name() == LOOP_MESSAGE) =>
            {
                begin_loop(shared, crossfade);
                let loop_result = (|| {
                    playback
                        .pipeline
                        .set_state(gst::State::Paused)
                        .map_err(|_| ())?;
                    playback
                        .pipeline
                        .seek_simple(
                            gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
                            gst::ClockTime::ZERO,
                        )
                        .map_err(|_| ())?;
                    playback
                        .pipeline
                        .set_state(gst::State::Playing)
                        .map_err(|_| ())?;
                    Ok::<(), ()>(())
                })();
                if loop_result.is_err() {
                    return PlaybackOutcome::Failed("wallpaper loop seek failed");
                }
            }
            gst::MessageView::Eos(..) => {
                return PlaybackOutcome::Failed("wallpaper reached its end before looping");
            }
            gst::MessageView::Error(_) => {
                return PlaybackOutcome::Failed("wallpaper stream failed");
            }
            _ => {}
        }
    }
    PlaybackOutcome::Stopped
}

fn playback_stall(
    state: &SharedState,
    started_at: Instant,
    startup_timeout: Duration,
    now: Instant,
) -> Option<Stall> {
    if state
        .awaiting_opening_since
        .is_some_and(|started| now.saturating_duration_since(started) >= SEEK_STALL_TIMEOUT)
    {
        return Some(Stall::Seek);
    }
    if state
        .last_frame_at
        .is_some_and(|frame| now.saturating_duration_since(frame) >= FRAME_STALL_TIMEOUT)
    {
        return Some(Stall::Frame);
    }
    if state.last_frame_at.is_none() && now.saturating_duration_since(started_at) >= startup_timeout
    {
        return Some(Stall::Startup);
    }
    None
}

fn fail_once(shared: &Shared, signal: &watch::Sender<u64>, message: &str) {
    if shared.failed.swap(true, Ordering::AcqRel) {
        return;
    }
    diagnostic(message);
    let mut state = lock(&shared.state);
    state.pending = Some(Update::Failed);
    state.last_frame = None;
    state.transition = None;
    state.last_frame_at = None;
    state.awaiting_opening_since = None;
    drop(state);
    notify(shared, signal);
}

fn notify(shared: &Shared, signal: &watch::Sender<u64>) {
    let sequence = shared.sequence.fetch_add(1, Ordering::AcqRel) + 1;
    signal.send_replace(sequence);
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn packaged_wallpaper_path(install_name: &str) -> Result<PathBuf, String> {
    let executable = std::env::current_exe()
        .map_err(|_| pipeline_error("could not locate the packaged wallpaper"))?;
    wallpaper_path_for_executable(&executable, install_name)
        .ok_or_else(|| pipeline_error("could not locate the packaged wallpaper"))
}

fn load_poster(catalog: Catalog) -> Result<image::Handle, String> {
    POSTERS[catalog.index()]
        .get_or_init(|| decode_poster(catalog.spec()))
        .clone()
}

fn decode_poster(spec: &PlaybackSpec) -> Result<image::Handle, String> {
    let path = packaged_wallpaper_path(spec.poster_name)
        .ok()
        .filter(|path| path.is_file())
        .or_else(|| {
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("assets/wallpapers")
                .join(spec.poster_name);
            path.is_file().then_some(path)
        })
        .ok_or_else(|| "wallpaper poster is unavailable; using generated background".to_owned())?;
    let rgba = image_rs::open(path)
        .map_err(|_| {
            "wallpaper poster could not be decoded; using generated background".to_owned()
        })?
        .into_rgba8();
    let (width, height) = rgba.dimensions();
    Ok(image::Handle::from_rgba(width, height, rgba.into_raw()))
}

fn wallpaper_path_for_executable(executable: &Path, install_name: &str) -> Option<PathBuf> {
    let prefix = executable.parent()?.parent()?;
    Some(prefix.join("share/genkan/wallpapers").join(install_name))
}

fn pipeline_error(reason: &str) -> String {
    format!("{reason}; wallpaper playback stopped; retaining current background")
}

fn diagnostic(message: &str) {
    eprintln!("genkan: {}", bounded_text(message));
}

fn bounded_text(message: &str) -> String {
    let mut bounded = message
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .take(MAX_DIAGNOSTIC_CHARS + 1)
        .collect::<String>();
    if bounded.chars().count() > MAX_DIAGNOSTIC_CHARS {
        bounded = bounded.chars().take(MAX_DIAGNOSTIC_CHARS - 1).collect();
        bounded.push('…');
    }
    bounded
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::*;
    use iced::Size;

    fn frame(value: u8, pts: Duration) -> Frame {
        Frame {
            width: 2,
            height: 1,
            pixels: Bytes::from(vec![value; 8]),
            pts: Some(pts),
        }
    }

    #[test]
    fn executable_path_resolves_the_packaged_default() {
        assert_eq!(
            wallpaper_path_for_executable(
                Path::new("/nix/store/genkan/bin/.genkan-wrapped"),
                "tahoe-beach.mov"
            ),
            Some(PathBuf::from(
                "/nix/store/genkan/share/genkan/wallpapers/tahoe-beach.mov"
            ))
        );
    }

    #[test]
    fn opened_wallpaper_is_stable_after_path_replacement() {
        let directory = std::env::temp_dir().join(format!(
            "genkan-wallpaper-source-replacement-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("wallpaper.mov");
        let replacement = directory.join("replacement.mov");
        std::fs::write(&path, b"original").unwrap();
        let mut file = open_wallpaper(&path).unwrap();
        std::fs::write(&replacement, b"replacement").unwrap();
        std::fs::rename(&replacement, &path).unwrap();

        file.seek(SeekFrom::Start(0)).unwrap();
        let mut contents = String::new();
        file.read_to_string(&mut contents).unwrap();
        assert_eq!(contents, "original");
        assert!(open_wallpaper(&directory).is_err());

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn wallpaper_fifo_is_rejected_without_waiting_for_a_writer() {
        let directory = std::env::temp_dir().join(format!(
            "genkan-wallpaper-source-fifo-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("wallpaper.mov");
        rustix::fs::mkfifoat(rustix::fs::CWD, &path, Mode::RUSR | Mode::WUSR).unwrap();

        assert!(open_wallpaper(&path).is_err());

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn wallpaper_device_is_rejected_without_opening_the_device() {
        let directory = std::env::temp_dir().join(format!(
            "genkan-wallpaper-source-device-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("wallpaper.mov");
        std::os::unix::fs::symlink("/dev/null", &path).unwrap();

        assert!(open_wallpaper(&path).is_err());

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn catalog_retains_each_verified_loop_transition() {
        let transitions = Catalog::value_variants()
            .iter()
            .map(|catalog| {
                let wallpaper = catalog.spec();
                (
                    catalog.to_possible_value().unwrap().get_name().to_owned(),
                    wallpaper.install_name,
                    wallpaper.poster_name,
                    wallpaper.duration.as_micros(),
                    wallpaper.crossfade.as_millis(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            transitions,
            vec![
                (
                    "tahoe-beach".into(),
                    "tahoe-beach.mov",
                    "tahoe-beach-poster.jpg",
                    120_004_167,
                    2_000
                ),
                (
                    "sequoia-sunrise".into(),
                    "sequoia-sunrise.mov",
                    "sequoia-sunrise-poster.jpg",
                    120_008_333,
                    1_000
                ),
                (
                    "sequoia-morning".into(),
                    "sequoia-morning.mov",
                    "sequoia-morning-poster.jpg",
                    243_336_667,
                    1_000
                ),
                (
                    "sequoia-night".into(),
                    "sequoia-night.mov",
                    "sequoia-night-poster.jpg",
                    291_603_333,
                    2_000
                ),
            ]
        );
    }

    #[test]
    fn static_catalog_entries_load_posters_without_players() {
        for catalog in Catalog::value_variants() {
            let state = State::start(Settings {
                catalog: *catalog,
                override_path: None,
                animate: false,
            });
            assert!(state.decoder_is_stopped(), "{catalog:?}");
            assert!(state.has_frame(), "{catalog:?}");
        }
    }

    #[test]
    fn padded_rgba_rows_are_tightly_packed() {
        let source = [1, 2, 3, 4, 0, 0, 5, 6, 7, 8, 0, 0];
        assert_eq!(
            pack_rgba(&source, 1, 2, 6),
            Some(Bytes::from_static(&[1, 2, 3, 4, 5, 6, 7, 8]))
        );
        assert_eq!(pack_rgba(&source[..5], 1, 1, 6), None);
        assert_eq!(pack_rgba(&source, 2, 1, 6), None);
    }

    #[test]
    fn crossfade_holds_then_dissolves_to_the_opening_frame() {
        let held = frame(0, Duration::ZERO);
        let opening = frame(200, Duration::from_secs(1));
        assert_eq!(blend(&held, opening.clone(), 0).pixels[0], 0);
        assert_eq!(blend(&held, opening.clone(), 128).pixels[0], 100);
        assert_eq!(blend(&held, opening, 256).pixels[0], 200);
    }

    #[test]
    fn publishing_replaces_an_unconsumed_frame() {
        let shared = Shared::default();
        let (signal, _) = watch::channel(0);
        publish_frame(&shared, &signal, frame(1, Duration::ZERO));
        publish_frame(&shared, &signal, frame(2, Duration::from_millis(33)));

        let state = lock(&shared.state);
        let Some(Update::Frame(latest)) = state.pending.as_ref() else {
            panic!("latest frame must remain pending");
        };
        assert_eq!(latest.pixels[0], 2);
        assert_eq!(shared.sequence.load(Ordering::Acquire), 2);
    }

    #[test]
    fn iced_keeps_displaying_the_previous_frame_until_allocation_finishes() {
        let shared = Arc::new(Shared::default());
        let (signal_sender, signal) = watch::channel(0);
        let poster = image::Handle::from_rgba(1, 1, vec![1, 2, 3, 255]);
        let poster_id = poster.id();
        let mut state = State {
            player: Some(Player {
                shared: Arc::clone(&shared),
                signal,
                stopping: Arc::new(AtomicBool::new(false)),
                worker: None,
            }),
            poster: Some(poster.clone()),
            frame: Some(poster),
            allocation: None,
            allocation_pending: false,
        };
        publish_frame(&shared, &signal_sender, frame(1, Duration::ZERO));

        let pending = state
            .prepare_latest()
            .expect("the first decoded frame must begin allocation");
        assert_ne!(pending.id(), poster_id);
        assert_eq!(state.frame.as_ref().unwrap().id(), poster_id);

        publish_frame(&shared, &signal_sender, frame(2, Duration::from_millis(33)));
        assert!(state.prepare_latest().is_none());
        let shared_state = lock(&shared.state);
        let Some(Update::Frame(latest)) = shared_state.pending.as_ref() else {
            panic!("the latest frame must remain queued during allocation");
        };
        assert_eq!(latest.pixels[0], 2);
    }

    #[test]
    fn late_terminal_frames_do_not_cancel_loop_transition() {
        let shared = Shared::default();
        let duration = Duration::from_secs(10);
        let terminal = frame(20, duration - LOOP_LEAD);
        let late_terminal = frame(40, duration);
        let opening = frame(100, Duration::ZERO);
        let (signal, _) = watch::channel(0);

        assert_eq!(
            loop_frame_action(&shared, &terminal, duration),
            LoopFrameAction::Request
        );
        assert!(publish_frame(&shared, &signal, terminal));
        begin_loop(&shared, Duration::from_secs(1));
        assert_eq!(
            loop_frame_action(&shared, &late_terminal, duration),
            LoopFrameAction::Drop
        );
        assert!(lock(&shared.state).transition.is_some());
        let without_pts = Frame {
            pts: None,
            ..frame(60, Duration::ZERO)
        };
        assert_eq!(
            loop_frame_action(&shared, &without_pts, duration),
            LoopFrameAction::Drop
        );
        assert!(lock(&shared.state).awaiting_opening_since.is_some());

        assert_eq!(
            loop_frame_action(&shared, &opening, duration),
            LoopFrameAction::Publish
        );
        assert!(publish_frame(&shared, &signal, opening));
        let state = lock(&shared.state);
        assert!(state.transition.is_some());
        assert_eq!(state.last_frame.as_ref().unwrap().pixels[0], 20);
    }

    #[test]
    fn terminal_failure_cannot_be_overwritten_by_a_frame() {
        let shared = Shared::default();
        let (signal, _) = watch::channel(0);

        fail_once(&shared, &signal, "expected test failure");
        assert!(!publish_frame(&shared, &signal, frame(1, Duration::ZERO)));

        assert!(matches!(lock(&shared.state).pending, Some(Update::Failed)));
        assert_eq!(shared.sequence.load(Ordering::Acquire), 1);
    }

    #[test]
    fn receiving_failure_disposes_the_player() {
        let shared = Arc::new(Shared::default());
        lock(&shared.state).pending = Some(Update::Failed);
        let (_, signal) = watch::channel(0);
        let poster = image::Handle::from_rgba(1, 1, vec![1, 2, 3, 255]);
        let mut state = State {
            player: Some(Player {
                shared,
                signal,
                stopping: Arc::new(AtomicBool::new(false)),
                worker: None,
            }),
            poster: Some(poster),
            frame: None,
            allocation: None,
            allocation_pending: false,
        };

        state.receive_latest();

        assert!(state.decoder_is_stopped());
        assert!(state.has_frame());
    }

    #[test]
    fn receiving_failure_preserves_the_last_displayed_frame() {
        let shared = Arc::new(Shared::default());
        lock(&shared.state).pending = Some(Update::Failed);
        let (_, signal) = watch::channel(0);
        let poster = image::Handle::from_rgba(1, 1, vec![1, 2, 3, 255]);
        let current = image::Handle::from_rgba(1, 1, vec![4, 5, 6, 255]);
        let current_id = current.id();
        let mut state = State {
            player: Some(Player {
                shared,
                signal,
                stopping: Arc::new(AtomicBool::new(false)),
                worker: None,
            }),
            poster: Some(poster),
            frame: Some(current),
            allocation: None,
            allocation_pending: false,
        };

        state.receive_latest();

        assert!(state.decoder_is_stopped());
        assert_eq!(state.frame.as_ref().unwrap().id(), current_id);
    }

    #[test]
    fn terminal_failure_during_allocation_preserves_the_displayed_frame() {
        let shared = Arc::new(Shared::default());
        let (signal_sender, signal) = watch::channel(0);
        let current = image::Handle::from_rgba(1, 1, vec![4, 5, 6, 255]);
        let current_id = current.id();
        let mut state = State {
            player: Some(Player {
                shared: Arc::clone(&shared),
                signal,
                stopping: Arc::new(AtomicBool::new(false)),
                worker: None,
            }),
            poster: None,
            frame: Some(current),
            allocation: None,
            allocation_pending: true,
        };
        fail_once(&shared, &signal_sender, "expected allocation race failure");

        assert!(state.stop_after_terminal_failure());

        assert!(state.decoder_is_stopped());
        assert!(!state.allocation_pending);
        assert_eq!(state.frame.as_ref().unwrap().id(), current_id);
    }

    #[test]
    fn playback_deadlines_cover_startup_frames_and_loop_seek() {
        let now = Instant::now();
        let startup = now - AUTOMATIC_STARTUP_TIMEOUT;
        let mut state = SharedState::default();

        assert_eq!(
            playback_stall(&state, startup, AUTOMATIC_STARTUP_TIMEOUT, now),
            Some(Stall::Startup)
        );

        state.last_frame_at = Some(now - FRAME_STALL_TIMEOUT);
        assert_eq!(
            playback_stall(&state, startup, AUTOMATIC_STARTUP_TIMEOUT, now),
            Some(Stall::Frame)
        );

        state.awaiting_opening_since = Some(now - SEEK_STALL_TIMEOUT);
        assert_eq!(
            playback_stall(&state, startup, AUTOMATIC_STARTUP_TIMEOUT, now),
            Some(Stall::Seek)
        );
    }

    #[test]
    fn software_retry_only_precedes_the_first_frame() {
        assert!(should_retry_with_software(DecoderMode::Automatic, false));
        assert!(!should_retry_with_software(DecoderMode::Automatic, true));
        assert!(!should_retry_with_software(DecoderMode::Software, false));
        assert!(!should_retry_with_software(DecoderMode::Software, true));
    }

    #[test]
    fn diagnostics_are_single_line_and_bounded() {
        let input = format!("bad\n{}", "x".repeat(300));
        let output = bounded_text(&input);
        assert!(!output.contains('\n'));
        assert_eq!(output.chars().count(), MAX_DIAGNOSTIC_CHARS);
        assert!(output.ends_with('…'));
    }

    #[test]
    fn playback_diagnostics_describe_the_retained_background() {
        assert_eq!(
            pipeline_error("wallpaper stream failed"),
            "wallpaper stream failed; wallpaper playback stopped; retaining current background"
        );
    }

    #[test]
    fn cover_preserves_aspect_ratio_and_fills_common_outputs() {
        let source = Size::new(3840.0, 2160.0);
        for bounds in [
            Size::new(1366.0, 768.0),
            Size::new(1920.0, 1080.0),
            Size::new(3840.0, 2160.0),
            Size::new(3440.0, 1440.0),
            Size::new(800.0, 1280.0),
        ] {
            let fitted = ContentFit::Cover.fit(source, bounds);
            assert!(fitted.width + f32::EPSILON >= bounds.width);
            assert!(fitted.height + f32::EPSILON >= bounds.height);
            assert!((fitted.width / fitted.height - source.width / source.height).abs() < 0.001);
        }
    }
}
