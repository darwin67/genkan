//! Explicit network inspection, isolated from authentication and session locking.

use std::collections::BTreeMap;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gstreamer::{self as gst, prelude::*};

use super::{catalog::Asset, Result};

const PAGE: u64 = 64 * 1024;
const TIMEOUT: Duration = Duration::from_secs(30);

/// Owns the worker's entire process group, including after an early error or a
/// native decoder crash. Parent-death bindings additionally cover SIGINT/KILL,
/// where destructors cannot run.
struct Worker(std::process::Child);

impl Drop for Worker {
    fn drop(&mut self) {
        // SAFETY: this child was spawned in its own process group.
        unsafe {
            libc::kill(-(self.0.id() as i32), libc::SIGKILL);
        }
        let _ = self.0.wait();
    }
}

pub(super) fn run(asset: &Asset, max_bytes: u64) -> Result<i32> {
    let (code, output) = run_worker(asset, max_bytes, None, None)?;
    print!("{output}");
    Ok(code)
}

pub(super) fn validate(asset: &Asset, file: &Path, poster: &Path) -> Result<toml::Table> {
    let (code, output) = run_worker(asset, 64 * 1024 * 1024, Some(file), Some(poster))?;
    if code != 0 {
        return Err(format!("downloaded media failed compatibility validation: {output}").into());
    }
    Ok(output.parse()?)
}

fn run_worker(
    asset: &Asset,
    max_bytes: u64,
    file: Option<&Path>,
    poster: Option<&Path>,
) -> Result<(i32, String)> {
    let mut input = asset.metadata.clone();
    input.insert("id".into(), asset.id.clone().into());
    let temporary = tempfile::tempdir()?;
    let mut command = Command::new(std::env::current_exe()?);
    command.env("TMPDIR", temporary.path());
    command.args([
        "assets",
        "preflight-worker",
        "--max-bytes",
        &max_bytes.to_string(),
    ]);
    if let Some(file) = file {
        command.arg("--local-file").arg(file);
    }
    if let Some(poster) = poster {
        command.arg("--poster-file").arg(poster);
    }
    run_child(&mut command, &input, TIMEOUT + Duration::from_secs(5))
}

pub(super) fn run_child(
    command: &mut Command,
    input: &toml::Table,
    timeout: Duration,
) -> Result<(i32, String)> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
    use tokio::signal::unix::{signal, SignalKind};
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    let parent = std::process::id() as i32;
    command.stdin(Stdio::piped()).stdout(Stdio::piped()).process_group(0);
    // SAFETY: the hook uses only prctl/getppid and constructs an OS error;
    // no allocation, synchronization, or inherited locks are involved.
    unsafe {
        command.pre_exec(move || {
            if crate::wallpaper::bind_to_parent(parent) {
                Ok(())
            } else {
                Err(io::Error::from_raw_os_error(libc::ECHILD))
            }
        });
    }
    let mut worker = Worker(command.spawn()?);
    worker
        .0
        .stdin
        .take()
        .ok_or("cannot send preflight metadata")?
        .write_all(input.to_string().as_bytes())?;
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = worker.0.try_wait()? {
            let mut output = String::new();
            worker
                .0
                .stdout
                .take()
                .unwrap()
                .take(65536)
                .read_to_string(&mut output)?;
            return Ok((status.code().unwrap_or(3), output));
        }
        if Instant::now() >= deadline {
            return Ok((
                3,
                format!(
                    "INCONCLUSIVE {}: inspection timed out; no asset installed",
                    "worker"
                ),
            ));
        }
        tokio::select! {
            _ = interrupt.recv() => return Err("operation cancelled; no asset installed".into()),
            _ = terminate.recv() => return Err("operation cancelled; no asset installed".into()),
            _ = tokio::time::sleep(Duration::from_millis(20)) => {},
        }
    }
    })
}

