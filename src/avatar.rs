//! Account pictures (RFD 7).
//!
//! A picture file comes from AccountsService, which copies whatever its user
//! passes to `SetIconFile` without checking that it is an image. The login and
//! lock processes therefore never decode it. [`decode_file`] hands the open
//! file to a separate `genkan avatar-decode` process ([`run_decoder`]) that
//! confines itself before reading a byte, and accepts nothing back but a
//! fixed-size block of pixels.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::File;
use std::io::{Cursor, Read, Write};
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use image::imageops::FilterType;
use image::ImageFormat;

/// Width and height of a decoded picture, in pixels: the 75-point avatar at
/// the lock's three pixels per point is 225 pixels.
pub const SIZE: u32 = 256;
/// Byte length of a decoded picture: premultiplied RGBA rows, top to bottom.
pub const PIXEL_BYTES: usize = (SIZE as usize) * (SIZE as usize) * 4;
/// AccountsService's own limit for `SetIconFile`.
pub const MAX_FILE_BYTES: u64 = 1024 * 1024;
/// Larger images are rejected from their header, before any pixels are
/// allocated.
pub const MAX_DIMENSION: u32 = 4096;
/// The longest a decode may take, including starting the process.
pub const TIMEOUT: Duration = Duration::from_secs(2);

/// Decoder allocations beyond this fail inside the image decoder.
const MAX_DECODER_ALLOCATION: u64 = 128 * 1024 * 1024;
/// The decoder process's whole address space, including the executable's own
/// mappings and shared libraries.
const MAX_ADDRESS_SPACE: u64 = 256 * 1024 * 1024;
/// CPU time after which the kernel kills the decoder.
const MAX_CPU_SECONDS: u64 = 2;

const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
const JPEG_SIGNATURE: &[u8] = &[0xff, 0xd8, 0xff];

/// A decoded account picture: [`SIZE`] by [`SIZE`] premultiplied RGBA.
#[derive(Clone, PartialEq, Eq)]
pub struct Picture {
    pixels: Arc<[u8]>,
}

impl Picture {
    /// The picture's [`PIXEL_BYTES`] bytes.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }
}

impl fmt::Debug for Picture {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Picture").finish_non_exhaustive()
    }
}

/// Why a picture was not decoded. Every case falls back to initials.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The path could not be opened, or is a symbolic link.
    Open,
    /// The path is not a regular file.
    NotRegular,
    /// The file is empty or larger than [`MAX_FILE_BYTES`].
    Size,
    /// The decoder process could not be started.
    Spawn,
    /// The decoder did not finish within [`TIMEOUT`].
    Timeout,
    /// The decoder rejected the picture, crashed, or returned anything other
    /// than exactly [`PIXEL_BYTES`] bytes.
    Rejected,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Open => "the picture could not be opened",
            Self::NotRegular => "the picture is not a regular file",
            Self::Size => "the picture is empty or larger than 1 MiB",
            Self::Spawn => "the picture decoder could not be started",
            Self::Timeout => "the picture decoder timed out",
            Self::Rejected => "the picture decoder rejected the picture",
        })
    }
}

impl std::error::Error for Error {}

/// Decodes the picture at `path` in a confined `executable avatar-decode`
/// process. `executable` is Genkan's own binary.
///
/// This blocks for at most about [`TIMEOUT`]; call it off the UI thread.
pub fn decode_file(executable: &Path, path: &Path) -> Result<Picture, Error> {
    let file = open_picture(path)?;
    let started = Instant::now();
    let mut child = Command::new(executable)
        .arg("avatar-decode")
        .arg("--parent-pid")
        .arg(std::process::id().to_string())
        .env_clear()
        .stdin(Stdio::from(file))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| Error::Spawn)?;
    let Some(mut stdout) = child.stdout.take() else {
        reap(&mut child);
        return Err(Error::Spawn);
    };
    let (sender, receiver) = mpsc::sync_channel(1);
    let reader = std::thread::Builder::new()
        .name("genkan-avatar-read".into())
        .spawn(move || {
            // One byte more than a picture shows an overlong answer.
            let mut output = Vec::with_capacity(PIXEL_BYTES + 1);
            let read = (&mut stdout)
                .take(PIXEL_BYTES as u64 + 1)
                .read_to_end(&mut output);
            let _ = sender.send(read.map(|_| output));
        });
    if reader.is_err() {
        reap(&mut child);
        return Err(Error::Spawn);
    }
    let output = match receiver.recv_timeout(TIMEOUT.saturating_sub(started.elapsed())) {
        Ok(Ok(output)) if output.len() == PIXEL_BYTES => output,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            reap(&mut child);
            return Err(Error::Timeout);
        }
        _ => {
            reap(&mut child);
            return Err(Error::Rejected);
        }
    };
    match wait_until(&mut child, started + TIMEOUT) {
        Some(status) if status.success() => Ok(Picture {
            pixels: output.into(),
        }),
        Some(_) => Err(Error::Rejected),
        None => Err(Error::Timeout),
    }
}

