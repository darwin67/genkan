use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use bytes::Bytes;
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use iced::futures::stream;
use iced::widget::{image, Image};
use iced::{ContentFit, Element, Fill, Subscription};
use tokio::sync::watch;

const DEFAULT_WALLPAPER_ID: &str = "tahoe-beach";
const OUTPUT_FRAMES_PER_SECOND: i32 = 30;
const MAX_DIAGNOSTIC_CHARS: usize = 240;
const LOOP_LEAD: Duration = Duration::from_millis(50);
const LOOP_MESSAGE: &str = "genkan-wallpaper-loop";

#[derive(Debug, Clone, Copy)]
struct PlaybackSpec {
    id: &'static str,
    install_name: &'static str,
    duration: Duration,
    crossfade: Duration,
}

const WALLPAPERS: [PlaybackSpec; 4] = [
    PlaybackSpec {
        id: "tahoe-beach",
        install_name: "tahoe-beach.mov",
        duration: Duration::from_micros(120_004_167),
        crossfade: Duration::from_millis(2_000),
    },
    PlaybackSpec {
        id: "sequoia-sunrise",
        install_name: "sequoia-sunrise.mov",
        duration: Duration::from_micros(120_008_333),
        crossfade: Duration::from_millis(1_000),
    },
    PlaybackSpec {
        id: "sequoia-morning",
        install_name: "sequoia-morning.mov",
        duration: Duration::from_micros(243_336_667),
        crossfade: Duration::from_millis(1_000),
    },
    PlaybackSpec {
        id: "sequoia-night",
        install_name: "sequoia-night.mov",
        duration: Duration::from_micros(291_603_333),
        crossfade: Duration::from_millis(2_000),
    },
];

#[derive(Debug)]
pub(crate) struct State {
    player: Option<Player>,
    frame: Option<image::Handle>,
}

impl State {
    pub(crate) fn start_default() -> Self {
        let spec = WALLPAPERS
            .iter()
            .find(|wallpaper| wallpaper.id == DEFAULT_WALLPAPER_ID)
            .expect("the default wallpaper must be in the catalog");
        let result = packaged_wallpaper_path(spec.install_name)
            .and_then(|path| Player::start(&path, spec.duration, spec.crossfade));
        match result {
            Ok(player) => Self {
                player: Some(player),
                frame: None,
            },
            Err(error) => {
                diagnostic(&error);
                Self::disabled()
            }
        }
    }

    pub(crate) fn disabled() -> Self {
        Self {
            player: None,
            frame: None,
        }
    }

    pub(crate) fn subscription(&self) -> Subscription<()> {
        self.player
            .as_ref()
            .map_or_else(Subscription::none, Player::subscription)
    }

    pub(crate) fn receive_latest(&mut self) {
        let Some(update) = self.player.as_ref().and_then(Player::take_latest) else {
            return;
        };

        match update {
            Update::Frame(frame) => {
                self.frame = Some(image::Handle::from_rgba(
                    frame.width,
                    frame.height,
                    frame.pixels,
                ));
            }
            Update::Failed => self.frame = None,
        }
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
    pub(crate) fn is_disabled(&self) -> bool {
        self.player.is_none()
    }
}

struct Player {
    pipeline: gst::Pipeline,
    bus: gst::Bus,
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
        if !path.is_file() {
            return Err("packaged wallpaper is unavailable; using generated background".into());
        }

        gst::init().map_err(|_| {
            "GStreamer initialization failed; using generated background".to_owned()
        })?;

        let pipeline = gst::Pipeline::new();
        let source = element("filesrc")?;
        source.set_property("location", path.to_string_lossy().as_ref());
        let demux = element("qtdemux")?;
        let parser = element("h265parse")?;
        let decoder = element("decodebin")?;
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

        let (signal_sender, signal) = watch::channel(0);
        let shared = Arc::new(Shared::default());

