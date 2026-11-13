//! Explicit downloads. Playback never calls this module.

use std::fs::{self, File};
use std::io::{self, IsTerminal, Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use super::{
    catalog::{self, Asset},
    preflight, Result,
};

// Apple aerials use Apple's own root. Add it only to this curl invocation,
// only for this exact HTTPS host. Never disable peer or hostname validation.
pub(super) fn configure_tls(
    command: &mut Command,
    url: &str,
) -> io::Result<Option<tempfile::NamedTempFile>> {
    if !url.starts_with("https://sylvan.apple.com/") {
        return Ok(None);
    }
    let mut certificate = tempfile::NamedTempFile::new()?;
    certificate.write_all(include_bytes!("../../assets/wallpapers/apple-root.pem"))?;
    command.arg("--cacert").arg(certificate.path());
    Ok(Some(certificate))
}

struct Transfer(Child);

impl Drop for Transfer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Progress {
    terminal: bool,
    total: Option<u64>,
    started: Instant,
    last_draw: Instant,
}

impl Progress {
    fn draw(&mut self, received: u64, force: bool) {
        if !self.terminal || (!force && self.last_draw.elapsed() < Duration::from_millis(200)) {
            return;
        }
        self.last_draw = Instant::now();
        let speed = received as f64 / self.started.elapsed().as_secs_f64().max(0.001);
        let (bar, percent, total, eta) = if let Some(total) = self.total {
            let percent = if total == 0 {
                100
            } else {
                (received * 100 / total).min(100)
            };
            let filled = (percent * 16 / 100) as usize;
            let eta = if received >= total {
                "00:00".into()
            } else if speed > 0.0 {
                let seconds = ((total - received) as f64 / speed).ceil() as u64;
                format!("{:02}:{:02}", seconds / 60, seconds % 60)
            } else {
                "--:--".into()
            };
            (
                format!("{}{}", "#".repeat(filled), "-".repeat(16 - filled)),
                format!("{percent:>3}%"),
                format!("{:.1}", total as f64 / 1_048_576.0),
                eta,
            )
        } else {
            (
                "????????????????".into(),
                " --%".into(),
                "?".into(),
                "--:--".into(),
            )
        };
        // Progress is advisory: a closed terminal must not break installation.
        let mut output = io::stderr().lock();
        let _ = write!(
            output,
            "\r\x1b[2K[{bar}] {percent}  {:.1} / {total} MiB  {:.1} MiB/s  ETA {eta}",
            received as f64 / 1_048_576.0,
            speed / 1_048_576.0
        );
        let _ = output.flush();
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        if self.terminal {
            let _ = writeln!(io::stderr());
        }
    }
}

fn text<'a>(asset: &'a Asset, key: &str) -> Option<&'a str> {
    asset.metadata.get(key).and_then(toml::Value::as_str)
}

fn number(asset: &Asset, key: &str) -> Option<u64> {
    asset
        .metadata
        .get(key)
        .and_then(toml::Value::as_integer)
        .map(|n| n as u64)
}

fn identity(file: &mut File, max_bytes: u64) -> Result<(u64, String)> {
    file.rewind()?;
    let mut hash = Sha256::new();
    let mut bytes = 0;
    let mut buffer = [0; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
        bytes += count as u64;
        if bytes > max_bytes {
            return Err("media exceeds --max-bytes; no media installed".into());
        }
    }
    Ok((bytes, format!("{:x}", hash.finalize())))
}

fn verify(
    file: &mut File,
    bytes: Option<u64>,
    hash: Option<&str>,
    max_bytes: u64,
) -> Result<(u64, String)> {
    let observed = identity(file, max_bytes)?;
    if bytes.is_some_and(|bytes| bytes != observed.0) {
        return Err("download byte size differs from catalog metadata".into());
    }
    if hash.is_some_and(|hash| !hash.eq_ignore_ascii_case(&observed.1)) {
        return Err("download SHA-256 differs from catalog metadata".into());
    }
    Ok(observed)
}

