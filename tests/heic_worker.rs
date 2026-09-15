//! End-to-end checks for the dynamic HEIC relay worker process.
//!
//! These run the real `genkan heic-worker` binary that `login` and `lock` spawn
//! for untrusted wallpapers, proving the child decodes frames and reports
//! bounded failures over the relay protocol.

use std::io::Read;
use std::process::{Child, Command, Stdio};

const FRAME_TAG: u8 = b'F';
const FAILED_TAG: u8 = b'E';

fn worker() -> Command {
    Command::new(env!("CARGO_BIN_EXE_genkan"))
}

fn spawn(arguments: &[&str]) -> Child {
    worker()
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the HEIC worker")
}

fn read_frame(reader: &mut impl Read) -> (u32, u32, Vec<u8>) {
    let mut tag = [0u8; 1];
    reader.read_exact(&mut tag).expect("frame tag");
    assert_eq!(tag[0], FRAME_TAG, "worker did not emit a frame");
    let mut header = [0u8; 12];
    reader.read_exact(&mut header).expect("frame header");
    let width = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
    let height = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
    let length = u32::from_le_bytes([header[8], header[9], header[10], header[11]]) as usize;
    assert_eq!(length, width as usize * height as usize * 4);
    let mut pixels = vec![0u8; length];
    reader.read_exact(&mut pixels).expect("frame pixels");
    (width, height, pixels)
}

#[test]
fn worker_decodes_a_scheduled_frame() {
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/dynamic-heic/synthetic-all-properties.heic"
    );
    let mut child = spawn(&[
        "heic-worker",
        "--file",
        fixture,
        "--appearance",
        "automatic",
        "--reduce-motion",
    ]);
    let mut stdout = child.stdout.take().expect("worker stdout");
    let (width, height, pixels) = read_frame(&mut stdout);
    assert_eq!((width, height), (8, 8));
    assert_eq!(pixels.len(), 8 * 8 * 4);
    assert!(
        pixels.chunks_exact(4).all(|pixel| pixel[3] == 255),
        "relayed frames must be opaque RGBA"
    );

    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn worker_reports_a_missing_file_as_a_bounded_failure() {
    let mut child = spawn(&[
        "heic-worker",
        "--file",
        "/nonexistent/genkan-missing-dynamic-wallpaper.heic",
        "--appearance",
        "automatic",
    ]);
    let mut stdout = child.stdout.take().expect("worker stdout");
    let mut tag = [0u8; 1];
    stdout.read_exact(&mut tag).expect("failure tag");
    assert_eq!(tag[0], FAILED_TAG);

    let _ = child.kill();
    let _ = child.wait();
}
