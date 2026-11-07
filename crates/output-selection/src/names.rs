//! Monitor identities missing from wl_output: wlroots heads for Cage, niri's
//! bounded read-only IPC query otherwise. Never infer serials from descriptions.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;
use wayland_client::globals::GlobalList;
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols_wlr::output_management::v1::client::{
    zwlr_output_head_v1 as head, zwlr_output_manager_v1 as manager, zwlr_output_mode_v1 as mode,
};

#[derive(Default)]
struct Identity {
    connector: String,
    make: Option<String>,
    model: Option<String>,
    serial: Option<String>,
}

impl Identity {
    fn matches(&self, requested: &str) -> bool {
        self.connector.eq_ignore_ascii_case(requested)
            || ([&self.make, &self.model, &self.serial]
                .iter()
                .any(|s| s.is_some())
                && format!(
                    "{} {} {}",
                    self.make.as_deref().unwrap_or("Unknown"),
                    self.model.as_deref().unwrap_or("Unknown"),
                    self.serial.as_deref().unwrap_or("Unknown")
                )
                .eq_ignore_ascii_case(requested))
    }
}

#[derive(Default)]
struct Metadata {
    heads: Vec<(head::ZwlrOutputHeadV1, Identity)>,
    changed: bool,
}

pub struct OutputNames {
    queue: EventQueue<Metadata>,
    state: Metadata,
    _manager: Option<manager::ZwlrOutputManagerV1>,
    niri: Vec<Identity>,
    dirty: bool,
    enabled: bool,
}

impl OutputNames {
    pub fn new(connection: &Connection, globals: &GlobalList, enabled: bool) -> Self {
        let queue = connection.new_event_queue();
        let manager = enabled
            .then(|| globals.bind(&queue.handle(), 2..=4, ()).ok())
            .flatten();
        Self {
            queue,
            state: Metadata::default(),
            _manager: manager,
            niri: Vec::new(),
            dirty: true,
            enabled,
        }
    }

    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    /// Dispatch already-read metadata without blocking the credential event
    /// loop. The niri query runs only on output changes, with bounded I/O/size.
    pub fn update(&mut self) -> bool {
        let _ = self.queue.dispatch_pending(&mut self.state);
        let changed = std::mem::take(&mut self.state.changed);
        let dirty = std::mem::take(&mut self.dirty);
        if dirty && self.enabled && self._manager.is_none() {
            self.niri = niri_outputs().unwrap_or_default();
        }
        changed || dirty
    }

    pub fn resolve(&self, requested: Option<&str>) -> Option<String> {
        let requested = requested?;
        self.state
            .heads
            .iter()
            .map(|(_, identity)| identity)
            .chain(self.niri.iter())
            .find(|identity| identity.matches(requested))
            .map(|identity| identity.connector.clone())
            .or_else(|| Some(requested.to_owned()))
    }
}

fn niri_outputs() -> Option<Vec<Identity>> {
    let socket = std::env::var_os("NIRI_SOCKET")?;
    let mut socket = UnixStream::connect(socket).ok()?;
    let timeout = Some(Duration::from_millis(100));
    socket.set_read_timeout(timeout).ok()?;
    socket.set_write_timeout(timeout).ok()?;
    socket.write_all(b"\"Outputs\"\n").ok()?;
    let mut reply = String::new();
    BufReader::new(socket)
        .take(1024 * 1024)
        .read_line(&mut reply)
        .ok()?;
    if !reply.ends_with('\n') {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(&reply).ok()?;
    Some(
        value
            .get("Ok")?
            .get("Outputs")?
            .as_object()?
            .iter()
            .map(|(name, value)| {
                let field = |key| value.get(key).and_then(|v| v.as_str()).map(str::to_owned);
                Identity {
                    connector: name.clone(),
                    make: field("make"),
                    model: field("model"),
                    serial: field("serial"),
                }
            })
            .collect(),
    )
}

impl Dispatch<manager::ZwlrOutputManagerV1, ()> for Metadata {
    fn event(
        state: &mut Self,
        _: &manager::ZwlrOutputManagerV1,
        event: manager::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            manager::Event::Head { head } => state.heads.push((head, Identity::default())),
            manager::Event::Done { .. } => state.changed = true,
            _ => {}
        }
    }
    wayland_client::event_created_child!(Self, manager::ZwlrOutputManagerV1, [0 => (head::ZwlrOutputHeadV1, ())]);
}

impl Dispatch<head::ZwlrOutputHeadV1, ()> for Metadata {
    fn event(
        state: &mut Self,
        proxy: &head::ZwlrOutputHeadV1,
        event: head::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some((_, identity)) = state.heads.iter_mut().find(|(head, _)| head == proxy) else {
            return;
        };
        match event {
            head::Event::Name { name } => identity.connector = name,
            head::Event::Make { make } => identity.make = Some(make),
            head::Event::Model { model } => identity.model = Some(model),
            head::Event::SerialNumber { serial_number } => identity.serial = Some(serial_number),
            head::Event::Finished => {
                state.heads.retain(|(head, _)| head != proxy);
                if proxy.version() >= 3 {
                    proxy.release();
                }
            }
            _ => {}
        }
    }
    wayland_client::event_created_child!(Self, head::ZwlrOutputHeadV1, [3 => (mode::ZwlrOutputModeV1, ())]);
}

impl Dispatch<mode::ZwlrOutputModeV1, ()> for Metadata {
    fn event(
        _: &mut Self,
        proxy: &mode::ZwlrOutputModeV1,
        event: mode::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if matches!(event, mode::Event::Finished) && proxy.version() >= 3 {
            proxy.release();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn persistent_names_match_niri_including_missing_fields_and_serials() {
        let identity = Identity {
            connector: "DP-3".into(),
            make: Some("Dell Inc.".into()),
            model: Some("DELL U3223QE".into()),
            serial: Some("42".into()),
        };
        assert!(identity.matches("dell inc. dell u3223qe 42"));
        assert!(identity.matches("dp-3"));
        assert!(!identity.matches("Dell Inc. DELL U3223QE 43"));
        let identity = Identity {
            serial: None,
            ..identity
        };
        assert!(identity.matches("Dell Inc. DELL U3223QE Unknown"));
        assert!(!identity.matches("Dell Inc. DELL U3223QE"));
        assert!(!Identity::default().matches("Unknown Unknown Unknown"));
    }
}
