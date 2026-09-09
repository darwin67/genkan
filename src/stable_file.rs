use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::Path;

use rustix::fs::{open, Mode, OFlags};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OpenError {
    Unavailable,
    Metadata,
    NotRegular,
    Reopen,
}

pub(crate) fn open_regular(path: &Path) -> Result<File, OpenError> {
    let bound = open(path, OFlags::PATH | OFlags::CLOEXEC, Mode::empty())
        .map(File::from)
        .map_err(|_| OpenError::Unavailable)?;
    if !bound.metadata().map_err(|_| OpenError::Metadata)?.is_file() {
        return Err(OpenError::NotRegular);
    }

    open(
        format!("/proc/self/fd/{}", bound.as_raw_fd()),
        OFlags::RDONLY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(|_| OpenError::Reopen)
}
