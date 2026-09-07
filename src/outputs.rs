use std::hash::{Hash, Hasher};
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::thread;

use iced::futures::stream;
use iced::Subscription;
use smithay_client_toolkit::{
    delegate_output, delegate_registry,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
};
use tokio::sync::watch;
use wayland_client::{
    backend::WaylandError, globals::registry_queue_init, protocol::wl_output, Connection,
    QueueHandle,
};

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Region {
    pub(crate) x: f32,
    pub(crate) y: f32,
    pub(crate) width: f32,
    pub(crate) height: f32,
    pub(crate) layout_width: f32,
    pub(crate) layout_height: f32,
}

impl Region {
    pub(crate) fn scale_to(self, width: f32, height: f32) -> Self {
        let scale_x = width / self.layout_width;
        let scale_y = height / self.layout_height;
        Self {
            x: self.x * scale_x,
            y: self.y * scale_y,
            width: self.width * scale_x,
            height: self.height * scale_y,
            layout_width: width,
            layout_height: height,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Output {
    name: Option<String>,
    position: (i32, i32),
    size: (i32, i32),
}

#[derive(Clone)]
pub(crate) struct Monitor {
    shared: Arc<Shared>,
    signal: watch::Receiver<u64>,
    _worker: Arc<Worker>,
}

impl std::fmt::Debug for Monitor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Monitor").finish_non_exhaustive()
    }
}

struct Shared {
    region: Mutex<Option<Region>>,
}

struct Worker {
    stop: UnixStream,
    thread: Mutex<Option<thread::JoinHandle<()>>>,
}

impl Monitor {
    pub(crate) fn connect(requested: Option<String>) -> Result<Self, Box<dyn std::error::Error>> {
        let (connection, mut queue, mut state) = connect()?;
        queue.roundtrip(&mut state)?;
        let region = select_region(&state.outputs(), requested.as_deref());
        eprintln!("genkan login: selected authentication region {region:?}");
        let shared = Arc::new(Shared {
            region: Mutex::new(region),
        });
        let (signal_sender, signal) = watch::channel(0_u64);
        let (stop, worker_stop) = UnixStream::pair()?;
        let worker_shared = Arc::clone(&shared);
        let worker = thread::Builder::new()
            .name("output-layout".into())
            .spawn(move || {
                monitor_outputs(
                    connection,
                    &mut queue,
                    &mut state,
                    requested.as_deref(),
                    &worker_shared,
                    &signal_sender,
                    &worker_stop,
                )
            })?;
        Ok(Self {
            shared,
            signal,
            _worker: Arc::new(Worker {
                stop,
                thread: Mutex::new(Some(worker)),
            }),
        })
    }

    pub(crate) fn region(&self) -> Option<Region> {
        *lock(&self.shared.region)
    }

    pub(crate) fn subscription(&self) -> Subscription<()> {
        Subscription::run_with(
            MonitorSignal {
                monitor: Arc::as_ptr(&self.shared) as usize,
                receiver: self.signal.clone(),
            },
            |signal| {
                stream::unfold(signal.receiver.clone(), |mut receiver| async move {
                    receiver.changed().await.ok().map(|()| ((), receiver))
                })
            },
        )
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = std::io::Write::write_all(&mut &self.stop, &[0]);
        if let Some(worker) = lock(&self.thread).take() {
            let _ = worker.join();
        }
    }
}

struct MonitorSignal {
    monitor: usize,
    receiver: watch::Receiver<u64>,
}

impl Hash for MonitorSignal {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.monitor.hash(state);
    }
}

fn connect() -> Result<
    (Connection, wayland_client::EventQueue<Discovery>, Discovery),
    Box<dyn std::error::Error>,
> {
    if inherited_wayland_socket(std::env::var_os("WAYLAND_SOCKET").as_deref()) {
        return Err(std::io::Error::other(
            "dynamic output monitoring is unavailable with an inherited WAYLAND_SOCKET",
        )
        .into());
    }
    let connection = Connection::connect_to_env()?;
    let (globals, queue) = registry_queue_init(&connection)?;
    let handle = queue.handle();
    let state = Discovery {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &handle),
        changed: false,
    };
    Ok((connection, queue, state))
}

fn inherited_wayland_socket(value: Option<&std::ffi::OsStr>) -> bool {
    value.is_some_and(|value| !value.is_empty())
}

