use super::*;

// Parameter sets and slice headers only, without any encoded image payload.
// These exercise eligibility/admission, not decoder correctness or image quality.
// The 17-picture hierarchy is in decode order: 0,16,8,4,2,1,3,6,...
include!("temporal_headers.rs");

fn fixture_buffers() -> Vec<gst::Buffer> {
    HEADERS
        .iter()
        .enumerate()
        .map(|(decode_index, (index, header))| {
            let mut bytes = (header.len() as u32).to_be_bytes().to_vec();
            bytes.extend_from_slice(header);
            let mut buffer = gst::Buffer::from_mut_slice(bytes);
            let metadata = buffer.get_mut().unwrap();
            metadata.set_pts(gst::ClockTime::from_nseconds(
                (index + 4) * 25_000_000_000 / 5994,
            ));
            metadata.set_dts(gst::ClockTime::from_nseconds(
                decode_index as u64 * 25_000_000_000 / 5994,
            ));
            metadata.set_duration(gst::ClockTime::from_nseconds(4_170_837));
            if decode_index != 0 {
                metadata.set_flags(gst::BufferFlags::DELTA_UNIT);
            }
            buffer
        })
        .collect()
}

fn replace_rps(buffer: &mut gst::Buffer, rps: &str) {
    let bytes = buffer.map_readable().unwrap();
    let mut bits: Vec<u8> = bytes[4..]
        .iter()
        .flat_map(|b| (0..8).rev().map(move |bit| (b >> bit) & 1))
        .collect();
    drop(bytes);
    // This fixture's short_term_ref_pic_set_sps_flag and six-bit index.
    bits.splice(29..36, rps.bytes().map(|b| b - b'0'));
    bits.resize(bits.len().next_multiple_of(8), 0);
    let mut payload = (bits.len() as u32 / 8).to_be_bytes().to_vec();
    payload.extend(
        bits.chunks_exact(8)
            .map(|bits| bits.iter().fold(0, |byte, bit| (byte << 1) | bit)),
    );
    let metadata = buffer.make_mut();
    metadata.remove_all_memory();
    metadata.append_memory(gst::Memory::from_mut_slice(payload));
}

fn movie(buffers: &[gst::Buffer]) -> tempfile::NamedTempFile {
    gst::init().unwrap();
    let file = tempfile::NamedTempFile::new().unwrap();
    let pipeline = gst::parse::launch(
        "appsrc name=input format=time ! mux.video_0 qtmux name=mux trak-timescale=5994 movie-timescale=5994 ! filesink name=output",
    )
    .unwrap()
    .downcast::<gst::Pipeline>()
    .unwrap();
    pipeline
        .by_name("output")
        .unwrap()
        .set_property("location", file.path().to_str().unwrap());
    let source = pipeline
        .by_name("input")
        .unwrap()
        .downcast::<gst_app::AppSrc>()
        .unwrap();
    source.set_caps(Some(
        &gst::Caps::builder("video/x-h265")
            .field("stream-format", "hvc1")
            .field("alignment", "au")
            .field("width", 3840)
            .field("height", 2160)
            .field("framerate", gst::Fraction::new(5994, 25))
            .field("codec_data", gst::Buffer::from_slice(CONFIG))
            .build(),
    ));
    pipeline.set_state(gst::State::Playing).unwrap();
    for buffer in buffers {
        source.push_buffer(buffer.clone()).unwrap();
    }
    source.end_of_stream().unwrap();
    let result = pipeline.bus().unwrap().timed_pop_filtered(
        gst::ClockTime::from_seconds(5),
        &[gst::MessageType::Eos, gst::MessageType::Error],
    );
    pipeline.set_state(gst::State::Null).unwrap();
    assert!(
        matches!(
            result.as_ref().map(|m| m.view()),
            Some(gst::MessageView::Eos(_))
        ),
        "{result:?}"
    );
    file
}