/// Opens a picture without following a final symbolic link or blocking on a
/// FIFO, and requires a non-empty regular file within the size limit.
fn open_picture(path: &Path) -> Result<File, Error> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY)
        .open(path)
        .map_err(|_| Error::Open)?;
    let metadata = file.metadata().map_err(|_| Error::Open)?;
    let kind = metadata.file_type();
    if !kind.is_file() || kind.is_fifo() || kind.is_socket() {
        return Err(Error::NotRegular);
    }
    if metadata.len() == 0 || metadata.len() > MAX_FILE_BYTES {
        return Err(Error::Size);
    }
    Ok(file)
}

/// Waits for `child` until `deadline`, then kills it. Returns its status, or
/// `None` when it had to be killed.
fn wait_until(child: &mut Child, deadline: Instant) -> Option<std::process::ExitStatus> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            _ => {
                reap(child);
                return None;
            }
        }
    }
}

fn reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// The `genkan avatar-decode` process. Never returns.
///
/// It confines itself before reading its input, decodes standard input with
/// [`decode_bytes`], and writes exactly [`PIXEL_BYTES`] bytes to standard
/// output, or exits with a failure status.
pub fn run_decoder(parent_pid: i32) -> ! {
    let decoded = bind_to_parent(parent_pid)
        .then(confine)
        .and_then(|confined| confined.ok())
        .and_then(|()| {
            let mut input = Vec::new();
            std::io::stdin()
                .lock()
                .take(MAX_FILE_BYTES + 1)
                .read_to_end(&mut input)
                .ok()?;
            decode_bytes(&input)
        });
    let written = decoded.is_some_and(|pixels| {
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(&pixels).is_ok() && stdout.flush().is_ok()
    });
    std::process::exit(if written { 0 } else { 1 });
}

/// Ties the decoder to the process that started it, so it never outlives it.
fn bind_to_parent(expected_parent: i32) -> bool {
    // SAFETY: `PR_SET_PDEATHSIG` changes only this process, and `getppid` has
    // no preconditions.
    let requested = unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) };
    let parent = unsafe { libc::getppid() };
    requested == 0 && parent == expected_parent
}

/// Confines the decoder: no new privileges, no descriptors beyond the three
/// standard ones, resource limits, and a seccomp filter that kills the process
/// on any system call a memory-only decoder does not need.
fn confine() -> Result<(), ()> {
    // SAFETY: these calls change only this process. `close_range` closes
    // descriptors this single-threaded process does not use.
    unsafe {
        if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
            return Err(());
        }
        if libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 0u32) != 0 {
            return Err(());
        }
    }
    for (resource, limit) in [
        (libc::RLIMIT_AS, MAX_ADDRESS_SPACE),
        (libc::RLIMIT_CPU, MAX_CPU_SECONDS),
        (libc::RLIMIT_FSIZE, 0),
        (libc::RLIMIT_NOFILE, 3),
        (libc::RLIMIT_NPROC, 0),
        (libc::RLIMIT_CORE, 0),
    ] {
        let limit = libc::rlimit {
            rlim_cur: limit,
            rlim_max: limit,
        };
        // SAFETY: `limit` is a valid `rlimit` for the duration of the call.
        if unsafe { libc::setrlimit(resource, &limit) } != 0 {
            return Err(());
        }
    }
    let program: seccompiler::BpfProgram = seccomp_filter()?.try_into().map_err(|_| ())?;
    seccompiler::apply_filter(&program).map_err(|_| ())
}

