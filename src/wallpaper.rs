use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use ::image as image_rs;
use bytes::Bytes;
use chrono::{Datelike, Offset, Timelike};
use clap::ValueEnum;
use genkan::dynamic_wallpaper::heic::{Document, RgbaFrame as HeicFrame};
use genkan::dynamic_wallpaper::playback::{DecodeOutcome, DecodeRequest, Playback as HeicPlayback};
use genkan::dynamic_wallpaper::{AppearancePreference, CivilDate, CivilTime, ClockSnapshot};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use iced::futures::stream;
use iced::widget::{image, Image};
use iced::{ContentFit, Element, Fill, Subscription};
use iced_runtime::image::{Allocation, Error as AllocationError};
use rustix::time::{clock_gettime, ClockId, Timespec};
use tokio::sync::watch;

use crate::stable_file::{open_regular, OpenError};

const OUTPUT_FRAMES_PER_SECOND: i32 = 30;
const MAX_DIAGNOSTIC_CHARS: usize = 240;
const LOOP_LEAD: Duration = Duration::from_millis(50);
const LOOP_MESSAGE: &str = "genkan-wallpaper-loop";
const AUTOMATIC_STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const SOFTWARE_STARTUP_TIMEOUT: Duration = Duration::from_secs(60);
const FRAME_STALL_TIMEOUT: Duration = Duration::from_secs(10);
const SEEK_STALL_TIMEOUT: Duration = Duration::from_secs(10);
const HEIC_TRANSITION_INTERVAL: Duration = Duration::from_millis(16);
const HEIC_POLL_INTERVAL: Duration = Duration::from_millis(100);
// The helper process relays a frame as a tag, a width/height/length header, and
// tightly packed RGBA bytes. The ceilings repeat `dynamic_wallpaper::heic`'s
// per-axis and output-byte limits so a corrupt or hostile worker cannot make
// the greeter allocate a frame the decoder would have refused.
const HEIC_FRAME_TAG: u8 = b'F';
const HEIC_FAILED_TAG: u8 = b'E';
const HEIC_HEADER_BYTES: usize = 12;
const MAX_HEIC_FRAME_DIMENSION: u32 = 16_384;
const MAX_HEIC_FRAME_BYTES: usize = 128 * 1024 * 1024;
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
    /// Reduced-motion HEIC keeps time-of-day scheduling but switches frames
    /// immediately instead of dissolving. It has no effect on MOV playback,
    /// whose reduced-motion behavior is a fixed poster selected by `animate`.
    pub(crate) reduced_motion: bool,
    /// Explicit light or dark preference for a dynamic HEIC fallback.
    pub(crate) appearance: AppearancePreference,
}

impl Settings {
    /// The local dynamic HEIC override, if one was supplied.
    pub(crate) fn heic_path(&self) -> Option<&Path> {
        self.override_path
            .as_deref()
            .filter(|path| is_heic_path(path))
    }
}

/// Whether a path names a dynamic HEIC/HEIF wallpaper by extension.
pub(crate) fn is_heic_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("heic") || extension.eq_ignore_ascii_case("heif")
        })
}

#[derive(Debug)]
pub(crate) struct State {
    player: Option<Player>,
    heic: Option<HeicPlayer>,
    poster: Option<image::Handle>,
    frame: Option<image::Handle>,
    allocation: Option<Allocation>,
    allocation_pending: bool,
    heic_adopted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refresh {
    Unchanged,
    Frame,
    Failed,
}

impl State {
    pub(crate) fn start(settings: Settings) -> Self {
        let poster = load_poster(settings.catalog)
            .map_err(|error| diagnostic(&error))
            .ok();

        if let Some(path) = settings.heic_path().map(Path::to_owned) {
            return Self::start_heic(settings, &path, poster);
        }

        if !settings.animate {
            return Self::poster_only(poster);
        }

        let spec = settings.catalog.spec();
        let result = settings
            .override_path
            .map_or_else(|| packaged_wallpaper_path(spec.install_name), Ok)
            .and_then(|path| Player::start(&path, spec.duration, spec.crossfade));
        match result {
            Ok(player) => Self {
                player: Some(player),
                heic: None,
                frame: poster.clone(),
                poster,
                allocation: None,
                allocation_pending: false,
                heic_adopted: false,
            },
            Err(error) => {
                diagnostic(&error);
                Self::poster_only(poster)
            }
        }
    }

    fn start_heic(settings: Settings, path: &Path, poster: Option<image::Handle>) -> Self {
        if !settings.animate {
            return Self::poster_only(poster);
        }
        match HeicPlayer::start(path, settings.appearance, settings.reduced_motion) {
            Ok(heic) => Self {
                player: None,
                heic: Some(heic),
                frame: poster.clone(),
                poster,
                allocation: None,
                allocation_pending: false,
                heic_adopted: false,
            },
            Err(error) => {
                diagnostic(&error);
                Self::poster_only(poster)
            }
        }
    }

    fn poster_only(poster: Option<image::Handle>) -> Self {
        Self {
            player: None,
            heic: None,
            frame: poster.clone(),
            poster,
            allocation: None,
            allocation_pending: false,
            heic_adopted: false,
        }
    }

    #[cfg(test)]
    pub(crate) fn disabled() -> Self {
        Self {
            player: None,
            heic: None,
            poster: None,
            frame: None,
            allocation: None,
            allocation_pending: false,
            heic_adopted: false,
        }
    }

    pub(crate) fn subscription(&self) -> Subscription<()> {
        match (&self.player, &self.heic) {
            (Some(player), _) => player.subscription(),
            (None, Some(heic)) => heic.subscription(),
            (None, None) => Subscription::none(),
        }
    }

    fn take_latest(&self) -> Option<Update> {
        self.player
            .as_ref()
            .and_then(Player::take_latest)
            .or_else(|| self.heic.as_ref().and_then(HeicPlayer::take_latest))
    }

    pub(crate) fn receive_latest(&mut self) -> Refresh {
        let Some(update) = self.take_latest() else {
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
                self.note_heic_adoption();
                Refresh::Frame
            }
            Update::Failed => {
                self.stop_playback();
                Refresh::Failed
            }
        }
    }

    /// Records the first dynamic-HEIC frame this consumer actually renders.
    ///
    /// The `lock-test` build emits one bounded line so a smoke test can prove
    /// a decoded frame reached the locker instead of the source falling back.
    fn note_heic_adoption(&mut self) {
        if self.heic.is_none() || self.heic_adopted {
            return;
        }
        self.heic_adopted = true;
        #[cfg(feature = "lock-test")]
        diagnostic("dynamic wallpaper frame adopted");
    }

    pub(crate) fn prepare_latest(&mut self) -> Option<image::Handle> {
        if self.allocation_pending {
            return None;
        }
        let update = self.take_latest()?;

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
                self.note_heic_adoption();
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
        let failed = self.player.as_ref().is_some_and(Player::has_failed)
            || self.heic.as_ref().is_some_and(HeicPlayer::has_failed);
        if !failed {
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
        self.heic.take();
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
        self.player.is_none() && self.heic.is_none()
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
struct HeicShared {
    pending: Mutex<Option<Update>>,
    sequence: AtomicU64,
    failed: AtomicBool,
    /// Set once the supervisor has killed and reaped the worker.
    ///
    /// Tests need this barrier before probing reaping: `waitpid` is itself a
    /// competing reaper, so probing before the supervisor has waited would
    /// collect the zombie and misreport the supervisor.
    #[cfg(test)]
    cleanup_complete: AtomicBool,
}

/// A dynamic HEIC source for login and lock.
///
/// It reuses the shared `Document` reader, `Playback` scheduler, and RGBA
/// frame type from `genkan::dynamic_wallpaper`. Location/solar selection is
/// deliberately unavailable here: `Playback::new` disables solar, so login and
/// lock never contact GeoClue.
///
/// A dedicated supervisor thread owns the worker process end to end: spawning,
/// framing, termination, and reaping never run on the lock-owning thread. The
/// only process operation the lock-owning thread performs is a non-blocking
/// `kill` on cancellation.
struct HeicPlayer {
    shared: Arc<HeicShared>,
    signal: watch::Receiver<u64>,
    stopping: Arc<AtomicBool>,
    /// The live worker, published by the supervisor before it reads. It is
    /// never held across a blocking call, so cancellation cannot be delayed.
    child: Arc<Mutex<Option<Child>>>,
}

impl std::fmt::Debug for HeicPlayer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("HeicPlayer").finish_non_exhaustive()
    }
}

/// The civil and monotonic time source a HEIC worker schedules against.
///
/// Both values are produced by one call so a test clock cannot hand out a torn
/// civil/monotonic pair and fake a clock discontinuity.
type HeicTime = dyn Fn() -> (Option<ClockSnapshot>, Duration) + Send + Sync;

impl HeicPlayer {
    #[cfg(not(test))]
    fn start(
        path: &Path,
        appearance: AppearancePreference,
        reduced_motion: bool,
    ) -> Result<Self, String> {
        // Do not stat the path here: a stalled automount would block lock
        // acquisition before READY. The worker's `Document::open` validates
        // existence and regular-file status and reports a decorative failure.
        let executable = std::env::current_exe()
            .map_err(|_| pipeline_error("could not locate the dynamic wallpaper worker"))?;
        Self::start_command(worker_command(
            &executable,
            path,
            appearance,
            reduced_motion,
        ))
    }

    /// Starts an in-process worker for tests that need a deterministic injected
    /// clock. The production source always uses `start_command`.
    #[cfg(test)]
    fn start(
        path: &Path,
        appearance: AppearancePreference,
        reduced_motion: bool,
    ) -> Result<Self, String> {
        let started = Instant::now();
        Self::start_with_time(
            path,
            appearance,
            reduced_motion,
            Box::new(move || (current_clock(), started.elapsed())),
        )
    }