pub(super) fn confine(cpu_seconds: u64) -> Result<()> {
    // Bound native parser/decoder faults independently of network deadlines.
    // This process never owns credentials, display surfaces, or session locks.
    for (resource, ceiling) in [
        (libc::RLIMIT_AS, 3 * 1024 * 1024 * 1024),
        (libc::RLIMIT_CPU, cpu_seconds),
        (libc::RLIMIT_CORE, 0),
    ] {
        let limit = libc::rlimit {
            rlim_cur: ceiling,
            rlim_max: ceiling,
        };
        if unsafe { libc::setrlimit(resource, &limit) } != 0 {
            return Err("could not apply preflight resource limits".into());
        }
    }
    Ok(())
}

pub(super) fn worker(max_bytes: u64, local: Option<&Path>, poster: Option<&Path>) -> Result<i32> {
    confine(20)?;
    let mut input = String::new();
    io::stdin()
        .take(1024 * 1024 + 1)
        .read_to_string(&mut input)?;
    if input.len() > 1024 * 1024 {
        return Err("preflight metadata exceeds 1 MiB".into());
    }
    let table: toml::Table = input.parse()?;
    let id = table
        .get("id")
        .and_then(toml::Value::as_str)
        .ok_or("missing preflight key")?
        .to_owned();
    let asset = Asset::from_entry(&id, table)?;
    let url = asset.url.as_deref().unwrap_or("");
    if local.is_none()
        && (!(url.starts_with("https://") || url.starts_with("http://"))
            || url.chars().any(char::is_control))
    {
        println!("UNSUPPORTED {id}: source must be a direct HTTP(S) media URL");
        return Ok(2);
    }
    let file = local.map(crate::wallpaper::open_wallpaper).transpose()?;
    let size = file
        .as_ref()
        .map(|file| file.metadata().map(|metadata| metadata.len()))
        .transpose()?;
    let source = Arc::new(Mutex::new(Source {
        url: url.into(),
        file,
        size,
        validator: None,
        pages: BTreeMap::new(),
        consumed: 0,
        requests: 0,
        max_bytes,
        deadline: Instant::now() + TIMEOUT,
        failure: None,
    }));
    let outcome = inspect(&asset, &source, poster);
    let state = source.lock().unwrap();
    if local.is_some() && state.failure.is_none() {
        if let Ok(ref inspection) = outcome {
            println!(
                "format = {:?}\nwidth = {}\nheight = {}",
                inspection.format, inspection.width, inspection.height
            );
            return Ok(0);
        }
    }
    let (code, detail) = if let Some(reason) = &state.failure {
        (3, reason.clone())
    } else {
        match outcome {
            Ok(inspection) if state.requests > 1 && state.validator.is_none() => (
                3,
                format!(
                    "{}; server supplied no strong ETag to bind the partial reads to one version",
                    inspection.detail
                ),
            ),
            Ok(inspection) => (0, inspection.detail),
            Err((code, reason)) => (code, reason),
        }
    };
    let label = match code {
        0 => "PREFLIGHT PASSED",
        2 => "UNSUPPORTED",
        _ => "INCONCLUSIVE",
    };
    println!("{label} {id}: {detail}");
    println!(
        "Media bytes consumed: {} / {max_bytes}; range requests: {}; source size: {}",
        state.consumed,
        state.requests,
        state
            .size
            .map_or_else(|| "unknown".into(), |s| s.to_string())
    );
    println!("No asset installed. Whole-file SHA-256 and uninspected frames are not verified.");
    Ok(code)
}

type Verdict<T> = std::result::Result<T, (i32, String)>;

struct Inspection {
    format: &'static str,
    width: u32,
    height: u32,
    detail: String,
}

