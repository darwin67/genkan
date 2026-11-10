//! End-to-end checks for the account picture decoder (RFD 7).
//!
//! These run the real `genkan avatar-decode` process that `login` and `lock`
//! start for AccountsService pictures, proving that it decodes inside its
//! sandbox, that the parent rejects every unsafe or malformed input, and that a
//! stuck or misbehaving decoder costs at most the timeout.

#![cfg(feature = "gui")]

use std::io::{Cursor, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use genkan::avatar::{self, Error, PIXEL_BYTES};
use image::{ImageBuffer, ImageFormat, Rgba};

fn genkan() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_genkan"))
}

/// A fresh directory under the system temporary directory, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "genkan-avatar-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).expect("create a scratch directory");
        Self(path)
    }

    fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.0.join(name);
        std::fs::write(&path, bytes).expect("write a fixture");
        path
    }

    /// An executable shell script standing in for a misbehaving decoder.
    fn script(&self, name: &str, body: &str) -> PathBuf {
        let path = self.write(name, format!("#!/bin/sh\n{body}\n").as_bytes());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("make a script executable");
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn photo(format: ImageFormat) -> Vec<u8> {
    let image = ImageBuffer::from_fn(320, 200, |x, y| {
        Rgba([(x % 256) as u8, (y % 256) as u8, 160, 255])
    });
    let mut bytes = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image)
        .into_rgb8()
        .write_to(&mut bytes, format)
        .expect("encode a fixture");
    bytes.into_inner()
}

#[test]
fn the_process_decodes_png_and_jpeg_exactly_as_the_decoder_does() {
    let scratch = Scratch::new();
    for (name, format) in [
        ("photo.png", ImageFormat::Png),
        ("photo.jpg", ImageFormat::Jpeg),
    ] {
        let bytes = photo(format);
        let path = scratch.write(name, &bytes);
        let picture = avatar::decode_file(genkan(), &path).expect("decode in the sandbox");
        assert_eq!(picture.pixels().len(), PIXEL_BYTES);
        assert_eq!(
            picture.pixels(),
            avatar::decode_bytes(&bytes).unwrap().as_slice(),
            "{name}"
        );
    }
}

#[test]
fn photo_import_is_a_metadata_free_512_pixel_centered_png_with_straight_alpha() {
    let scratch = Scratch::new();
    // Fine stripes in the centered square distinguish a genuine 512px
    // decode from upscaling the 256px display picture. Outer thirds must go.
    let image = ImageBuffer::<Rgba<u8>, _>::from_fn(1536, 512, |x, _| {
        if !(512..1024).contains(&x) {
            Rgba([255, 0, 0, 255])
        } else if x % 2 == 0 {
            Rgba([0, 128, 255, 128])
        } else {
            Rgba([0, 255, 0, 255])
        }
    });
    let mut input = Cursor::new(Vec::new());
    image.write_to(&mut input, ImageFormat::Png).unwrap();
    let path = scratch.write("wide.png", input.get_ref());
    let png = avatar::photo_png(genkan(), &path).unwrap();
    assert!(png.len() as u64 <= avatar::MAX_FILE_BYTES);
    assert_eq!(image::guess_format(&png).unwrap(), ImageFormat::Png);
    let decoded = image::load_from_memory(&png).unwrap().into_rgba8();
    assert_eq!(decoded.dimensions(), (512, 512));
    assert_eq!(decoded.get_pixel(0, 0).0, [0, 128, 255, 128]);
    assert_eq!(decoded.get_pixel(1, 0).0, [0, 255, 0, 255]);
    assert_eq!(decoded.get_pixel(511, 511).0, [0, 255, 0, 255]);
    // No ancillary chunks: in particular no EXIF, text, timestamps or ICC.
    let mut remaining = &png[8..];
    while !remaining.is_empty() {
        let length = u32::from_be_bytes(remaining[..4].try_into().unwrap()) as usize;
        assert!(matches!(&remaining[4..8], b"IHDR" | b"IDAT" | b"IEND"));
        remaining = &remaining[length + 12..];
    }
    let jpeg = scratch.write("photo.jpg", &photo(ImageFormat::Jpeg));
    let imported = avatar::photo_png(genkan(), &jpeg).unwrap();
    assert_eq!(image::load_from_memory(&imported).unwrap().width(), 512);
    let malformed = scratch.write("broken", b"not an image");
    assert_eq!(
        avatar::photo_png(genkan(), &malformed),
        Err(Error::Rejected)
    );
}

