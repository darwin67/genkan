mod identity;

use std::os::fd::RawFd;

use genkan_session_lock::{Presentation, Refresh};
use thiserror::Error;

use crate::wallpaper;

#[derive(Debug, Clone)]
pub(crate) struct Config {
    pub(crate) wallpaper: wallpaper::Settings,
    pub(crate) ready_fd: Option<RawFd>,
    #[cfg(feature = "lock-test")]
    pub(crate) test_unlock_after_ready: bool,
}

#[derive(Debug, Error)]
pub(crate) enum Error {
    #[error("could not resolve lock identity: {0}")]
    Identity(#[from] identity::Error),
    #[error(transparent)]
    Runtime(#[from] genkan_session_lock::Error),
}

struct WallpaperPresentation(wallpaper::State);

impl Presentation for WallpaperPresentation {
    fn receive_latest(&mut self) -> Refresh {
        match self.0.receive_latest() {
            wallpaper::Refresh::Unchanged => Refresh::Unchanged,
            wallpaper::Refresh::Frame => Refresh::Frame,
            wallpaper::Refresh::Failed => Refresh::Failed,
        }
    }

    fn frame(&self) -> Option<genkan_session_lock::RgbaFrame> {
        self.0.rgba_frame()
    }
}

pub(crate) fn run(config: Config) -> Result<(), Error> {
    let identity = identity::Identity::current()?;
    let runtime_identity =
        genkan_session_lock::Identity::new(identity.uid, identity.username, identity.display_name);
    let presentation = WallpaperPresentation(wallpaper::State::start(config.wallpaper));
    let runtime = genkan_session_lock::Config::new(runtime_identity, presentation, config.ready_fd);
    #[cfg(feature = "lock-test")]
    let runtime = runtime.with_test_unlock_after_ready(config.test_unlock_after_ready);
    genkan_session_lock::run(runtime)?;
    Ok(())
}