fn inspect(
    asset: &Asset,
    source: &Arc<Mutex<Source>>,
    poster: Option<&Path>,
) -> Verdict<Inspection> {
    let header = source
        .lock()
        .unwrap()
        .read_at(0, PAGE as usize)
        .map_err(|e| (3, e.to_string()))?;
    let size = source.lock().unwrap().size.unwrap();
    if asset.bytes.is_some_and(|expected| expected != size) {
        return Err((2, "source byte size differs from catalog metadata".into()));
    }
    let kind = header
        .get(4..8)
        .ok_or((2, "truncated container header".into()))?;
    if !matches!(kind, b"ftyp" | b"wide" | b"mdat" | b"moov") {
        return Err((
            2,
            "source is not a direct MOV/HEIC container (webpages and ZIP archives are not media)"
                .into(),
        ));
    }
    let heic = if kind == b"ftyp" {
        let box_size = u32::from_be_bytes(header[..4].try_into().unwrap());
        let (end, start) = match box_size {
            0 => (size, 8),
            1 => (
                u64::from_be_bytes(
                    header
                        .get(8..16)
                        .ok_or((2, "truncated extended ftyp".into()))?
                        .try_into()
                        .unwrap(),
                ),
                16,
            ),
            length => (u64::from(length), 8),
        };
        if end < start as u64 + 8 || end > size {
            return Err((2, "invalid file-type box size".into()));
        }
        let brands = header
            .get(start..usize::try_from(end).unwrap_or(usize::MAX))
            .ok_or((3, "container brands exceed the inspected prefix".into()))?;
        brands.chunks_exact(4).enumerate().any(|(index, brand)| {
            // The second word is a minor version, not a compatible brand.
            index != 1
                && matches!(
                    brand,
                    b"heic" | b"heix" | b"hevc" | b"hevx" | b"mif1" | b"msf1"
                )
        })
    } else {
        false
    };
    let format = if heic { "heic" } else { "mov" };
    if asset
        .format
        .as_deref()
        .is_some_and(|expected| expected != format)
    {
        return Err((2, "container format differs from catalog metadata".into()));
    }
    let (width, height, detail) = if heic {
        let reader = RemoteReader {
            source: source.clone(),
            position: 0,
        };
        let probe = genkan::dynamic_wallpaper::heic::probe(reader, size)
            .map_err(|error| (2, error.to_string()))?;
        (
            probe.width,
            probe.height,
            format!(
                "HEIC primary image decoded; {} images; dynamic schedule: {}",
                probe.image_count, probe.has_schedule
            ),
        )
    } else {
        probe_movie(source, size, poster)?
    };
    if asset.width.is_some_and(|expected| expected != width)
        || asset.height.is_some_and(|expected| expected != height)
    {
        return Err((
            2,
            format!("decoded {width}x{height} differs from catalog dimensions"),
        ));
    }
    Ok(Inspection {
        format,
        width,
        height,
        detail: format!("{width}x{height}; {detail}"),
    })
}

struct Source {
    url: String,
    file: Option<std::fs::File>,
    size: Option<u64>,
    validator: Option<String>,
    pages: BTreeMap<u64, Vec<u8>>,
    consumed: u64,
    requests: u32,
    max_bytes: u64,
    deadline: Instant,
    failure: Option<String>,
}

impl Source {
    fn read_at(&mut self, offset: u64, length: usize) -> io::Result<Vec<u8>> {
        if let Some(reason) = &self.failure {
            return Err(io::Error::other(reason.clone()));
        }
        let mut bytes = Vec::new();
        let mut cursor = offset;
        // Random-access appsrc requires the requested length, not one page.
        // Fetch incrementally so oversized requests still obey the byte budget.
        while bytes.len() < length && self.size.is_none_or(|size| cursor < size) {
            let start = cursor / PAGE * PAGE;
            if !self.pages.contains_key(&start) {
                if let Err(error) = self.fetch(start) {
                    self.failure = Some(error.to_string());
                    return Err(error);
                }
            }
            let page = &self.pages[&start];
            let within = (cursor - start) as usize;
            if within >= page.len() {
                let reason = "preflight byte budget exhausted";
                self.failure = Some(reason.into());
                return Err(io::Error::other(reason));
            }
            let count = (length - bytes.len()).min(page.len() - within);
            bytes.extend_from_slice(&page[within..within + count]);
            cursor += count as u64;
        }
        Ok(bytes)
    }

