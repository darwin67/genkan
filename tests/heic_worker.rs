//! End-to-end checks for the dynamic HEIC relay worker process.
//!
//! These run the real `genkan heic-worker` binary that `login` and `lock` spawn
//! for untrusted wallpapers, proving the child decodes frames, reports bounded
//! failures over the relay protocol, and cannot outlive the greeter.
//!
//! Every read is deadline-aware, and every spawned process is guarded by
//! bounded best-effort cleanup, so a stalled, crashed, or orphaned helper fails
//! the test instead of hanging CI. Cleanup is a last resort for a failing
//! assertion, not a guarantee on every conceivable failure path.

use std::io::ErrorKind;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

const FRAME_TAG: u8 = b'F';
const FAILED_TAG: u8 = b'E';
const HEADER_BYTES: usize = 12;
/// Mirrors `src/wallpaper.rs`: the relay refuses more than the decoder allows.
const MAX_FRAME_DIMENSION: u32 = 16_384;
const MAX_FRAME_BYTES: usize = 256 * 1024 * 1024;
/// Generous enough for a real libheif decode, short enough to fail CI.
const DEADLINE: Duration = Duration::from_secs(60);

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/dynamic-heic")
        .join(name)
}

fn worker_command() -> Command {
    Command::new(env!("CARGO_BIN_EXE_genkan"))
}

/// Spawns the real worker with its stdout piped for a caller that reads the
/// relay protocol.
fn spawn_worker(arguments: &[&str]) -> Child {
    let mut command = worker_command();
    command
        .arg("heic-worker")
        .args(arguments)
        .arg("--parent-pid")
        .arg(std::process::id().to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    command.spawn().expect("spawn the HEIC worker")
}

/// Waits for a child to exit without ever blocking forever.
fn wait_for_exit(child: &mut Child, timeout: Duration) -> ExitStatus {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().expect("try_wait a child") {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "a child did not exit within the deadline"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Kills and reaps a child on every exit path, including a panic.
struct ChildGuard {
    child: Child,
}

impl ChildGuard {
    fn new(child: Child) -> Self {
        Self { child }
    }

    fn stdout(&mut self) -> ChildStdout {
        self.child.stdout.take().expect("worker stdout")
    }

    fn wait_bounded(&mut self) -> ExitStatus {
        wait_for_exit(&mut self.child, DEADLINE)
    }

    /// Waits for exit and proves the parent reaped it.
    ///
    /// `try_wait` returns a status only after `waitpid` has collected the
    /// child, so a returned status is itself proof of reaping.
    fn reap(&mut self) -> ExitStatus {
        self.wait_bounded()
    }

    /// Kills the child, then proves the parent reaped the signal death.
    fn kill_and_reap(&mut self) {
        let _ = self.child.kill();
        let status = self.reap();
        assert!(
            status.code().is_none(),
            "a killed worker must report a signal, got {status:?}"
        );
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        // Bounded best effort: a killed helper exits promptly, and a failing
        // test must not hang on cleanup.
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(_)) | Err(_) => break,
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
    }
}

/// Restores the previous child-subreaper setting on every exit path.
struct SubreaperGuard {
    previous: i32,
}

impl SubreaperGuard {
    fn install() -> Self {
        let mut previous = 0;
        // SAFETY: both calls change only this process and write to a live
        // local.
        assert_eq!(
            unsafe { libc::prctl(libc::PR_GET_CHILD_SUBREAPER, &mut previous) },
            0,
            "the test needs child-subreaper support"
        );
        assert_eq!(
            unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1) },
            0,
            "the test needs child-subreaper support"
        );
        Self { previous }
    }
}

impl Drop for SubreaperGuard {
    fn drop(&mut self) {
        // SAFETY: changes only this process.
        let _ = unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, self.previous) };
    }
}

