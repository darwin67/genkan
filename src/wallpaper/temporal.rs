//! All-or-nothing extraction of a validated 239.76-fps HEVC temporal hierarchy.
//! Unknown files keep ordinary decoding. Approval covers the complete original
//! demuxed stream, not h265parse's potentially repaired output. Playback checks
//! each consumed AU against that approval before admitting it to the parser.
use std::ffi::{c_int, c_void};
use std::fs::File;
use std::io::{Seek, SeekFrom};
use std::os::fd::AsRawFd;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gstreamer::{self as gst, prelude::*};
use gstreamer_app as gst_app;
use sha2::{Digest, Sha256};

const MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_SAMPLES: usize = 250_000;
const MAX_AU_BYTES: usize = 64 * 1024 * 1024;
const SCAN_BUDGET: Duration = Duration::from_secs(5);

pub(super) fn output_rate() -> gst::Fraction {
    gst::Fraction::new(2997, 100)
}

unsafe extern "C" {
    fn temporal_parser_new(data: *const u8, size: usize) -> *mut c_void;
    fn temporal_parser_free(parser: *mut c_void);
    fn temporal_parser_access_unit(
        parser: *mut c_void,
        data: *const u8,
        size: usize,
        index: u64,
    ) -> c_int;
}

// This owner stays on the inspecting worker. No native parser crosses threads.
struct Parser(NonNull<c_void>);

impl Parser {
    fn new(caps: &gst::CapsRef) -> Option<Self> {
        let structure = caps.structure(0)?;
        if caps.size() != 1
            || structure.name() != "video/x-h265"
            || structure.get::<&str>("stream-format").ok()? != "hvc1"
            || structure.get::<&str>("alignment").ok()? != "au"
        {
            return None;
        }
        let config = structure.get::<gst::Buffer>("codec_data").ok()?;
        let data = config.map_readable().ok()?;
        // SAFETY: the native constructor reads this live slice and copies its
        // parameter sets. Its non-null result has one owner and one destructor.
        NonNull::new(unsafe { temporal_parser_new(data.as_ptr(), data.len()) }).map(Self)
    }

    fn temporal_id(&mut self, data: &[u8], index: u64) -> Option<u8> {
        // SAFETY: exclusive ownership, valid input slice, no retained pointer.
        let tid = unsafe {
            temporal_parser_access_unit(self.0.as_ptr(), data.as_ptr(), data.len(), index)
        };
        (0..=4).contains(&tid).then_some(tid as u8)
    }
}

impl Drop for Parser {
    fn drop(&mut self) {
        // SAFETY: this is the unique owner returned by temporal_parser_new.
        unsafe { temporal_parser_free(self.0.as_ptr()) };
    }
}

struct Entry {
    digest: [u8; 32],
    pts: gst::ClockTime,
    dts: Option<gst::ClockTime>,
    duration: gst::ClockTime,
    flags: gst::BufferFlags,
    keep: bool,
}

impl Entry {
    fn matches(&self, buffer: &gst::BufferRef) -> bool {
        buffer.pts() == Some(self.pts)
            && buffer.dts() == self.dts
            && buffer.duration() == Some(self.duration)
            && stream_flags(buffer) == self.flags
            && buffer.size() <= MAX_AU_BYTES
            && buffer
                .map_readable()
                .is_ok_and(|bytes| Sha256::digest(bytes.as_slice()).as_slice() == self.digest)
    }
}

fn stream_flags(buffer: &gst::BufferRef) -> gst::BufferFlags {
    // Neither discontinuity at a flushing seek nor memory-block replacement
    // changes the encoded AU. TAG_MEMORY can change when a buffer is copied.
    buffer.flags() & !(gst::BufferFlags::DISCONT | gst::BufferFlags::TAG_MEMORY)
}

pub(super) struct Plan {
    caps: gst::Caps,
    segment: gst::FormattedSegment<gst::ClockTime>,
    entries: Vec<Entry>,
}