    fn fetch(&mut self, start: u64) -> io::Result<()> {
        let remaining = self.max_bytes - self.consumed;
        let wanted = remaining
            .min(PAGE)
            .min(self.size.map_or(PAGE, |size| size - start));
        let time = self.deadline.saturating_duration_since(Instant::now());
        let request_limit = if self.file.is_some() { 1024 } else { 128 };
        if wanted == 0 || time.is_zero() || self.requests >= request_limit {
            return Err(io::Error::other(
                "preflight byte, time, or request budget exhausted",
            ));
        }
        if let Some(file) = self.file.as_mut() {
            file.seek(SeekFrom::Start(start))?;
            let mut bytes = Vec::new();
            file.take(wanted).read_to_end(&mut bytes)?;
            self.requests += 1;
            self.consumed += bytes.len() as u64;
            self.pages.insert(start, bytes);
            return Ok(());
        }
        let headers = tempfile::NamedTempFile::new()?;
        let mut command = Command::new("curl");
        // Never follow redirects: curl can drain their bodies internally
        // without applying max-filesize or exposing them through stdout.
        // Every response body must pass through our aggregate byte counter.
        command
            .args([
                "--disable",
                "--globoff",
                "--silent",
                "--proto",
                "=http,https",
                "--header",
                "Accept-Encoding: identity",
                "--range",
                &format!("{start}-{}", start + wanted - 1),
                "--max-filesize",
                &wanted.to_string(),
                "--max-time",
                &format!("{:.3}", time.as_secs_f64()),
                "--connect-timeout",
                "5",
                "--dump-header",
            ])
            .arg(headers.path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let _certificate = super::download::configure_tls(&mut command, &self.url)?;
        if let Some(etag) = &self.validator {
            command.args(["--header", &format!("If-Match: {etag}")]);
        }
        command.arg("--url").arg(&self.url);
        let parent = std::process::id() as i32;
        // SAFETY: as for the worker hook, only async-signal-safe syscalls run.
        unsafe {
            command.pre_exec(move || {
                if crate::wallpaper::bind_to_parent(parent) {
                    Ok(())
                } else {
                    Err(io::Error::from_raw_os_error(libc::ECHILD))
                }
            });
        }
        let mut child = command.spawn().map_err(|_| {
            io::Error::other("curl is unavailable; use the Genkan Nix package or development shell")
        })?;
        self.requests += 1;
        let mut body = Vec::new();
        let read = child
            .stdout
            .take()
            .unwrap()
            .take(wanted)
            .read_to_end(&mut body);
        self.consumed += body.len() as u64;
        let status = child.wait()?;
        read?;
        let mut fields = BTreeMap::new();
        let mut http_status = 0;
        for line in std::fs::read_to_string(headers.path())?.lines() {
            if line.starts_with("HTTP/") {
                fields.clear();
                http_status = line
                    .split_whitespace()
                    .nth(1)
                    .and_then(|s| s.parse::<u16>().ok())
                    .unwrap_or(0);
            } else if let Some((key, value)) = line.split_once(':') {
                fields.insert(key.to_ascii_lowercase(), value.trim().to_owned());
            }
        }
        if http_status == 0 {
            let reason = match status.code() {
                Some(5 | 6) => "source hostname or proxy could not be resolved",
                Some(7) => "connection to source failed",
                Some(28) => "source connection or transfer timed out",
                Some(35 | 60 | 77) => "TLS connection or certificate validation failed",
                _ => "HTTP transport failed before a response was received",
            };
            return Err(io::Error::other(reason));
        }
        if (300..400).contains(&http_status) {
            return Err(io::Error::other("redirects are not followed during bounded inspection; provide the final direct media URL"));
        }
        if http_status != 206 {
            return Err(io::Error::other(format!("server did not honor the byte-range request (HTTP {http_status}); no full-download fallback")));
        }
        if !status.success() {
            return Err(io::Error::other(
                "range transfer failed or exceeded its byte/time limit",
            ));
        }
        let range = fields
            .get("content-range")
            .and_then(|s| s.strip_prefix("bytes "))
            .and_then(|s| s.split_once('/'))
            .and_then(|(span, total)| Some((span.split_once('-')?, total)))
            .and_then(|((first, last), total)| {
                Some((
                    first.parse::<u64>().ok()?,
                    last.parse::<u64>().ok()?,
                    total.parse::<u64>().ok()?,
                ))
            })
            .ok_or_else(|| io::Error::other("missing or malformed Content-Range"))?;
        let (first, last, total) = range;
        if total == 0
            || total > i64::MAX as u64
            || first != start
            || last != (start + wanted - 1).min(total - 1)
            || last.checked_sub(first).and_then(|n| n.checked_add(1)) != Some(body.len() as u64)
            || self.size.is_some_and(|size| size != total)
            || fields
                .get("content-encoding")
                .is_some_and(|s| s != "identity")
        {
            return Err(io::Error::other(
                "range response does not match the requested source bytes",
            ));
        }
        let etag = fields
            .get("etag")
            .filter(|s| s.starts_with('"') && s.ends_with('"'))
            .cloned();
        if self.requests > 1 && etag != self.validator {
            return Err(io::Error::other(
                "remote source version changed during inspection",
            ));
        }
        self.validator = etag;
        self.size = Some(total);
        self.pages.insert(start, body);
        Ok(())
    }
}

struct RemoteReader {
    source: Arc<Mutex<Source>>,
    position: u64,
}

impl Read for RemoteReader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        let bytes = self
            .source
            .lock()
            .unwrap()
            .read_at(self.position, output.len())?;
        output[..bytes.len()].copy_from_slice(&bytes);
        self.position += bytes.len() as u64;
        Ok(bytes.len())
    }
}