/// Owns the intermediate greeter and its whole process group for the test.
///
/// A single owner avoids two independently active cleanup guards: group-level
/// reaping collects the leader, so a later numeric `Child::kill` on it could
/// target a recycled pid. The group is established when the greeter is spawned,
/// so a worker whose pid was never read is still cleaned up. Cleanup is bounded
/// best effort.
struct GreeterGroup {
    child: Child,
    pgid: i32,
    finished: bool,
}

impl GreeterGroup {
    fn new(child: Child) -> Self {
        let pgid = child.id() as i32;
        Self {
            child,
            pgid,
            finished: false,
        }
    }

    fn pid(&self) -> u32 {
        self.pgid as u32
    }

    fn stdout(&mut self) -> ChildStdout {
        self.child.stdout.take().expect("greeter stdout")
    }

    fn wait_bounded(&mut self) -> ExitStatus {
        wait_for_exit(&mut self.child, DEADLINE)
    }

    /// Records that every process in the group was collected.
    ///
    /// Call this before asserting on collected exit statuses so a failing
    /// assertion cannot trigger cleanup of already-reaped pids.
    fn finish(&mut self) {
        self.finished = true;
    }
}

impl Drop for GreeterGroup {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        // Kill the whole group rather than the leader by number, so no signal
        // can target a pid that group reaping has already collected.
        // SAFETY: signals the process group this test created.
        unsafe { libc::kill(-self.pgid, libc::SIGKILL) };
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut status = 0;
        while Instant::now() < deadline {
            // SAFETY: a process-group id and a valid out pointer are passed.
            let result = unsafe { libc::waitpid(-self.pgid, &mut status, libc::WNOHANG) };
            if result == 0 {
                // Every group member is either reaped or still running; retry
                // briefly so a just-signalled member is collected.
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
            if result < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == ErrorKind::Interrupted {
                    continue;
                }
                break;
            }
        }
    }
}

/// Reads relay bytes from a worker with a deadline, so a stalled helper fails
/// the test rather than blocking it forever.
struct ProtocolReader {
    fd: RawFd,
    deadline: Instant,
}

impl ProtocolReader {
    fn new(stdout: &impl AsRawFd) -> Self {
        Self {
            fd: stdout.as_raw_fd(),
            deadline: Instant::now() + DEADLINE,
        }
    }

    fn read_exact(&mut self, buffer: &mut [u8]) {
        let mut filled = 0;
        while filled < buffer.len() {
            self.wait_readable();
            // SAFETY: the descriptor stays live for the reader's lifetime.
            let read = unsafe {
                libc::read(
                    self.fd,
                    buffer[filled..].as_mut_ptr().cast(),
                    buffer.len() - filled,
                )
            };
            if read < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == ErrorKind::Interrupted {
                    continue;
                }
                panic!("reading the worker stream failed: {error}");
            }
            assert!(
                read > 0,
                "the worker stream ended after {filled} of {} bytes",
                buffer.len()
            );
            filled += read as usize;
        }
    }

    fn wait_readable(&mut self) {
        loop {
            let remaining = self.deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                panic!("the HEIC worker did not speak within the deadline");
            }
            let mut descriptor = libc::pollfd {
                fd: self.fd,
                events: libc::POLLIN,
                revents: 0,
            };
            let millis = i32::try_from(remaining.as_millis()).unwrap_or(i32::MAX);
            // SAFETY: one initialized `pollfd` is passed with a count of one.
            let ready = unsafe { libc::poll(&mut descriptor, 1, millis) };
            if ready > 0 {
                return;
            }
            if ready == 0 {
                continue;
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != ErrorKind::Interrupted {
                panic!("polling the worker stream failed: {error}");
            }
        }
    }

    fn read_tag(&mut self) -> u8 {
        let mut tag = [0u8; 1];
        self.read_exact(&mut tag);
        tag[0]
    }

    /// Reads one frame, enforcing the same ceilings the relay enforces.
    fn read_frame(&mut self) -> (u32, u32, Vec<u8>) {
        assert_eq!(self.read_tag(), FRAME_TAG, "worker did not emit a frame");
        let mut header = [0u8; HEADER_BYTES];
        self.read_exact(&mut header);
        let width = u32::from_le_bytes(header[0..4].try_into().unwrap());
        let height = u32::from_le_bytes(header[4..8].try_into().unwrap());
        let length = u32::from_le_bytes(header[8..12].try_into().unwrap()) as usize;

        assert!(
            width > 0 && height > 0,
            "relayed dimensions must be nonzero"
        );
        assert!(
            width <= MAX_FRAME_DIMENSION && height <= MAX_FRAME_DIMENSION,
            "relayed dimensions must respect the per-axis cap"
        );
        assert_eq!(
            length,
            width as usize * height as usize * 4,
            "relayed length must be the exact RGBA size"
        );
        assert!(
            length <= MAX_FRAME_BYTES,
            "relayed frames must respect the byte cap"
        );

        let mut pixels = vec![0u8; length];
        self.read_exact(&mut pixels);
        (width, height, pixels)
    }
}

