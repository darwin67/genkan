//! Local presentation data. Link state is not Internet reachability.
use std::os::unix::fs::FileExt;

use genkan_session_lock::keyboard::{KeyboardStates, KeyboardStatus};
use iced_runtime::window::raw_window_handle::{RawDisplayHandle, RawWindowHandle};
use wayland_client::protocol::{wl_keyboard, wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};
use xkbcommon::xkb;

/// A short-lived observer on iced's *own* connection. All foreign-display
/// resources are destroyed inside `window::run`, before its borrowed handle
/// expires. We never create another surface or change keyboard focus.
pub(crate) fn keyboard(window: &dyn iced_runtime::window::Window) -> Option<KeyboardStatus> {
    let RawDisplayHandle::Wayland(display) = window.display_handle().ok()?.as_raw() else {
        return None;
    };
    let RawWindowHandle::Wayland(surface) = window.window_handle().ok()?.as_raw() else {
        return None;
    };
    // SAFETY: the window callback keeps the display alive for this entire
    // scope. The foreign backend does not disconnect it on drop.
    let backend = unsafe {
        wayland_client::backend::Backend::from_foreign_display(display.display.as_ptr().cast())
    };
    let connection = Connection::from_backend(backend);
    let mut queue = connection.new_event_queue();
    let mut observer = Observer {
        surface: surface.surface.as_ptr() as usize,
        keyboards: Vec::new(),
        seats: Vec::new(),
        key_states: std::collections::HashMap::new(),
        statuses: Default::default(),
    };
    let registry = connection.display().get_registry(&queue.handle(), ());
    let result = (|| {
        queue.roundtrip(&mut observer).ok()?;
        queue.roundtrip(&mut observer).ok()?;
        queue.roundtrip(&mut observer).ok()?;
        observer.statuses.status().cloned()
    })();
    for keyboard in &observer.keyboards {
        keyboard.release();
    }
    for seat in &observer.seats {
        seat.release();
    }
    // wl_registry has no protocol destructor; dropping a Rust proxy alone
    // does not free its libwayland object on this borrowed connection.
    let _ = connection.backend().destroy_object(&registry.id());
    let _ = connection.flush();
    result
}

#[derive(Default)]
struct KeyState {
    map: Option<xkb::Keymap>,
    focused: bool,
}

struct Observer {
    surface: usize,
    keyboards: Vec<wl_keyboard::WlKeyboard>,
    seats: Vec<wl_seat::WlSeat>,
    key_states: std::collections::HashMap<wayland_client::backend::ObjectId, KeyState>,
    statuses: KeyboardStates<wayland_client::backend::ObjectId>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for Observer {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            if interface == "wl_seat" && version >= 5 {
                let seat: wl_seat::WlSeat = registry.bind(name, version.min(7), qh, ());
                state.seats.push(seat);
            }
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for Observer {
    fn event(
        state: &mut Self,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
        {
            if capabilities.contains(wl_seat::Capability::Keyboard) {
                state.keyboards.push(seat.get_keyboard(qh, ()));
            }
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for Observer {
    fn event(
        state: &mut Self,
        keyboard: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let data = state.key_states.entry(keyboard.id()).or_default();
        match event {
            wl_keyboard::Event::Keymap { format, fd, size } => {
                state.statuses.update(&keyboard.id(), None);
                data.map = None;
                if !matches!(format, WEnum::Value(wl_keyboard::KeymapFormat::XkbV1))
                    || size == 0
                    || size > 1024 * 1024
                {
                    return;
                }
                let mut bytes = vec![0; size as usize];
                // SCM_RIGHTS descriptors can share an open-file offset with
                // the compositor and other keyboard resources. Never seek or
                // consume it: every snapshot must read the keymap from zero.
                if std::fs::File::from(fd).read_exact_at(&mut bytes, 0).is_ok() {
                    let Ok(text) = std::str::from_utf8(&bytes) else {
                        return;
                    };
                    let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
                    data.map = xkb::Keymap::new_from_string(
                        &context,
                        text.trim_end_matches('\0').into(),
                        xkb::KEYMAP_FORMAT_TEXT_V1,
                        xkb::KEYMAP_COMPILE_NO_FLAGS,
                    );
                }
            }
            wl_keyboard::Event::Enter { surface, .. } => {
                data.focused = surface.id().as_ptr() as usize == state.surface;
                if data.focused {
                    state.statuses.enter(keyboard.id());
                }
            }
            wl_keyboard::Event::Leave { .. } => {
                data.focused = false;
                state.statuses.leave(&keyboard.id());
            }
            wl_keyboard::Event::Modifiers {
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
                ..
            } if data.focused => {
                if let Some(map) = &data.map {
                    let mut xkb = xkb::State::new(map);
                    xkb.update_mask(mods_depressed, mods_latched, mods_locked, 0, 0, group);
                    let layout = xkb.serialize_layout(xkb::STATE_LAYOUT_EFFECTIVE);
                    state.statuses.update(
                        &keyboard.id(),
                        Some(KeyboardStatus {
                            caps_lock: xkb
                                .mod_name_is_active(xkb::MOD_NAME_CAPS, xkb::STATE_MODS_EFFECTIVE),
                            layout: (layout < map.num_layouts())
                                .then(|| map.layout_get_name(layout).to_owned())
                                .filter(|name| !name.trim().is_empty()),
                        }),
                    );
                }
            }
            // Never decode, retain or log key events or response text.
            _ => {}
        }
    }
}

pub(crate) fn network() -> Option<&'static str> {
    let entries = std::fs::read_dir("/sys/class/net").ok()?;
    let mut known = false;
    let mut unknown = false;
    for entry in entries.flatten() {
        if entry.file_name() == "lo" {
            continue;
        }
        let path = entry.path();
        let Ok(state) = std::fs::read_to_string(path.join("operstate")) else {
            unknown = true;
            continue;
        };
        match state.trim() {
            "up" => return Some("Network link up"),
            "down" | "lowerlayerdown" | "notpresent" => known = true,
            _ => unknown = true,
        }
    }
    (known && !unknown).then_some("Network link down")
}