impl Seek for RemoteReader {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        let position = match position {
            SeekFrom::Start(position) => i128::from(position),
            SeekFrom::Current(delta) => i128::from(self.position) + i128::from(delta),
            SeekFrom::End(delta) => {
                i128::from(self.source.lock().unwrap().size.unwrap()) + i128::from(delta)
            }
        };
        self.position =
            u64::try_from(position).map_err(|_| io::Error::other("invalid media seek"))?;
        Ok(self.position)
    }
}

fn probe_movie(
    source: &Arc<Mutex<Source>>,
    size: u64,
    poster: Option<&Path>,
) -> Verdict<(u32, u32, String)> {
    gst::init().map_err(|_| (3, "GStreamer initialization failed".into()))?;
    let element = |factory| {
        gst::ElementFactory::make(factory).build().map_err(|_| {
            (
                3,
                format!("required decoder element {factory} is unavailable"),
            )
        })
    };
    let input = gstreamer_app::AppSrc::builder()
        .size(size as i64)
        .stream_type(gstreamer_app::AppStreamType::RandomAccess)
        .format(gst::Format::Bytes)
        .build();
    let demux = element("qtdemux")?;
    let parser = element("h265parse")?;
    let decoder = element("avdec_h265")?;
    let convert = element("videoconvert")?;
    decoder.set_property("max-threads", 2i32);
    let sink = gstreamer_app::AppSink::builder()
        .caps(
            &gst::Caps::builder("video/x-raw")
                .field("format", "RGBA")
                .build(),
        )
        .max_buffers(1)
        .drop(true)
        .sync(false)
        .build();
    let pipeline = gst::Pipeline::new();
    pipeline
        .add_many([
            input.upcast_ref(),
            &demux,
            &parser,
            &decoder,
            &convert,
            sink.upcast_ref(),
        ])
        .map_err(|_| (3, "cannot assemble decoder".into()))?;
    input
        .link(&demux)
        .map_err(|_| (3, "cannot link source".into()))?;
    gst::Element::link_many([&parser, &decoder, &convert, sink.upcast_ref()])
        .map_err(|_| (3, "cannot link decoder".into()))?;
    let has_hevc = Arc::new(AtomicBool::new(false));
    let observed = has_hevc.clone();
    let tracks_complete = Arc::new(AtomicBool::new(false));
    let discovery = tracks_complete.clone();
    demux.connect_no_more_pads(move |_| discovery.store(true, Ordering::Release));
    let parser_sink = parser.static_pad("sink").unwrap();
    demux.connect_pad_added(move |_, pad| {
        let caps = pad.current_caps().unwrap_or_else(|| pad.query_caps(None));
        if let Some(caps) = caps.structure(0) {
            if caps.name() == "video/x-h265" {
                observed.store(true, Ordering::Release);
                if !parser_sink.is_linked() {
                    let _ = pad.link(&parser_sink);
                }
            }
        }
    });
    let remote = source.clone();
    let position = Arc::new(Mutex::new(0));
    let cursor = position.clone();
    input.set_callbacks(
        gstreamer_app::AppSrcCallbacks::builder()
            .seek_data(move |_, offset| {
                *cursor.lock().unwrap() = offset;
                true
            })
            .need_data(move |input, length| {
                let mut cursor = position.lock().unwrap();
                match remote.lock().unwrap().read_at(*cursor, length as usize) {
                    Ok(bytes) if !bytes.is_empty() => {
                        *cursor += bytes.len() as u64;
                        let _ = input.push_buffer(gst::Buffer::from_mut_slice(bytes));
                    }
                    _ => {
                        let _ = input.end_of_stream();
                    }
                }
            })
            .build(),
    );
    let result = (|| {
        pipeline
            .set_state(gst::State::Paused)
            .map_err(|_| (3, "decoder could not start".into()))?;
        let deadline = Instant::now() + Duration::from_secs(10);
        let bus = pipeline.bus().unwrap();
        let sample = loop {
            if let Some(sample) = sink.try_pull_preroll(gst::ClockTime::from_mseconds(100)) {
                break Some(sample);
            }
            if Instant::now() >= deadline
                || source.lock().unwrap().failure.is_some()
                || bus
                    .pop_filtered(&[gst::MessageType::Error, gst::MessageType::Eos])
                    .is_some()
                || (tracks_complete.load(Ordering::Acquire) && !has_hevc.load(Ordering::Acquire))
            {
                break None;
            }
        };
        let Some(sample) = sample else {
            return if tracks_complete.load(Ordering::Acquire) && !has_hevc.load(Ordering::Acquire) {
                Err((2, "MOV has no HEVC video track, the codec supported by Genkan's MOV playback pipeline".into()))
            } else {
                Err((
                    3,
                    "no representative MOV frame decoded within the inspection limit".into(),
                ))
            };
        };
        let info = sample
            .caps()
            .and_then(|caps| gstreamer_video::VideoInfo::from_caps(caps).ok())
            .ok_or((3, "decoder produced no video geometry".into()))?;
        if sample.buffer().is_none_or(|buffer| buffer.size() == 0) {
            return Err((3, "decoder returned an empty frame".into()));
        }
        if let Some(path) = poster {
            let frame = gstreamer_video::VideoFrameRef::from_buffer_ref_readable(
                sample.buffer().unwrap(),
                &info,
            )
            .map_err(|_| (3, "cannot map poster pixels".into()))?;
            let plane = frame
                .plane_data(0)
                .map_err(|_| (3, "cannot read poster pixels".into()))?;
            let stride = info.stride()[0] as usize;
            let row = info.width() as usize * 4;
            let mut pixels = Vec::new();
            for line in plane.chunks(stride).take(info.height() as usize) {
                pixels.extend_from_slice(&line[..row]);
            }
            let image = image::RgbaImage::from_raw(info.width(), info.height(), pixels)
                .ok_or((3, "invalid poster geometry".into()))?;
            image::DynamicImage::ImageRgba8(image)
                .into_rgb8()
                .save_with_format(path, image::ImageFormat::Jpeg)
                .map_err(|error| (3, format!("cannot save poster: {error}")))?;
        }
        Ok((
            info.width(),
            info.height(),
            format!(
                "MOV HEVC frame decoded; pixel format {}",
                info.format().to_str()
            ),
        ))
    })();
    let _ = pipeline.set_state(gst::State::Null);
    result
}