        let parser_sink = parser
            .static_pad("sink")
            .ok_or_else(|| pipeline_error("the HEVC parser has no input"))?;
        let pad_shared = Arc::clone(&shared);
        let pad_signal = signal_sender.clone();
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
                fail_once(
                    &pad_shared,
                    &pad_signal,
                    "wallpaper stream could not be linked; using generated background",
                );
            }
        });

        let rate_sink = rate
            .static_pad("sink")
            .ok_or_else(|| pipeline_error("the frame-rate converter has no input"))?;
        let decode_shared = Arc::clone(&shared);
        let decode_signal = signal_sender.clone();
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
                fail_once(
                    &decode_shared,
                    &decode_signal,
                    "wallpaper decoder output could not be linked; using generated background",
                );
            }
        });

        let sample_shared = Arc::clone(&shared);
        let sample_signal = signal_sender.clone();
        let sample_bus = bus.clone();
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
                        .map(|frame| {
                            let loop_frame = frame.clone();
                            publish_frame(&sample_shared, &sample_signal, frame);
                            request_loop_before_eos(
                                &sample_shared,
                                &sample_bus,
                                &loop_frame,
                                duration,
                            );
                        });
                    if result.is_err() {
                        fail_once(
                            &sample_shared,
                            &sample_signal,
                            "wallpaper frame decoding failed; using generated background",
                        );
                    }
                    result.map(|_| gst::FlowSuccess::Ok)
                })
                .build(),
        );

        if pipeline.set_state(gst::State::Playing).is_err() {
            let _ = pipeline.set_state(gst::State::Null);
            return Err(pipeline_error("the decode pipeline did not start"));
        }

        let stopping = Arc::new(AtomicBool::new(false));
        let worker_pipeline = pipeline.clone();
        let worker_bus = bus.clone();
        let worker_shared = Arc::clone(&shared);
        let worker_signal = signal_sender;
        let worker_stopping = Arc::clone(&stopping);
        let worker = thread::Builder::new()
            .name("wallpaper-events".into())
            .spawn(move || {
                run_bus(
                    &worker_pipeline,
                    &worker_bus,
                    &worker_shared,
                    &worker_signal,
                    &worker_stopping,
                    crossfade,
                )
            })
            .map_err(|_| {
                let _ = pipeline.set_state(gst::State::Null);
                pipeline_error("could not start the wallpaper event worker")
            })?;

        Ok(Self {
            pipeline,
            bus,
            shared,
            signal,
            stopping,
            worker: Some(worker),
        })
    }

    fn subscription(&self) -> Subscription<()> {
        let receiver = self.signal.clone();
        Subscription::run_with_id(
            "wallpaper-frame-ready",
            stream::unfold(receiver, |mut receiver| async move {
                receiver.changed().await.ok().map(|()| ((), receiver))
            }),
        )
    }

    fn take_latest(&self) -> Option<Update> {
        lock(&self.shared.state).pending.take()
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        let _ = self.pipeline.set_state(gst::State::Null);
        self.bus.set_flushing(true);
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

fn element(factory: &str) -> Result<gst::Element, String> {
    gst::ElementFactory::make(factory).build().map_err(|_| {
        pipeline_error(&format!(
            "required GStreamer element {factory} is unavailable"
        ))
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

fn publish_frame(shared: &Shared, signal: &watch::Sender<u64>, frame: Frame) {
    let mut state = lock(&shared.state);
    let frame = state.transition.take().map_or(frame.clone(), |transition| {
        let progress = transition_progress(&transition, frame.pts);
        if progress < 256 && same_dimensions(&transition.held, &frame) {
            state.transition = Some(transition);
            blend(
                &state.transition.as_ref().expect("transition retained").held,
                frame,
                progress,
            )
        } else {
            frame
        }
    });
    state.last_frame = Some(frame.clone());
    state.pending = Some(Update::Frame(frame));
    drop(state);
    notify(shared, signal);
}

fn request_loop_before_eos(shared: &Shared, bus: &gst::Bus, frame: &Frame, duration: Duration) {
    let Some(pts) = frame.pts else {
        return;
    };
    let loop_at = duration.saturating_sub(LOOP_LEAD);
    if pts < loop_at {
        shared.loop_requested.store(false, Ordering::Release);
        return;
    }
    if shared.loop_requested.swap(true, Ordering::AcqRel) {
        return;
    }
    let message = gst::message::Application::new(gst::Structure::new_empty(LOOP_MESSAGE));
    if bus.post(message).is_err() {
        shared.loop_requested.store(false, Ordering::Release);
    }
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
    let mut state = lock(&shared.state);
    state.transition = state.last_frame.clone().map(|held| LoopTransition {
        held,
        started_at: Instant::now(),
        duration: crossfade,
    });
}

fn run_bus(
    pipeline: &gst::Pipeline,
    bus: &gst::Bus,
    shared: &Shared,
    signal: &watch::Sender<u64>,
    stopping: &AtomicBool,
    crossfade: Duration,
) {
    while !stopping.load(Ordering::Acquire) {
        let Some(message) = bus.timed_pop(gst::ClockTime::from_mseconds(100)) else {
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
                    pipeline.set_state(gst::State::Paused).map_err(|_| ())?;
                    pipeline
                        .seek_simple(
                            gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
                            gst::ClockTime::ZERO,
                        )
                        .map_err(|_| ())?;
                    pipeline.set_state(gst::State::Playing).map_err(|_| ())?;
                    Ok::<(), ()>(())
                })();
                if loop_result.is_err() {
                    fail_once(
                        shared,
                        signal,
                        "wallpaper loop seek failed; using generated background",
                    );
                    let _ = pipeline.set_state(gst::State::Null);
                    return;
                }
            }
            gst::MessageView::Eos(..) => {
                fail_once(
                    shared,
                    signal,
                    "wallpaper reached its end before looping; using generated background",
                );
                let _ = pipeline.set_state(gst::State::Null);
                return;
            }
            gst::MessageView::Error(_) => {
                fail_once(
                    shared,
                    signal,
                    "wallpaper stream failed; using generated background",
                );
                let _ = pipeline.set_state(gst::State::Null);
                return;
            }
            _ => {}
        }
    }
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
    let executable = std::env::current_exe().map_err(|_| {
        "could not locate the packaged wallpaper; using generated background".to_owned()
    })?;
    wallpaper_path_for_executable(&executable, install_name).ok_or_else(|| {
        "could not locate the packaged wallpaper; using generated background".to_owned()
    })
}

fn wallpaper_path_for_executable(executable: &Path, install_name: &str) -> Option<PathBuf> {
    let prefix = executable.parent()?.parent()?;
    Some(prefix.join("share/genkan/wallpapers").join(install_name))
}

fn pipeline_error(reason: &str) -> String {
    format!("{reason}; using generated background")
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
    fn catalog_retains_each_verified_loop_transition() {
        let transitions = WALLPAPERS.map(|wallpaper| {
            (
                wallpaper.id,
                wallpaper.install_name,
                wallpaper.duration.as_micros(),
                wallpaper.crossfade.as_millis(),
            )
        });
        assert_eq!(
            transitions,
            [
                ("tahoe-beach", "tahoe-beach.mov", 120_004_167, 2_000),
                ("sequoia-sunrise", "sequoia-sunrise.mov", 120_008_333, 1_000),
                ("sequoia-morning", "sequoia-morning.mov", 243_336_667, 1_000),
                ("sequoia-night", "sequoia-night.mov", 291_603_333, 2_000),
            ]
        );
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
    fn diagnostics_are_single_line_and_bounded() {
        let input = format!("bad\n{}", "x".repeat(300));
        let output = bounded_text(&input);
        assert!(!output.contains('\n'));
        assert_eq!(output.chars().count(), MAX_DIAGNOSTIC_CHARS);
        assert!(output.ends_with('…'));
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