impl Discovery {
    fn outputs(&self) -> Vec<Output> {
        self.output_state
            .outputs()
            .filter_map(|output| self.output_state.info(&output))
            .filter_map(|info| {
                Some(Output {
                    name: info.name,
                    position: info.logical_position?,
                    size: info.logical_size?,
                })
            })
            .filter(|output| output.size.0 > 0 && output.size.1 > 0)
            .collect()
    }
}

fn monitor_outputs(
    connection: Connection,
    queue: &mut wayland_client::EventQueue<Discovery>,
    state: &mut Discovery,
    requested: Option<&str>,
    shared: &Shared,
    signal: &watch::Sender<u64>,
    stop: &UnixStream,
) {
    loop {
        if queue.dispatch_pending(state).is_err() {
            return;
        }
        if state.changed {
            state.changed = false;
            publish_region(&state.outputs(), requested, shared, signal);
        }
        let flush_pending = match queue.flush() {
            Ok(()) => false,
            Err(error) if is_would_block(&error) => true,
            Err(error) => {
                eprintln!("genkan login: output monitor stopped while flushing events: {error}");
                return;
            }
        };
        let Some(read) = queue.prepare_read() else {
            continue;
        };
        let mut descriptors = [
            libc::pollfd {
                fd: connection.as_fd().as_raw_fd(),
                events: libc::POLLIN | if flush_pending { libc::POLLOUT } else { 0 },
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
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return;
        }
        if descriptors[1].revents != 0 {
            return;
        }
        if descriptors[0].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
            match read.read() {
                Ok(_) => {}
                Err(error) if is_would_block(&error) => continue,
                Err(error) => {
                    eprintln!("genkan login: output monitor stopped while reading events: {error}");
                    return;
                }
            }
        } else if descriptors[0].revents & libc::POLLNVAL != 0 {
            eprintln!("genkan login: output monitor stopped after its Wayland socket closed");
            return;
        }
    }
}

fn is_would_block(error: &WaylandError) -> bool {
    matches!(error, WaylandError::Io(error) if error.kind() == std::io::ErrorKind::WouldBlock)
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn publish_region(
    outputs: &[Output],
    requested: Option<&str>,
    shared: &Shared,
    signal: &watch::Sender<u64>,
) -> bool {
    let region = select_region(outputs, requested);
    let mut current = lock(&shared.region);
    if *current == region {
        return false;
    }
    eprintln!("genkan login: selected authentication region {region:?}");
    *current = region;
    signal.send_modify(|sequence| *sequence = sequence.wrapping_add(1));
    true
}

fn select_region(outputs: &[Output], requested: Option<&str>) -> Option<Region> {
    let selected = genkan_output_selection::select(
        outputs
            .iter()
            .enumerate()
            .map(|(index, output)| (index, output.name.as_deref())),
        requested,
    )?;
    let min_x = outputs.iter().map(|output| output.position.0).min()?;
    let min_y = outputs.iter().map(|output| output.position.1).min()?;
    let max_x = outputs
        .iter()
        .map(|output| output.position.0.saturating_add(output.size.0))
        .max()?;
    let max_y = outputs
        .iter()
        .map(|output| output.position.1.saturating_add(output.size.1))
        .max()?;
    let output = &outputs[selected];
    Some(Region {
        x: (output.position.0 - min_x) as f32,
        y: (output.position.1 - min_y) as f32,
        width: output.size.0 as f32,
        height: output.size.1 as f32,
        layout_width: (max_x - min_x) as f32,
        layout_height: (max_y - min_y) as f32,
    })
}

struct Discovery {
    registry_state: RegistryState,
    output_state: OutputState,
    changed: bool,
}

impl OutputHandler for Discovery {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
        self.changed = true;
    }
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
        self.changed = true;
    }
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
        self.changed = true;
    }
}

impl ProvidesRegistryState for Discovery {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }

    registry_handlers![OutputState];
}