fn demux(file: &mut File) -> Vec<gst::Sample> {
    file.rewind().unwrap();
    let pipeline =
        gst::parse::launch("fdsrc name=input ! qtdemux ! appsink name=output sync=false")
            .unwrap()
            .downcast::<gst::Pipeline>()
            .unwrap();
    pipeline
        .by_name("input")
        .unwrap()
        .set_property("fd", file.as_raw_fd());
    let sink = pipeline
        .by_name("output")
        .unwrap()
        .downcast::<gst_app::AppSink>()
        .unwrap();
    pipeline.set_state(gst::State::Playing).unwrap();
    let mut samples = Vec::new();
    while let Some(sample) = sink.try_pull_sample(gst::ClockTime::from_seconds(5)) {
        samples.push(sample);
    }
    let eos = sink.is_eos();
    pipeline.set_state(gst::State::Null).unwrap();
    assert!(eos);
    samples
}

#[test]
fn preflight_checks_the_complete_original_movie() {
    gst::init().unwrap();
    let original = fixture_buffers();
    let mut file = movie(&original);
    assert!(inspect(file.as_file_mut(), &AtomicBool::new(false)).is_some());
    assert!(inspect(file.as_file_mut(), &AtomicBool::new(true)).is_none());

    for case in [
        "missing_picture",
        "late_invalid_nal",
        "late_wrong_pts",
        "mixed_nals",
        "discarded_reference",
    ] {
        let mut buffers = original.clone();
        let last = buffers.last_mut().unwrap().make_mut();
        match case {
            "missing_picture" => {
                buffers.pop();
            }
            "late_invalid_nal" => {
                last.map_writable().unwrap()[4] |= 0x80;
            }
            "late_wrong_pts" => {
                last.set_pts(last.pts().unwrap() + gst::ClockTime::from_mseconds(1));
            }
            "mixed_nals" => {
                // A low-layer parameter set cannot disappear with a high-layer picture.
                let nal = &CONFIG[28..54];
                last.append_memory(gst::Memory::from_slice((nal.len() as u32).to_be_bytes()));
                last.append_memory(gst::Memory::from_slice(nal));
            }
            "discarded_reference" => {
                // T0 POC16 now declares used references POC12 (T2) and POC8.
                // Parsing and the POC/TID grid alone do not reject this input.
                buffers[1].make_mut().map_writable().unwrap()[7] ^= 0x02;
            }
            _ => unreachable!(),
        }
        let mut file = movie(&buffers);
        assert!(
            inspect(file.as_file_mut(), &AtomicBool::new(false)).is_none(),
            "{case}"
        );
    }
}

#[test]
fn preflight_tracks_picture_order_across_lsb_wraps() {
    gst::init().unwrap();
    let gop = fixture_buffers();
    let mut buffers = gop.clone();
    for group in 1..65 {
        for (buffer, (index, _)) in gop[1..].iter().zip(&HEADERS[1..]) {
            let index = index + group * 16;
            let mut buffer = buffer.copy();
            let metadata = buffer.get_mut().unwrap();
            metadata.set_pts(gst::ClockTime::from_nseconds(
                (index + 4) * 25_000_000_000 / 5994,
            ));
            metadata.set_dts(gst::ClockTime::from_nseconds(
                buffers.len() as u64 * 25_000_000_000 / 5994,
            ));
            {
                // This fixture's SPS declares ten POC bits, starting at bit19
                // in each non-IDR NAL. At POC1024 those bits wrap to zero.
                let mut bytes = metadata.map_writable().unwrap();
                bytes[6] = (bytes[6] & 0xe0) | ((index >> 5) & 31) as u8;
                bytes[7] = (bytes[7] & 0x07) | ((index & 31) << 3) as u8;
            }
            buffers.push(buffer);
        }
    }
    let mut file = movie(&buffers);
    assert!(inspect(file.as_file_mut(), &AtomicBool::new(false)).is_some());
    // T0 POC32 can use POC14 in the full decoder, but not after extraction.
    // Replace its SPS RPS index with an explicit set: POC16 unused but retained,
    // POC14 used. Preserving POC16 also keeps the following full-stream GOP valid.
    let mut dependent = buffers.clone();
    let mut buffer = dependent[17].copy();
    // sps_flag=0, predicted=0, negative_count=2, positive_count=0,
    // delta_minus1=15/used=0, delta_minus1=1/used=1 (unsigned Exp-Golomb).
    replace_rps(&mut buffer, "00011100001000000101");
    dependent[17] = buffer;
    let mut file = movie(&dependent);
    assert!(inspect(file.as_file_mut(), &AtomicBool::new(false)).is_none());
    // Keep the complete timestamp/TID grid, but change the final picture's POC.
    buffers
        .last_mut()
        .unwrap()
        .make_mut()
        .map_writable()
        .unwrap()[7] ^= 0x08;
    let mut file = movie(&buffers);
    assert!(inspect(file.as_file_mut(), &AtomicBool::new(false)).is_none());
}

