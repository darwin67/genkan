//! Asset discovery and rejection through the public CLI, without a display.

#![cfg(feature = "gui")]

use std::path::Path;
use std::process::Command;

fn genkan(directory: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_genkan"));
    command
        .env("GENKAN_WALLPAPER_DIR", directory)
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("GREETD_SOCK")
        .env(
            "DBUS_SYSTEM_BUS_ADDRESS",
            "unix:path=/nonexistent-genkan-assets-test-bus",
        );
    command
}

#[test]
fn list_reports_local_availability_and_preview_rejects_missing_or_unknown_ids() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir(directory.path().join("mov")).unwrap();
    // Availability does not decode the file; listing is cheap and display-free.
    std::fs::write(directory.path().join("mov/tahoe-beach.mov"), []).unwrap();
    let output = genkan(directory.path())
        .args(["assets", "list"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    let listing = String::from_utf8(output.stdout).unwrap();
    assert!(listing.contains("tahoe-beach\tMOV\t3840x2160\t162.3\tinstalled"));
    assert!(listing.contains("iceland\tMOV\t3840x2160\t754.1\tmissing"));
    assert!(listing.contains("palau-jellies\tMOV\t3840x2160\t637.0\tmissing"));
    assert!(listing.contains("tahoe-time-of-day\tHEIC\t6016x3384\t36.3\tmissing"));
    assert!(listing.contains("big-sur-1\tHEIC\t5120x2880\t89.6\tmissing"));

    for id in ["sequoia-night", "tahoe-time-of-day"] {
        let output = genkan(directory.path())
            .args(["assets", "preview", id])
            .output()
            .unwrap();
        assert!(!output.status.success());
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(error.contains("is not installed at"), "{error}");
        assert!(
            error.contains(directory.path().to_str().unwrap()),
            "{error}"
        );
    }
    for id in [
        "missing",
        "../tahoe-beach",
        "/tmp/movie.mov",
        "https://example.com/movie.mov",
    ] {
        let output = genkan(directory.path())
            .args(["assets", "preview", id])
            .output()
            .unwrap();
        assert!(!output.status.success());
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(error.contains("unknown wallpaper"), "{error}");
        assert!(error.contains("genkan assets list"), "{error}");
    }

    let entries = directory.path().join("entries");
    std::fs::create_dir(&entries).unwrap();
    let receipt = entries.join("mine.toml");
    for symlink in [false, true] {
        let fifo = directory.path().join("receipt.fifo");
        rustix::fs::mkfifoat(
            rustix::fs::CWD,
            &fifo,
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        )
        .unwrap();
        if symlink {
            std::os::unix::fs::symlink(&fifo, &receipt).unwrap();
        } else {
            std::fs::rename(&fifo, &receipt).unwrap();
        }
        for catalog in [false, true] {
            let mut command = genkan(directory.path());
            command.args(["assets", "list"]);
            if catalog {
                command.arg("--catalog").arg(&receipt);
            }
            let output = output_with_timeout(&mut command);
            assert!(!output.status.success());
            assert!(String::from_utf8(output.stderr)
                .unwrap()
                .contains("not a readable regular file"));
        }
        std::fs::remove_file(&receipt).unwrap();
    }
}

/// Tail metadata spans several range pages beyond an 8 MiB padding box. This
/// requires seeking without downloading everything or truncating decoder reads.
fn movie_with_tail_metadata() -> Vec<u8> {
    let source = include_bytes!("fixtures/asset-preflight/hevc.mov");
    let mut output = Vec::new();
    let mut offset = 0;
    while offset < source.len() {
        let size = u32::from_be_bytes(source[offset..offset + 4].try_into().unwrap()) as usize;
        assert!(size >= 8);
        if &source[offset + 4..offset + 8] == b"moov" {
            let padding = 8 * 1024 * 1024u32;
            output.extend_from_slice(&padding.to_be_bytes());
            output.extend_from_slice(b"free");
            output.resize(output.len() + padding as usize - 8, 0);
            let metadata_padding = 3 * 65536u32;
            output.extend_from_slice(&(size as u32 + metadata_padding).to_be_bytes());
            output.extend_from_slice(&source[offset + 4..offset + size]);
            output.extend_from_slice(&metadata_padding.to_be_bytes());
            output.extend_from_slice(b"free");
            output.resize(output.len() + metadata_padding as usize - 8, 0);
        } else {
            output.extend_from_slice(&source[offset..offset + size]);
        }
        offset += size;
    }
    output
}

struct SourceServer {
    address: std::net::SocketAddr,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    served: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl SourceServer {
    fn new(bytes: Vec<u8>) -> Self {
        use std::io::{BufRead, BufReader, Write};
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use std::sync::Arc;
        use std::time::Duration;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let served = Arc::new(AtomicUsize::new(0));
        let stopping = stop.clone();
        let sent = served.clone();
        let thread = std::thread::spawn(move || {
            while !stopping.load(Ordering::Relaxed) {
                let Ok((mut stream, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_millis(500)))
                    .unwrap();
                let mut lines = BufReader::new(&stream).lines();
                let request = lines.next().unwrap().unwrap();
                let path = request.split_whitespace().nth(1).unwrap();
                let mut range = None;
                for line in lines {
                    let line = line.unwrap();
                    if line.is_empty() {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("range: bytes=") {
                        let (start, end) = value.split_once('-').unwrap();
                        range = Some((
                            start.parse::<usize>().unwrap(),
                            end.parse::<usize>().unwrap(),
                        ));
                    }
                }
                let (start, end) = range.unwrap_or((0, bytes.len() - 1));
                assert!(start < bytes.len());
                let end = end.min(bytes.len() - 1);
                let (status, body) = if path == "/redirect" {
                    ("302 Found", &bytes[..])
                } else if path == "/ignore" || range.is_none() {
                    ("200 OK", &bytes[..])
                } else {
                    ("206 Partial Content", &bytes[start..=end])
                };
                let tag = if path == "/no-etag" {
                    String::new()
                } else if path == "/changing" && start != 0 {
                    "ETag: \"changed\"\r\n".into()
                } else {
                    "ETag: \"fixture-v1\"\r\n".into()
                };
                let reported_start = if path == "/bad-range" {
                    start + 1
                } else {
                    start
                };
                let connection = if path == "/redirect" {
                    "Location: /ok\r\nConnection: keep-alive"
                } else {
                    "Connection: close"
                };
                let header = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Range: bytes {reported_start}-{end}/{}\r\n{tag}{connection}\r\n\r\n", body.len(), bytes.len());
                if stream.write_all(header.as_bytes()).is_err() {
                    continue;
                }
                if path == "/slow" && start != 0 {
                    // Delay media beyond the first page until after the
                    // demuxer has announced both video tracks.
                    std::thread::sleep(Duration::from_millis(300));
                }
                let body = if path == "/truncated" {
                    &body[..body.len() / 2]
                } else {
                    body
                };
                let mut offset = 0;
                while offset < body.len() {
                    match stream.write(&body[offset..]) {
                        Ok(0) | Err(_) => break,
                        Ok(count) => {
                            offset += count;
                            sent.fetch_add(count, Ordering::Relaxed);
                        }
                    }
                }
            }
        });
        Self {
            address,
            stop,
            served,
            thread: Some(thread),
        }
    }

    fn url(&self, mode: &str) -> String {
        format!("http://{}{mode}", self.address)
    }
}

impl Drop for SourceServer {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        self.thread.take().unwrap().join().unwrap();
    }
}

fn output_with_timeout(command: &mut Command) -> std::process::Output {
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("asset command did not finish: {command:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    child.wait_with_output().unwrap()
}

fn successful(command: &mut Command) -> String {
    let output = output_with_timeout(command);
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).unwrap()
}

fn cached_media(directory: &Path, key: &str) -> std::path::PathBuf {
    let receipt: toml::Table =
        std::fs::read_to_string(directory.join(format!("entries/{key}.toml")))
            .unwrap()
            .parse()
            .unwrap();
    directory
        .join("objects")
        .join(receipt["cache_generation"].as_str().unwrap())
        .join(format!("{key}.{}", receipt["format"].as_str().unwrap()))
}

#[test]
fn custom_download_preserves_originals_generates_poster_and_is_rediscovered_offline() {
    let directory = tempfile::tempdir().unwrap();
    let bytes = include_bytes!("fixtures/asset-preflight/hevc.mov");
    let server = SourceServer::new(bytes.to_vec());
    let url = server.url("/no-etag");
    let output =
        successful(genkan(directory.path()).args(["assets", "download", "mine", "--url", &url]));
    assert!(output.contains("Computed hash recorded locally"));
    assert_eq!(
        std::fs::read(cached_media(directory.path(), "mine")).unwrap(),
        bytes
    );
    let poster =
        image::open(cached_media(directory.path(), "mine").with_file_name("mine-poster.jpg"))
            .unwrap();
    assert_eq!((poster.width(), poster.height()), (96, 64));
    assert!(
        successful(genkan(directory.path()).args(["assets", "list"]))
            .contains("mine\tMOV\t96x64\t0.0\tinstalled")
    );
    let receipt: toml::Table = std::fs::read_to_string(directory.path().join("entries/mine.toml"))
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(
        receipt["sha256"].as_str().unwrap(),
        "97bafd684fc1f48ecaafe71dcbc220b5ba2b33fc24e7b16f0d448434cf6269f6"
    );
    drop(server);
    assert!(
        successful(genkan(directory.path()).args(["assets", "download", "mine"]))
            .contains("Already cached mine")
    );
}

#[test]
fn zip_download_extracts_only_the_selected_heic_and_checks_archive_and_media_pins() {
    use sha2::{Digest, Sha256};
    use std::io::{Cursor, Write};
    let directory = tempfile::tempdir().unwrap();
    let heic = include_bytes!("fixtures/dynamic-heic/synthetic-all-properties.heic");
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in [
        ("../not-extracted", &b"do not unpack"[..]),
        ("selected.heic", &heic[..]),
    ] {
        zip.start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(bytes).unwrap();
    }
    let bytes = zip.finish().unwrap().into_inner();
    let server = SourceServer::new(bytes.clone());
    let catalog = directory.path().join("catalog.toml");
    let metadata = format!("[assets.dynamic]\nurl = {:?}\nformat = 'heic'\nwidth = 8\nheight = 8\nbyte_size = {}\nsha256 = '{:x}'\narchive_member = 'selected.heic'\narchive_byte_size = {}\narchive_sha256 = '{:x}'\n", server.url("/ok"), heic.len(), Sha256::digest(heic), bytes.len(), Sha256::digest(&bytes));
    std::fs::write(&catalog, &metadata).unwrap();
    successful(
        genkan(directory.path())
            .args(["assets", "download", "dynamic", "--catalog"])
            .arg(&catalog),
    );
    assert_eq!(
        std::fs::read(cached_media(directory.path(), "dynamic")).unwrap(),
        heic
    );
    assert!(!directory.path().join("not-extracted").exists());
    assert!(successful(
        genkan(directory.path())
            .args(["assets", "list", "--catalog"])
            .arg(&catalog)
    )
    .contains("dynamic\tHEIC\t8x8"));
    // Force source revalidation under a new key: a bad archive pin must fail
    // before extraction, and a correct archive with a bad media pin must fail too.
    for field in ["archive_sha256", "sha256"] {
        let mut table: toml::Table = metadata.parse().unwrap();
        let mut entry = table["assets"]["dynamic"].as_table().unwrap().clone();
        entry.insert(field.into(), "0".repeat(64).into());
        table["assets"]
            .as_table_mut()
            .unwrap()
            .insert("bad".into(), entry.into());
        std::fs::write(&catalog, table.to_string()).unwrap();
        let output = genkan(directory.path())
            .args(["assets", "download", "bad", "--catalog"])
            .arg(&catalog)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("SHA-256"),
            "{output:?}"
        );
        assert!(!directory.path().join("heic/bad.heic").exists());
    }
}

#[test]
fn failed_downloads_preserve_existing_media_and_receipt() {
    let directory = tempfile::tempdir().unwrap();
    let bytes = include_bytes!("fixtures/asset-preflight/hevc.mov");
    let server = SourceServer::new(bytes.to_vec());
    successful(genkan(directory.path()).args([
        "assets",
        "download",
        "mine",
        "--url",
        &server.url("/ok"),
    ]));
    let receipt = std::fs::read(directory.path().join("entries/mine.toml")).unwrap();
    let html = SourceServer::new(b"<!doctype html>not media".to_vec());
    for (url, flags) in [
        (
            server.url("/ok"),
            vec!["--sha256".to_owned(), "0".repeat(64)],
        ),
        (server.url("/redirect"), vec![]),
        (server.url("/truncated"), vec![]),
        (html.url("/ok"), vec![]),
    ] {
        let output = genkan(directory.path())
            .args(["assets", "download", "mine", "--url", &url])
            .args(flags)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{output:?}");
        assert_eq!(
            std::fs::read(cached_media(directory.path(), "mine")).unwrap(),
            bytes
        );
        assert_eq!(
            std::fs::read(directory.path().join("entries/mine.toml")).unwrap(),
            receipt
        );
    }
    let large = SourceServer::new(vec![0; 100_000]);
    assert!(!genkan(directory.path())
        .args([
            "assets",
            "download",
            "large",
            "--url",
            &large.url("/ok"),
            "--max-bytes",
            "4096"
        ])
        .output()
        .unwrap()
        .status
        .success());
    assert!(!directory.path().join("entries/large.toml").exists());
    for key in ["../escape", "tahoe-beach"] {
        assert!(!genkan(directory.path())
            .args(["assets", "download", key, "--url", &server.url("/ok")])
            .output()
            .unwrap()
            .status
            .success());
    }
}

#[test]
fn publication_failure_preserves_the_previous_complete_generation() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let first = SourceServer::new(include_bytes!("fixtures/asset-preflight/hevc.mov").to_vec());
    let second = SourceServer::new(include_bytes!("fixtures/asset-preflight/mixed.mov").to_vec());
    successful(genkan(directory.path()).args([
        "assets",
        "download",
        "mine",
        "--url",
        &first.url("/ok"),
    ]));
    let old = cached_media(directory.path(), "mine");
    let original = std::fs::read(&old).unwrap();
    let receipt = std::fs::read(directory.path().join("entries/mine.toml")).unwrap();
    let entries = directory.path().join("entries");
    std::fs::set_permissions(&entries, std::fs::Permissions::from_mode(0o555)).unwrap();
    let result = genkan(directory.path())
        .args(["assets", "download", "mine", "--url", &second.url("/ok")])
        .output()
        .unwrap();
    std::fs::set_permissions(&entries, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!result.status.success(), "{result:?}");
    assert_eq!(cached_media(directory.path(), "mine"), old);
    assert_eq!(std::fs::read(&old).unwrap(), original);
    assert_eq!(
        std::fs::read(directory.path().join("entries/mine.toml")).unwrap(),
        receipt
    );
    successful(genkan(directory.path()).args([
        "assets",
        "download",
        "mine",
        "--url",
        &second.url("/ok"),
    ]));
    let current = cached_media(directory.path(), "mine");
    assert_ne!(current, old);
    assert_eq!(
        std::fs::read(&current).unwrap(),
        include_bytes!("fixtures/asset-preflight/mixed.mov")
    );
    // Corrupt poster detection must not permanently report a broken cache as
    // reusable. Regeneration publishes another complete generation.
    std::fs::write(current.with_file_name("mine-poster.jpg"), []).unwrap();
    assert!(
        !successful(genkan(directory.path()).args(["assets", "download", "mine"]))
            .contains("Already cached")
    );
    assert!(
        image::open(cached_media(directory.path(), "mine").with_file_name("mine-poster.jpg"))
            .is_ok()
    );
    for symlink in [false, true] {
        let poster = cached_media(directory.path(), "mine").with_file_name("mine-poster.jpg");
        std::fs::remove_file(&poster).unwrap();
        let fifo = poster.with_extension("fifo");
        rustix::fs::mkfifoat(
            rustix::fs::CWD,
            &fifo,
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        )
        .unwrap();
        if symlink {
            std::os::unix::fs::symlink(&fifo, &poster).unwrap();
        } else {
            std::fs::rename(&fifo, &poster).unwrap();
        }
        assert!(
            !successful(genkan(directory.path()).args(["assets", "download", "mine"]))
                .contains("Already cached")
        );
        assert!(image::open(
            cached_media(directory.path(), "mine").with_file_name("mine-poster.jpg")
        )
        .is_ok());
    }
    let heic = SourceServer::new(
        include_bytes!("fixtures/dynamic-heic/synthetic-all-properties.heic").to_vec(),
    );
    successful(genkan(directory.path()).args([
        "assets",
        "download",
        "mine",
        "--url",
        &heic.url("/ok"),
    ]));
    successful(genkan(directory.path()).args(["assets", "download", "mine"]));
    assert!(
        successful(genkan(directory.path()).args(["assets", "list"]))
            .contains("mine\tHEIC\t8x8\t0.0\tinstalled")
    );
    assert_eq!(
        std::fs::read(&old).unwrap(),
        original,
        "in-flight consumers must retain their selected generation across later downloads"
    );
    assert_eq!(
        std::fs::read(current).unwrap(),
        include_bytes!("fixtures/asset-preflight/mixed.mov")
    );
}

