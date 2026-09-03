mod identity;

use std::os::fd::{FromRawFd, OwnedFd, RawFd};

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
    #[error("could not adopt readiness descriptor: {0}")]
    ReadyFd(#[source] std::io::Error),
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
    let ready_fd = config.ready_fd.map(adopt_ready_fd).transpose()?;
    let runtime_identity =
        genkan_session_lock::Identity::new(identity.uid, identity.username, identity.display_name);
    let presentation = WallpaperPresentation(wallpaper::State::start(config.wallpaper));
    let runtime = genkan_session_lock::Config::new(runtime_identity, presentation, ready_fd);
    #[cfg(feature = "lock-test")]
    let runtime = runtime.with_test_unlock_after_ready(config.test_unlock_after_ready);
    genkan_session_lock::run(runtime)?;
    Ok(())
}

fn adopt_ready_fd(fd: RawFd) -> Result<OwnedFd, Error> {
    // SAFETY: `fcntl` accepts an integer descriptor and reports EBADF without
    // assuming ownership. A non-negative result is a new owned descriptor.
    let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
    if duplicate < 0 {
        return Err(Error::ReadyFd(std::io::Error::last_os_error()));
    }
    // The CLI's inherited descriptor is explicitly transferred to Genkan.
    // Close it after successful duplication so only the typed owner remains.
    // SAFETY: successful duplication proves `fd` was open at this boundary.
    let close_result = unsafe { libc::close(fd) };
    if close_result != 0 {
        // SAFETY: `duplicate` is uniquely owned and converted exactly once.
        drop(unsafe { OwnedFd::from_raw_fd(duplicate) });
        return Err(Error::ReadyFd(std::io::Error::last_os_error()));
    }
    // SAFETY: `duplicate` is a fresh descriptor uniquely owned by this call.
    Ok(unsafe { OwnedFd::from_raw_fd(duplicate) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::{Read, Write};
    use std::os::fd::{AsRawFd, IntoRawFd};
    use std::os::unix::net::UnixStream;

    #[test]
    fn invalid_inherited_readiness_descriptor_is_rejected() {
        assert!(matches!(adopt_ready_fd(1_000_000), Err(Error::ReadyFd(_))));
    }

    #[test]
    fn inherited_readiness_descriptor_is_transferred_to_typed_ownership() {
        let (mut reader, writer) = UnixStream::pair().unwrap();
        let original = writer.into_raw_fd();

        let owned = adopt_ready_fd(original).unwrap();
        // SAFETY: F_GETFD only inspects the descriptor integer.
        assert_eq!(unsafe { libc::fcntl(original, libc::F_GETFD) }, -1);
        assert_ne!(owned.as_raw_fd(), original);

        let mut writer = File::from(owned);
        writer.write_all(b"READY\n").unwrap();
        drop(writer);
        let mut message = String::new();
        reader.read_to_string(&mut message).unwrap();
        assert_eq!(message, "READY\n");
    }
}