fn fetch(url: &str, file: &mut File, max_bytes: u64, expected_size: Option<u64>) -> Result<()> {
    if !(url.starts_with("https://") || url.starts_with("http://"))
        || url.chars().any(char::is_control)
    {
        return Err("download source must be a direct HTTP(S) URL".into());
    }
    let headers = tempfile::NamedTempFile::new()?;
    let mut command = Command::new("curl");
    command
        .args([
            "--disable",
            "--globoff",
            "--silent",
            "--no-buffer",
            "--fail",
            "--proto",
            "=http,https",
            "--header",
            "Accept-Encoding: identity",
            "--max-time",
            "600",
            "--connect-timeout",
            "15",
            "--max-filesize",
            &max_bytes.to_string(),
            "--dump-header",
        ])
        .arg(headers.path())
        .arg("--url")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let _certificate = configure_tls(&mut command, url)?;
    let parent = std::process::id() as i32;
    // SAFETY: only async-signal-safe syscalls run between fork and exec.
    unsafe {
        command.pre_exec(move || {
            if crate::wallpaper::bind_to_parent(parent) {
                Ok(())
            } else {
                Err(io::Error::from_raw_os_error(libc::ECHILD))
            }
        });
    }
    let mut transfer = Transfer(command.spawn().map_err(|_| "curl is unavailable")?);
    let mut progress = Progress {
        terminal: io::stderr().is_terminal(),
        total: expected_size,
        started: Instant::now(),
        last_draw: Instant::now(),
    };
    progress.draw(0, true);
    let mut body = transfer.0.stdout.take().unwrap().take(max_bytes + 1);
    let mut received = 0;
    let mut buffer = [0; 65536];
    loop {
        let count = body.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        if received == 0 {
            // Curl flushes headers before its unbuffered body. Use the final
            // response's length, not a proxy/interim response or expanded ZIP size.
            let mut header = String::new();
            if headers
                .as_file()
                .take(65536)
                .read_to_string(&mut header)
                .is_ok()
            {
                progress.total = header
                    .rsplit("\r\n\r\n")
                    .find(|block| block.starts_with("HTTP/"))
                    .and_then(|block| {
                        block.lines().find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<u64>().ok())
                                .flatten()
                        })
                    })
                    .filter(|size| *size > 0)
                    .or(progress.total);
            }
        }
        file.write_all(&buffer[..count])?;
        received += count as u64;
        progress.draw(received, false);
    }
    if received > max_bytes {
        return Err("download exceeds --max-bytes; no media installed".into());
    }
    let status = transfer.0.wait()?;
    let headers = fs::read_to_string(headers.path())?;
    let response = headers
        .lines()
        .rfind(|line| line.starts_with("HTTP/"))
        .and_then(|line| line.split_whitespace().nth(1));
    if response.is_some_and(|code| code.starts_with('3')) {
        return Err(
            "download redirects are not followed; provide the final direct source URL".into(),
        );
    }
    if !status.success() || response != Some("200") {
        return Err(format!(
            "source download failed (HTTP {}, curl {}); no media installed",
            response.unwrap_or("unknown"),
            status.code().unwrap_or(-1)
        )
        .into());
    }
    progress.total = Some(received);
    progress.draw(received, true);
    if !progress.terminal {
        eprintln!(
            "Download complete: {:.1} MiB.",
            received as f64 / 1_048_576.0
        );
    }
    Ok(())
}

