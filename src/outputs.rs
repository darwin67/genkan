use smithay_client_toolkit::{
    delegate_output, delegate_registry,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
};
use wayland_client::{globals::registry_queue_init, protocol::wl_output, Connection, QueueHandle};

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

#[derive(Debug)]
struct Output {
    name: Option<String>,
    position: (i32, i32),
    size: (i32, i32),
}

pub(crate) fn authentication_region(requested: Option<&str>) -> Option<Region> {
    discover()
        .inspect_err(|error| eprintln!("genkan login: could not discover output layout: {error}"))
        .ok()
        .and_then(|outputs| select_region(&outputs, requested))
}

fn discover() -> Result<Vec<Output>, Box<dyn std::error::Error>> {
    let connection = Connection::connect_to_env()?;
    let (globals, mut queue) = registry_queue_init(&connection)?;
    let handle = queue.handle();
    let mut state = Discovery {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &handle),
    };
    queue.roundtrip(&mut state)?;

    Ok(state
        .output_state
        .outputs()
        .filter_map(|output| state.output_state.info(&output))
        .filter_map(|info| {
            Some(Output {
                name: info.name,
                position: info.logical_position?,
                size: info.logical_size?,
            })
        })
        .filter(|output| output.size.0 > 0 && output.size.1 > 0)
        .collect())
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
}

impl OutputHandler for Discovery {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
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
}