#[test]
fn worker_decodes_a_scheduled_frame() {
    let mut worker = ChildGuard::new(spawn_worker(&[
        "--file",
        fixture("synthetic-all-properties.heic")
            .to_str()
            .expect("fixture path"),
        "--appearance",
        "automatic",
        "--reduce-motion",
    ]));
    let stdout = worker.stdout();
    let mut reader = ProtocolReader::new(&stdout);

    let (width, height, pixels) = reader.read_frame();
    assert_eq!((width, height), (8, 8));
    assert_eq!(pixels.len(), 8 * 8 * 4);
    assert!(
        pixels.chunks_exact(4).all(|pixel| pixel[3] == 255),
        "relayed frames must be opaque RGBA"
    );

    worker.kill_and_reap();
}

#[test]
fn worker_reports_a_missing_file_as_a_bounded_failure() {
    let mut worker = ChildGuard::new(spawn_worker(&[
        "--file",
        "/nonexistent/genkan-missing-dynamic-wallpaper.heic",
        "--appearance",
        "automatic",
    ]));
    let stdout = worker.stdout();
    let mut reader = ProtocolReader::new(&stdout);

    assert_eq!(reader.read_tag(), FAILED_TAG);

    // A bounded failure ends the worker instead of leaving it polling.
    let status = worker.reap();
    assert_eq!(
        status.code(),
        Some(0),
        "a reported failure is a normal worker exit"
    );
}

#[test]
fn worker_requires_a_parent_binding() {
    let mut command = worker_command();
    command
        .arg("heic-worker")
        .arg("--file")
        .arg(fixture("synthetic-all-properties.heic"))
        .arg("--appearance")
        .arg("automatic")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    // The guard keeps a defective worker that ignores the missing argument
    // from hanging the suite: it would decode forever and never exit.
    let mut worker = ChildGuard::new(command.spawn().expect("spawn the worker without a binding"));
    let status = worker.wait_bounded();
    assert!(
        !status.success(),
        "a worker without --parent-pid must not run"
    );
    let mut stderr = String::new();
    use std::io::Read;
    worker
        .child
        .stderr
        .take()
        .expect("worker stderr")
        .read_to_string(&mut stderr)
        .expect("read worker stderr");
    assert!(
        stderr.contains("--parent-pid"),
        "the missing parent binding must be named: {stderr}"
    );
}