    /// Spawns the worker as a resource-bounded child process and relays frames.
    ///
    /// Parsing and decoding run outside the lock-owning process, so an
    /// allocation abort or crash in the parser or decoder cannot terminate the
    /// greeter or locker. The relay reports a decorative failure that retains
    /// the poster or last frame instead.
    ///
    /// Only thread creation happens here. The supervisor thread performs the
    /// process spawn, every read, and every termination and reap, so a slow
    /// executable, a stalled filesystem, or an uninterruptible child cannot
    /// delay lock acquisition or READY.
    fn start_command(command: Command) -> Result<Self, String> {
        let (signal_sender, signal) = watch::channel(0);
        let shared = Arc::new(HeicShared::default());
        let stopping = Arc::new(AtomicBool::new(false));
        let child = Arc::new(Mutex::new(None));
        {
            let shared = Arc::clone(&shared);
            let stopping = Arc::clone(&stopping);
            let child = Arc::clone(&child);
            // The handle is dropped: the supervisor detaches, and `Drop` only
            // signals cancellation. Nothing on this thread ever waits for it.
            thread::Builder::new()
                .name("wallpaper-heic-supervisor".into())
                .spawn(move || {
                    supervise_heic_worker(command, shared, signal_sender, stopping, child);
                })
                .map_err(|_| pipeline_error("could not start the dynamic wallpaper worker"))?;
        }
        Ok(Self {
            shared,
            signal,
            stopping,
            child,
        })
    }

    #[cfg(test)]
    fn start_with_time(
        path: &Path,
        appearance: AppearancePreference,
        reduced_motion: bool,
        time: Box<HeicTime>,
    ) -> Result<Self, String> {
        let (signal_sender, signal) = watch::channel(0);
        let shared = Arc::new(HeicShared::default());
        let stopping = Arc::new(AtomicBool::new(false));
        let worker_shared = Arc::clone(&shared);
        let worker_stopping = Arc::clone(&stopping);
        let worker_path = path.to_owned();
        let _ = thread::Builder::new()
            .name("wallpaper-heic".into())
            .spawn(move || {
                let mut sink = SharedSink {
                    shared: &worker_shared,
                    signal: &signal_sender,
                };
                run_heic_with_sink(
                    &worker_path,
                    appearance,
                    reduced_motion,
                    time.as_ref(),
                    &mut sink,
                    &worker_stopping,
                )
            })
            .map_err(|_| pipeline_error("could not start the dynamic wallpaper worker"))?;
        Ok(Self {
            shared,
            signal,
            stopping,
            child: Arc::new(Mutex::new(None)),
        })
    }

    fn subscription(&self) -> Subscription<()> {
        Subscription::run_with(
            HeicFrameSignal {
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
        lock(&self.shared.pending).take()
    }

    fn has_failed(&self) -> bool {
        self.shared.failed.load(Ordering::Acquire)
    }
}

struct HeicFrameSignal {
    player: usize,
    receiver: watch::Receiver<u64>,
}

impl Hash for HeicFrameSignal {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.player.hash(state);
    }
}

impl Drop for HeicPlayer {
    fn drop(&mut self) {
        // Cancellation must not wait. A HEIC decode is not interruptible, so a
        // synchronous wait here would keep the lock coordinator alive after the
        // compositor lock is destroyed. `stopping` stops the relay from
        // publishing, and the kill releases a child blocked in decode; the
        // supervisor thread performs the wait and reap off this thread.
        self.stopping.store(true, Ordering::Release);
        signal_heic_child(&self.child);
    }
}

/// Kills the live worker without waiting.
///
/// The supervisor removes the child from the slot before it waits, so a signal
/// can never target a reaped and recycled process. The lock is held only for a
/// non-blocking `kill`, never across a wait, so cancellation cannot block on
/// the supervisor.
fn signal_heic_child(child: &Mutex<Option<Child>>) {
    if let Some(child) = lock(child).as_mut() {
        let _ = child.kill();
    }
}

/// Takes ownership of the worker and reaps it.
///
/// Only the supervisor calls this, after it has stopped reading, so the wait
/// cannot block a reader or the lock-owning thread.
fn reap_heic_child(child: &Mutex<Option<Child>>) {
    let Some(mut child) = lock(child).take() else {
        return;
    };
    let _ = child.wait();
}

/// Kills and reaps the worker when the supervisor leaves, including on unwind.
///
/// The supervisor is the only reaper, so the reap must not depend on reaching
/// the end of the function: a panic in the relay or in a diagnostic would
/// otherwise leave an exited child as a zombie with nobody left to wait for it.
struct HeicWorkerGuard {
    child: Arc<Mutex<Option<Child>>>,
    #[cfg(test)]
    shared: Arc<HeicShared>,
}

impl Drop for HeicWorkerGuard {
    fn drop(&mut self) {
        signal_heic_child(&self.child);
        reap_heic_child(&self.child);
        #[cfg(test)]
        self.shared.cleanup_complete.store(true, Ordering::Release);
    }
}

/// Owns the worker process for its whole lifetime.
///
/// Every blocking operation — `spawn`, the framing reads, `kill`, and `wait` —
/// happens on this thread. The lock-owning thread only signals cancellation.
fn supervise_heic_worker(
    mut command: Command,
    shared: Arc<HeicShared>,
    signal: watch::Sender<u64>,
    stopping: Arc<AtomicBool>,
    child_slot: Arc<Mutex<Option<Child>>>,
) {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(_) => {
            fail_heic(&shared, &signal);
            diagnostic(HEIC_DECODE_FAILURE);
            return;
        }
    };
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        fail_heic(&shared, &signal);
        diagnostic(HEIC_DECODE_FAILURE);
        return;
    };
    // Publish the child before reading so a concurrent cancellation can signal
    // it. A cancellation that arrived first is honoured here, and the guard
    // kills and reaps the worker on every exit path, including a panic.
    *lock(&child_slot) = Some(child);
    let reported = {
        let _reap_on_exit = HeicWorkerGuard {
            child: Arc::clone(&child_slot),
            #[cfg(test)]
            shared: Arc::clone(&shared),
        };
        if !stopping.load(Ordering::Acquire) {
            run_heic_reader(std::io::BufReader::new(stdout), &shared, &signal, &stopping);
        }
        shared.failed.load(Ordering::Acquire)
    };
    // The worker is already killed and reaped, so a blocked diagnostic cannot
    // delay cleanup. A published failure is always reported: a consumer that
    // cancelled after observing it must not erase the obligation to log it.
    if reported {
        diagnostic(HEIC_DECODE_FAILURE);
    }
}

/// The command that runs the HEIC parser and decoder in a child process.
///
/// The worker is told which process spawned it so it can bind its lifetime to
/// the greeter: if the greeter is killed without running its own destructors,
/// the worker must not outlive it.
fn worker_command(
    executable: &Path,
    path: &Path,
    appearance: AppearancePreference,
    reduced_motion: bool,
) -> Command {
    let mut command = Command::new(executable);
    command
        .arg("heic-worker")
        .arg("--file")
        .arg(path)
        .arg("--appearance")
        .arg(appearance_argument(appearance));
    if reduced_motion {
        command.arg("--reduce-motion");
    }
    command
}

/// The CLI name of an appearance preference, matching `WallpaperAppearance`.
fn appearance_argument(appearance: AppearancePreference) -> &'static str {
    match appearance {
        AppearancePreference::Automatic => "automatic",
        AppearancePreference::Light => "light",
        AppearancePreference::Dark => "dark",
    }
}

/// The exact RGBA byte length of a relayed frame, bounded before allocating.
fn expected_frame_bytes(width: u32, height: u32) -> Option<usize> {
    if width == 0 || height == 0 {
        return None;
    }
    if width > MAX_HEIC_FRAME_DIMENSION || height > MAX_HEIC_FRAME_DIMENSION {
        return None;
    }
    let bytes = usize::try_from(width)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)?
        .checked_mul(4)?;
    (bytes <= MAX_HEIC_FRAME_BYTES).then_some(bytes)
}

/// Reads one frame with a fallible allocation so a hostile length cannot abort
/// the lock-owning process.
fn read_frame_buffer<R: Read>(reader: &mut R, length: usize) -> Option<Vec<u8>> {
    let mut buffer = reserve_frame_buffer(length)?;
    buffer.resize(length, 0);
    reader.read_exact(&mut buffer).ok()?;
    Some(buffer)
}

/// Relays frames from the worker process until it exits or fails.
///
/// Any unexpected end of stream, protocol violation, or refused buffer
/// allocation is a decorative failure: the caller retains the poster or last
/// frame. An OOM abort or crash in the worker therefore cannot affect lock
/// readiness or unlock.
fn run_heic_reader<R: Read>(
    mut reader: R,
    shared: &Arc<HeicShared>,
    signal: &watch::Sender<u64>,
    stopping: &AtomicBool,
) {
    loop {
        let mut tag = [0u8; 1];
        if reader.read_exact(&mut tag).is_err() {
            break;
        }
        if tag[0] == HEIC_FAILED_TAG {
            fail_heic(shared, signal);
            return;
        }
        if tag[0] != HEIC_FRAME_TAG {
            break;
        }
        let mut header = [0u8; HEIC_HEADER_BYTES];
        if reader.read_exact(&mut header).is_err() {
            break;
        }
        let width = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
        let height = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
        let length = u32::from_le_bytes([header[8], header[9], header[10], header[11]]) as usize;
        let Some(expected) = expected_frame_bytes(width, height) else {
            break;
        };
        if length != expected {
            break;
        }
        let Some(pixels) = read_frame_buffer(&mut reader, length) else {
            break;
        };
        if stopping.load(Ordering::Acquire) {
            // Cancellation wins over a frame that was already read: the
            // consumer is going away, so publishing it has no owner.
            break;
        }
        emit_heic_frame(shared, signal, width, height, Some(Bytes::from(pixels)));
    }
    if !stopping.load(Ordering::Acquire) {
        fail_heic(shared, signal);
    }
}

fn reserve_frame_buffer(len: usize) -> Option<Vec<u8>> {
    let mut buffer = Vec::new();
    buffer.try_reserve_exact(len).ok()?;
    Some(buffer)
}

/// Copies a decoded frame with a fallible allocation so memory pressure
/// retains the last valid frame instead of aborting the process.
#[cfg(test)]
fn copy_frame_pixels(pixels: &[u8]) -> Option<Bytes> {
    let mut buffer = reserve_frame_buffer(pixels.len())?;
    buffer.extend_from_slice(pixels);
    Some(Bytes::from(buffer))
}

/// Stores a frame only when its handoff allocation succeeded. A refused
/// allocation leaves the previous pending frame untouched.
fn emit_heic_frame(
    shared: &HeicShared,
    signal: &watch::Sender<u64>,
    width: u32,
    height: u32,
    pixels: Option<Bytes>,
) -> bool {
    let Some(pixels) = pixels else {
        return false;
    };
    if shared.failed.load(Ordering::Acquire) {
        return false;
    }
    *lock(&shared.pending) = Some(Update::Frame(Frame {
        width,
        height,
        pixels,
        pts: None,
    }));
    let sequence = shared.sequence.fetch_add(1, Ordering::AcqRel) + 1;
    signal.send_replace(sequence);
    true
}