/// The decoder's system call allowlist. Anything else kills the process.
fn seccomp_filter() -> Result<seccompiler::SeccompFilter, ()> {
    use seccompiler::{SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompRule};

    let argument = |index: u8, operator: SeccompCmpOp, value: u64| {
        SeccompCondition::new(index, SeccompCmpArgLen::Dword, operator, value)
            .and_then(|condition| SeccompRule::new(vec![condition]))
            .map_err(|_| ())
    };
    let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();
    // Input only from standard input; output only to standard output and to
    // standard error, which the parent connects to /dev/null.
    rules.insert(libc::SYS_read, vec![argument(0, SeccompCmpOp::Eq, 0)?]);
    rules.insert(
        libc::SYS_write,
        vec![
            argument(0, SeccompCmpOp::Eq, 1)?,
            argument(0, SeccompCmpOp::Eq, 2)?,
        ],
    );
    // Memory for the decoder, never executable.
    rules.insert(
        libc::SYS_mmap,
        vec![argument(
            2,
            SeccompCmpOp::MaskedEq(libc::PROT_EXEC as u64),
            0,
        )?],
    );
    for call in [
        libc::SYS_munmap,
        libc::SYS_mremap,
        libc::SYS_brk,
        libc::SYS_madvise,
        libc::SYS_futex,
        libc::SYS_getrandom,
        libc::SYS_clock_gettime,
        libc::SYS_sigaltstack,
        libc::SYS_rt_sigprocmask,
        libc::SYS_rt_sigreturn,
        libc::SYS_close,
        libc::SYS_exit,
        libc::SYS_exit_group,
    ] {
        rules.insert(call, Vec::new());
    }
    seccompiler::SeccompFilter::new(
        rules,
        seccompiler::SeccompAction::KillProcess,
        seccompiler::SeccompAction::Allow,
        std::env::consts::ARCH.try_into().map_err(|_| ())?,
    )
    .map_err(|_| ())
}

/// Decodes a PNG or JPEG into a [`SIZE`] by [`SIZE`] premultiplied RGBA
/// picture: the largest centered square, scaled. Returns `None` for any other
/// format, an image larger than [`MAX_DIMENSION`] on either side, or a decoding
/// error.
///
/// This parses untrusted bytes; only [`run_decoder`] calls it outside tests.
pub fn decode_bytes(data: &[u8]) -> Option<Vec<u8>> {
    if data.len() as u64 > MAX_FILE_BYTES {
        return None;
    }
    let image = if data.starts_with(PNG_SIGNATURE) {
        decode_png(data)?
    } else if data.starts_with(JPEG_SIGNATURE) {
        decode_jpeg(data)?
    } else {
        return None;
    };
    let (width, height) = image.dimensions();
    let side = width.min(height);
    if side == 0 {
        return None;
    }
    let square =
        image::imageops::crop_imm(&image, (width - side) / 2, (height - side) / 2, side, side)
            .to_image();
    let mut pixels = image::imageops::resize(&square, SIZE, SIZE, FilterType::Triangle).into_raw();
    for pixel in pixels.chunks_exact_mut(4) {
        let alpha = u16::from(pixel[3]);
        for channel in &mut pixel[..3] {
            *channel = ((u16::from(*channel) * alpha + 127) / 255) as u8;
        }
    }
    Some(pixels)
}

fn decode_png(data: &[u8]) -> Option<image::RgbaImage> {
    let mut reader = image::io::Reader::with_format(Cursor::new(data), ImageFormat::Png);
    let mut limits = image::io::Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    limits.max_alloc = Some(MAX_DECODER_ALLOCATION);
    reader.limits(limits);
    Some(reader.decode().ok()?.into_rgba8())
}