pub(super) fn run(asset: Asset, directory: &Path, max_bytes: u64) -> Result<std::path::PathBuf> {
    let local = text(&asset, "local_file");
    if asset.url.is_none() && local.is_none() {
        return Err("this asset has no source URL or local file".into());
    }
    let local_identity = local
        .map(|path| {
            let mut file = crate::wallpaper::open_wallpaper(Path::new(path))?;
            if file.metadata()?.len() > max_bytes {
                return Err("local file exceeds --max-bytes; raise the limit explicitly".into());
            }
            verify(&mut file, asset.bytes, text(&asset, "sha256"), max_bytes)
        })
        .transpose()?;
    fs::create_dir_all(directory)?;
    let lock = File::options()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(directory.join(".download.lock"))?;
    // Serialize publication and reuse decisions across all downloader processes.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    let objects = directory.join("objects");
    let entries = directory.join("entries");
    fs::create_dir_all(&objects)?;
    fs::create_dir_all(&entries)?;
    // Recover abandoned staging, not published generations: a preview may
    // still be opening the old generation after a receipt replacement.
    let mut referenced = std::collections::HashSet::new();
    for entry in fs::read_dir(&entries)? {
        let path = entry?.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "toml")
        {
            if let Some(name) = catalog::read_table(&path)?
                .get("cache_generation")
                .and_then(toml::Value::as_str)
            {
                referenced.insert(name.to_owned());
            }
        }
    }
    for entry in fs::read_dir(&objects)? {
        let entry = entry?;
        if entry.file_type()?.is_dir()
            && !entry.path().join("published").exists()
            && entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with("download-") && !referenced.contains(name))
        {
            fs::remove_dir_all(entry.path())?;
        }
    }
    let receipt_path = directory.join("entries").join(format!("{}.toml", asset.id));
    if receipt_path.is_file() {
        let receipt = catalog::read_table(&receipt_path)?;
        if receipt.get("url") == asset.metadata.get("url")
            && receipt.get("local_file") == asset.metadata.get("local_file")
            && local_identity.as_ref().is_none_or(|(_, hash)| {
                receipt.get("sha256").and_then(toml::Value::as_str) == Some(hash)
            })
        {
            let cached = Asset::from_entry(&asset.id, receipt)?;
            let declarations_match = ["format", "width", "height", "archive_member"]
                .into_iter()
                .all(|key| {
                    asset
                        .metadata
                        .get(key)
                        .is_none_or(|value| cached.metadata.get(key) == Some(value))
                })
                && text(&asset, "sha256").is_none_or(|hash| {
                    text(&cached, "sha256").is_some_and(|cached| cached.eq_ignore_ascii_case(hash))
                });
            if declarations_match && cached.bytes.is_some_and(|bytes| bytes > max_bytes) {
                return Err("cached media exceeds --max-bytes; raise the limit explicitly".into());
            }
            if let Some(path) = cached.path(directory).filter(|_| declarations_match) {
                let poster = path.with_file_name(format!("{}-poster.jpg", asset.id));
                if let Ok(mut file) = crate::wallpaper::open_wallpaper(&path) {
                    if verify(
                        &mut file,
                        asset.bytes.or(cached.bytes),
                        text(&asset, "sha256").or_else(|| text(&cached, "sha256")),
                        max_bytes,
                    )
                    .is_ok()
                        && (cached.format.as_deref() != Some("mov")
                            || crate::stable_file::open_regular(&poster).ok().is_some_and(
                                |mut poster| {
                                    verify(
                                        &mut poster,
                                        None,
                                        text(&cached, "poster_sha256"),
                                        u64::MAX,
                                    )
                                    .is_ok()
                                        && text(&cached, "poster_sha256").is_some()
                                },
                            ))
                    {
                        println!("Already cached {}: {}", asset.id, path.display());
                        return Ok(path);
                    }
                }
            }
        }
    }
    let archive = text(&asset, "archive_member");
    let expected_size = if archive.is_some() {
        number(&asset, "archive_byte_size")
    } else {
        asset.bytes
    };
    if expected_size.is_some_and(|size| size > max_bytes)
        || asset.bytes.is_some_and(|size| size > max_bytes)
    {
        return Err("catalog size exceeds --max-bytes; raise the limit explicitly".into());
    }
    let stage = tempfile::Builder::new()
        .prefix(&format!("download-{}-", asset.id))
        .tempdir_in(&objects)?;
    let mut input = asset.metadata.clone();
    input.insert("id".into(), asset.id.clone().into());
    if let Some((_, hash)) = local_identity {
        // Bind the worker's copy to the version used for the reuse decision.
        input.insert("sha256".into(), hash.into());
    }
    let mut command = Command::new(std::env::current_exe()?);
    command.env("TMPDIR", stage.path());
    command
        .args([
            "assets",
            "download-worker",
            "--max-bytes",
            &max_bytes.to_string(),
            "--stage",
        ])
        .arg(stage.path());
    eprintln!(
        "Downloading {} from its source; originals are not resized.",
        asset.id
    );
    let result = preflight::run_child(&mut command, &input, Duration::from_secs(700));
    // Cancellation kills the worker before its progress-line destructor runs.
    if result.is_err() && io::stderr().is_terminal() {
        eprintln!();
    }
    let (code, output) = result?;
    if code != 0 {
        return Err(format!("download validation failed: {output}").into());
    }
    let mut metadata = catalog::read_table(&stage.path().join("entry.toml"))?;
    metadata.insert(
        "cache_generation".into(),
        stage.path().file_name().unwrap().to_str().unwrap().into(),
    );
    let installed = Asset::from_entry(&asset.id, metadata.clone())?;
    let mut receipt = tempfile::NamedTempFile::new_in(&entries)?;
    receipt.write_all(metadata.to_string().as_bytes())?;
    receipt.as_file().sync_all()?;
    // Mark before switching the receipt so even a crash immediately after
    // publication cannot make the selected generation look disposable later.
    fs::write(stage.path().join("published"), [])?;
    // One atomic receipt switch publishes media, poster, and metadata together.
    // Before it, errors/cancellation remove staging and leave the old cache intact.
    let generation = stage.keep();
    if let Err(error) = receipt.persist(&receipt_path) {
        fs::remove_dir_all(generation)?;
        return Err(error.into());
    }
    println!(
        "Downloaded {}: {}\nSHA-256: {}\nPreview: genkan assets preview {}",
        asset.id,
        installed.path(directory).unwrap().display(),
        metadata["sha256"].as_str().unwrap(),
        asset.id
    );
    if text(&asset, "sha256").is_none() {
        println!("Computed hash recorded locally; no trusted upstream SHA-256 was supplied.");
    }
    Ok(installed.path(directory).unwrap())
}