#[cfg(test)]
fn publish_heic(shared: &HeicShared, signal: &watch::Sender<u64>, frame: Option<&HeicFrame>) {
    let Some(frame) = frame else {
        return;
    };
    if shared.failed.load(Ordering::Acquire) {
        return;
    }
    emit_heic_frame(
        shared,
        signal,
        frame.width,
        frame.height,
        copy_frame_pixels(&frame.pixels),
    );
}

/// Publishes a terminal decorative failure exactly once.
///
/// This only updates shared state. The supervisor emits the diagnostic after
/// the worker is reaped, so a blocked or failing stderr cannot delay cleanup.
fn fail_heic(shared: &HeicShared, signal: &watch::Sender<u64>) {
    if shared.failed.swap(true, Ordering::AcqRel) {
        return;
    }
    *lock(&shared.pending) = Some(Update::Failed);
    let sequence = shared.sequence.fetch_add(1, Ordering::AcqRel) + 1;
    signal.send_replace(sequence);
}

const HEIC_DECODE_FAILURE: &str =
    "dynamic wallpaper could not be decoded; retaining current background";

/// Applies a schedule update and reports whether the presented pixels changed
/// without a decode (a discontinuity snapping an active dissolve).
fn apply_heic_schedule(
    playback: &mut HeicPlayback,
    clock: ClockSnapshot,
    monotonic: Duration,
    discontinuity: bool,
) -> bool {
    let outcome = if discontinuity {
        playback.resynchronize_after_discontinuity(clock, monotonic)
    } else {
        playback.synchronize(clock, monotonic)
    };
    outcome.presentation_changed
}

/// Reports at most one decode-failure diagnostic per successful frame.
#[derive(Default)]
struct HeicDiagnostics {
    decode_failure_reported: bool,
}

impl HeicDiagnostics {
    fn decode_failure(&mut self) {
        if !self.decode_failure_reported {
            self.decode_failure_reported = true;
            diagnostic(
                "dynamic wallpaper frame could not be decoded; retaining the last valid frame",
            );
        }
    }

    fn decoded(&mut self) {
        self.decode_failure_reported = false;
    }
}

/// Completes a decode against the freshly sampled time and reports whether the
/// presented frame should be published. `clock` and `monotonic` must be
/// sampled after the decode returned.
fn finish_heic_decode(
    playback: &mut HeicPlayback,
    request: DecodeRequest,
    result: Result<HeicFrame, ()>,
    clock: ClockSnapshot,
    monotonic: Duration,
    discontinuity: bool,
    diagnostics: &mut HeicDiagnostics,
) -> bool {
    let mut publish = apply_heic_schedule(playback, clock, monotonic, discontinuity);
    match playback.complete_decode(request, result, monotonic) {
        DecodeOutcome::Presented | DecodeOutcome::Transitioning => {
            diagnostics.decoded();
            publish = true;
        }
        DecodeOutcome::Failed | DecodeOutcome::Rejected => diagnostics.decode_failure(),
        DecodeOutcome::Ignored => {}
    }
    publish
}

/// Receives worker frames and terminal failures.
///
/// `publish` returns `false` when the consumer is gone, so a worker writing to
/// a closed relay pipe stops instead of blocking forever.
trait HeicSink {
    fn publish(&mut self, frame: Option<&HeicFrame>) -> bool;
    fn failed(&mut self);
}

/// The in-process sink used by the deterministic test worker.
#[cfg(test)]
struct SharedSink<'a> {
    shared: &'a Arc<HeicShared>,
    signal: &'a watch::Sender<u64>,
}

#[cfg(test)]
impl HeicSink for SharedSink<'_> {
    fn publish(&mut self, frame: Option<&HeicFrame>) -> bool {
        publish_heic(self.shared, self.signal, frame);
        true
    }

    fn failed(&mut self) {
        fail_heic(self.shared, self.signal);
    }
}

/// Serializes worker frames to the relay pipe for the lock-owning process.
struct ProcessSink<W: Write> {
    writer: W,
}

impl<W: Write> HeicSink for ProcessSink<W> {
    fn publish(&mut self, frame: Option<&HeicFrame>) -> bool {
        let Some(frame) = frame else {
            return true;
        };
        write_heic_frame(&mut self.writer, frame).is_ok()
    }

    fn failed(&mut self) {
        let _ = self.writer.write_all(&[HEIC_FAILED_TAG]);
        let _ = self.writer.flush();
    }
}

/// Writes one length-checked frame to the relay pipe.
fn write_heic_frame<W: Write>(writer: &mut W, frame: &HeicFrame) -> std::io::Result<()> {
    let length = u32::try_from(frame.pixels.len()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "decoded frame is too large",
        )
    })?;
    let mut header = [0u8; 13];
    header[0] = HEIC_FRAME_TAG;
    header[1..5].copy_from_slice(&frame.width.to_le_bytes());
    header[5..9].copy_from_slice(&frame.height.to_le_bytes());
    header[9..13].copy_from_slice(&length.to_le_bytes());
    writer.write_all(&header)?;
    writer.write_all(&frame.pixels)?;
    writer.flush()
}

/// Runs the parser and decoder for the helper process, streaming frames to
/// stdout for the lock-owning parent.
///
/// This is the only supported entry point for untrusted dynamic wallpapers in
/// login and lock. Running it in a child process keeps an allocation abort or
/// crash in quick-xml, base64, `plist`, or libheif from terminating the
/// lock-owning process.
pub(crate) fn run_heic_worker(path: &Path, appearance: AppearancePreference, reduced_motion: bool) {
    let started = Instant::now();
    let time_source: Box<HeicTime> = Box::new(move || (current_clock(), started.elapsed()));
    let mut sink = ProcessSink {
        writer: std::io::stdout().lock(),
    };
    run_heic_with_sink(
        path,
        appearance,
        reduced_motion,
        time_source.as_ref(),
        &mut sink,
        &AtomicBool::new(false),
    );
}

fn run_heic_with_sink(
    path: &Path,
    appearance: AppearancePreference,
    reduced_motion: bool,
    time_source: &HeicTime,
    sink: &mut dyn HeicSink,
    stopping: &AtomicBool,
) {
    let document = match Document::open(path) {
        Ok(document) => document,
        Err(_) => {
            sink.failed();
            return;
        }
    };
    let mut suspend = SuspendDetector::new(suspend_clock_offset());
    let mut diagnostics = HeicDiagnostics::default();
    let (Some(initial_clock), initial_monotonic) = time_source() else {
        sink.failed();
        return;
    };
    let mut playback = HeicPlayback::new(
        document.metadata(),
        document.primary_image(),
        appearance,
        initial_clock,
        initial_monotonic,
        reduced_motion,
    );
    while !stopping.load(Ordering::Acquire) {
        let (clock, monotonic) = time_source();
        let clock = clock.unwrap_or(initial_clock);
        if apply_heic_schedule(&mut playback, clock, monotonic, suspend.sample())
            && !sink.publish(playback.frame())
        {
            return;
        }
        if playback.advance_transition(monotonic) && !sink.publish(playback.frame()) {
            return;
        }
        if let Some(request) = playback.take_decode_request() {
            let result = document.decode(request.image).map_err(|_| ());
            // The consumer may have dropped while the decode was running; do
            // not synchronize, blend, copy, or publish work nobody can adopt.
            if stopping.load(Ordering::Acquire) {
                return;
            }
            // Resample after the blocking decode: a request that crossed a
            // boundary or the dissolve window must be completed against the
            // current time, not the time before the decode started. One call
            // returns a coherent civil/monotonic pair.
            let (clock, monotonic) = time_source();
            let clock = clock.unwrap_or(initial_clock);
            if finish_heic_decode(
                &mut playback,
                request,
                result,
                clock,
                monotonic,
                suspend.sample(),
                &mut diagnostics,
            ) && !sink.publish(playback.frame())
            {
                return;
            }
        }
        let delay = if playback.is_transitioning() {
            HEIC_TRANSITION_INTERVAL
        } else {
            HEIC_POLL_INTERVAL
        };
        thread::sleep(delay);
    }
}

/// Runs the in-process worker used by deterministic unit tests.
#[cfg(test)]
fn run_heic(
    path: &Path,
    appearance: AppearancePreference,
    reduced_motion: bool,
    time_source: &HeicTime,
    shared: &Arc<HeicShared>,
    signal: &watch::Sender<u64>,
    stopping: &AtomicBool,
) {
    let mut sink = SharedSink { shared, signal };
    run_heic_with_sink(
        path,
        appearance,
        reduced_motion,
        time_source,
        &mut sink,
        stopping,
    );
}

const SUSPEND_DETECTION_THRESHOLD: Duration = Duration::from_millis(10);

/// Detects suspend/resume by watching the CLOCK_BOOTTIME minus CLOCK_MONOTONIC
/// offset. Unlike `Playback`'s one-second wall-clock tolerance, this catches a
/// subsecond suspend during a dissolve. The greeter, locker, and desktop
/// runtime all use it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SuspendDetector {
    offset: Option<Duration>,
}

impl SuspendDetector {
    pub(crate) const fn new(offset: Option<Duration>) -> Self {
        Self { offset }
    }