impl Plan {
    pub(super) fn attach(self: &Arc<Self>, pad: &gst::Pad, faulted: Arc<AtomicBool>) {
        let plan = Arc::clone(self);
        let cursor = Mutex::new(0);
        pad.add_probe(
            gst::PadProbeType::BUFFER
                | gst::PadProbeType::BUFFER_LIST
                | gst::PadProbeType::EVENT_DOWNSTREAM
                | gst::PadProbeType::EVENT_FLUSH,
            move |pad, probe| {
                if faulted.load(Ordering::Acquire) {
                    return if matches!(probe.data, Some(gst::PadProbeData::Event(_))) {
                        gst::PadProbeReturn::Ok
                    } else {
                        gst::PadProbeReturn::Drop
                    };
                }
                let mut cursor = super::lock(&cursor);
                let valid = match &probe.data {
                    Some(gst::PadProbeData::Buffer(buffer)) => {
                        // The initial sticky caps/segment may predate this
                        // probe. Check the metadata actually accompanying data.
                        let metadata_matches = pad.current_caps().as_deref()
                            == Some(plan.caps.as_ref())
                            && pad
                                .sticky_event::<gst::event::Segment>(0)
                                .is_some_and(|event| {
                                    event
                                        .segment()
                                        .downcast_ref::<gst::ClockTime>()
                                        .is_some_and(|segment| {
                                            same_timeline(segment, &plan.segment)
                                        })
                                });
                        if let Some(entry) = plan
                            .entries
                            .get(*cursor)
                            .filter(|e| metadata_matches && e.matches(buffer))
                        {
                            *cursor += 1;
                            return if entry.keep {
                                gst::PadProbeReturn::Ok
                            } else {
                                gst::PadProbeReturn::Drop
                            };
                        }
                        false
                    }
                    Some(gst::PadProbeData::BufferList(_)) => false,
                    Some(gst::PadProbeData::Event(event)) => match event.view() {
                        gst::EventView::Caps(event) => event.caps() == plan.caps.as_ref(),
                        gst::EventView::Segment(event) => {
                            *cursor = 0;
                            event
                                .segment()
                                .downcast_ref::<gst::ClockTime>()
                                .is_some_and(|segment| same_timeline(segment, &plan.segment))
                        }
                        gst::EventView::FlushStop(_) => {
                            *cursor = 0;
                            true
                        }
                        gst::EventView::Eos(_) => *cursor == plan.entries.len(),
                        _ => true,
                    },
                    _ => true,
                };
                if valid {
                    gst::PadProbeReturn::Ok
                } else {
                    // Do not re-enable high layers in a depleted decoder. The
                    // worker stops this attempt and restarts full decoding.
                    faulted.store(true, Ordering::Release);
                    gst::PadProbeReturn::Drop
                }
            },
        );
    }
}

fn same_timeline(
    a: &gst::FormattedSegment<gst::ClockTime>,
    b: &gst::FormattedSegment<gst::ClockTime>,
) -> bool {
    a.rate() == b.rate()
        && a.applied_rate() == b.applied_rate()
        && a.start() == b.start()
        && a.stop() == b.stop()
        && a.time() == b.time()
        && a.offset() == b.offset()
}

/// A bounded compressed-only scan; no decoder or GPU context is created.
/// Cancellation, unsupported input and exhausted budgets all decline extraction.
pub(super) fn inspect(file: &mut File, stopping: &AtomicBool) -> Option<Arc<Plan>> {
    if stopping.load(Ordering::Acquire) || file.metadata().ok()?.len() > MAX_BYTES {
        return None;
    }
    file.seek(SeekFrom::Start(0)).ok()?;
    let pipeline = gst::Pipeline::new();
    let source = super::element("fdsrc").ok()?;
    source.set_property("fd", file.as_raw_fd());
    let demux = super::element("qtdemux").ok()?;
    let sink = gst_app::AppSink::builder()
        .sync(false)
        .max_buffers(1)
        .enable_last_sample(false)
        .build();
    pipeline
        .add_many([&source, &demux, sink.upcast_ref()])
        .ok()?;
    source.link(&demux).ok()?;
    let input = sink.static_pad("sink")?;
    demux.connect_pad_added(move |_, output| {
        let hevc = output.current_caps().is_some_and(|caps| {
            caps.structure(0)
                .is_some_and(|structure| structure.name() == "video/x-h265")
        });
        if hevc && !input.is_linked() {
            let _ = output.link(&input);
        }
    });
    let bus = pipeline.bus()?;
    let started = Instant::now();
    let result = (|| {
        pipeline.set_state(gst::State::Playing).ok()?;
        let mut inspection = None;
        loop {
            if stopping.load(Ordering::Acquire)
                || started.elapsed() >= SCAN_BUDGET
                || bus.pop_filtered(&[gst::MessageType::Error]).is_some()
            {
                return None;
            }
            if let Some(sample) = sink.try_pull_sample(gst::ClockTime::from_mseconds(20)) {
                if inspection.is_none() {
                    inspection = Some(Inspection::new(&sample)?);
                }
                inspection.as_mut()?.push(&sample)?;
            } else if sink.is_eos() {
                return inspection?.finish().map(Arc::new);
            }
        }
    })();
    // Stop the source's streaming task before the shared file is rewound.
    let _ = pipeline.set_state(gst::State::Null);
    result
}