#[test]
fn unsafe_and_malformed_inputs_are_rejected() {
    let scratch = Scratch::new();
    let png = scratch.write("photo.png", &photo(ImageFormat::Png));
    assert_eq!(
        avatar::decode_file(genkan(), &scratch.0.join("missing.png")),
        Err(Error::Open)
    );

    let link = scratch.0.join("link.png");
    std::os::unix::fs::symlink(&png, &link).unwrap();
    assert_eq!(avatar::decode_file(genkan(), &link), Err(Error::Open));

    assert_eq!(
        avatar::decode_file(genkan(), &scratch.0),
        Err(Error::NotRegular)
    );

    // Opening a FIFO with no writer must not block.
    let fifo = scratch.0.join("fifo");
    let name = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: `name` is a valid NUL-terminated path.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    assert_eq!(avatar::decode_file(genkan(), &fifo), Err(Error::NotRegular));

    let empty = scratch.write("empty.png", b"");
    assert_eq!(avatar::decode_file(genkan(), &empty), Err(Error::Size));

    let mut large = photo(ImageFormat::Png);
    large.resize(avatar::MAX_FILE_BYTES as usize + 1, 0);
    let large = scratch.write("large.png", &large);
    assert_eq!(avatar::decode_file(genkan(), &large), Err(Error::Size));

    for (name, bytes) in [
        ("garbage.png", b"not an image at all".as_slice()),
        (
            "picture.svg",
            b"<svg xmlns='http://www.w3.org/2000/svg'/>".as_slice(),
        ),
        ("truncated.png", &photo(ImageFormat::Png)[..200]),
    ] {
        let path = scratch.write(name, bytes);
        assert_eq!(
            avatar::decode_file(genkan(), &path),
            Err(Error::Rejected),
            "{name}"
        );
    }
}

/// Runs a decode whose executable is a script written moments earlier.
///
/// Another test thread's fork can briefly inherit that script's write
/// descriptor, and Linux refuses to execute a file open for writing
/// (`ETXTBSY`), which the parent reports as `Spawn`. Retry only that.
fn with_script<T: PartialEq + std::fmt::Debug>(
    run: impl Fn() -> Result<T, Error>,
) -> Result<T, Error> {
    for _ in 0..100 {
        match run() {
            Err(Error::Spawn) => std::thread::sleep(Duration::from_millis(10)),
            result => return result,
        }
    }
    run()
}

/// A shell script, using only built-ins because the decoder runs with an
/// empty environment and so no `PATH`, that writes `bytes` bytes of a
/// valid premultiplied pixel value and exits successfully.
fn writes(bytes: usize) -> String {
    format!(
        "i=0\nwhile [ $i -lt {} ]; do printf 0000000000000000; i=$((i+1)); done\nprintf '{}'",
        bytes / 16,
        "0".repeat(bytes % 16)
    )
}

#[test]
fn a_stuck_decoder_is_killed_at_the_timeout() {
    let scratch = Scratch::new();
    let png = scratch.write("photo.png", &photo(ImageFormat::Png));
    let stuck = scratch.script("stuck", "while :; do :; done");
    let started = Instant::now();
    assert_eq!(
        with_script(|| {
            let attempt = Instant::now();
            let result = avatar::decode_file(&stuck, &png);
            assert!(result != Err(Error::Timeout) || attempt.elapsed() >= avatar::TIMEOUT);
            result
        }),
        Err(Error::Timeout)
    );
    let elapsed = started.elapsed();
    assert!(elapsed >= avatar::TIMEOUT, "{elapsed:?}");
    assert!(
        elapsed < avatar::TIMEOUT + Duration::from_secs(2),
        "{elapsed:?}"
    );
}