delegate_output!(Discovery);
delegate_registry!(Discovery);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inherited_wayland_socket_is_reserved_for_the_gui_connection() {
        assert!(inherited_wayland_socket(Some(std::ffi::OsStr::new("17"))));
        assert!(!inherited_wayland_socket(None));
        assert!(!inherited_wayland_socket(Some(std::ffi::OsStr::new(""))));
    }

    #[test]
    fn wayland_backpressure_is_recoverable() {
        assert!(is_would_block(&WaylandError::Io(std::io::Error::from(
            std::io::ErrorKind::WouldBlock
        ))));
        assert!(!is_would_block(&WaylandError::Io(std::io::Error::from(
            std::io::ErrorKind::ConnectionReset
        ))));
    }

    #[test]
    fn region_uses_external_output_and_normalizes_negative_layout_coordinates() {
        let outputs = [
            Output {
                name: Some("eDP-1".into()),
                position: (0, 0),
                size: (1920, 1080),
            },
            Output {
                name: Some("DP-1".into()),
                position: (-2560, -360),
                size: (2560, 1440),
            },
        ];

        assert_eq!(
            select_region(&outputs, None),
            Some(Region {
                x: 0.0,
                y: 0.0,
                width: 2560.0,
                height: 1440.0,
                layout_width: 4480.0,
                layout_height: 1440.0,
            })
        );
    }

    #[test]
    fn region_falls_back_to_internal_panel() {
        let outputs = [Output {
            name: Some("eDP-1".into()),
            position: (0, 0),
            size: (1920, 1080),
        }];
        assert_eq!(select_region(&outputs, Some("DP-1")).unwrap().width, 1920.0);
    }

    #[test]
    fn region_scales_to_the_actual_compositor_window() {
        let region = Region {
            x: 1920.0,
            y: 0.0,
            width: 2560.0,
            height: 1440.0,
            layout_width: 4480.0,
            layout_height: 1440.0,
        };

        assert_eq!(
            region.scale_to(2240.0, 720.0),
            Region {
                x: 960.0,
                y: 0.0,
                width: 1280.0,
                height: 720.0,
                layout_width: 2240.0,
                layout_height: 720.0,
            }
        );
    }

    #[test]
    fn topology_snapshots_reselect_after_hotplug_removal_and_rearrangement() {
        let internal = Output {
            name: Some("eDP-1".into()),
            position: (0, 0),
            size: (1920, 1080),
        };
        assert_eq!(
            select_region(std::slice::from_ref(&internal), None)
                .unwrap()
                .x,
            0.0
        );

        let external = Output {
            name: Some("DP-1".into()),
            position: (1920, 0),
            size: (2560, 1440),
        };
        let docked = select_region(&[internal.clone(), external.clone()], None).unwrap();
        assert_eq!((docked.x, docked.width), (1920.0, 2560.0));

        let rearranged = select_region(
            &[
                internal.clone(),
                Output {
                    position: (-2560, -360),
                    ..external.clone()
                },
            ],
            None,
        )
        .unwrap();
        assert_eq!((rearranged.x, rearranged.y), (0.0, 0.0));

        let removed = select_region(&[internal], None).unwrap();
        assert_eq!((removed.width, removed.layout_width), (1920.0, 1920.0));
    }

    #[test]
    fn logical_geometry_handles_mixed_output_scales() {
        let outputs = [
            Output {
                name: Some("eDP-1".into()),
                position: (0, 0),
                size: (1600, 1000),
            },
            Output {
                name: Some("DP-1".into()),
                position: (1600, 0),
                size: (1920, 1080),
            },
        ];

        let selected = select_region(&outputs, None).unwrap();

        assert_eq!(selected.x, 1600.0);
        assert_eq!(selected.layout_width, 3520.0);
        assert_eq!(selected.height, 1080.0);
    }

    #[test]
    fn changed_topology_publishes_only_new_layout_snapshots() {
        let internal = Output {
            name: Some("eDP-1".into()),
            position: (0, 0),
            size: (1920, 1080),
        };
        let external = Output {
            name: Some("DP-1".into()),
            position: (1920, 0),
            size: (2560, 1440),
        };
        let shared = Shared {
            region: Mutex::new(select_region(std::slice::from_ref(&internal), None)),
        };
        let (signal, receiver) = watch::channel(0_u64);

        assert!(!publish_region(
            std::slice::from_ref(&internal),
            None,
            &shared,
            &signal
        ));
        assert_eq!(*receiver.borrow(), 0);
        assert!(publish_region(
            &[internal, external],
            None,
            &shared,
            &signal
        ));
        assert_eq!(*receiver.borrow(), 1);
        assert_eq!(lock(&shared.region).unwrap().x, 1920.0);
    }
}