/// A static worker keeps polling after its first frame. If the greeter is
/// killed without unwinding, that poll must not survive it.
#[test]
fn worker_exits_when_its_greeter_is_killed() {
    // Become a subreaper so the orphaned worker is reparented to this test and
    // can be reaped here, which also yields its real exit signal.
    let _subreaper = SubreaperGuard::install();

    let pid_file = std::env::temp_dir().join(format!(
        "genkan-heic-parent-death-{}.pid",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&pid_file);

    // An intermediate shell is the worker's parent. It stays alive until it is
    // killed, which is exactly the greeter death the worker must survive only
    // as a corpse. It leads its own process group so cleanup can reach the
    // worker even if its pid is never read.
    let mut greeter = GreeterGroup::new(
        Command::new("sh")
            .arg("-c")
            .arg("\"$1\" heic-worker --file \"$2\" --appearance automatic --parent-pid $$ & echo $! > \"$3\"; wait")
            .arg("sh")
            .arg(env!("CARGO_BIN_EXE_genkan"))
            .arg(fixture("synthetic-all-properties.heic"))
            .arg(&pid_file)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .expect("spawn the intermediate greeter"),
    );
    let stdout = greeter.stdout();

    let worker_pid = wait_for_pid_file(&pid_file);
    // The first frame proves the worker is past its parent binding and is
    // actively decoding, so the kill below is a real parent death.
    let mut reader = ProtocolReader::new(&stdout);
    let (width, height, _) = reader.read_frame();
    assert_eq!((width, height), (8, 8));

    // Kill the greeter without giving it a chance to run any destructor.
    // SAFETY: the pid is the live intermediate greeter.
    assert_eq!(
        unsafe { libc::kill(greeter.pid() as i32, libc::SIGKILL) },
        0
    );
    // Observe the greeter's death without reaping it. The unreaped leader
    // anchors the process-group id, so cleanup cannot signal a recycled group
    // while the worker is collected.
    assert!(
        wait_for_greeter_exit_without_reaping(greeter.pid(), DEADLINE),
        "the greeter did not exit"
    );
    // Collect the worker while the greeter's unreaped zombie still anchors the
    // group id, then reap the greeter and disarm cleanup before asserting.
    let worker_status = wait_for_orphan_exit(worker_pid);
    let greeter_status = greeter.wait_bounded();
    greeter.finish();

    assert_eq!(
        greeter_status.signal(),
        Some(libc::SIGKILL),
        "the greeter must have been killed by a signal, got {greeter_status:?}"
    );
    assert_eq!(
        worker_status.signal(),
        Some(libc::SIGKILL),
        "the parent-death signal must kill the worker, got {worker_status:?}"
    );

    let _ = std::fs::remove_file(&pid_file);
}

/// Waits for a child to exit without reaping it.
///
/// `WNOWAIT` leaves the child waitable, which keeps its process-group id in use
/// so group cleanup cannot signal a recycled group.
fn wait_for_greeter_exit_without_reaping(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: `waitid` is called with a specific pid, a valid out pointer,
        // and `WNOWAIT`, which leaves the child waitable.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        assert_eq!(
            result,
            0,
            "waitid failed: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: `waitid` succeeded, so `info` was filled in.
        if unsafe { info.si_pid() } == pid as libc::pid_t {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_pid_file(path: &Path) -> u32 {
    let deadline = Instant::now() + DEADLINE;
    loop {
        if let Ok(contents) = std::fs::read_to_string(path) {
            if let Ok(pid) = contents.trim().parse::<u32>() {
                if pid > 1 {
                    return pid;
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "the intermediate greeter did not report its worker"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Reaps the orphaned worker and returns its real exit status.
///
/// The test is a child subreaper, so the worker is reparented here when its
/// greeter dies.
fn wait_for_orphan_exit(pid: u32) -> ExitStatus {
    let deadline = Instant::now() + DEADLINE;
    let mut status = 0;
    loop {
        // SAFETY: `waitpid` is called with a specific pid and a valid out
        // pointer. `WNOHANG` keeps it from blocking past the deadline.
        let result = unsafe { libc::waitpid(pid as i32, &mut status, libc::WNOHANG) };
        if result == pid as i32 {
            return ExitStatus::from_raw(status);
        }
        if result < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == ErrorKind::Interrupted {
                continue;
            }
            panic!("the orphaned worker was not reparented to the test: {error}");
        }
        assert!(Instant::now() < deadline, "the helper survived its greeter");
        std::thread::sleep(Duration::from_millis(10));
    }
}
