use std::ffi::CStr;
use std::io;
use std::mem::MaybeUninit;

use thiserror::Error;

use crate::accounts;

const INITIAL_BUFFER_SIZE: usize = 1024;
const MAX_BUFFER_SIZE: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Identity {
    pub(crate) uid: u32,
    pub(crate) username: String,
    pub(crate) display_name: String,
}

#[derive(Debug, Error)]
pub(crate) enum Error {
    #[error("could not resolve the real user ID through NSS")]
    NotFound,
    #[error("NSS returned an invalid login identity")]
    Invalid,
    #[error("could not resolve the real user ID: {0}")]
    System(#[from] io::Error),
}

impl Identity {
    pub(crate) fn current() -> Result<Self, Error> {
        resolve(rustix::process::getuid().as_raw())
    }

    #[cfg(test)]
    pub(crate) fn fixture() -> Self {
        Self {
            uid: 1000,
            username: "alice".into(),
            display_name: "Alice".into(),
        }
    }
}

fn resolve(uid: u32) -> Result<Identity, Error> {
    let mut size = INITIAL_BUFFER_SIZE;
    loop {
        let mut entry = MaybeUninit::<libc::passwd>::uninit();
        let mut result = std::ptr::null_mut();
        let mut buffer = vec![0_u8; size];
        // SAFETY: `entry`, `result`, and `buffer` remain valid for the call. The
        // returned strings are copied before `buffer` is dropped.
        let status = unsafe {
            libc::getpwuid_r(
                uid,
                entry.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            )
        };
        if status == libc::ERANGE && size < MAX_BUFFER_SIZE {
            size = (size * 2).min(MAX_BUFFER_SIZE);
            continue;
        }
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status).into());
        }
        if result.is_null() {
            return Err(Error::NotFound);
        }
        // SAFETY: a successful `getpwuid_r` initialized `entry`; its string
        // pointers refer into `buffer` and are NUL-terminated for this scope.
        let entry = unsafe { entry.assume_init() };
        let username = copy_string(entry.pw_name)?;
        if !accounts::valid_username(&username) {
            return Err(Error::Invalid);
        }
        let gecos = copy_string(entry.pw_gecos).unwrap_or_default();
        let display_name = accounts::presentation_label(gecos.split(',').next().unwrap_or(""))
            .unwrap_or_else(|| username.clone());
        return Ok(Identity {
            uid,
            username,
            display_name,
        });
    }
}

fn copy_string(value: *const libc::c_char) -> Result<String, Error> {
    if value.is_null() {
        return Err(Error::Invalid);
    }
    // SAFETY: callers pass pointers returned by successful NSS lookup.
    let value = unsafe { CStr::from_ptr(value) };
    value
        .to_str()
        .map(str::to_owned)
        .map_err(|_| Error::Invalid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_the_process_real_uid_without_an_identity_override() {
        let identity = Identity::current().unwrap();
        assert_eq!(identity.uid, rustix::process::getuid().as_raw());
        assert!(accounts::valid_username(&identity.username));
        assert!(!identity.display_name.is_empty());
    }
}
