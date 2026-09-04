use std::ffi::CString;
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread::{self, JoinHandle};

use genkan_lock_auth::{Connection, Message, ProtocolError, Secret};
use thiserror::Error;

use super::identity::Identity;

const CHILD_SOCKET_FD: RawFd = 3;
const EVENT_CAPACITY: usize = 128;
const READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

#[derive(Debug)]
pub(super) enum Event {
    Prompt { id: u64, secret: bool, text: String },
    Notice { error: bool, text: String },
    Success,
    Failure,
}

#[derive(Debug, Error)]
pub(crate) enum Error {
    #[error("could not locate the private authentication worker")]
    WorkerNotFound,
    #[error("could not start the authentication worker: {0}")]
    Spawn(#[source] io::Error),
    #[error("authentication worker did not provide the expected identity")]
    UnexpectedWorker,
    #[error("authentication protocol failed: {0}")]
    Protocol(#[from] ProtocolError),
}

pub(super) struct Client {
    writer: Connection,
    events: Option<Receiver<Event>>,
    pidfd: OwnedFd,
    reader: Option<JoinHandle<()>>,
}

impl Client {
    pub(super) fn start(identity: &Identity) -> Result<Self, Error> {
        let worker = worker_path()?;
        Self::start_with_worker(identity, &worker)
    }

    fn start_with_worker(identity: &Identity, worker: &Path) -> Result<Self, Error> {
        Self::start_with_worker_timeout(identity, worker, READY_TIMEOUT)
    }

    fn start_with_worker_timeout(
        identity: &Identity,
        worker: &Path,
        ready_timeout: std::time::Duration,
    ) -> Result<Self, Error> {
        let (parent, child_socket) = UnixStream::pair().map_err(Error::Spawn)?;
        let (pid, pidfd) = spawn_worker(worker, child_socket.as_raw_fd())?;
        drop(child_socket);

        let reader_stream = match parent.try_clone() {
            Ok(stream) => stream,
            Err(error) => {
                let _ = signal_pidfd(pidfd.as_raw_fd(), libc::SIGKILL);
                let _ = wait_child(pid);
                return Err(Error::Spawn(error));
            }
        };
        let mut writer = Connection::new(parent);
        let ready = writer.receive_with_deadline(std::time::Instant::now() + ready_timeout);
        if !matches!(ready, Ok(Message::Ready { uid, ref username }) if uid == identity.uid && username == &identity.username)
        {
            let _ = signal_pidfd(pidfd.as_raw_fd(), libc::SIGKILL);
            let _ = wait_child(pid);
            return match ready {
                Err(error) => Err(Error::Protocol(error)),
                Ok(_) => Err(Error::UnexpectedWorker),
            };
        }
        if let Err(error) = reader_stream.set_read_timeout(None) {
            let _ = signal_pidfd(pidfd.as_raw_fd(), libc::SIGKILL);
            let _ = wait_child(pid);
            return Err(Error::Spawn(error));
        }
        // Keep worker output bounded. A PAM module that emits notices faster
        // than the UI can render them is backpressured by this private queue
        // and then by the socket protocol.
        let (send, events) = mpsc::sync_channel(EVENT_CAPACITY);
        let reader = thread::Builder::new()
            .name("lock-auth-events".into())
            .spawn(move || read_worker(pid, reader_stream, &send))
            .map_err(|error| {
                let _ = signal_pidfd(pidfd.as_raw_fd(), libc::SIGKILL);
                let _ = wait_child(pid);
                Error::Spawn(error)
            })?;
        Ok(Self {
            writer,
            events: Some(events),
            pidfd,
            reader: Some(reader),
        })
    }

    pub(super) fn begin(&mut self) -> Result<(), Error> {
        self.writer.send(&Message::Begin)?;
        Ok(())
    }

    pub(super) fn try_receive(&self) -> Option<Event> {
        self.events.as_ref()?.try_recv().ok()
    }

    pub(super) fn respond(&mut self, id: u64, response: String) -> Result<(), Error> {
        let response = Secret::new(response.into_bytes())?;
        self.writer.send(&Message::Response {
            id,
            value: response,
        })?;
        Ok(())
    }

    pub(super) fn retry(&mut self) -> Result<(), Error> {
        self.writer.send(&Message::Retry)?;
        Ok(())
    }

    pub(super) fn cancel(&mut self) {
        if self.reader.is_none() {
            return;
        }
        // Unblock a reader waiting to publish into the bounded event queue.
        self.events.take();
        // Cancellation owns the whole PAM transaction. Do not depend on a PAM
        // module returning to the socket conversation before it takes effect.
        let _ = self.writer.shutdown();
        let _ = signal_pidfd(self.pidfd.as_raw_fd(), libc::SIGKILL);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn read_worker(pid: libc::pid_t, stream: UnixStream, events: &SyncSender<Event>) {
    let mut connection = Connection::new(stream);
    loop {
        match connection.receive() {
            Ok(Message::Prompt { id, secret, text }) => {
                if events.send(Event::Prompt { id, secret, text }).is_err() {
                    break;
                }
            }
            Ok(Message::Info(text)) => {
                if events.send(Event::Notice { error: false, text }).is_err() {
                    break;
                }
            }
            Ok(Message::Error(text)) => {
                if events.send(Event::Notice { error: true, text }).is_err() {
                    break;
                }
            }
            Ok(Message::Success) => {
                send_terminal(pid, events, true);
                return;
            }
            Ok(Message::Failure) => {
                if events.send(Event::Failure).is_err() {
                    break;
                }
            }
            Ok(_) | Err(_) => break,
        }
    }
    // The child remains unreaped here, so its numeric PID cannot be reused.
    unsafe { libc::kill(pid, libc::SIGKILL) };
    let _ = wait_child(pid);
    let _ = events.send(Event::Failure);
}

fn send_terminal(pid: libc::pid_t, events: &SyncSender<Event>, success: bool) {
    let clean_exit = wait_child(pid).is_ok_and(|status| status);
    let event = if success && clean_exit {
        Event::Success
    } else {
        Event::Failure
    };
    let _ = events.send(event);
}

fn wait_child(pid: libc::pid_t) -> io::Result<bool> {
    let mut status = 0;
    loop {
        // SAFETY: this process created `pid`, and this function is the sole
        // owner responsible for reaping it.
        let result = unsafe { libc::waitpid(pid, &mut status, 0) };
        if result == pid {
            return Ok(libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

fn signal_pidfd(pidfd: RawFd, signal: libc::c_int) -> io::Result<()> {
    // SAFETY: pidfd_send_signal targets the process referenced by this pidfd,
    // never a later process that reuses its numeric PID.
    let result = unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            pidfd,
            signal,
            std::ptr::null::<libc::siginfo_t>(),
            0,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn spawn_worker(path: &Path, child_socket: RawFd) -> Result<(libc::pid_t, OwnedFd), Error> {
    let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| Error::WorkerNotFound)?;
    let fd = CString::new(CHILD_SOCKET_FD.to_string()).expect("numeric descriptor");
    let parent_pid = CString::new(std::process::id().to_string()).expect("numeric process ID");
    let fd_flag = c"--fd";
    let parent_flag = c"--parent-pid";
    let null = c"/dev/null";
    let mut actions = MaybeUninit::<libc::posix_spawn_file_actions_t>::uninit();
    // SAFETY: each file action receives valid C data and is destroyed before
    // this function returns. posix_spawn performs no Rust at-fork callback, so
    // it remains safe when replacement workers are launched after UI threads.
    let init = unsafe { libc::posix_spawn_file_actions_init(actions.as_mut_ptr()) };
    if init != 0 {
        return Err(Error::Spawn(io::Error::from_raw_os_error(init)));
    }
    let mut actions = SpawnActions(unsafe { actions.assume_init() });
    // Duplicate the protocol socket before replacing potentially closed stdio
    // descriptors: socketpair may have allocated 0, 1, or 2 as its source.
    let result = unsafe {
        libc::posix_spawn_file_actions_adddup2(&mut actions.0, child_socket, CHILD_SOCKET_FD)
    };
    if result != 0 {
        return Err(Error::Spawn(io::Error::from_raw_os_error(result)));
    }
    if child_socket != CHILD_SOCKET_FD {
        let result =
            unsafe { libc::posix_spawn_file_actions_addclose(&mut actions.0, child_socket) };
        if result != 0 {
            return Err(Error::Spawn(io::Error::from_raw_os_error(result)));
        }
    }
    for (target, flags) in [
        (libc::STDIN_FILENO, libc::O_RDONLY),
        (libc::STDOUT_FILENO, libc::O_WRONLY),
        (libc::STDERR_FILENO, libc::O_WRONLY),
    ] {
        let result = unsafe {
            libc::posix_spawn_file_actions_addopen(&mut actions.0, target, null.as_ptr(), flags, 0)
        };
        if result != 0 {
            return Err(Error::Spawn(io::Error::from_raw_os_error(result)));
        }
    }
    // Keep only stdio and the private protocol socket across exec. This is a
    // spawn file action rather than post-exec cleanup, so neither the dynamic
    // loader nor the worker can observe the locker's readiness descriptor.
    let result = unsafe { libc::posix_spawn_file_actions_addclosefrom_np(&mut actions.0, 4) };
    if result != 0 {
        return Err(Error::Spawn(io::Error::from_raw_os_error(result)));
    }

    let mut argv = [
        path.as_ptr().cast_mut(),
        fd_flag.as_ptr().cast_mut(),
        fd.as_ptr().cast_mut(),
        parent_flag.as_ptr().cast_mut(),
        parent_pid.as_ptr().cast_mut(),
        std::ptr::null_mut(),
    ];
    let mut pid = 0;
    unsafe extern "C" {
        static mut environ: *mut *mut libc::c_char;
    }
    let result = unsafe {
        libc::posix_spawn(
            &mut pid,
            path.as_ptr(),
            &actions.0,
            std::ptr::null(),
            argv.as_mut_ptr(),
            environ,
        )
    };
    if result != 0 {
        return Err(Error::Spawn(io::Error::from_raw_os_error(result)));
    }
    // SAFETY: pidfd_open obtains a stable reference to the new child process.
    let pidfd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) } as RawFd;
    if pidfd < 0 {
        let error = io::Error::last_os_error();
        unsafe { libc::kill(pid, libc::SIGKILL) };
        let _ = wait_child(pid);
        return Err(Error::Spawn(error));
    }
    // SAFETY: pidfd_open returned a fresh descriptor owned by this client.
    Ok((pid, unsafe { OwnedFd::from_raw_fd(pidfd) }))
}

struct SpawnActions(libc::posix_spawn_file_actions_t);

impl Drop for SpawnActions {
    fn drop(&mut self) {
        // SAFETY: the actions were initialized once and are destroyed once.
        unsafe { libc::posix_spawn_file_actions_destroy(&mut self.0) };
    }
}

fn worker_path() -> Result<PathBuf, Error> {
    let executable = std::env::current_exe().map_err(Error::Spawn)?;
    let directory = executable.parent().ok_or(Error::WorkerNotFound)?;
    let development = directory.join("genkan-lock-auth");
    if development.is_file() {
        return Ok(development);
    }
    let installed = directory
        .parent()
        .ok_or(Error::WorkerNotFound)?
        .join("libexec/genkan-lock-auth");
    installed
        .is_file()
        .then_some(installed)
        .ok_or(Error::WorkerNotFound)
}

#[cfg(test)]
mod tests {
    use super::super::PROCESS_TEST_LOCK;
    use super::*;
    use std::fs::{self, File};
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SCRIPT_ID: AtomicU64 = AtomicU64::new(0);

    fn script(body: &str) -> PathBuf {
        let id = SCRIPT_ID.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("genkan-lock-auth-test-{}-{id}", std::process::id()));
        let mut file = File::create(&path).unwrap();
        file.write_all(format!("#!/bin/sh\n{body}\n").as_bytes())
            .unwrap();
        file.sync_all().unwrap();
        drop(file);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    fn shell_bytes(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("\\{:03o}", byte)).collect()
    }

    fn detached_child(exit_status: i32) -> libc::pid_t {
        let child = Command::new("sh")
            .arg("-c")
            .arg(format!("exit {exit_status}"))
            .spawn()
            .unwrap();
        let pid = child.id() as libc::pid_t;
        std::mem::forget(child);
        pid
    }

    fn terminal_event(exit_status: i32) -> Event {
        let (worker, ui) = UnixStream::pair().unwrap();
        let (send, receive) = mpsc::sync_channel(4);
        let mut worker = Connection::new(worker);
        worker.send(&Message::Success).unwrap();
        drop(worker);

        read_worker(detached_child(exit_status), ui, &send);
        receive.recv().unwrap()
    }

    #[test]
    fn terminal_success_requires_expected_identity_and_clean_exit() {
        let _guard = PROCESS_TEST_LOCK.lock().unwrap();
        assert!(matches!(terminal_event(0), Event::Success));
        assert!(matches!(terminal_event(1), Event::Failure));
    }

    #[test]
    fn malformed_worker_output_fails_closed() {
        let _guard = PROCESS_TEST_LOCK.lock().unwrap();
        let (mut worker, ui) = UnixStream::pair().unwrap();
        let (send, receive) = mpsc::sync_channel(4);
        worker.write_all(b"not a protocol frame").unwrap();
        drop(worker);

        read_worker(detached_child(0), ui, &send);

        assert!(matches!(receive.recv().unwrap(), Event::Failure));
    }

    #[test]
    fn reaped_pidfd_cannot_signal_a_later_process() {
        let _guard = PROCESS_TEST_LOCK.lock().unwrap();
        let pid = detached_child(0);
        // SAFETY: pidfd_open obtains a stable reference to this child.
        let descriptor = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) } as RawFd;
        assert!(descriptor >= 0);
        // SAFETY: pidfd_open returned a fresh descriptor.
        let descriptor = unsafe { OwnedFd::from_raw_fd(descriptor) };
        assert!(wait_child(pid).unwrap());

        assert_eq!(
            signal_pidfd(descriptor.as_raw_fd(), libc::SIGKILL)
                .unwrap_err()
                .raw_os_error(),
            Some(libc::ESRCH)
        );
    }

    #[test]
    fn spawned_worker_cannot_inherit_unrelated_descriptors() {
        let _guard = PROCESS_TEST_LOCK.lock().unwrap();
        let file = File::open("/dev/null").unwrap();
        // SAFETY: F_DUPFD creates a new descriptor owned by this test.
        let leaked = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD, 100) };
        assert!(leaked >= 100);
        let worker = script(&format!("test ! -e /proc/self/fd/{leaked}"));
        let (_parent, child) = UnixStream::pair().unwrap();

        let (pid, _pidfd) = spawn_worker(&worker, child.as_raw_fd()).unwrap();
        // SAFETY: the duplicated descriptor remains owned by this test.
        unsafe { libc::close(leaked) };

        assert!(wait_child(pid).unwrap());
        fs::remove_file(worker).unwrap();
    }

    #[test]
    fn process_level_cancellation_is_idempotent_without_worker_cooperation() {
        let _guard = PROCESS_TEST_LOCK.lock().unwrap();
        let marker = std::env::temp_dir().join(format!(
            "genkan-lock-auth-descendant-{}-{}",
            std::process::id(),
            SCRIPT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let worker = script(&format!(
            "sleep 10 & echo $! > '{}'\ntrap '' TERM\nwhile :; do :; done",
            marker.display()
        ));
        let (parent, child) = UnixStream::pair().unwrap();
        let (pid, pidfd) = spawn_worker(&worker, child.as_raw_fd()).unwrap();
        drop(child);
        let reader_stream = parent.try_clone().unwrap();
        let (send, events) = mpsc::sync_channel(1);
        let reader = thread::spawn(move || read_worker(pid, reader_stream, &send));
        let mut client = Client {
            writer: Connection::new(parent),
            events: Some(events),
            pidfd,
            reader: Some(reader),
        };
        for _ in 0..100 {
            if marker.is_file() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let descendant: libc::pid_t = fs::read_to_string(&marker).unwrap().trim().parse().unwrap();

        let started = std::time::Instant::now();
        client.cancel();
        client.cancel();

        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        assert!(client.reader.is_none());
        assert_eq!(
            wait_child(pid).unwrap_err().raw_os_error(),
            Some(libc::ECHILD)
        );
        // SAFETY: terminate the intentionally leaked fake-worker descendant.
        unsafe { libc::kill(descendant, libc::SIGKILL) };
        fs::remove_file(marker).unwrap();
        fs::remove_file(worker).unwrap();
    }

    #[test]
    fn injected_worker_cannot_authorize_before_begin() {
        let _guard = PROCESS_TEST_LOCK.lock().unwrap();
        let identity = Identity {
            uid: 1000,
            username: "alice".into(),
            display_name: "Alice".into(),
        };
        let mut ready = b"GNKA\x02\x01".to_vec();
        let payload_length = 4 + identity.username.len();
        ready.extend_from_slice(&(payload_length as u32).to_be_bytes());
        ready.extend_from_slice(&identity.uid.to_be_bytes());
        ready.extend_from_slice(identity.username.as_bytes());
        let success = b"GNKA\x02\x05\0\0\0\0";
        let worker = script(&format!(
            "printf '{}' >&3\ndd bs=10 count=1 status=none <&3\nprintf '{}' >&3",
            shell_bytes(&ready),
            shell_bytes(success)
        ));
        let mut client = Client::start_with_worker(&identity, &worker).unwrap();

        assert!(client
            .events
            .as_ref()
            .unwrap()
            .recv_timeout(std::time::Duration::from_millis(20))
            .is_err());
        client.begin().unwrap();
        assert!(matches!(
            client
                .events
                .as_ref()
                .unwrap()
                .recv_timeout(std::time::Duration::from_secs(1))
                .unwrap(),
            Event::Success
        ));

        fs::remove_file(worker).unwrap();
    }

    #[test]
    fn startup_rejects_the_pre_begin_protocol_version() {
        let _guard = PROCESS_TEST_LOCK.lock().unwrap();
        let identity = Identity {
            uid: 1000,
            username: "alice".into(),
            display_name: "Alice".into(),
        };
        let mut ready = b"GNKA\x01\x01".to_vec();
        ready.extend_from_slice(&((4 + identity.username.len()) as u32).to_be_bytes());
        ready.extend_from_slice(&identity.uid.to_be_bytes());
        ready.extend_from_slice(identity.username.as_bytes());
        let worker = script(&format!(
            "printf '{}' >&3\nwhile :; do :; done",
            shell_bytes(&ready)
        ));

        assert!(matches!(
            Client::start_with_worker(&identity, &worker),
            Err(Error::Protocol(ProtocolError::InvalidPayload))
        ));
        fs::remove_file(worker).unwrap();
    }

    #[test]
    fn ready_handshake_uses_one_absolute_deadline() {
        let _guard = PROCESS_TEST_LOCK.lock().unwrap();
        let identity = Identity {
            uid: 1000,
            username: "alice".into(),
            display_name: "Alice".into(),
        };
        let mut ready = b"GNKA\x02\x01".to_vec();
        ready.extend_from_slice(&((4 + identity.username.len()) as u32).to_be_bytes());
        ready.extend_from_slice(&identity.uid.to_be_bytes());
        ready.extend_from_slice(identity.username.as_bytes());
        let octets = ready
            .iter()
            .map(|byte| format!("{byte:03o}"))
            .collect::<Vec<_>>()
            .join(" ");
        let worker = script(&format!(
            "for byte in {octets}; do printf \"\\\\$byte\" >&3; sleep 0.02; done\nwhile :; do :; done"
        ));
        let started = std::time::Instant::now();

        assert!(matches!(
            Client::start_with_worker_timeout(
                &identity,
                &worker,
                std::time::Duration::from_millis(100),
            ),
            Err(Error::Protocol(ProtocolError::Io(_)))
        ));
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        fs::remove_file(worker).unwrap();
    }

    #[test]
    fn startup_rejects_missing_or_mismatched_ready_identity() {
        let _guard = PROCESS_TEST_LOCK.lock().unwrap();
        let identity = Identity {
            uid: 1000,
            username: "alice".into(),
            display_name: "Alice".into(),
        };
        for (uid, username) in [(1001_u32, "alice"), (1000, "mallory")] {
            let mut ready = b"GNKA\x02\x01".to_vec();
            ready.extend_from_slice(&((4 + username.len()) as u32).to_be_bytes());
            ready.extend_from_slice(&uid.to_be_bytes());
            ready.extend_from_slice(username.as_bytes());
            let worker = script(&format!(
                "printf '{}' >&3\nwhile :; do :; done",
                shell_bytes(&ready)
            ));

            assert!(matches!(
                Client::start_with_worker(&identity, &worker),
                Err(Error::UnexpectedWorker)
            ));
            fs::remove_file(worker).unwrap();
        }

        let worker = script("exit 0");
        assert!(matches!(
            Client::start_with_worker(&identity, &worker),
            Err(Error::Protocol(_))
        ));
        fs::remove_file(worker).unwrap();
    }

    #[test]
    fn closed_stdio_spawn_helper() {
        if std::env::var_os("GENKAN_TEST_CLOSED_STDIO").is_none() {
            return;
        }
        // SAFETY: this disposable subprocess intentionally closes two stdio
        // descriptors to force socketpair to allocate from the low range.
        unsafe {
            libc::close(libc::STDIN_FILENO);
            libc::close(libc::STDOUT_FILENO);
        }
        let worker = script("exit 0");
        let (_parent, child) = UnixStream::pair().unwrap();
        let outcome = spawn_worker(&worker, child.as_raw_fd())
            .and_then(|(pid, _)| wait_child(pid).map_err(Error::Spawn));
        let _ = fs::remove_file(worker);
        std::process::exit(i32::from(!matches!(outcome, Ok(true))));
    }

    #[test]
    fn worker_spawn_handles_multiple_closed_standard_descriptors() {
        let _guard = PROCESS_TEST_LOCK.lock().unwrap();
        let status = Command::new(std::env::current_exe().unwrap())
            .env("GENKAN_TEST_CLOSED_STDIO", "1")
            .arg("--exact")
            .arg("locker::auth::tests::closed_stdio_spawn_helper")
            .status()
            .unwrap();

        assert!(status.success());
    }
}
