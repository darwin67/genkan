//! Owns only the greeter lifetime. greetd still authenticates and starts sessions.

use std::ffi::OsString;
use std::io::{self, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use clap::{Args, ValueEnum};
use tokio::process::{Child, Command};
use tokio::signal::unix::{signal, SignalKind};

use crate::{parse_output_name, parse_wallpaper_file, wallpaper, WallpaperAppearance};

#[derive(Debug, Args)]
pub struct Arguments {
    #[arg(long, value_enum, default_value = "tahoe-beach")]
    wallpaper: wallpaper::Catalog,
    #[arg(long, value_parser = parse_wallpaper_file)]
    wallpaper_file: Option<std::path::PathBuf>,
    #[arg(long)]
    reduce_motion: bool,
    #[arg(long, value_enum, default_value = "automatic")]
    appearance: WallpaperAppearance,
    #[arg(long, value_parser = parse_output_name)]
    authentication_output: Option<String>,
}

/// Keep an IPC connection before spawning anything, so early errors and unwinds
/// also quit niri. Quit closes the compositor; no response is required.
struct QuitOnDrop(UnixStream);

impl Drop for QuitOnDrop {
    fn drop(&mut self) {
        if self
            .0
            .write_all(b"{\"Action\":{\"Quit\":{\"skip_confirmation\":true}}}\n")
            .is_err()
        {
            eprintln!("genkan greeter-session: could not ask niri to quit");
        }
    }
}

pub fn run(arguments: Arguments) -> Result<(), Box<dyn std::error::Error>> {
    let socket = std::env::var_os("NIRI_SOCKET")
        .ok_or_else(|| io::Error::other("greeter-session requires NIRI_SOCKET"))?;
    let socket = UnixStream::connect(socket)?;
    socket.set_write_timeout(Some(Duration::from_secs(1)))?;
    let _quit = QuitOnDrop(socket);
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(supervise(arguments))?;
    Ok(())
}

async fn supervise(arguments: Arguments) -> io::Result<()> {
    // Install handlers before children exist, including during their startup.
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    let executable = std::env::current_exe()?;
    let media = media_arguments(&arguments);
    let mut wallpaper = Command::new(&executable);
    wallpaper.arg("wallpaper").args(&media).kill_on_drop(true);
    if let Some(output) = &arguments.authentication_output {
        wallpaper.args(["--skip-output", output]);
    }
    let mut wallpaper = wallpaper.spawn()?;
    let mut login = Command::new(executable);
    login
        .args(["login", "--placement", "compositor"])
        .args(media)
        .kill_on_drop(true);
    let mut login = match login.spawn() {
        Ok(login) => login,
        Err(error) => {
            stop(&mut wallpaper).await;
            return Err(error);
        }
    };
    let outcome = tokio::select! {
        result = login.wait() => result.and_then(|status| {
            if status.success() { Ok(()) } else { Err(io::Error::other("login exited unsuccessfully")) }
        }),
        _ = terminate.recv() => Ok(()),
        _ = interrupt.recv() => Ok(()),
        _ = wallpaper.wait() => Err(io::Error::other("wallpaper exited unexpectedly")),
    };
    stop(&mut login).await;
    stop(&mut wallpaper).await;
    outcome
}

async fn stop(child: &mut Child) {
    // A wedged decoder or login must not delay releasing the greeter compositor.
    let _ = child.kill().await;
    let _ = child.wait().await;
}

fn media_arguments(arguments: &Arguments) -> Vec<OsString> {
    let mut media = vec![
        "--wallpaper".into(),
        arguments
            .wallpaper
            .to_possible_value()
            .unwrap()
            .get_name()
            .into(),
        "--appearance".into(),
        arguments
            .appearance
            .to_possible_value()
            .unwrap()
            .get_name()
            .into(),
    ];
    if let Some(path) = &arguments.wallpaper_file {
        media.extend(["--wallpaper-file".into(), path.as_os_str().to_owned()]);
    }
    if arguments.reduce_motion {
        media.push("--reduce-motion".into());
    }
    media
}
