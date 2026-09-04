use std::ffi::{CString, OsStr};
use std::io::{self, Read};
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use thiserror::Error;

const CHILD_READY_FD: RawFd = 3;
const CHILD_WAYLAND_FD: RawFd = 4;

#[derive(Debug, Error)]
pub(crate) enum Error {
    #[error("could not start the foreground locker: {0}")]
    Spawn(#[source] io::Error),
    #[error("foreground locker exited before compositor confirmation{0}")]
    ChildFailed(String),
    #[error("foreground locker sent an invalid readiness message")]
    InvalidReady,
}

pub(super) fn launch(executable: &Path, arguments: &[CString]) -> Result<(), Error> {
    let wayland = preserve_wayland_socket()?;
    let mut pipe = [0; 2];
    // SAFETY: pipe points to storage for two newly owned descriptors.
    if unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(Error::Spawn(io::Error::last_os_error()));
    }
    // SAFETY: pipe2 returned two fresh descriptors, each converted exactly once.
    let mut reader = unsafe { OwnedFd::from_raw_fd(pipe[0]) };
    let writer = ensure_not_child_ready_fd(unsafe { OwnedFd::from_raw_fd(pipe[1]) })?;
    let pid = spawn(
        executable,
        arguments,
        reader.as_raw_fd(),
        writer.as_raw_fd(),
        wayland.as_ref().map(AsRawFd::as_raw_fd),
    )?;
    drop(writer);

    let mut message = Vec::with_capacity(b"READY\n".len() + 1);
    FileReader(&mut reader)
        .take((b"READY\n".len() + 1) as u64)
        .read_to_end(&mut message)
        .map_err(Error::Spawn)?;
    if message == b"READY\n" {
        return Ok(());
    }
    if !message.is_empty() {
        return Err(Error::InvalidReady);
    }
    let status = wait_child(pid).map_err(Error::Spawn)?;
    Err(Error::ChildFailed(status))
}

fn preserve_wayland_socket() -> Result<Option<OwnedFd>, Error> {
    let Some(value) = std::env::var_os("WAYLAND_SOCKET") else {
        return Ok(None);
    };
    let raw_fd = value
        .to_str()
        .and_then(|value| value.parse::<RawFd>().ok())
        .filter(|fd| *fd >= 0)
        .ok_or_else(|| Error::Spawn(io::Error::from(io::ErrorKind::InvalidInput)))?;
    // Keep the source away from the fixed child descriptors and the pipe that
    // launch creates next. The child copy has CLOEXEC cleared by dup2.
    let duplicate = unsafe { libc::fcntl(raw_fd, libc::F_DUPFD_CLOEXEC, 64) };
    if duplicate < 0 {
        return Err(Error::Spawn(io::Error::last_os_error()));
    }
    // SAFETY: fcntl returned a fresh descriptor uniquely owned here.
    Ok(Some(unsafe { OwnedFd::from_raw_fd(duplicate) }))
}

fn ensure_not_child_ready_fd(fd: OwnedFd) -> Result<OwnedFd, Error> {
    if fd.as_raw_fd() != CHILD_READY_FD {
        return Ok(fd);
    }
    // dup2(fd, fd) would leave O_CLOEXEC set, so move an unlucky pipe writer
    // away from the child's fixed readiness descriptor before spawning.
    // SAFETY: fcntl only borrows fd and returns a fresh descriptor on success.
    let duplicate = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 4) };
    if duplicate < 0 {
        return Err(Error::Spawn(io::Error::last_os_error()));
    }
    // SAFETY: duplicate is a fresh descriptor uniquely owned by this call.
    Ok(unsafe { OwnedFd::from_raw_fd(duplicate) })
}

struct FileReader<'a>(&'a mut OwnedFd);

