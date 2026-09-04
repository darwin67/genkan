use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use genkan_lock_auth::{Connection, Message, ProtocolError, Secret};
use thiserror::Error;

use super::identity::Identity;

const CHILD_SOCKET_FD: RawFd = 3;
const EVENT_CAPACITY: usize = 128;

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
    #[error("authentication protocol failed: {0}")]
    Protocol(#[from] ProtocolError),
}

pub(super) struct Client {
    writer: Connection,
    events: Option<Receiver<Event>>,
    pid: u32,
    reader: Option<JoinHandle<()>>,
}

impl Client {
    pub(super) fn start(identity: &Identity) -> Result<Self, Error> {
        let worker = worker_path()?;
        let (parent, child_socket) = UnixStream::pair().map_err(Error::Spawn)?;
        let child_fd = child_socket.as_raw_fd();
        let mut command = Command::new(worker);
        command
            .arg("--fd")
            .arg(CHILD_SOCKET_FD.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // SAFETY: this closure uses only async-signal-safe descriptor operations.
        unsafe {
            command.pre_exec(move || inherit_socket(child_fd));
        }
        let child = command.spawn().map_err(Error::Spawn)?;
        drop(child_socket);

        let reader_stream = parent.try_clone().map_err(Error::Spawn)?;
        let writer = Connection::new(parent);
        let pid = child.id();
        let expected_uid = identity.uid;
        let expected_username = identity.username.clone();
        // Keep worker output bounded. A PAM module that emits notices faster
        // than the UI can render them is backpressured by this private queue
        // and then by the socket protocol.
        let (send, events) = mpsc::sync_channel(EVENT_CAPACITY);
        let reader = thread::Builder::new()
            .name("lock-auth-events".into())
            .spawn(move || {
                read_worker(
                    child,
                    reader_stream,
                    expected_uid,
                    &expected_username,
                    &send,
                )
            })
            .map_err(Error::Spawn)?;
        Ok(Self {
            writer,
            events: Some(events),
            pid,
            reader: Some(reader),
        })
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

    pub(super) fn cancel_attempt(&mut self) -> Result<(), Error> {
        self.writer.send(&Message::Cancel)?;
        Ok(())
    }

    pub(super) fn cancel(&mut self) {
        // Unblock a reader waiting to publish into the bounded event queue.
        self.events.take();
        let _ = self.writer.send(&Message::Cancel);
        // SAFETY: the pid belongs to the child spawned and retained by this client.
        unsafe { libc::kill(self.pid as libc::pid_t, libc::SIGTERM) };
        thread::sleep(Duration::from_millis(100));
        // PAM modules are not trusted to honor cancellation or SIGTERM.
        unsafe { libc::kill(self.pid as libc::pid_t, libc::SIGKILL) };
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

fn read_worker(
    mut child: Child,
    stream: UnixStream,
    expected_uid: u32,
    expected_username: &str,
    events: &SyncSender<Event>,
) {
    let mut connection = Connection::new(stream);
    let ready = connection.receive();
    if !matches!(ready, Ok(Message::Ready { uid, ref username }) if uid == expected_uid && username == expected_username)
    {
        let _ = child.kill();
        let _ = child.wait();
        let _ = events.send(Event::Failure);
        return;
    }

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
                send_terminal(&mut child, events, true);
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
    let _ = child.kill();
    let _ = child.wait();
    let _ = events.send(Event::Failure);
}

fn send_terminal(child: &mut Child, events: &SyncSender<Event>, success: bool) {
    let clean_exit = child.wait().is_ok_and(|status| status.success());
    let event = if success && clean_exit {
        Event::Success
    } else {
        Event::Failure
    };
    let _ = events.send(event);
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

fn inherit_socket(source: RawFd) -> io::Result<()> {
    if source == CHILD_SOCKET_FD {
        // SAFETY: F_SETFD only changes flags on the valid inherited descriptor.
        if unsafe { libc::fcntl(source, libc::F_SETFD, 0) } == -1 {
            return Err(io::Error::last_os_error());
        }
    } else {
        // SAFETY: dup2 atomically replaces the child descriptor.
        if unsafe { libc::dup2(source, CHILD_SOCKET_FD) } == -1 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the duplicate now owns the inherited connection.
        unsafe { libc::close(source) };
    }
    Ok(())
}
