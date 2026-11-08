//! Optional exact-size Wayland rendering shared by the lock and wallpaper.
//! Both protocols are needed: a fractional buffer uses buffer scale one and a
//! viewport destination in logical coordinates. Otherwise use integer scaling.

use wayland_client::globals::GlobalList;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Dispatch, QueueHandle};
pub use wayland_protocols::wp::fractional_scale::v1::client::{
    wp_fractional_scale_manager_v1, wp_fractional_scale_v1,
};
pub use wayland_protocols::wp::viewporter::client::{wp_viewport, wp_viewporter};

pub struct Scaling {
    fractional: Option<wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1>,
    viewporter: Option<wp_viewporter::WpViewporter>,
}

impl Scaling {
    pub fn bind<D>(globals: &GlobalList, qh: &QueueHandle<D>) -> Self
    where
        D: Dispatch<wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1, ()>
            + Dispatch<wp_viewporter::WpViewporter, ()>
            + 'static,
    {
        Self {
            fractional: globals.bind(qh, 1..=1, ()).ok(),
            viewporter: globals.bind(qh, 1..=1, ()).ok(),
        }
    }

    pub fn create<D>(&self, surface: &WlSurface, qh: &QueueHandle<D>) -> SurfaceScale
    where
        D: Dispatch<wp_fractional_scale_v1::WpFractionalScaleV1, WlSurface>
            + Dispatch<wp_viewport::WpViewport, ()>
            + 'static,
    {
        let pair = self.fractional.as_ref().zip(self.viewporter.as_ref());
        SurfaceScale {
            fractional: pair
                .map(|(manager, _)| manager.get_fractional_scale(surface, qh, surface.clone())),
            viewport: pair.map(|(_, manager)| manager.get_viewport(surface, qh, ())),
            integer: 1,
            preferred: None,
        }
    }
}

pub struct SurfaceScale {
    fractional: Option<wp_fractional_scale_v1::WpFractionalScaleV1>,
    viewport: Option<wp_viewport::WpViewport>,
    integer: i32,
    preferred: Option<u32>,
}

impl SurfaceScale {
    pub fn set_integer(&mut self, scale: i32) -> bool {
        let before = self.units();
        self.integer = scale.max(1);
        self.units() != before
    }

    /// Zero is not a valid preferred scale; callers must fail presentation.
    pub fn set_preferred(&mut self, scale: u32) -> bool {
        let before = self.units();
        self.preferred = Some(scale);
        self.units() != before
    }

    pub fn units(&self) -> u32 {
        self.preferred
            .unwrap_or_else(|| (self.integer as u32).saturating_mul(120))
    }

    pub fn factor(&self) -> f64 {
        f64::from(self.units()) / 120.0
    }

    pub fn buffer_size(&self, logical: (u32, u32)) -> Option<(u32, u32)> {
        buffer_size(logical, self.units())
    }

    /// Apply with the same commit that attaches the corresponding buffer.
    pub fn apply(&self, surface: &WlSurface, logical: (u32, u32)) {
        if let Some(viewport) = &self.viewport {
            surface.set_buffer_scale(1);
            viewport.set_destination(logical.0 as i32, logical.1 as i32);
        } else {
            surface.set_buffer_scale(self.integer);
        }
    }
}

impl Drop for SurfaceScale {
    fn drop(&mut self) {
        if let Some(fractional) = &self.fractional {
            fractional.destroy();
        }
        if let Some(viewport) = &self.viewport {
            viewport.destroy();
        }
    }
}

fn buffer_size((width, height): (u32, u32), scale: u32) -> Option<(u32, u32)> {
    if width == 0 || height == 0 || scale == 0 {
        return None;
    }
    let pixels =
        |logical| u32::try_from((u64::from(logical) * u64::from(scale)).div_ceil(120)).ok();
    Some((pixels(width)?, pixels(height)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fractional_buffers_round_up_each_axis_without_integer_oversampling() {
        for (scale, expected) in [
            (153, (1024, 767)), // 1.275
            (156, (1044, 782)), // 1.3
            (144, (964, 722)),  // 1.2
            (120, (803, 601)),
            (180, (1205, 902)),
        ] {
            assert_eq!(buffer_size((803, 601), scale), Some(expected));
        }
        assert_eq!(buffer_size((800, 600), 153), Some((1020, 765)));
        assert_eq!(buffer_size((803, 601), 240), Some((1606, 1202)));
        assert_eq!(buffer_size((u32::MAX, 1), 240), None);
        assert_eq!(buffer_size((800, 600), 0), None);
    }
}