#[test]
fn preflight_resets_picture_order_and_references_at_later_idr() {
    gst::init().unwrap();
    let templates = fixture_buffers();
    let mut buffers = Vec::new();
    for index in 0..17_u64 {
        let local = index % 8;
        let mut buffer = templates[usize::from(local != 0)].copy();
        if local != 0 {
            {
                let mut bytes = buffer.make_mut().map_writable().unwrap();
                bytes[4] = 4; // TSA_N
                bytes[5] = 5; // T4: never a reference or a retained picture
                bytes[6] &= 0xe0;
                bytes[7] = (bytes[7] & 7) | (local << 3) as u8;
            }
            // One used short-term reference, to this epoch's IDR.
            let code = format!("{local:b}");
            replace_rps(
                &mut buffer,
                &format!("000101{}{code}1", "0".repeat(code.len() - 1)),
            );
        }
        let metadata = buffer.make_mut();
        metadata.set_pts(gst::ClockTime::from_nseconds(
            (index + 4) * 25_000_000_000 / 5994,
        ));
        metadata.set_dts(gst::ClockTime::from_nseconds(index * 25_000_000_000 / 5994));
        buffers.push(buffer);
    }
    let mut file = movie(&buffers);
    assert!(inspect(file.as_file_mut(), &AtomicBool::new(false)).is_some());
    // Identical timestamps/TIDs, but POC1 after IDR8 claims POC2: an IDR must
    // reset the POC epoch, not disable subsequent POC/timestamp validation.
    buffers[9].make_mut().map_writable().unwrap()[7] ^= 0x18;
    let mut file = movie(&buffers);
    assert!(inspect(file.as_file_mut(), &AtomicBool::new(false)).is_none());
}

struct Playback {
    source: gst::Pad,
    sink: gst::Pad,
    received: Arc<Mutex<Vec<gst::Buffer>>>,
    faulted: Arc<AtomicBool>,
}

impl Playback {
    fn new(plan: &Arc<Plan>, first: &gst::Sample) -> Self {
        let received = Arc::new(Mutex::new(Vec::new()));
        let output = Arc::clone(&received);
        let source = gst::Pad::builder(gst::PadDirection::Src).build();
        let sink = gst::Pad::builder(gst::PadDirection::Sink)
            .chain_function(move |_, _, buffer| {
                output.lock().unwrap().push(buffer);
                Ok(gst::FlowSuccess::Ok)
            })
            .event_function(|_, _, _| true)
            .build();
        source.set_active(true).unwrap();
        sink.set_active(true).unwrap();
        source.link(&sink).unwrap();
        let faulted = Arc::new(AtomicBool::new(false));
        plan.attach(&source, Arc::clone(&faulted));
        assert!(source.push_event(gst::event::StreamStart::new("test")));
        assert!(source.push_event(gst::event::Caps::new(&first.caps().unwrap().to_owned())));
        assert!(source.push_event(gst::event::Segment::new(first.segment().unwrap())));
        Self {
            source,
            sink,
            received,
            faulted,
        }
    }

    fn push(&self, sample: &gst::Sample) {
        self.source
            .push(sample.buffer().unwrap().to_owned())
            .unwrap();
    }
}

impl Drop for Playback {
    fn drop(&mut self) {
        self.source.set_active(false).unwrap();
        self.sink.set_active(false).unwrap();
    }
}

