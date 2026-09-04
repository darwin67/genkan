use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use thiserror::Error;

const READY: &[u8] = b"READY\n";
const MAX_WAITERS: usize = 64;

#[derive(Debug, Error)]
pub(crate) enum Error {
    #[error("XDG_RUNTIME_DIR must name an absolute private directory owned by the invoking user")]
    RuntimeDirectory,
    #[error("WAYLAND_SOCKET or WAYLAND_DISPLAY must identify a compositor connection")]
    WaylandConnection,
    #[error("could not coordinate the compositor lock lifecycle: {0}")]
    Io(#[from] io::Error),
    #[error("the existing locker exited before compositor confirmation")]
    ExistingLockerFailed,
}

pub(super) enum Entry {
    Owner {
        coordination: Owner,
        wayland: UnixStream,
    },
    Joined,
}

enum LeaseEntry {
    Owner(Owner),
    Joined,
}

pub(super) struct Owner {
    _lease: File,
    listener: UnixListener,
    socket_path: PathBuf,
}

pub(super) struct Active {
    _lease: File,
    stop: UnixStream,
    thread: Option<JoinHandle<()>>,
    socket_path: PathBuf,
}

pub(super) fn enter() -> Result<Entry, Error> {
    let runtime = runtime_directory()?;
    let (wayland, _) = wayland_connection(&runtime)?;
    let stem = coordination_stem_for(&wayland)?;
    match enter_paths(
        runtime.join(format!("{stem}.lease")),
        runtime.join(format!("{stem}.sock")),
    )? {
        LeaseEntry::Owner(coordination) => Ok(Entry::Owner {
            coordination,
            wayland,
        }),
        LeaseEntry::Joined => Ok(Entry::Joined),
    }
}

fn wayland_connection(runtime: &Path) -> Result<(UnixStream, fs::Metadata), Error> {
    if let Some(raw_fd) = std::env::var_os("WAYLAND_SOCKET") {
        let raw_fd = wayland_socket_fd(&raw_fd)?;
        std::env::remove_var("WAYLAND_SOCKET");
        return adopt_wayland_socket(raw_fd);
    }

    let display = wayland_socket(runtime)?;
    connect_wayland(&display)
}

fn wayland_socket_fd(value: &OsStr) -> Result<RawFd, Error> {
    value
        .to_str()
        .and_then(|value| value.parse::<RawFd>().ok())
        .filter(|fd| *fd >= 0)
        .ok_or(Error::WaylandConnection)
}

fn adopt_wayland_socket(raw_fd: RawFd) -> Result<(UnixStream, fs::Metadata), Error> {
    // Validate before constructing an owned descriptor so malformed inherited
    // values never reach FromRawFd's valid-descriptor precondition.
    let flags = unsafe { libc::fcntl(raw_fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(Error::WaylandConnection);
    }
    // SAFETY: WAYLAND_SOCKET transfers ownership of this inherited descriptor
    // to the Wayland client, matching wayland-client's connect_to_env contract.
    let wayland = UnixStream::from(unsafe { OwnedFd::from_raw_fd(raw_fd) });
    set_close_on_exec(&wayland, flags).map_err(|_| Error::WaylandConnection)?;
    let metadata =
        fs::metadata(format!("/proc/self/fd/{raw_fd}")).map_err(|_| Error::WaylandConnection)?;
    if !metadata.file_type().is_socket() {
        return Err(Error::WaylandConnection);
    }
    Ok((wayland, metadata))
}

fn set_close_on_exec(wayland: &UnixStream, flags: libc::c_int) -> io::Result<()> {
    // SAFETY: fcntl only updates descriptor flags without taking ownership.
    if unsafe { libc::fcntl(wayland.as_raw_fd(), libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn connect_wayland(display: &Path) -> Result<(UnixStream, fs::Metadata), Error> {
    let before = display.metadata().map_err(|_| Error::WaylandConnection)?;
    if !before.file_type().is_socket() {
        return Err(Error::WaylandConnection);
    }
    let wayland = UnixStream::connect(display).map_err(|_| Error::WaylandConnection)?;
    let after = display.metadata().map_err(|_| Error::WaylandConnection)?;
    if socket_metadata(&before) != socket_metadata(&after) {
        return Err(Error::WaylandConnection);
    }
    Ok((wayland, after))
}

fn socket_metadata(metadata: &fs::Metadata) -> (u64, u64, i64, i64) {
    (
        metadata.dev(),
        metadata.ino(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    )
}

fn peer_identity(wayland: &UnixStream) -> io::Result<(i32, u64)> {
    let mut credentials = std::mem::MaybeUninit::<libc::ucred>::uninit();
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: credentials and length point to writable storage of the declared size.
    let result = unsafe {
        libc::getsockopt(
            wayland.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            credentials.as_mut_ptr().cast(),
            &mut length,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    if length as usize != std::mem::size_of::<libc::ucred>() {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    // SAFETY: getsockopt initialized the full ucred value after returning success.
    let credentials = unsafe { credentials.assume_init() };
    if credentials.pid <= 0 {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    let stat = fs::read_to_string(format!("/proc/{}/stat", credentials.pid))?;
    let fields = stat
        .rsplit_once(") ")
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?
        .1;
    let start_time = fields
        .split_whitespace()
        .nth(19)
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?
        .parse()
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
    Ok((credentials.pid, start_time))
}

fn coordination_stem(peer_pid: i32, peer_start_time: u64) -> String {
    format!("genkan-lock-{peer_pid:x}-{peer_start_time:x}")
}

fn coordination_stem_for(wayland: &UnixStream) -> io::Result<String> {
    let (peer_pid, peer_start_time) = peer_identity(wayland)?;
    Ok(coordination_stem(peer_pid, peer_start_time))
}

fn runtime_directory() -> Result<PathBuf, Error> {
    let path = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or(Error::RuntimeDirectory)?;
    let metadata = fs::symlink_metadata(&path).map_err(|_| Error::RuntimeDirectory)?;
    if !metadata.is_dir()
        || metadata.uid() != unsafe { libc::getuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err(Error::RuntimeDirectory);
    }
    Ok(path)
}

fn wayland_socket(runtime: &Path) -> Result<PathBuf, Error> {
    let display = std::env::var_os("WAYLAND_DISPLAY").ok_or(Error::WaylandConnection)?;
    let display = PathBuf::from(display);
    if display.is_absolute() {
        Ok(display)
    } else if display.components().count() == 1 {
        Ok(runtime.join(display))
    } else {
        Err(Error::WaylandConnection)
    }
}

fn enter_paths(lease_path: PathBuf, socket_path: PathBuf) -> Result<LeaseEntry, Error> {
    let lease = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&lease_path)?;
    let metadata = lease.metadata()?;
    if !metadata.is_file() || metadata.uid() != unsafe { libc::getuid() } {
        return Err(Error::RuntimeDirectory);
    }

    if try_lock(&lease)? {
        match fs::remove_file(&socket_path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let listener = UnixListener::bind(&socket_path)?;
        fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        return Ok(LeaseEntry::Owner(Owner {
            _lease: lease,
            listener,
            socket_path,
        }));
    }

    loop {
        match UnixStream::connect(&socket_path) {
            Ok(mut stream) => {
                let mut message = Vec::with_capacity(READY.len());
                stream.read_to_end(&mut message)?;
                return if message == READY {
                    Ok(LeaseEntry::Joined)
                } else {
                    Err(Error::ExistingLockerFailed)
                };
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                ) =>
            {
                // This invocation already observed another owner. If that
                // lifecycle disappears before accepting us, fail with it
                // rather than silently starting and blessing a second lock.
                if try_lock(&lease)? {
                    return Err(Error::ExistingLockerFailed);
                }
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn try_lock(file: &File) -> io::Result<bool> {
    // SAFETY: flock only inspects the open descriptor and does not take ownership.
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        Ok(true)
    } else {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::WouldBlock {
            Ok(false)
        } else {
            Err(error)
        }
    }
}

impl Owner {
    pub(super) fn activate(self) -> Result<(OwnedFd, Active), Error> {
        let (ready_reader, ready_writer) = UnixStream::pair()?;
        let (stop_reader, stop) = UnixStream::pair()?;
        let socket_path = self.socket_path.clone();
        let thread = thread::Builder::new()
            .name("lock-coordinator".into())
            .spawn(move || coordinate(self.listener, ready_reader, stop_reader))?;
        let ready_writer = ready_writer.into();
        Ok((
            ready_writer,
            Active {
                _lease: self._lease,
                stop,
                thread: Some(thread),
                socket_path,
            },
        ))
    }
}

fn coordinate(listener: UnixListener, mut ready: UnixStream, stop: UnixStream) {
    let mut waiters: Vec<UnixStream> = Vec::new();
    let mut confirmed = false;
    loop {
        let mut descriptors = [
            libc::pollfd {
                fd: listener.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: if confirmed { -1 } else { ready.as_raw_fd() },
                events: libc::POLLIN | libc::POLLHUP,
                revents: 0,
            },
            libc::pollfd {
                fd: stop.as_raw_fd(),
                events: libc::POLLIN | libc::POLLHUP,
                revents: 0,
            },
        ];
        // SAFETY: descriptors points to initialized pollfd values for this call.
        let result = unsafe { libc::poll(descriptors.as_mut_ptr(), descriptors.len() as _, -1) };
        if result < 0 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return;
        }
        if descriptors[2].revents != 0 {
            return;
        }
        if descriptors[1].revents != 0 && !confirmed {
            let mut message = [0_u8; READY.len()];
            if ready.read_exact(&mut message).is_err() || message != READY {
                return;
            }
            confirmed = true;
            for mut waiter in waiters.drain(..) {
                let _ = waiter.write_all(READY);
            }
        }
        if descriptors[0].revents & libc::POLLIN != 0 {
            while let Ok((mut waiter, _)) = listener.accept() {
                if confirmed {
                    let _ = waiter.write_all(READY);
                } else if waiters.len() < MAX_WAITERS {
                    waiters.push(waiter);
                }
            }
        }
    }
}

impl Drop for Active {
    fn drop(&mut self) {
        let _ = self.stop.write_all(&[0]);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = fs::remove_file(&self.socket_path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::IntoRawFd;
    use std::process::Command;
    use std::sync::mpsc;

    fn paths(name: &str) -> (PathBuf, PathBuf, PathBuf) {
        let root =
            std::env::temp_dir().join(format!("genkan-coordination-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        (root.join("lease"), root.join("socket"), root)
    }

    #[test]
    fn compositor_generation_changes_the_coordination_identity() {
        let original = coordination_stem(1, 2);

        assert_ne!(original, coordination_stem(3, 2));
        assert_ne!(original, coordination_stem(1, 3));
    }

    #[test]
    fn pathname_and_inherited_connections_share_a_compositor_identity() {
        let (_, display, root) = paths("shared-compositor");
        let listener = UnixListener::bind(&display).unwrap();
        let (pathname, _) = connect_wayland(&display).unwrap();
        let inherited = UnixStream::connect(&display).unwrap().into_raw_fd();
        let (inherited, _) = adopt_wayland_socket(inherited).unwrap();

        assert_eq!(
            coordination_stem_for(&pathname).unwrap(),
            coordination_stem_for(&inherited).unwrap()
        );

        drop(listener);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn inherited_wayland_errors_name_both_environment_sources() {
        assert!(matches!(
            wayland_socket_fd(OsStr::new("not-a-descriptor")),
            Err(Error::WaylandConnection)
        ));
        let file = File::open("/dev/null").unwrap().into_raw_fd();
        assert!(matches!(
            adopt_wayland_socket(file),
            Err(Error::WaylandConnection)
        ));
        let (closed, _peer) = UnixStream::pair().unwrap();
        let closed = closed.into_raw_fd();
        // SAFETY: ownership is intentionally transferred to the closed-descriptor case.
        unsafe { libc::close(closed) };
        assert!(matches!(
            adopt_wayland_socket(closed),
            Err(Error::WaylandConnection)
        ));
        assert_eq!(
            Error::WaylandConnection.to_string(),
            "WAYLAND_SOCKET or WAYLAND_DISPLAY must identify a compositor connection"
        );
    }

    #[test]
    fn connected_wayland_socket_survives_path_replacement() {
        let (_, display, root) = paths("display-replacement");
        let first_listener = UnixListener::bind(&display).unwrap();
        let (mut first_connection, first_metadata) = connect_wayland(&display).unwrap();

        fs::remove_file(&display).unwrap();
        let second_listener = UnixListener::bind(&display).unwrap();
        let (mut second_connection, second_metadata) = connect_wayland(&display).unwrap();

        let first_peer = peer_identity(&first_connection).unwrap();
        let second_peer = peer_identity(&second_connection).unwrap();
        assert_eq!(first_peer.0, std::process::id() as i32);
        assert_eq!(first_peer, second_peer);
        assert!(first_peer.1 > 0);
        assert_ne!(
            socket_metadata(&first_metadata),
            socket_metadata(&second_metadata)
        );
        let (mut first_server, _) = first_listener.accept().unwrap();
        let (mut second_server, _) = second_listener.accept().unwrap();
        first_server.write_all(b"first").unwrap();
        second_server.write_all(b"second").unwrap();
        let mut first = [0; 5];
        let mut second = [0; 6];
        first_connection.read_exact(&mut first).unwrap();
        second_connection.read_exact(&mut second).unwrap();
        assert_eq!(&first, b"first");
        assert_eq!(&second, b"second");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn inherited_wayland_socket_helper() {
        if std::env::var_os("GENKAN_TEST_INHERITED_WAYLAND").is_none() {
            return;
        }
        let (mut connection, metadata) = wayland_connection(Path::new("/unused")).unwrap();
        assert!(std::env::var_os("WAYLAND_SOCKET").is_none());
        assert!(metadata.file_type().is_socket());
        let flags = unsafe { libc::fcntl(connection.as_raw_fd(), libc::F_GETFD) };
        assert_ne!(flags & libc::FD_CLOEXEC, 0);
        connection.write_all(b"wayland").unwrap();
    }

    #[test]
    fn inherited_wayland_socket_is_adopted_without_a_display_path() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let inherited = unsafe { libc::fcntl(client.as_raw_fd(), libc::F_DUPFD, 64) };
        assert!(inherited >= 0);
        let status = Command::new(std::env::current_exe().unwrap())
            .env("GENKAN_TEST_INHERITED_WAYLAND", "1")
            .env("WAYLAND_SOCKET", inherited.to_string())
            .arg("--exact")
            .arg("locker::coordination::tests::inherited_wayland_socket_helper")
            .status()
            .unwrap();
        // SAFETY: inherited remains owned by this parent after spawning the test child.
        unsafe { libc::close(inherited) };
        assert!(status.success());
        let mut message = [0; 7];
        server.read_exact(&mut message).unwrap();
        assert_eq!(&message, b"wayland");
    }

    #[test]
    fn duplicate_waits_for_the_owner_confirmation() {
        let (lease, socket, root) = paths("pending");
        let LeaseEntry::Owner(owner) = enter_paths(lease.clone(), socket.clone()).unwrap() else {
            panic!("first entrant must own the lifecycle");
        };
        let (ready, active) = owner.activate().unwrap();
        let (send, receive) = mpsc::channel();
        let duplicate = thread::spawn(move || send.send(enter_paths(lease, socket)).unwrap());

        assert!(receive.recv_timeout(Duration::from_millis(50)).is_err());
        let mut ready = File::from(ready);
        ready.write_all(READY).unwrap();
        assert!(matches!(
            receive.recv_timeout(Duration::from_secs(1)).unwrap(),
            Ok(LeaseEntry::Joined)
        ));

        duplicate.join().unwrap();
        drop(active);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn duplicate_fails_when_owner_exits_before_confirmation() {
        let (lease, socket, root) = paths("failure");
        let LeaseEntry::Owner(owner) = enter_paths(lease.clone(), socket.clone()).unwrap() else {
            panic!("first entrant must own the lifecycle");
        };
        let (_ready, active) = owner.activate().unwrap();
        let duplicate = thread::spawn(move || enter_paths(lease, socket));
        thread::sleep(Duration::from_millis(30));
        drop(active);

        assert!(matches!(
            duplicate.join().unwrap(),
            Err(Error::ExistingLockerFailed)
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stale_socket_is_replaced_only_after_lease_ownership() {
        let (lease, socket, root) = paths("stale");
        UnixListener::bind(&socket).unwrap();

        let LeaseEntry::Owner(owner) = enter_paths(lease, socket).unwrap() else {
            panic!("stale endpoint must not be joined");
        };
        drop(owner);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn caller_that_observed_an_owner_cannot_replace_its_failed_lifecycle() {
        let (lease_path, socket, root) = paths("generation");
        let lease = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&lease_path)
            .unwrap();
        assert!(try_lock(&lease).unwrap());
        let duplicate = thread::spawn(move || enter_paths(lease_path, socket));
        thread::sleep(Duration::from_millis(30));
        drop(lease);

        assert!(matches!(
            duplicate.join().unwrap(),
            Err(Error::ExistingLockerFailed)
        ));
        fs::remove_dir_all(root).unwrap();
    }
}