/// JPEG uses zune-jpeg, which decodes on the calling thread. The `image`
/// crate's JPEG decoder starts worker threads, which the sandbox forbids.
fn decode_jpeg(data: &[u8]) -> Option<image::RgbaImage> {
    let options = zune_core::options::DecoderOptions::default()
        .set_max_width(MAX_DIMENSION as usize)
        .set_max_height(MAX_DIMENSION as usize)
        .jpeg_set_out_colorspace(zune_core::colorspace::ColorSpace::RGBA);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(data, options);
    let pixels = decoder.decode().ok()?;
    let (width, height) = decoder.dimensions()?;
    image::RgbaImage::from_raw(
        u32::try_from(width).ok()?,
        u32::try_from(height).ok()?,
        pixels,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgba};

    fn encode(image: &ImageBuffer<Rgba<u8>, Vec<u8>>, format: ImageFormat) -> Vec<u8> {
        let mut bytes = Cursor::new(Vec::new());
        match format {
            ImageFormat::Jpeg => image::DynamicImage::ImageRgba8(image.clone())
                .into_rgb8()
                .write_to(&mut bytes, format),
            _ => image.write_to(&mut bytes, format),
        }
        .expect("encode a fixture");
        bytes.into_inner()
    }

    /// A wide image: red on the left third, green in the middle, blue on the
    /// right.
    fn stripes(width: u32, height: u32) -> ImageBuffer<Rgba<u8>, Vec<u8>> {
        ImageBuffer::from_fn(width, height, |x, _| match 3 * x / width {
            0 => Rgba([255, 0, 0, 255]),
            1 => Rgba([0, 255, 0, 255]),
            _ => Rgba([0, 0, 255, 255]),
        })
    }

    fn pixel(pixels: &[u8], x: u32, y: u32) -> [u8; 4] {
        let index = ((y * SIZE + x) * 4) as usize;
        pixels[index..index + 4].try_into().unwrap()
    }

    #[test]
    fn png_is_cropped_to_its_centered_square_and_scaled() {
        let pixels = decode_bytes(&encode(&stripes(600, 200), ImageFormat::Png)).unwrap();
        assert_eq!(pixels.len(), PIXEL_BYTES);
        // The centered 200-pixel square is the green middle third.
        assert_eq!(pixel(&pixels, 0, 0), [0, 255, 0, 255]);
        assert_eq!(pixel(&pixels, SIZE - 1, SIZE - 1), [0, 255, 0, 255]);
    }

    #[test]
    fn jpeg_decodes() {
        let pixels = decode_bytes(&encode(&stripes(90, 300), ImageFormat::Jpeg)).unwrap();
        assert_eq!(pixels.len(), PIXEL_BYTES);
        let [red, green, blue, alpha] = pixel(&pixels, SIZE / 2, SIZE / 2);
        assert!(green > 200 && red < 60 && blue < 60 && alpha == 255);
    }

    #[test]
    fn alpha_is_premultiplied() {
        let image = ImageBuffer::from_pixel(8, 8, Rgba([255, 128, 0, 128]));
        let pixels = decode_bytes(&encode(&image, ImageFormat::Png)).unwrap();
        assert_eq!(pixel(&pixels, 10, 10), [128, 64, 0, 128]);
    }

    #[test]
    fn other_formats_and_garbage_are_rejected() {
        assert_eq!(decode_bytes(b"GIF89a\x01\x00\x01\x00\x00\x00\x00;"), None);
        assert_eq!(
            decode_bytes(b"BM\x1e\x00\x00\x00\x00\x00\x00\x00\x1a\x00"),
            None
        );
        assert_eq!(decode_bytes(b""), None);
        assert_eq!(
            decode_bytes(b"<svg xmlns='http://www.w3.org/2000/svg'/>"),
            None
        );
        assert_eq!(decode_bytes(PNG_SIGNATURE), None);
        assert_eq!(decode_bytes(&[0xff, 0xd8, 0xff, 0xe0, 0, 0]), None);
    }

    #[test]
    fn truncated_images_are_rejected() {
        let png = encode(&stripes(64, 64), ImageFormat::Png);
        assert_eq!(decode_bytes(&png[..png.len() / 2]), None);
    }

    #[test]
    fn oversized_dimensions_are_rejected() {
        for format in [ImageFormat::Png, ImageFormat::Jpeg] {
            let wide = ImageBuffer::from_pixel(MAX_DIMENSION + 1, 8, Rgba([0, 0, 0, 255]));
            assert_eq!(decode_bytes(&encode(&wide, format)), None, "{format:?}");
            let fits = ImageBuffer::from_pixel(MAX_DIMENSION, 8, Rgba([0, 0, 0, 255]));
            assert!(decode_bytes(&encode(&fits, format)).is_some(), "{format:?}");
        }
    }

    #[test]
    fn oversized_input_is_rejected_before_decoding() {
        let mut png = encode(&stripes(8, 8), ImageFormat::Png);
        png.resize(MAX_FILE_BYTES as usize + 1, 0);
        assert_eq!(decode_bytes(&png), None);
    }

    #[test]
    fn the_seccomp_filter_compiles_for_this_architecture() {
        let program: seccompiler::BpfProgram = seccomp_filter().unwrap().try_into().unwrap();
        assert!(!program.is_empty());
    }
}