impl Read for FileReader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        // SAFETY: the descriptor is open for reading and buffer is valid for writes.
        let result =
            unsafe { libc::read(self.0.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len()) };
        if result >= 0 {
            Ok(result as usize)
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

fn spawn(
    executable: &Path,
    arguments: &[CString],
    reader: RawFd,
    writer: RawFd,
    wayland: Option<RawFd>,
) -> Result<libc::pid_t, Error> {
    let executable = CString::new(executable.as_os_str().as_bytes())
        .map_err(|_| Error::Spawn(io::Error::from(io::ErrorKind::InvalidInput)))?;
    let mut actions = MaybeUninit::<libc::posix_spawn_file_actions_t>::uninit();
    // SAFETY: the actions are initialized once and destroyed by SpawnActions.
    let result = unsafe { libc::posix_spawn_file_actions_init(actions.as_mut_ptr()) };
    if result != 0 {
        return Err(Error::Spawn(io::Error::from_raw_os_error(result)));
    }
    let mut actions = SpawnActions(unsafe { actions.assume_init() });
    add_action(unsafe {
        libc::posix_spawn_file_actions_adddup2(&mut actions.0, writer, CHILD_READY_FD)
    })?;
    if let Some(wayland) = wayland {
        add_action(unsafe {
            libc::posix_spawn_file_actions_adddup2(&mut actions.0, wayland, CHILD_WAYLAND_FD)
        })?;
    }
    let reserves_wayland_fd = wayland.is_some();
    if reader != CHILD_READY_FD && (!reserves_wayland_fd || reader != CHILD_WAYLAND_FD) {
        add_action(unsafe { libc::posix_spawn_file_actions_addclose(&mut actions.0, reader) })?;
    }
    if writer != CHILD_READY_FD && (!reserves_wayland_fd || writer != CHILD_WAYLAND_FD) {
        add_action(unsafe { libc::posix_spawn_file_actions_addclose(&mut actions.0, writer) })?;
    }
    let close_from = if reserves_wayland_fd { 5 } else { 4 };
    add_action(unsafe {
        libc::posix_spawn_file_actions_addclosefrom_np(&mut actions.0, close_from)
    })?;

    let mut argv = Vec::with_capacity(arguments.len() + 2);
    argv.push(executable.as_ptr().cast_mut());
    argv.extend(
        arguments
            .iter()
            .map(|argument| argument.as_ptr().cast_mut()),
    );
    argv.push(std::ptr::null_mut());
    let environment = child_environment(wayland.is_some())?;
    let mut envp: Vec<_> = environment
        .iter()
        .map(|entry| entry.as_ptr().cast_mut())
        .collect();
    envp.push(std::ptr::null_mut());
    let mut pid = 0;
    // SAFETY: all argv and environment strings remain alive for this call.
    let result = unsafe {
        libc::posix_spawn(
            &mut pid,
            executable.as_ptr(),
            &actions.0,
            std::ptr::null(),
            argv.as_mut_ptr(),
            envp.as_mut_ptr(),
        )
    };
    if result != 0 {
        Err(Error::Spawn(io::Error::from_raw_os_error(result)))
    } else {
        Ok(pid)
    }
}

fn child_environment(has_wayland_socket: bool) -> Result<Vec<CString>, Error> {
    let mut environment = Vec::new();
    for (key, value) in std::env::vars_os() {
        if key == OsStr::new("WAYLAND_SOCKET") {
            continue;
        }
        let mut entry = key.as_bytes().to_vec();
        entry.push(b'=');
        entry.extend_from_slice(value.as_bytes());
        environment.push(
            CString::new(entry)
                .map_err(|_| Error::Spawn(io::Error::from(io::ErrorKind::InvalidInput)))?,
        );
    }
    if has_wayland_socket {
        environment.push(CString::new("WAYLAND_SOCKET=4").expect("static environment entry"));
    }
    Ok(environment)
}

fn add_action(result: libc::c_int) -> Result<(), Error> {
    if result == 0 {
        Ok(())
    } else {
        Err(Error::Spawn(io::Error::from_raw_os_error(result)))
    }
}

fn wait_child(pid: libc::pid_t) -> io::Result<String> {
    let mut status = 0;
    loop {
        // SAFETY: this launcher created pid and is its sole reaper before readiness.
        let result = unsafe { libc::waitpid(pid, &mut status, 0) };
        if result == pid {
            return Ok(if libc::WIFEXITED(status) {
                format!(" with status {}", libc::WEXITSTATUS(status))
            } else if libc::WIFSIGNALED(status) {
                format!(" after signal {}", libc::WTERMSIG(status))
            } else {
                String::new()
            });
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

struct SpawnActions(libc::posix_spawn_file_actions_t);

impl Drop for SpawnActions {
    fn drop(&mut self) {
        // SAFETY: the actions were initialized once and are destroyed once.
        unsafe { libc::posix_spawn_file_actions_destroy(&mut self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    #[test]
    fn closed_stderr_launcher_helper() {
        if std::env::var_os("GENKAN_TEST_LAUNCHER_FD_THREE").is_none() {
            return;
        }
        // SAFETY: this disposable subprocess closes descriptors 2 and 3 so
        // pipe2 assigns its writer to the fixed child readiness descriptor.
        unsafe {
            libc::close(libc::STDERR_FILENO);
            libc::close(CHILD_READY_FD);
        }
        let script =
            std::env::temp_dir().join(format!("genkan-launcher-ready-{}", std::process::id()));
        fs::write(&script, "#!/bin/sh\nprintf 'READY\\n' >&3\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
        let result = launch(&script, &[]);
        let _ = fs::remove_file(script);
        std::process::exit(i32::from(result.is_err()));
    }

    #[test]
    fn launcher_handles_pipe_writer_allocated_as_fd_three() {
        let status = Command::new(std::env::current_exe().unwrap())
            .env("GENKAN_TEST_LAUNCHER_FD_THREE", "1")
            .arg("--exact")
            .arg("locker::launcher::tests::closed_stderr_launcher_helper")
            .status()
            .unwrap();

        assert!(status.success());
    }

    #[test]
    fn inherited_wayland_socket_launcher_helper() {
        if std::env::var_os("GENKAN_TEST_LAUNCHER_WAYLAND_SOCKET").is_none() {
            return;
        }
        let (client, _server) = std::os::unix::net::UnixStream::pair().unwrap();
        std::env::set_var("WAYLAND_SOCKET", client.as_raw_fd().to_string());
        let script =
            std::env::temp_dir().join(format!("genkan-launcher-wayland-{}", std::process::id()));
        fs::write(
            &script,
            "#!/bin/sh\ntest \"$WAYLAND_SOCKET\" = 4\ntest -e /proc/self/fd/4\nprintf 'READY\\n' >&3\n",
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
        let result = launch(&script, &[]);
        let _ = fs::remove_file(script);
        std::process::exit(i32::from(result.is_err()));
    }

    #[test]
    fn launcher_relocates_and_preserves_an_inherited_wayland_socket() {
        let status = Command::new(std::env::current_exe().unwrap())
            .env("GENKAN_TEST_LAUNCHER_WAYLAND_SOCKET", "1")
            .arg("--exact")
            .arg("locker::launcher::tests::inherited_wayland_socket_launcher_helper")
            .status()
            .unwrap();

        assert!(status.success());
    }
}