#[test]
fn extraction_preserves_kept_bytes_and_timing_across_flush_loops() {
    gst::init().unwrap();
    let mut file = movie(&fixture_buffers());
    let plan = inspect(file.as_file_mut(), &AtomicBool::new(false)).unwrap();
    let samples = demux(file.as_file_mut());
    let playback = Playback::new(&plan, &samples[0]);
    for _ in 0..2 {
        for sample in &samples {
            playback.push(sample);
        }
        assert!(!playback.faulted.load(Ordering::Acquire));
        assert!(playback.source.push_event(gst::event::Eos::new()));
        assert!(playback.source.push_event(gst::event::FlushStart::new()));
        assert!(playback.source.push_event(gst::event::FlushStop::new(true)));
        assert!(playback
            .source
            .push_event(gst::event::Segment::new(samples[0].segment().unwrap())));
    }
    let output = playback.received.lock().unwrap();
    assert_eq!(output.len(), 6);
    for (actual, index) in output.iter().zip([0, 1, 2, 0, 1, 2]) {
        let expected = samples[index].buffer().unwrap();
        assert_eq!(
            actual.map_readable().unwrap().as_slice(),
            expected.map_readable().unwrap().as_slice()
        );
        assert_eq!(
            (actual.pts(), actual.dts(), actual.duration()),
            (expected.pts(), expected.dts(), expected.duration())
        );
    }
}

#[test]
fn changed_input_faults_before_admission_and_cannot_resume_in_place() {
    gst::init().unwrap();
    let mut file = movie(&fixture_buffers());
    let plan = inspect(file.as_file_mut(), &AtomicBool::new(false)).unwrap();
    let samples = demux(file.as_file_mut());
    for case in [
        "bytes",
        "pts",
        "dts",
        "duration",
        "flags",
        "caps",
        "segment",
        "early_eos",
        "extra",
        "order",
    ] {
        let playback = Playback::new(&plan, &samples[0]);
        playback.push(&samples[0]);
        let mut changed = samples[1].buffer().unwrap().to_owned();
        let buffer = changed.make_mut();
        match case {
            "bytes" => {
                buffer.map_writable().unwrap()[8] ^= 1;
            }
            "pts" => buffer.set_pts(buffer.pts().unwrap() + gst::ClockTime::NSECOND),
            "dts" => buffer.set_dts(buffer.dts().unwrap() + gst::ClockTime::NSECOND),
            "duration" => buffer.set_duration(gst::ClockTime::MSECOND),
            "flags" => buffer.set_flags(gst::BufferFlags::CORRUPTED),
            "caps" => {
                let mut caps = samples[0].caps().unwrap().to_owned();
                caps.make_mut().set("width", 1920);
                playback.source.push_event(gst::event::Caps::new(&caps));
            }
            "segment" => {
                let mut segment = samples[0].segment().unwrap().clone();
                segment.set_rate(2.0);
                playback
                    .source
                    .push_event(gst::event::Segment::new(&segment));
            }
            "early_eos" => {
                playback.source.push_event(gst::event::Eos::new());
            }
            "extra" => {
                for sample in &samples[1..] {
                    playback.push(sample);
                }
            }
            "order" => {
                changed = samples[2].buffer().unwrap().to_owned();
            }
            _ => unreachable!(),
        }
        let before = playback.received.lock().unwrap().len();
        let _ = playback.source.push(changed);
        assert!(playback.faulted.load(Ordering::Acquire), "{case}");
        assert_eq!(playback.received.lock().unwrap().len(), before, "{case}");
        playback.source.push_event(gst::event::FlushStart::new());
        playback.source.push_event(gst::event::FlushStop::new(true));
        playback
            .source
            .push_event(gst::event::Segment::new(samples[0].segment().unwrap()));
        let _ = playback
            .source
            .push(samples[0].buffer().unwrap().to_owned());
        assert_eq!(
            playback.received.lock().unwrap().len(),
            before,
            "fault must latch: {case}"
        );
    }
}