#[test]
fn cancelled_downloads_remove_partial_files_and_recover_after_a_crash() {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    let directory = tempfile::tempdir().unwrap();
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGKILL] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut child = genkan(directory.path())
            .args([
                "assets",
                "download",
                "cancel",
                "--url",
                &format!("http://{}/stall", listener.local_addr().unwrap()),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut stream = loop {
            if let Ok((stream, _)) = listener.accept() {
                break stream;
            }
            if Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("no download request");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        for line in BufReader::new(&stream).lines() {
            if line.unwrap().is_empty() {
                break;
            }
        }
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3000000\r\n\r\n")
            .unwrap();
        stream.write_all(&[42; 65536]).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(unsafe { libc::kill(child.id() as i32, signal) }, 0);
        assert!(!child.wait().unwrap().success());
        let closed = stream.read(&mut [0]);
        assert!(
            matches!(closed, Ok(0))
                || closed.is_err_and(|error| error.kind() == std::io::ErrorKind::ConnectionReset)
        );
        if signal != libc::SIGKILL {
            assert_eq!(
                std::fs::read_dir(directory.path().join("objects"))
                    .unwrap()
                    .count(),
                0
            );
        }
        assert!(!directory.path().join("entries/cancel.toml").exists());
    }
    let healthy = SourceServer::new(include_bytes!("fixtures/asset-preflight/hevc.mov").to_vec());
    successful(genkan(directory.path()).args([
        "assets",
        "download",
        "healthy",
        "--url",
        &healthy.url("/ok"),
    ]));
    assert_eq!(
        std::fs::read_dir(directory.path().join("objects"))
            .unwrap()
            .count(),
        1,
        "abandoned crash staging must be reclaimed under the download lock"
    );
}

#[test]
fn downloads_and_listing_share_home_xdg_and_override_cache_resolution() {
    let directory = tempfile::tempdir().unwrap();
    let server = SourceServer::new(
        include_bytes!("fixtures/dynamic-heic/synthetic-all-properties.heic").to_vec(),
    );
    for (mode, suffix) in [
        ("home", ".cache/genkan/wallpapers"),
        ("xdg", "xdg/genkan/wallpapers"),
        ("override", "override"),
    ] {
        let home = directory.path().join(mode);
        let mut download = genkan(directory.path());
        download
            .env_remove("GENKAN_WALLPAPER_DIR")
            .env_remove("XDG_CACHE_HOME")
            .env("HOME", &home);
        if mode == "xdg" {
            download.env("XDG_CACHE_HOME", home.join("xdg"));
        }
        if mode == "override" {
            download.env("GENKAN_WALLPAPER_DIR", home.join("override"));
        }
        successful(download.args(["assets", "download", mode, "--url", &server.url("/ok")]));
        let cache = home.join(suffix);
        assert!(cached_media(&cache, mode).is_file());
        assert!(successful(genkan(&cache).args(["assets", "list"]))
            .contains(&format!("{mode}\tHEIC\t8x8\t0.0\tinstalled")));
    }
}

fn check_preflight(command: &mut Command, code: i32, expected: &[&str]) -> String {
    let output = command.output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        output.status.code(),
        Some(code),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for text in expected {
        assert!(stdout.contains(text), "missing {text:?}: {stdout}");
    }
    stdout
}

#[test]
fn preflight_decodes_a_movie_with_remote_tail_metadata_without_full_download() {
    let directory = tempfile::tempdir().unwrap();
    let source = SourceServer::new(movie_with_tail_metadata());
    let catalog = directory.path().join("sources.toml");
    std::fs::write(&catalog, format!("[assets.mountains]\nurl = {:?}\nname = 'My Mountains'\nformat = 'mov'\nwidth = 96\nheight = 64\nrights = 'Authored test pattern'\n", source.url("/ok"))).unwrap();
    let listing = genkan(directory.path())
        .args(["assets", "list", "--catalog"])
        .arg(&catalog)
        .output()
        .unwrap();
    assert!(listing.status.success());
    assert!(String::from_utf8(listing.stdout)
        .unwrap()
        .contains("mountains\tMOV\t96x64\t?\tmissing\tMy Mountains"));
    assert_eq!(
        source.served.load(std::sync::atomic::Ordering::Relaxed),
        0,
        "listing must remain offline"
    );
    check_preflight(
        genkan(directory.path())
            .args(["assets", "preflight", "mountains", "--catalog"])
            .arg(&catalog)
            .args(["--max-bytes", "393216"]),
        0,
        &[
            "PREFLIGHT PASSED mountains",
            "96x64",
            "MOV HEVC frame decoded",
            "SHA-256 and uninspected frames are not verified",
        ],
    );
    assert!(source.served.load(std::sync::atomic::Ordering::Relaxed) <= 393216);
    assert!(
        !directory.path().join("mov").exists(),
        "preflight must not install media"
    );
    std::fs::write(
        &catalog,
        format!(
            "[assets.mountains]\nurl = {:?}\nwidth = 97\n",
            source.url("/ok")
        ),
    )
    .unwrap();
    check_preflight(
        genkan(directory.path())
            .args(["assets", "preflight", "mountains", "--catalog"])
            .arg(&catalog),
        2,
        &[
            "UNSUPPORTED",
            "decoded 96x64 differs from catalog dimensions",
        ],
    );
}

#[test]
fn user_key_and_url_need_no_curated_entry_and_decode_heic_metadata_and_image() {
    let directory = tempfile::tempdir().unwrap();
    let mut heic = include_bytes!("fixtures/dynamic-heic/synthetic-all-properties.heic").to_vec();
    // Additional unreferenced media must not be fetched just to decode the
    // primary image and parse its real dynamic-wallpaper metadata.
    let padding = 8 * 1024 * 1024u32;
    heic.extend_from_slice(&padding.to_be_bytes());
    heic.extend_from_slice(b"mdat");
    heic.resize(heic.len() + padding as usize - 8, 0);
    let source = SourceServer::new(heic);
    check_preflight(
        genkan(directory.path()).args([
            "assets",
            "preflight",
            "my-own-key",
            "--url",
            &source.url("/ok"),
            "--max-bytes",
            "131072",
        ]),
        0,
        &[
            "PREFLIGHT PASSED my-own-key",
            "8x8",
            "4 images; dynamic schedule: true",
            "HEIC primary image decoded",
        ],
    );
    assert!(source.served.load(std::sync::atomic::Ordering::Relaxed) <= 131072);
    let unsupported = SourceServer::new(
        include_bytes!("fixtures/dynamic-heic/synthetic-icc-unsupported.heic").to_vec(),
    );
    check_preflight(
        genkan(directory.path()).args([
            "assets",
            "preflight",
            "bad-color",
            "--url",
            &unsupported.url("/ok"),
        ]),
        2,
        &["UNSUPPORTED bad-color", "color profile"],
    );
    check_preflight(
        genkan(directory.path()).args([
            "assets",
            "preflight",
            "bad-scheme",
            "--url",
            "file:///etc/passwd",
        ]),
        2,
        &["UNSUPPORTED", "HTTP(S)"],
    );
}

#[test]
fn preflight_is_inconclusive_when_ranges_versions_or_budgets_prevent_a_sound_verdict() {
    let directory = tempfile::tempdir().unwrap();
    let source = SourceServer::new(movie_with_tail_metadata());
    for (mode, reason) in [
        ("/ignore", "did not honor"),
        ("/bad-range", "does not match"),
        ("/truncated", "range transfer failed"),
        ("/changing", "version changed"),
        ("/no-etag", "no strong ETag"),
    ] {
        check_preflight(
            genkan(directory.path()).args([
                "assets",
                "preflight",
                "custom",
                "--url",
                &source.url(mode),
                "--max-bytes",
                "393216",
            ]),
            3,
            &["INCONCLUSIVE custom", reason, "No asset installed"],
        );
    }
    check_preflight(
        genkan(directory.path()).args([
            "assets",
            "preflight",
            "custom",
            "--url",
            &source.url("/ok"),
            "--max-bytes",
            "4096",
        ]),
        3,
        &[
            "INCONCLUSIVE",
            "budget exhausted",
            "Media bytes consumed: 4096 / 4096",
        ],
    );
}

#[test]
fn preflight_does_not_drain_large_redirect_bodies_or_follow_them() {
    let directory = tempfile::tempdir().unwrap();
    let bytes = movie_with_tail_metadata();
    let total = bytes.len();
    let source = SourceServer::new(bytes);
    check_preflight(
        genkan(directory.path()).args([
            "assets",
            "preflight",
            "redirect",
            "--url",
            &source.url("/redirect"),
            "--max-bytes",
            "4096",
        ]),
        3,
        &[
            "INCONCLUSIVE redirect",
            "redirects are not followed",
            "Media bytes consumed: 0 / 4096",
        ],
    );
    // Server writes can enter the TCP buffer before curl closes, but the
    // full redirect body must not be drained outside the inspection budget.
    assert!(source.served.load(std::sync::atomic::Ordering::Relaxed) < total);
}

#[test]
fn preflight_selects_hevc_even_when_an_unrelated_video_track_follows_it() {
    let directory = tempfile::tempdir().unwrap();
    let source = SourceServer::new(include_bytes!("fixtures/asset-preflight/mixed.mov").to_vec());
    check_preflight(
        genkan(directory.path()).args([
            "assets",
            "preflight",
            "mixed",
            "--url",
            &source.url("/slow"),
        ]),
        0,
        &["PREFLIGHT PASSED mixed", "96x64", "MOV HEVC frame decoded"],
    );
}

#[test]
fn interrupting_preflight_closes_an_inflight_network_request() {
    use std::io::{BufRead, BufReader, Read};
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    let directory = tempfile::tempdir().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}/stall", listener.local_addr().unwrap());
    let mut command = genkan(directory.path());
    command
        .args(["assets", "preflight", "cancel", "--url", &url])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // The test runner may inherit ignored SIGINT; give this CLI the normal
    // terminal disposition so interruption exercises the public behavior.
    unsafe {
        command.pre_exec(|| {
            libc::signal(libc::SIGINT, libc::SIG_DFL);
            Ok(())
        });
    }
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut stream = loop {
        if let Ok((stream, _)) = listener.accept() {
            break stream;
        }
        if Instant::now() >= deadline || child.try_wait().unwrap().is_some() {
            let _ = child.kill();
            let _ = child.wait();
            panic!("preflight did not request the source");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    for line in BufReader::new(&stream).lines() {
        if line.unwrap().is_empty() {
            break;
        }
    }
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGINT) }, 0);
    assert!(!child.wait().unwrap().success());
    let result = stream.read(&mut [0]);
    assert!(
        matches!(result, Ok(0))
            || result.is_err_and(|error| error.kind() == std::io::ErrorKind::ConnectionReset),
        "the request must close promptly after cancellation, not run until its network timeout"
    );
}