pub(super) fn worker(stage: &Path, max_bytes: u64) -> Result<()> {
    preflight::confine(60)?;
    let mut input = String::new();
    io::stdin()
        .take(1024 * 1024 + 1)
        .read_to_string(&mut input)?;
    if input.len() > 1024 * 1024 {
        return Err("download metadata exceeds 1 MiB".into());
    }
    let metadata: toml::Table = input.parse()?;
    let id = metadata
        .get("id")
        .and_then(toml::Value::as_str)
        .ok_or("missing download key")?
        .to_owned();
    let asset = Asset::from_entry(&id, metadata)?;
    let archive = text(&asset, "archive_member");
    let expected_size = if archive.is_some() {
        number(&asset, "archive_byte_size")
    } else {
        asset.bytes
    };
    let media_path = stage.join("media");
    let source_path = stage.join("source");
    let mut source = File::options()
        .create_new(true)
        .read(true)
        .write(true)
        .open(&source_path)?;
    if let Some(path) = text(&asset, "local_file") {
        let input = crate::wallpaper::open_wallpaper(Path::new(path))?;
        if io::copy(&mut input.take(max_bytes + 1), &mut source)? > max_bytes {
            return Err("local file exceeds --max-bytes; no media installed".into());
        }
    } else {
        fetch(
            asset.url.as_deref().ok_or("missing download URL")?,
            &mut source,
            expected_size.unwrap_or(max_bytes).min(max_bytes),
            expected_size,
        )?;
    }
    eprintln!(
        "Validating {}: checking integrity and decoding media...",
        asset.id
    );
    if let Some(member) = archive {
        verify(
            &mut source,
            expected_size,
            text(&asset, "archive_sha256"),
            max_bytes,
        )?;
        source.seek(SeekFrom::Start(0))?;
        let mut zip = zip::ZipArchive::new(source)?;
        let mut member = zip.by_name(member)?;
        if member.is_dir() || member.size() > max_bytes {
            return Err("archive member is not media or exceeds --max-bytes".into());
        }
        let mut output = File::create(&media_path)?;
        // Write only the named member into our own filename, never unpack paths.
        if io::copy(&mut member.by_ref().take(max_bytes + 1), &mut output)? > max_bytes {
            return Err("expanded archive exceeds --max-bytes".into());
        }
        drop(member);
        drop(zip);
        fs::remove_file(&source_path)?;
    } else {
        drop(source);
        fs::rename(&source_path, &media_path)?;
    }
    let (bytes, hash) = verify(
        &mut File::open(&media_path)?,
        asset.bytes,
        text(&asset, "sha256"),
        max_bytes,
    )?;
    let poster_path = stage.join(format!("{}-poster.jpg", asset.id));
    let mut metadata = asset.metadata.clone();
    metadata.extend(preflight::validate(&asset, &media_path, &poster_path)?);
    metadata.insert("byte_size".into(), i64::try_from(bytes)?.into());
    metadata.insert("sha256".into(), hash.clone().into());
    if poster_path.is_file() {
        metadata.insert(
            "poster_sha256".into(),
            identity(&mut File::open(poster_path)?, u64::MAX)?.1.into(),
        );
    }
    let format = metadata["format"]
        .as_str()
        .ok_or("missing decoded format")?;
    fs::rename(media_path, stage.join(format!("{}.{format}", asset.id)))?;
    fs::write(stage.join("entry.toml"), metadata.to_string())?;
    Ok(())
}