#[test]
fn a_decoder_that_answers_with_the_wrong_length_is_rejected() {
    let scratch = Scratch::new();
    let png = scratch.write("photo.png", &photo(ImageFormat::Png));
    let endless = scratch.script("endless", "while :; do printf xxxxxxxxxxxxxxxx; done");
    assert_eq!(
        with_script(|| avatar::decode_file(&endless, &png)),
        Err(Error::Rejected)
    );
    let short = scratch.script("short", "printf xxxx");
    assert_eq!(
        with_script(|| avatar::decode_file(&short, &png)),
        Err(Error::Rejected)
    );
    let failing = scratch.script("failing", "exit 3");
    assert_eq!(
        with_script(|| avatar::decode_file(&failing, &png)),
        Err(Error::Rejected)
    );
    // A well-formed display-sized reply is accepted for display...
    let display = scratch.script("display-sized", &writes(PIXEL_BYTES));
    assert!(with_script(|| avatar::decode_file(&display, &png)).is_ok());
    // ...but import must expect 512px, not accept a display-sized reply, and
    // must reject one byte too many.
    assert_eq!(
        with_script(|| avatar::photo_png(&display, &png)),
        Err(Error::Rejected)
    );
    let import = 512 * 512 * 4;
    let exact = scratch.script("import-sized", &writes(import));
    assert!(with_script(|| avatar::photo_png(&exact, &png)).is_ok());
    let overlong = scratch.script("overlong", &writes(import + 1));
    assert_eq!(
        with_script(|| avatar::photo_png(&overlong, &png)),
        Err(Error::Rejected)
    );
}

#[test]
fn the_decoder_refuses_to_run_for_another_parent() {
    let mut child = Command::new(genkan())
        .args(["avatar-decode", "--parent-pid", "1"])
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("start the decoder");
    std::io::Write::write_all(child.stdin.as_mut().unwrap(), &photo(ImageFormat::Png)).ok();
    drop(child.stdin.take());
    let mut output = Vec::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_end(&mut output)
        .unwrap();
    assert!(!child.wait().unwrap().success());
    assert!(output.is_empty());
}

/// The number of seccomp filters a `/proc/<pid>/status` reports.
fn filters(status: &str) -> u64 {
    status
        .lines()
        .find_map(|line| line.strip_prefix("Seccomp_filters:"))
        .and_then(|count| count.trim().parse().ok())
        .expect("the kernel reports Seccomp_filters")
}

/// The decoder confines itself before reading its input, so while it waits on
/// an open pipe the kernel already reports its sandbox.
#[test]
fn the_decoder_is_confined_before_it_reads_input() {
    let mut child = Command::new(genkan())
        .arg("avatar-decode")
        .arg("--parent-pid")
        .arg(std::process::id().to_string())
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start the decoder");
    let proc = PathBuf::from(format!("/proc/{}", child.id()));
    // A build sandbox may already run this test under its own seccomp filter,
    // which the decoder inherits, so "Seccomp: 2" alone does not show that
    // the decoder has confined itself. Wait until it has one more filter than
    // this process; the filter is the last step of its confinement.
    let own_filters = filters(&std::fs::read_to_string("/proc/self/status").unwrap());
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        let status = std::fs::read_to_string(proc.join("status")).unwrap_or_default();
        if filters(&status) > own_filters || Instant::now() > deadline {
            break status;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(filters(&status) > own_filters, "{status}");
    let limits = std::fs::read_to_string(proc.join("limits")).unwrap_or_default();
    let descriptors = std::fs::read_dir(proc.join("fd")).map(Iterator::count);
    drop(child.stdin.take());
    let exited = child.wait().expect("reap the decoder");

    assert!(status.lines().any(|line| line == "Seccomp:\t2"), "{status}");
    assert!(
        status.lines().any(|line| line == "NoNewPrivs:\t1"),
        "{status}"
    );
    let limit = |name: &str| {
        limits
            .lines()
            .find(|line| line.starts_with(name))
            .map(|line| line.split_whitespace().collect::<Vec<_>>())
            .unwrap_or_default()
    };
    assert!(
        limit("Max address space").contains(&"268435456"),
        "{limits}"
    );
    assert!(limit("Max cpu time").contains(&"2"), "{limits}");
    assert!(limit("Max file size").contains(&"0"), "{limits}");
    assert!(limit("Max open files").contains(&"3"), "{limits}");
    assert!(limit("Max processes").contains(&"0"), "{limits}");
    if let Ok(descriptors) = descriptors {
        assert!(descriptors <= 3, "{descriptors} open descriptors");
    }
    // Empty input is not a picture.
    assert!(!exited.success());
}