    pub(crate) fn observe(&mut self, offset: Duration) -> bool {
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

    pub(crate) fn sample(&mut self) -> bool {
        suspend_clock_offset().is_some_and(|offset| self.observe(offset))
    }
}

pub(crate) fn suspend_clock_offset() -> Option<Duration> {
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

/// The current local civil time, matching the desktop runtime's clock source.
pub(crate) fn current_clock() -> Option<ClockSnapshot> {
    let now = chrono::Local::now();
    let date = CivilDate::new(now.year(), now.month() as u8, now.day() as u8).ok()?;
    let time = CivilTime::new(now.hour() as u8, now.minute() as u8, now.second() as u8).ok()?;
    ClockSnapshot::new_with_nanosecond(
        date,
        time,
        now.nanosecond(),
        now.offset().fix().local_minus_utc(),
    )
    .ok()
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

pub(crate) fn open_wallpaper(path: &Path) -> Result<File, String> {
    open_regular(path).map_err(|error| {
        pipeline_error(match error {
            OpenError::Unavailable => "wallpaper file is unavailable",
            OpenError::Metadata => "wallpaper file metadata is unavailable",
            OpenError::NotRegular => "wallpaper path is not a regular file",
            OpenError::Reopen => "wallpaper file could not be opened for playback",
        })
    })
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
    // A diagnostic must never panic. The supervisor's cleanup and the greeter's
    // lifecycle must not depend on a writable stderr, and a failed write must
    // not skip the reap that follows a terminal relay failure.
    let _ = writeln!(std::io::stderr(), "genkan: {}", bounded_text(message));
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
    use std::io::{ErrorKind, Read};
    use std::sync::atomic::AtomicUsize;

    use super::*;
    use genkan::dynamic_wallpaper::{
        Appearance, AppleProperty, ImageReference, Metadata, NormalizedTime, PropertyValue,
        Schedule, SolarPoint, SolarPosition, TimePoint,
    };
    use iced::Size;
    use rustix::fs::Mode;

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
                reduced_motion: false,
                appearance: AppearancePreference::Automatic,
            });
            assert!(state.decoder_is_stopped(), "{catalog:?}");
            assert!(state.has_frame(), "{catalog:?}");
        }
    }

    fn heic_fixture() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/dynamic-heic/synthetic-all-properties.heic")
    }

    fn heic_settings(path: Option<PathBuf>, animate: bool, reduced_motion: bool) -> Settings {
        Settings {
            catalog: Catalog::TahoeBeach,
            override_path: path,
            animate,
            reduced_motion,
            appearance: AppearancePreference::Automatic,
        }
    }

    fn wait_for_heic(state: &mut State) -> Refresh {
        for _ in 0..1000 {
            match state.receive_latest() {
                Refresh::Unchanged => thread::sleep(Duration::from_millis(10)),
                other => return other,
            }
        }
        Refresh::Unchanged
    }

    /// A test clock whose civil and monotonic samples advance together, so a
    /// slow decode cannot be misread as a wall-clock discontinuity.
    #[derive(Clone)]
    struct FakeTime(Arc<Mutex<(ClockSnapshot, Duration)>>);

    impl FakeTime {
        fn new(clock: ClockSnapshot, monotonic: Duration) -> Self {
            Self(Arc::new(Mutex::new((clock, monotonic))))
        }

        fn set(&self, clock: ClockSnapshot, monotonic: Duration) {
            *lock(&self.0) = (clock, monotonic);
        }

        /// One callback returning both samples under one lock, so a concurrent
        /// `set` cannot produce a torn civil/monotonic pair.
        fn time_source(&self) -> Box<HeicTime> {
            let time = self.clone();
            Box::new(move || {
                let sample = lock(&time.0);
                (Some(sample.0), sample.1)
            })
        }
    }

    /// Bounded wait for a raw worker frame that fails immediately on a worker
    /// failure and on timeout instead of hanging the test suite.
    fn wait_for_heic_frame_labeled(shared: &HeicShared, timeout: Duration, label: &str) -> Frame {
        let deadline = Instant::now() + timeout;
        loop {
            // Bind the taken update before matching so the mutex guard is
            // released before the sleep; holding it would starve the worker.
            let pending = lock(&shared.pending).take();
            match pending {
                Some(Update::Frame(frame)) => return frame,
                Some(Update::Failed) => panic!("HEIC worker reported a failure instead of a frame"),
                None => {
                    assert!(
                        Instant::now() < deadline,
                        "timed out waiting for a HEIC worker frame ({label})"
                    );
                    thread::sleep(Duration::from_millis(10));
                }
            }
        }
    }

    fn wait_for_heic_frame(shared: &HeicShared, timeout: Duration) -> Frame {
        wait_for_heic_frame_labeled(shared, timeout, "frame")
    }

    #[test]
    fn heic_paths_are_detected_case_insensitively() {
        assert!(is_heic_path(Path::new("/tmp/wallpaper.heic")));
        assert!(is_heic_path(Path::new("/tmp/wallpaper.HEIF")));
        assert!(!is_heic_path(Path::new("/tmp/wallpaper.mov")));
        assert!(!is_heic_path(Path::new("/tmp/wallpaper")));
    }

    #[test]
    fn dynamic_heic_source_produces_scheduled_frames() {
        for reduced_motion in [false, true] {
            let mut state = State::start(heic_settings(Some(heic_fixture()), true, reduced_motion));
            assert_eq!(
                wait_for_heic(&mut state),
                Refresh::Frame,
                "reduced_motion={reduced_motion}"
            );
            assert!(!state.decoder_is_stopped());
            assert_eq!(state.rgba_frame().unwrap().dimensions(), (8, 8));
        }
    }

    #[test]
    fn heic_adoption_is_recorded_when_a_frame_is_consumed() {
        let mut state = State::start(heic_settings(Some(heic_fixture()), true, true));
        assert!(!state.heic_adopted);
        assert_eq!(wait_for_heic(&mut state), Refresh::Frame);
        assert!(
            state.heic_adopted,
            "consuming a HEIC frame records adoption"
        );

        let mov = State::start(heic_settings(None, false, false));
        assert!(!mov.heic_adopted);
    }

    #[test]
    fn dynamic_heic_static_uses_only_the_poster() {
        let state = State::start(heic_settings(Some(heic_fixture()), false, false));
        assert!(state.decoder_is_stopped());
        assert!(state.has_frame());
    }

    #[test]
    fn dynamic_heic_failure_retains_the_poster() {
        let directory =
            std::env::temp_dir().join(format!("genkan-heic-failure-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("broken.heic");
        std::fs::write(&path, b"not a heic container").unwrap();

        let mut state = State::start(heic_settings(Some(path), true, false));
        assert_eq!(wait_for_heic(&mut state), Refresh::Failed);
        assert!(state.decoder_is_stopped());
        assert!(state.has_frame());

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn dynamic_heic_missing_file_falls_back_after_worker_failure() {
        let path =
            std::env::temp_dir().join(format!("genkan-heic-missing-{}.heic", std::process::id()));
        let _ = std::fs::remove_file(&path);

        // The player starts without a synchronous stat; the worker validates
        // the path and reports a decorative failure.
        let mut state = State::start(heic_settings(Some(path), true, false));
        assert!(!state.decoder_is_stopped());
        assert_eq!(wait_for_heic(&mut state), Refresh::Failed);
        assert!(state.decoder_is_stopped());
        assert!(state.has_frame());
    }

    fn clock_snapshot(hour: u8, minute: u8, second: u8) -> ClockSnapshot {
        ClockSnapshot::new(
            CivilDate::new(2026, 9, 10).unwrap(),
            CivilTime::new(hour, minute, second).unwrap(),
            0,
        )
        .unwrap()
    }

    fn time_metadata(points: Vec<TimePoint>) -> Metadata {
        let mut metadata = Metadata::default();
        metadata
            .insert(
                AppleProperty::Time,
                PropertyValue::Time(Schedule::new(points, None).unwrap()),
            )
            .unwrap();
        metadata
    }

    fn heic_frame(value: u8) -> HeicFrame {
        let pixels = (0..2).flat_map(|_| [value, value, value, 255]).collect();
        HeicFrame {
            width: 2,
            height: 1,
            pixels,
        }
    }

    #[test]
    fn copy_frame_pixels_reports_allocation_failure() {
        assert_eq!(
            copy_frame_pixels(&[1, 2, 3, 4]),
            Some(Bytes::from_static(&[1, 2, 3, 4]))
        );
        // A capacity overflow is reported instead of aborting the process.
        assert!(reserve_frame_buffer(usize::MAX).is_none());
    }

    #[test]
    fn discontinuity_snaps_a_dissolve_and_reports_a_publish() {
        let metadata = time_metadata(vec![
            TimePoint {
                image: ImageReference::from_position(0),
                time: NormalizedTime::new(0.0).unwrap(),
            },
            TimePoint {
                image: ImageReference::from_position(1),
                time: NormalizedTime::new(60.0 / 86_400.0).unwrap(),
            },
        ]);
        let mut playback = HeicPlayback::new(
            &metadata,
            ImageReference::from_position(0),
            AppearancePreference::Automatic,
            clock_snapshot(0, 0, 0),
            Duration::ZERO,
            false,
        );
        let request = playback.take_decode_request().unwrap();
        playback.complete_decode(request, Ok(heic_frame(0)), Duration::ZERO);
        playback.synchronize(clock_snapshot(0, 1, 0), Duration::from_secs(60));
        let request = playback.take_decode_request().unwrap();
        playback.complete_decode(request, Ok(heic_frame(200)), Duration::from_millis(60_500));
        assert!(playback.is_transitioning());

        assert!(
            apply_heic_schedule(
                &mut playback,
                clock_snapshot(0, 1, 1),
                Duration::from_secs(61),
                true,
            ),
            "a discontinuity snap must request a publish"
        );
        assert!(!playback.is_transitioning());
    }

    #[test]
    fn suspend_detector_catches_subsecond_gaps() {
        let mut detector = SuspendDetector::new(Some(Duration::ZERO));
        // 800 ms is below Playback's one-second clock tolerance but must be
        // treated as a suspend.
        assert!(detector.observe(Duration::from_millis(800)));
        assert!(!detector.observe(Duration::from_millis(805)));
        assert!(detector.observe(Duration::from_millis(1_605)));
    }

    /// The relay framing of one opaque 1x1 RGBA frame, as POSIX `printf`
    /// escapes for a harness-free shell helper. A real helper must never share
    /// stdout with a test harness, because harness output is not protocol.
    const SHELL_FRAME: &str =
        r"\106\001\000\000\000\001\000\000\000\004\000\000\000\001\002\003\377";

    /// A harness-free protocol helper: a shell that writes relay bytes on its
    /// own stdout and then `exec`s a blocking program, so the tracked process
    /// stays alive until it is signalled.
    fn shell_worker(frame: bool) -> Command {
        let script = if frame {
            format!("printf '{SHELL_FRAME}'; exec sleep 300")
        } else {
            "exec sleep 300".to_owned()
        };
        let mut command = Command::new("sh");
        command.arg("-c").arg(script);
        command
    }

    fn worker_pid(player: &HeicPlayer) -> u32 {
        lock(&player.child)
            .as_ref()
            .map(Child::id)
            .expect("the supervisor publishes the worker before reading")
    }

    /// Cancels a supervisor and signals its worker on every exit path.
    ///
    /// A handshake that only completes on the success path leaves the helper
    /// running when an assertion fails, so cancellation must be unwind-safe.
    /// Signalling is a no-op once the supervisor has reaped, because reaping
    /// clears the child slot before waiting.
    struct SupervisorCancel {
        stopping: Arc<AtomicBool>,
        child: Arc<Mutex<Option<Child>>>,
    }

    impl Drop for SupervisorCancel {
        fn drop(&mut self) {
            self.stopping.store(true, Ordering::Release);
            signal_heic_child(&self.child);
        }
    }

    /// Waits for the supervisor to finish killing and reaping the worker.
    ///
    /// The reaping probe must not run before this: `waitpid` is a competing
    /// reaper, so an early probe would collect the zombie itself and misreport
    /// a correct supervisor.
    fn wait_for_supervisor_cleanup(shared: &HeicShared, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if shared.cleanup_complete.load(Ordering::Acquire) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    /// Whether the supervisor has already reaped the worker.
    ///
    /// A reaped pid is no longer waitable (`ECHILD`), while a running worker
    /// reports no state change. A zombie means the supervisor exited without
    /// waiting, which is reported as a failure and never retried: the probe
    /// itself would have collected that zombie.
    fn probe_child_reaped(pid: u32) -> bool {
        let mut status = 0;
        // SAFETY: `waitpid` is called with a specific pid and a valid out
        // pointer, and `WNOHANG` keeps it from blocking.
        let result = unsafe { libc::waitpid(pid as i32, &mut status, libc::WNOHANG) };
        assert_ne!(
            result, pid as i32,
            "the worker was still a zombie, so the supervisor never reaped it"
        );
        if result == 0 {
            return false;
        }
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::Interrupted {
            return false;
        }
        assert_eq!(
            error.raw_os_error(),
            Some(libc::ECHILD),
            "unexpected waitpid result for the worker"
        );
        true
    }

    /// Waits for the supervisor to reap the worker. This needs no procfs, and a
    /// pid cannot be recycled while it is still waitable.
    fn wait_for_reap(pid: u32, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if probe_child_reaped(pid) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// Waits for the terminal failure update to be *published*.
    ///
    /// `has_failed` is set before the pending update and its notification, so
    /// it is not a publication barrier for a consumer.
    fn wait_for_failure(shared: &HeicShared, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if matches!(lock(&shared.pending).as_ref(), Some(Update::Failed)) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    /// Waits for the supervisor to publish the worker without blocking on it.
    fn wait_for_worker_pid(child: &Mutex<Option<Child>>, timeout: Duration) -> u32 {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(pid) = lock(child).as_ref().map(Child::id) {
                return pid;
            }
            assert!(
                Instant::now() < deadline,
                "the supervisor did not publish a worker"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn wait_for_worker(player: &HeicPlayer, timeout: Duration) -> u32 {
        wait_for_worker_pid(&player.child, timeout)
    }

    #[test]
    fn dropping_a_heic_player_kills_and_reaps_the_worker() {
        let player = HeicPlayer::start_command(shell_worker(true)).expect("blocked worker process");
        let pid = wait_for_worker(&player, Duration::from_secs(5));
        // Prove the helper reached its blocked state before dropping, so the
        // cleanup below cannot pass on a process that never started.
        wait_for_heic_frame(&player.shared, Duration::from_secs(5));

        // A generous bound: this is a sanity check that Drop performs no
        // unbounded work, not a proof that Drop never waits. The structural
        // guarantee is that only the supervisor thread calls `wait`, which
        // `a_supervisor_reaps_the_worker` exercises.
        let shared = Arc::clone(&player.shared);
        let started = Instant::now();
        drop(player);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "dropping a HEIC player must not block on the worker"
        );
        assert!(
            wait_for_supervisor_cleanup(&shared, Duration::from_secs(10)),
            "the supervisor must finish cleaning up"
        );
        assert!(
            wait_for_reap(pid, Duration::from_secs(10)),
            "the supervisor must reap the killed worker"
        );
    }

    #[test]
    fn a_cancelled_supervisor_publishes_nothing() {
        // Cancellation that arrives before the worker is read must leave the
        // shared state untouched and clear the child slot. This asserts the
        // observable outcome of the startup race `Drop` can lose; the reap
        // itself is proven by `a_supervisor_reaps_the_worker`.
        let shared = Arc::new(HeicShared::default());
        let (signal, _receiver) = watch::channel(0);
        let stopping = Arc::new(AtomicBool::new(true));
        let child = Arc::new(Mutex::new(None));

        supervise_heic_worker(
            shell_worker(true),
            Arc::clone(&shared),
            signal,
            Arc::clone(&stopping),
            Arc::clone(&child),
        );

        assert!(lock(&child).is_none());
        assert!(lock(&shared.pending).is_none());
        assert!(!shared.failed.load(Ordering::Acquire));
        assert_eq!(shared.sequence.load(Ordering::Acquire), 0);
    }

    #[test]
    fn a_supervisor_reaps_the_worker() {
        // The helper holds its stdout open until the test releases it, so the
        // supervisor blocks in its framing read and the published child is
        // observable without a timing race. Once released, the supervisor must
        // reap it: an unreaped worker would still be waitable here.
        let release = std::env::temp_dir().join(format!(
            "genkan-heic-supervisor-release-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&release);
        let shared = Arc::new(HeicShared::default());
        let (signal, _receiver) = watch::channel(0);
        let stopping = Arc::new(AtomicBool::new(false));
        let child = Arc::new(Mutex::new(None));
        // The release handshake only completes on the success path, so a failed
        // assertion before it must still stop the helper. This guard is
        // installed before the supervisor exists; a supervisor that publishes
        // after the test unwinds observes the cancellation and reaps the child.
        let _cancel = SupervisorCancel {
            stopping: Arc::clone(&stopping),
            child: Arc::clone(&child),
        };
        let supervisor = {
            let shared = Arc::clone(&shared);
            let stopping = Arc::clone(&stopping);
            let child = Arc::clone(&child);
            let release = release.clone();
            thread::spawn(move || {
                let script = format!(
                    "printf '{SHELL_FRAME}'; while [ ! -e '{}' ]; do sleep 0.02; done",
                    release.display()
                );
                let mut command = Command::new("sh");
                command.arg("-c").arg(script);
                supervise_heic_worker(command, shared, signal, stopping, child);
            })
        };
        let pid = wait_for_worker_pid(&child, Duration::from_secs(5));
        std::fs::write(&release, b"release").expect("release the helper");
        supervisor.join().unwrap();
        let _ = std::fs::remove_file(&release);

        assert!(
            wait_for_supervisor_cleanup(&shared, Duration::from_secs(10)),
            "the supervisor must finish cleaning up"
        );
        assert!(
            wait_for_reap(pid, Duration::from_secs(10)),
            "the supervisor must reap the worker"
        );
        assert!(lock(&child).is_none());
    }

    #[test]
    fn a_crashing_worker_is_a_decorative_failure_that_is_reaped() {
        let player = HeicPlayer::start_command(shell_worker(true)).expect("worker process");
        let pid = wait_for_worker(&player, Duration::from_secs(5));
        let frame = wait_for_heic_frame(&player.shared, Duration::from_secs(5));
        assert_eq!((frame.width, frame.height), (1, 1));

        // A crash after a valid frame: the relay must report a bounded
        // decorative failure instead of a frame.
        // SAFETY: `pid` is the live worker published above.
        assert_eq!(unsafe { libc::kill(pid as i32, libc::SIGABRT) }, 0);

        assert!(
            wait_for_failure(&player.shared, Duration::from_secs(10)),
            "an aborted worker must publish a decorative failure"
        );
        assert!(player.has_failed());
        assert!(matches!(
            lock(&player.shared.pending).take(),
            Some(Update::Failed)
        ));
        assert!(
            wait_for_supervisor_cleanup(&player.shared, Duration::from_secs(10)),
            "the supervisor must finish cleaning up"
        );
        assert!(
            wait_for_reap(pid, Duration::from_secs(10)),
            "a crashed worker must still be reaped"
        );
    }

    #[test]
    fn a_worker_that_cannot_spawn_is_a_decorative_failure() {
        let mut command = Command::new("/nonexistent/genkan-heic-worker");
        command.arg("--file").arg("/tmp/missing.heic");
        let player = HeicPlayer::start_command(command).expect("the supervisor still starts");

        assert!(
            wait_for_failure(&player.shared, Duration::from_secs(10)),
            "a worker that cannot be spawned is a decorative failure"
        );
        assert!(lock(&player.child).is_none());
    }

    #[test]
    fn a_worker_that_exits_after_a_frame_retains_the_poster() {
        let mut command = Command::new("sh");
        command.arg("-c").arg(format!("printf '{SHELL_FRAME}'"));
        let mut state = State {
            player: None,
            heic: Some(HeicPlayer::start_command(command).expect("worker process")),
            poster: Some(image::Handle::from_rgba(1, 1, vec![9, 9, 9, 255])),
            frame: Some(image::Handle::from_rgba(1, 1, vec![9, 9, 9, 255])),
            allocation: None,
            allocation_pending: false,
            heic_adopted: false,
        };
        let poster_id = state.frame.as_ref().unwrap().id();

        // Wait for the terminal failure to be published *before* consuming, so
        // this covers the ordering where the failure wins the race with the
        // frame. The frame was never adopted, so the poster stays and no
        // decoder is left behind.
        let shared = Arc::clone(&state.heic.as_ref().expect("heic source").shared);
        assert!(
            wait_for_failure(&shared, Duration::from_secs(10)),
            "the worker exit was not reported"
        );
        assert_eq!(state.receive_latest(), Refresh::Failed);
        assert!(state.decoder_is_stopped());
        assert_eq!(state.frame.as_ref().unwrap().id(), poster_id);
    }

    #[test]
    fn a_crashed_worker_after_adoption_retains_the_adopted_frame() {
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(format!("printf '{SHELL_FRAME}'; exec sleep 300"));
        let mut state = State {
            player: None,
            heic: Some(HeicPlayer::start_command(command).expect("worker process")),
            poster: Some(image::Handle::from_rgba(1, 1, vec![9, 9, 9, 255])),
            frame: Some(image::Handle::from_rgba(1, 1, vec![9, 9, 9, 255])),
            allocation: None,
            allocation_pending: false,
            heic_adopted: false,
        };

        let deadline = Instant::now() + Duration::from_secs(10);
        while state.receive_latest() != Refresh::Frame {
            assert!(
                Instant::now() < deadline,
                "the worker never delivered a frame"
            );
            thread::sleep(Duration::from_millis(10));
        }
        let adopted_id = state.frame.as_ref().unwrap().id();
        let shared = Arc::clone(&state.heic.as_ref().expect("heic source").shared);
        let pid = worker_pid(state.heic.as_ref().unwrap());
        // SAFETY: `pid` is the live worker published by the supervisor.
        assert_eq!(unsafe { libc::kill(pid as i32, libc::SIGABRT) }, 0);

        assert!(
            wait_for_failure(&shared, Duration::from_secs(10)),
            "the crash was not reported"
        );
        assert_eq!(state.receive_latest(), Refresh::Failed);
        assert!(state.decoder_is_stopped());
        assert_eq!(state.frame.as_ref().unwrap().id(), adopted_id);
    }

    fn relay_frame_bytes(width: u32, height: u32, pixels: &[u8]) -> Vec<u8> {
        let mut bytes = vec![HEIC_FRAME_TAG];
        bytes.extend_from_slice(&width.to_le_bytes());
        bytes.extend_from_slice(&height.to_le_bytes());
        bytes.extend_from_slice(&(pixels.len() as u32).to_le_bytes());
        bytes.extend_from_slice(pixels);
        bytes
    }

    /// A raw tag and header with no payload, for malformed-protocol cases.
    fn relay_header_bytes(tag: u8, width: u32, height: u32, length: u32) -> Vec<u8> {
        let mut bytes = vec![tag];
        bytes.extend_from_slice(&width.to_le_bytes());
        bytes.extend_from_slice(&height.to_le_bytes());
        bytes.extend_from_slice(&length.to_le_bytes());
        bytes
    }

    /// Drives the production reader over `bytes` and asserts that its only
    /// published update is a terminal failure.
    ///
    /// The notification sequence distinguishes "no frame was ever adopted"
    /// from "a frame was published and then superseded": a published frame
    /// would leave the sequence above one.
    fn relay_fails_without_a_frame(bytes: Vec<u8>) {
        let shared = Arc::new(HeicShared::default());
        let (signal, _receiver) = watch::channel(0);
        let stopping = Arc::new(AtomicBool::new(false));
        run_heic_reader(std::io::Cursor::new(bytes), &shared, &signal, &stopping);
        assert!(shared.failed.load(Ordering::Acquire));
        assert_eq!(
            shared.sequence.load(Ordering::Acquire),
            1,
            "a malformed stream must publish exactly one terminal failure"
        );
        assert!(matches!(lock(&shared.pending).take(), Some(Update::Failed)));
    }

    /// Yields a fixed prefix and then fails every further read, recording that
    /// a payload read was attempted at all.
    struct PayloadProbe {
        prefix: Vec<u8>,
        offset: usize,
        payload_reads: Arc<AtomicUsize>,
    }

    impl Read for PayloadProbe {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            if self.offset < self.prefix.len() {
                let available = (self.prefix.len() - self.offset).min(buffer.len());
                buffer[..available]
                    .copy_from_slice(&self.prefix[self.offset..self.offset + available]);
                self.offset += available;
                return Ok(available);
            }
            self.payload_reads.fetch_add(1, Ordering::AcqRel);
            Err(std::io::Error::new(
                ErrorKind::WouldBlock,
                "the relay read a payload it should have refused",
            ))
        }
    }

    #[test]
    fn heic_relay_rejects_a_payload_above_the_byte_cap_without_reading_it() {
        // 8192 by 8192 RGBA is 256 MiB: above the 128 MiB ceiling without
        // overflowing, so only the byte ceiling can refuse it.
        assert!(expected_frame_bytes(8_192, 8_192).is_none());
        let payload_reads = Arc::new(AtomicUsize::new(0));
        let mut reader = PayloadProbe {
            prefix: relay_header_bytes(HEIC_FRAME_TAG, 8_192, 8_192, 256 * 1024 * 1024),
            offset: 0,
            payload_reads: Arc::clone(&payload_reads),
        };
        let shared = Arc::new(HeicShared::default());
        let (signal, _receiver) = watch::channel(0);
        let stopping = Arc::new(AtomicBool::new(false));

        run_heic_reader(&mut reader, &shared, &signal, &stopping);

        assert!(shared.failed.load(Ordering::Acquire));
        assert_eq!(
            payload_reads.load(Ordering::Acquire),
            0,
            "an over-cap payload must be refused before it is read or reserved"
        );
    }

    #[test]
    fn heic_relay_rejects_an_over_long_axis_before_allocating() {
        // Within the byte ceiling but past the decoder's per-axis limit.
        let width = MAX_HEIC_FRAME_DIMENSION + 1;
        let height = 1;
        let mut reader =
            std::io::Cursor::new(relay_header_bytes(HEIC_FRAME_TAG, width, height, width * 4));
        let shared = Arc::new(HeicShared::default());
        let (signal, _receiver) = watch::channel(0);
        let stopping = Arc::new(AtomicBool::new(false));

        run_heic_reader(&mut reader, &shared, &signal, &stopping);

        assert!(shared.failed.load(Ordering::Acquire));
        assert_eq!(reader.position(), (1 + HEIC_HEADER_BYTES) as u64);
        assert!(expected_frame_bytes(MAX_HEIC_FRAME_DIMENSION, 1).is_some());
        assert!(expected_frame_bytes(MAX_HEIC_FRAME_DIMENSION + 1, 1).is_none());
    }

    #[test]
    fn heic_relay_rejects_an_unknown_tag() {
        relay_fails_without_a_frame(vec![b'X']);
    }

    #[test]
    fn heic_relay_rejects_a_short_header() {
        let mut bytes = vec![HEIC_FRAME_TAG];
        bytes.extend_from_slice(&[1, 2, 3]);
        relay_fails_without_a_frame(bytes);
    }

    #[test]
    fn heic_relay_rejects_a_short_payload() {
        let mut bytes = relay_header_bytes(HEIC_FRAME_TAG, 1, 1, 4);
        bytes.extend_from_slice(&[1, 2]);
        relay_fails_without_a_frame(bytes);
    }

    #[test]
    fn heic_relay_rejects_a_length_mismatch() {
        // Two RGBA pixels are eight bytes, not four.
        relay_fails_without_a_frame(relay_header_bytes(HEIC_FRAME_TAG, 2, 1, 4));
        // A shorter payload than the header's exact length is also a failure.
        let mut bytes = relay_header_bytes(HEIC_FRAME_TAG, 1, 1, 8);
        bytes.extend_from_slice(&[1, 2, 3, 255]);
        relay_fails_without_a_frame(bytes);
    }

    #[test]
    fn heic_relay_rejects_zero_dimensions() {
        relay_fails_without_a_frame(relay_header_bytes(HEIC_FRAME_TAG, 0, 0, 0));
        relay_fails_without_a_frame(relay_header_bytes(HEIC_FRAME_TAG, 1, 0, 0));
        relay_fails_without_a_frame(relay_header_bytes(HEIC_FRAME_TAG, 0, 1, 0));
    }

    #[test]
    fn heic_relay_rejects_a_frame_the_decoder_would_have_refused() {
        // The relay's ceilings mirror `dynamic_wallpaper::heic`'s limits.
        assert_eq!(expected_frame_bytes(3_840, 2_160), Some(3_840 * 2_160 * 4));
        assert!(expected_frame_bytes(0, 2_160).is_none());
        assert!(expected_frame_bytes(3_840, 0).is_none());
        assert!(expected_frame_bytes(16_385, 1).is_none());
        assert!(expected_frame_bytes(1, 16_385).is_none());
        assert!(expected_frame_bytes(u32::MAX, u32::MAX).is_none());
    }

    /// A reader that never returns more than one byte per call, so framing
    /// must survive arbitrary fragmentation. A pipe cannot guarantee this:
    /// write boundaries are not read boundaries.
    struct OneByteReader {
        bytes: Vec<u8>,
        offset: usize,
    }

    impl Read for OneByteReader {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            if self.offset >= self.bytes.len() || buffer.is_empty() {
                return Ok(0);
            }
            buffer[0] = self.bytes[self.offset];
            self.offset += 1;
            Ok(1)
        }
    }

    #[test]
    fn heic_relay_assembles_a_frame_from_single_byte_reads() {
        let (reader, mut writer) = std::io::pipe().expect("relay pipe");
        let shared = Arc::new(HeicShared::default());
        let (signal, _receiver) = watch::channel(0);
        let stopping = Arc::new(AtomicBool::new(false));
        let relay = {
            let shared = Arc::clone(&shared);
            let stopping = Arc::clone(&stopping);
            thread::spawn(move || run_heic_reader(reader, &shared, &signal, &stopping))
        };

        // Hold the pipe open until the frame is adopted so the terminal EOF
        // cannot overwrite the pending frame.
        writer
            .write_all(&relay_frame_bytes(2, 1, &[1, 2, 3, 255, 4, 5, 6, 255]))
            .expect("relay frame");
        let frame = wait_for_heic_frame(&shared, Duration::from_secs(5));
        assert_eq!((frame.width, frame.height), (2, 1));
        assert_eq!(frame.pixels.as_ref(), &[1, 2, 3, 255, 4, 5, 6, 255]);

        stopping.store(true, Ordering::Release);
        drop(writer);
        relay.join().unwrap();
        assert!(!shared.failed.load(Ordering::Acquire));

        // The same stream delivered one byte per read must frame identically.
        let shared = Arc::new(HeicShared::default());
        let (signal, _receiver) = watch::channel(0);
        let stopping = Arc::new(AtomicBool::new(false));
        let bytes = relay_frame_bytes(2, 1, &[1, 2, 3, 255, 4, 5, 6, 255]);
        let mut reader = OneByteReader { bytes, offset: 0 };
        run_heic_reader(&mut reader, &shared, &signal, &stopping);
        assert_eq!(
            shared.sequence.load(Ordering::Acquire),
            2,
            "the fragmented frame must be adopted before the EOF failure"
        );
        assert!(shared.failed.load(Ordering::Acquire));
    }

    #[test]
    fn heic_relay_treats_eof_after_a_frame_as_a_terminal_failure() {
        let (reader, mut writer) = std::io::pipe().expect("relay pipe");
        let shared = Arc::new(HeicShared::default());
        let (signal, _receiver) = watch::channel(0);
        let stopping = Arc::new(AtomicBool::new(false));
        let relay = {
            let shared = Arc::clone(&shared);
            let stopping = Arc::clone(&stopping);
            thread::spawn(move || run_heic_reader(reader, &shared, &signal, &stopping))
        };

        writer
            .write_all(&relay_frame_bytes(2, 1, &[1, 2, 3, 255, 4, 5, 6, 255]))
            .expect("relay frame");
        let frame = wait_for_heic_frame(&shared, Duration::from_secs(5));
        assert_eq!((frame.width, frame.height), (2, 1));

        // The worker exited after a valid frame: the consumer has already
        // adopted it, so the terminal failure must not discard it.
        drop(writer);
        relay.join().unwrap();
        assert!(shared.failed.load(Ordering::Acquire));
        assert!(matches!(lock(&shared.pending).take(), Some(Update::Failed)));
    }

    /// A reader that delivers one header and then, once the payload read
    /// starts, records cancellation and reports end of stream.
    struct CancellingReader {
        header: Vec<u8>,
        offset: usize,
        stopping: Arc<AtomicBool>,
    }

    impl Read for CancellingReader {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            if self.offset < self.header.len() {
                let available = (self.header.len() - self.offset).min(buffer.len());
                buffer[..available]
                    .copy_from_slice(&self.header[self.offset..self.offset + available]);
                self.offset += available;
                return Ok(available);
            }
            // Cancellation lands mid-payload: the stream is abandoned and no
            // terminal failure may be published. The production reader also
            // checks cancellation before publishing a frame it already read.
            self.stopping.store(true, Ordering::Release);
            Ok(0)
        }
    }

    /// A reader that delivers a complete frame and records cancellation while
    /// returning the final payload bytes.
    struct CancellingPayloadReader {
        bytes: Vec<u8>,
        offset: usize,
        stopping: Arc<AtomicBool>,
    }

    impl Read for CancellingPayloadReader {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            if self.offset >= self.bytes.len() || buffer.is_empty() {
                return Ok(0);
            }
            let available = (self.bytes.len() - self.offset).min(buffer.len());
            let completes_frame = self.offset + available == self.bytes.len();
            buffer[..available].copy_from_slice(&self.bytes[self.offset..self.offset + available]);
            self.offset += available;
            if completes_frame {
                self.stopping.store(true, Ordering::Release);
            }
            Ok(available)
        }
    }

    #[test]
    fn heic_relay_cancellation_wins_over_a_read_frame() {
        // The payload is delivered completely, but cancellation lands before
        // the frame can be published: a consumer that is going away must not
        // receive it.
        let shared = Arc::new(HeicShared::default());
        let (signal, _receiver) = watch::channel(0);
        let stopping = Arc::new(AtomicBool::new(false));
        let mut reader = CancellingPayloadReader {
            bytes: relay_frame_bytes(2, 1, &[1, 2, 3, 255, 4, 5, 6, 255]),
            offset: 0,
            stopping: Arc::clone(&stopping),
        };

        run_heic_reader(&mut reader, &shared, &signal, &stopping);

        assert!(stopping.load(Ordering::Acquire));
        assert_eq!(shared.sequence.load(Ordering::Acquire), 0);
        assert!(lock(&shared.pending).is_none());
        assert!(!shared.failed.load(Ordering::Acquire));
    }

    #[test]
    fn heic_relay_cancellation_mid_payload_is_not_a_failure() {
        let shared = Arc::new(HeicShared::default());
        let (signal, _receiver) = watch::channel(0);
        let stopping = Arc::new(AtomicBool::new(false));
        let header = relay_header_bytes(HEIC_FRAME_TAG, 2, 1, 8);
        let mut reader = CancellingReader {
            header,
            offset: 0,
            stopping: Arc::clone(&stopping),
        };

        run_heic_reader(&mut reader, &shared, &signal, &stopping);

        assert!(stopping.load(Ordering::Acquire));
        assert!(!shared.failed.load(Ordering::Acquire));
        assert!(lock(&shared.pending).is_none());
    }

    #[test]
    fn heic_worker_command_forwards_file_appearance_and_motion() {
        let command = worker_command(
            Path::new("/usr/bin/genkan"),
            Path::new("/home/alice/wallpaper.heic"),
            AppearancePreference::Dark,
            true,
        );
        assert_eq!(command.get_program().to_string_lossy(), "/usr/bin/genkan");
        let arguments = command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            arguments,
            [
                "heic-worker",
                "--file",
                "/home/alice/wallpaper.heic",
                "--appearance",
                "dark",
                "--reduce-motion"
            ]
        );
    }

    #[test]
    fn heic_relay_adopts_a_framed_worker_frame() {
        let (reader, mut writer) = std::io::pipe().expect("relay pipe");
        let shared = Arc::new(HeicShared::default());
        let (signal, _receiver) = watch::channel(0);
        let stopping = Arc::new(AtomicBool::new(false));
        let relay = {
            let shared = Arc::clone(&shared);
            let stopping = Arc::clone(&stopping);
            thread::spawn(move || run_heic_reader(reader, &shared, &signal, &stopping))
        };
        writer
            .write_all(&relay_frame_bytes(2, 1, &[1, 2, 3, 255, 4, 5, 6, 255]))
            .expect("write frame");

        let frame = wait_for_heic_frame(&shared, Duration::from_secs(5));
        assert_eq!((frame.width, frame.height), (2, 1));
        assert_eq!(frame.pixels.as_ref(), &[1, 2, 3, 255, 4, 5, 6, 255]);

        stopping.store(true, Ordering::Release);
        drop(writer);
        relay.join().unwrap();
        assert!(!shared.failed.load(Ordering::Acquire));
    }

    #[test]
    fn heic_relay_reports_a_worker_failure_tag() {
        let shared = Arc::new(HeicShared::default());
        let (signal, _receiver) = watch::channel(0);
        let stopping = Arc::new(AtomicBool::new(false));
        run_heic_reader(
            std::io::Cursor::new(vec![HEIC_FAILED_TAG]),
            &shared,
            &signal,
            &stopping,
        );
        assert!(shared.failed.load(Ordering::Acquire));
        assert!(matches!(lock(&shared.pending).take(), Some(Update::Failed)));
    }

    #[test]
    fn heic_relay_treats_a_truncated_worker_as_a_decorative_failure() {
        let shared = Arc::new(HeicShared::default());
        let (signal, _receiver) = watch::channel(0);
        let stopping = Arc::new(AtomicBool::new(false));
        // A frame header that promises four pixels followed by an abrupt EOF,
        // exactly what an allocation abort after the header looks like.
        let mut bytes = vec![HEIC_FRAME_TAG];
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&4u32.to_le_bytes());
        bytes.extend_from_slice(&[1, 2]);

        run_heic_reader(std::io::Cursor::new(bytes), &shared, &signal, &stopping);
        assert!(shared.failed.load(Ordering::Acquire));
        assert!(matches!(lock(&shared.pending).take(), Some(Update::Failed)));
    }

    #[test]
    fn heic_relay_rejects_an_oversized_frame_without_allocating() {
        let shared = Arc::new(HeicShared::default());
        let (signal, _receiver) = watch::channel(0);
        let stopping = Arc::new(AtomicBool::new(false));
        let mut bytes = vec![HEIC_FRAME_TAG];
        bytes.extend_from_slice(&u32::MAX.to_le_bytes());
        bytes.extend_from_slice(&u32::MAX.to_le_bytes());
        bytes.extend_from_slice(&u32::MAX.to_le_bytes());

        run_heic_reader(std::io::Cursor::new(bytes), &shared, &signal, &stopping);
        assert!(shared.failed.load(Ordering::Acquire));
    }

    #[test]
    fn process_sink_emits_a_length_checked_frame() {
        let mut output = Vec::new();
        let mut sink = ProcessSink {
            writer: &mut output,
        };
        assert!(sink.publish(Some(&heic_frame(7))));
        assert_eq!(output[0], HEIC_FRAME_TAG);

        let shared = Arc::new(HeicShared::default());
        let (signal, _receiver) = watch::channel(0);
        let (reader, mut writer) = std::io::pipe().expect("relay pipe");
        let stopping = Arc::new(AtomicBool::new(false));
        let relay = {
            let shared = Arc::clone(&shared);
            let stopping = Arc::clone(&stopping);
            thread::spawn(move || run_heic_reader(reader, &shared, &signal, &stopping))
        };
        writer.write_all(&output).expect("relay frame");
        let frame = wait_for_heic_frame(&shared, Duration::from_secs(5));
        assert_eq!(frame.pixels[0], 7);
        stopping.store(true, Ordering::Release);
        drop(writer);
        relay.join().unwrap();
    }

    #[test]
    fn dynamic_heic_selects_the_scheduled_variant() {
        let path = heic_fixture();
        let time = FakeTime::new(clock_snapshot(13, 0, 0), Duration::ZERO);
        let time_source = time.time_source();
        let shared = Arc::new(HeicShared::default());
        let (signal, _receiver) = watch::channel(0);
        let stopping = Arc::new(AtomicBool::new(false));
        let worker = {
            let shared = Arc::clone(&shared);
            let stopping = Arc::clone(&stopping);
            thread::spawn(move || {
                run_heic(
                    &path,
                    AppearancePreference::Automatic,
                    true,
                    time_source.as_ref(),
                    &shared,
                    &signal,
                    &stopping,
                )
            })
        };
        let frame = wait_for_heic_frame(&shared, Duration::from_secs(10));
        stopping.store(true, Ordering::Release);
        worker.join().unwrap();

        // At 13:00, t = 0.5417 selects the h24 point at 0.5, image 2 (blue).
        assert!(
            frame.pixels[2] > 200 && frame.pixels[0] < 40 && frame.pixels[1] < 40,
            "unexpected scheduled frame {:?}",
            &frame.pixels[..4]
        );
    }

    #[test]
    fn dynamic_heic_transitions_at_an_ordinary_boundary() {
        let path = heic_fixture();
        let time = FakeTime::new(clock_snapshot(0, 0, 0), Duration::ZERO);
        let time_source = time.time_source();
        let shared = Arc::new(HeicShared::default());
        let (signal, _receiver) = watch::channel(0);
        let stopping = Arc::new(AtomicBool::new(false));
        let worker = {
            let shared = Arc::clone(&shared);
            let stopping = Arc::clone(&stopping);
            thread::spawn(move || {
                run_heic(
                    &path,
                    AppearancePreference::Automatic,
                    true,
                    time_source.as_ref(),
                    &shared,
                    &signal,
                    &stopping,
                )
            })
        };

        // 00:00 selects the h24 point at 0.0, image 0 (red).
        let first = wait_for_heic_frame_labeled(&shared, Duration::from_secs(10), "initial");
        assert!(
            first.pixels[0] > 200 && first.pixels[1] < 40 && first.pixels[2] < 40,
            "unexpected initial frame {:?}",
            &first.pixels[..4]
        );

        // Civil and monotonic advance together six hours, which is an ordinary
        // boundary rather than a clock discontinuity.
        time.set(clock_snapshot(6, 0, 0), Duration::from_secs(6 * 3_600));
        // 06:00 crosses the boundary to the point at 0.25, image 1 (green).
        let second = wait_for_heic_frame_labeled(&shared, Duration::from_secs(10), "boundary");
        assert!(
            second.pixels[1] > 200 && second.pixels[0] < 40 && second.pixels[2] < 40,
            "unexpected boundary frame {:?}",
            &second.pixels[..4]
        );

        stopping.store(true, Ordering::Release);
        worker.join().unwrap();
    }

    #[test]
    fn dynamic_heic_dissolves_at_an_ordinary_boundary() {
        let path = heic_fixture();
        let time = FakeTime::new(clock_snapshot(0, 0, 0), Duration::ZERO);
        let time_source = time.time_source();
        let shared = Arc::new(HeicShared::default());
        let (signal, _receiver) = watch::channel(0);
        let stopping = Arc::new(AtomicBool::new(false));
        let worker = {
            let shared = Arc::clone(&shared);
            let stopping = Arc::clone(&stopping);
            thread::spawn(move || {
                run_heic(
                    &path,
                    AppearancePreference::Automatic,
                    false,
                    time_source.as_ref(),
                    &shared,
                    &signal,
                    &stopping,
                )
            })
        };

        let first = wait_for_heic_frame_labeled(&shared, Duration::from_secs(10), "initial");
        assert!(
            first.pixels[0] > 200 && first.pixels[1] < 40 && first.pixels[2] < 40,
            "unexpected initial frame {:?}",
            &first.pixels[..4]
        );

        // One second past the boundary with a coherent monotonic sample: the
        // two-second dissolve is halfway between red and green.
        time.set(clock_snapshot(6, 0, 1), Duration::from_secs(6 * 3_600 + 1));
        let blended = wait_for_heic_frame_labeled(&shared, Duration::from_secs(10), "dissolve");
        assert!(
            (60..=200).contains(&blended.pixels[0])
                && (60..=200).contains(&blended.pixels[1])
                && blended.pixels[2] < 40,
            "expected a red/green dissolve blend, got {:?}",
            &blended.pixels[..4]
        );

        stopping.store(true, Ordering::Release);
        worker.join().unwrap();
    }

    #[test]
    fn refused_frame_allocation_retains_the_previous_frame() {
        let shared = HeicShared::default();
        let (signal, _receiver) = watch::channel(0);
        assert!(emit_heic_frame(
            &shared,
            &signal,
            1,
            1,
            Some(Bytes::from_static(&[1, 2, 3, 4]))
        ));
        let sequence = shared.sequence.load(Ordering::Acquire);

        // A refused handoff allocation must not replace the pending frame.
        assert!(!emit_heic_frame(&shared, &signal, 1, 1, None));
        let pending = lock(&shared.pending);
        let Some(Update::Frame(frame)) = pending.as_ref() else {
            panic!("valid pending frame must remain");
        };
        assert_eq!(frame.pixels[0], 1);
        assert_eq!(shared.sequence.load(Ordering::Acquire), sequence);
    }

    #[test]
    fn stale_decode_after_reselection_is_ignored() {
        let metadata = time_metadata(vec![
            TimePoint {
                image: ImageReference::from_position(0),
                time: NormalizedTime::new(0.0).unwrap(),
            },
            TimePoint {
                image: ImageReference::from_position(1),
                time: NormalizedTime::new(60.0 / 86_400.0).unwrap(),
            },
            TimePoint {
                image: ImageReference::from_position(2),
                time: NormalizedTime::new(120.0 / 86_400.0).unwrap(),
            },
        ]);
        let mut playback = HeicPlayback::new(
            &metadata,
            ImageReference::from_position(0),
            AppearancePreference::Automatic,
            clock_snapshot(0, 0, 0),
            Duration::ZERO,
            false,
        );
        let initial = playback.take_decode_request().unwrap();
        playback.complete_decode(initial, Ok(heic_frame(10)), Duration::ZERO);

        playback.synchronize(clock_snapshot(0, 1, 0), Duration::from_secs(60));
        let stale = playback.take_decode_request().unwrap();
        assert_eq!(stale.image, ImageReference::from_position(1));

        // The helper's own synchronization crosses a second boundary and
        // reselects image 2 before the stale result is completed, so it must be
        // rejected without overwriting the displayed frame.
        let mut diagnostics = HeicDiagnostics::default();
        assert!(!finish_heic_decode(
            &mut playback,
            stale,
            Ok(heic_frame(200)),
            clock_snapshot(0, 2, 0),
            Duration::from_secs(120),
            false,
            &mut diagnostics,
        ));
        assert_eq!(playback.selected(), ImageReference::from_position(2));
        assert_eq!(playback.frame().unwrap().pixels[0], 10);
    }

    #[test]
    fn decode_failure_retains_the_last_frame() {
        let metadata = time_metadata(vec![
            TimePoint {
                image: ImageReference::from_position(0),
                time: NormalizedTime::new(0.0).unwrap(),
            },
            TimePoint {
                image: ImageReference::from_position(1),
                time: NormalizedTime::new(60.0 / 86_400.0).unwrap(),
            },
        ]);
        let mut playback = HeicPlayback::new(
            &metadata,
            ImageReference::from_position(0),
            AppearancePreference::Automatic,
            clock_snapshot(0, 0, 0),
            Duration::ZERO,
            false,
        );
        let initial = playback.take_decode_request().unwrap();
        playback.complete_decode(initial, Ok(heic_frame(10)), Duration::ZERO);

        playback.synchronize(clock_snapshot(0, 1, 0), Duration::from_secs(60));
        let request = playback.take_decode_request().unwrap();
        let mut diagnostics = HeicDiagnostics::default();
        assert!(!finish_heic_decode(
            &mut playback,
            request,
            Err(()),
            clock_snapshot(0, 1, 0),
            Duration::from_secs(60),
            false,
            &mut diagnostics,
        ));
        assert_eq!(playback.frame().unwrap().pixels[0], 10);
        assert!(diagnostics.decode_failure_reported);
    }

    #[test]
    fn late_decode_after_the_dissolve_window_switches_immediately() {
        let metadata = time_metadata(vec![
            TimePoint {
                image: ImageReference::from_position(0),
                time: NormalizedTime::new(0.0).unwrap(),
            },
            TimePoint {
                image: ImageReference::from_position(1),
                time: NormalizedTime::new(60.0 / 86_400.0).unwrap(),
            },
        ]);
        let mut playback = HeicPlayback::new(
            &metadata,
            ImageReference::from_position(0),
            AppearancePreference::Automatic,
            clock_snapshot(0, 0, 0),
            Duration::ZERO,
            false,
        );
        let initial = playback.take_decode_request().unwrap();
        playback.complete_decode(initial, Ok(heic_frame(10)), Duration::ZERO);
        playback.synchronize(clock_snapshot(0, 1, 0), Duration::from_secs(60));
        let request = playback.take_decode_request().unwrap();

        // The decode finished five seconds after its boundary, past the
        // two-second dissolve window, so it must switch immediately rather than
        // starting an expired dissolve. The civil clock advances coherently
        // with the monotonic sample so this is not a clock discontinuity.
        let mut diagnostics = HeicDiagnostics::default();
        assert!(finish_heic_decode(
            &mut playback,
            request,
            Ok(heic_frame(200)),
            clock_snapshot(0, 1, 5),
            Duration::from_secs(65),
            false,
            &mut diagnostics,
        ));
        assert!(!playback.is_transitioning());
        assert_eq!(playback.frame().unwrap().pixels[0], 200);
    }

    #[test]
    fn heic_playback_never_selects_a_solar_schedule() {
        let solar = Schedule::new(
            vec![SolarPoint {
                image: ImageReference::from_position(2),
                position: SolarPosition::new(0.0, 0.0).unwrap(),
            }],
            Some(Appearance {
                light: ImageReference::from_position(0),
                dark: ImageReference::from_position(1),
            }),
        )
        .unwrap();
        let mut metadata = Metadata::default();
        metadata
            .insert(AppleProperty::Solar, PropertyValue::Solar(solar))
            .unwrap();
        let playback = HeicPlayback::new(
            &metadata,
            ImageReference::from_position(9),
            AppearancePreference::Automatic,
            clock_snapshot(12, 0, 0),
            Duration::ZERO,
            false,
        );
        // Solar is disabled for login/lock, so the fallback appearance's light
        // image is selected instead of the solar point's image 2.
        assert_eq!(playback.selected(), ImageReference::from_position(0));
    }

    #[test]
    fn dynamic_heic_decode_failure_after_a_frame_retains_it() {
        let mut state = State::start(heic_settings(Some(heic_fixture()), true, true));
        assert_eq!(wait_for_heic(&mut state), Refresh::Frame);
        let displayed = state.frame.as_ref().unwrap().id();
        let (signal, _receiver) = watch::channel(0);
        {
            let player = state.heic.as_ref().expect("heic player");
            fail_heic(&player.shared, &signal);
        }
        assert_eq!(wait_for_heic(&mut state), Refresh::Failed);
        assert!(state.decoder_is_stopped());
        assert_eq!(state.frame.as_ref().unwrap().id(), displayed);
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
            heic: None,
            poster: Some(poster.clone()),
            frame: Some(poster),
            allocation: None,
            allocation_pending: false,
            heic_adopted: false,
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
            heic: None,
            poster: Some(poster),
            frame: None,
            allocation: None,
            allocation_pending: false,
            heic_adopted: false,
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
            heic: None,
            poster: Some(poster),
            frame: Some(current),
            allocation: None,
            allocation_pending: false,
            heic_adopted: false,
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
            heic: None,
            poster: None,
            frame: Some(current),
            allocation: None,
            allocation_pending: true,
            heic_adopted: false,
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