struct Inspection {
    parser: Parser,
    plan: Plan,
    indices: Vec<u64>,
    origin: u64,
    bytes: u64,
}

impl Inspection {
    fn new(sample: &gst::Sample) -> Option<Self> {
        let caps = sample.caps()?;
        let segment = sample.segment()?.downcast_ref::<gst::ClockTime>()?;
        let origin = sample.buffer()?.pts()?;
        if segment.rate() != 1.0
            || segment.applied_rate() != 1.0
            || segment.start() != Some(origin)
            || segment.time() != Some(gst::ClockTime::ZERO)
            || segment.offset() != Some(gst::ClockTime::ZERO)
        {
            return None;
        }
        Some(Self {
            parser: Parser::new(caps)?,
            plan: Plan {
                caps: caps.to_owned(),
                segment: segment.clone(),
                entries: Vec::new(),
            },
            indices: Vec::new(),
            origin: origin.nseconds(),
            bytes: 0,
        })
    }

    fn push(&mut self, sample: &gst::Sample) -> Option<()> {
        if sample.caps()? != self.plan.caps.as_ref()
            || !same_timeline(
                sample.segment()?.downcast_ref::<gst::ClockTime>()?,
                &self.plan.segment,
            )
            || self.indices.len() >= MAX_SAMPLES
        {
            return None;
        }
        let buffer = sample.buffer()?;
        if buffer.size() > MAX_AU_BYTES
            || buffer.flags().intersects(
                gst::BufferFlags::CORRUPTED | gst::BufferFlags::DECODE_ONLY | gst::BufferFlags::GAP,
            )
        {
            return None;
        }
        let pts = buffer.pts()?;
        let duration = buffer.duration()?;
        let delta = pts.nseconds().checked_sub(self.origin)?;
        let index = ((u128::from(delta) * 5994 + 12_500_000_000) / 25_000_000_000) as u64;
        let expected = (u128::from(index) * 25_000_000_000 / 5994) as u64;
        if index >= MAX_SAMPLES as u64
            || delta.abs_diff(expected) > 2
            || duration.nseconds().abs_diff(4_170_837) > 2
        {
            return None;
        }
        let bytes = buffer.map_readable().ok()?;
        self.bytes = self.bytes.checked_add(bytes.len() as u64)?;
        if self.bytes > MAX_BYTES {
            return None;
        }
        let tid = self.parser.temporal_id(bytes.as_slice(), index)?;
        let keep = tid <= 1;
        if keep != index.is_multiple_of(8) {
            return None;
        }
        self.indices.push(index);
        self.plan.entries.push(Entry {
            digest: Sha256::digest(bytes.as_slice()).into(),
            pts,
            dts: buffer.dts(),
            duration,
            flags: stream_flags(buffer),
            keep,
        });
        Some(())
    }

    fn finish(mut self) -> Option<Plan> {
        // Decode order is deliberately not presentation order. Require every
        // source picture exactly once, including the terminal retained picture.
        self.indices.sort_unstable();
        if self.indices.len() < 17
            || self.indices.iter().enumerate().any(|(n, i)| *i != n as u64)
            || !(self.indices.len() - 1).is_multiple_of(8)
        {
            return None;
        }
        let last = self.plan.entries.iter().map(|e| e.pts).max()?;
        let stop = self.plan.segment.stop()?;
        let next = self.origin + ((self.indices.len() as u64 + 7) * 25_000_000_000 / 5994);
        if stop <= last || stop.nseconds() > next {
            return None;
        }
        Some(self.plan)
    }
}

#[cfg(test)]
#[path = "temporal_tests.rs"]
mod tests;
