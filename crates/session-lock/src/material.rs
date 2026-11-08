//! Bounded, wallpaper-only backdrop filtering shared by the CPU and iced UIs.
use crate::RgbaFrame;
use std::sync::Mutex;

// One wallpaper per process. Retaining the source also makes allocation identity
// a safe cache key; password edits and clock ticks never filter it again.
static CACHE: Mutex<Option<(RgbaFrame, RgbaFrame)>> = Mutex::new(None);

/// A dark material backing, not a wallpaper dimmer. Even white wallpaper
/// must leave enough contrast for the translucent field placeholder.
pub const MAX_CHANNEL: u8 = 96;

pub fn blurred(source: &RgbaFrame) -> RgbaFrame {
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((previous, blur)) = cache.as_ref() {
        if previous.dimensions() == source.dimensions()
            && previous.pixels().as_ptr() == source.pixels().as_ptr()
        {
            return blur.clone();
        }
    }
    let (sw, sh) = source.dimensions();
    let factor = (192.0 / sw.max(sh) as f32).min(1.0);
    let width = (sw as f32 * factor).round().max(1.0) as u32;
    let height = (sh as f32 * factor).round().max(1.0) as u32;
    let mut pixels = vec![0; width as usize * height as usize * 4];
    // Average a stratified grid in each cell, bounded to 4×4 samples. This
    // keeps animated 4K wallpapers cheap without point-sampling their detail.
    for y in 0..height {
        for x in 0..width {
            let mut sum = [0u64; 3];
            let (left, right) = (
                x * sw / width,
                ((x + 1) * sw / width).max(x * sw / width + 1),
            );
            let (top, bottom) = (
                y * sh / height,
                ((y + 1) * sh / height).max(y * sh / height + 1),
            );
            let nx = (right - left).min(4);
            let ny = (bottom - top).min(4);
            for iy in 0..ny {
                let sy = top + (2 * iy + 1) * (bottom - top) / (2 * ny);
                for ix in 0..nx {
                    let sx = left + (2 * ix + 1) * (right - left) / (2 * nx);
                    let i = (sy as usize * sw as usize + sx as usize) * 4;
                    for (c, total) in sum.iter_mut().enumerate() {
                        *total += u64::from(source.pixels()[i + c]);
                    }
                }
            }
            let count = u64::from(nx * ny);
            let i = (y as usize * width as usize + x as usize) * 4;
            for c in 0..3 {
                pixels[i + c] = (sum[c] / count) as u8;
            }
            pixels[i + 3] = 255;
        }
    }
    // Three separable five-tap box passes approximate a Gaussian. The work is
    // bounded by 192² regardless of the output's resolution or scale.
    for _ in 0..3 {
        for horizontal in [true, false] {
            let input = pixels.clone();
            for y in 0..height {
                for x in 0..width {
                    let i = (y as usize * width as usize + x as usize) * 4;
                    for c in 0..3 {
                        let mut sum = 0u32;
                        for d in -2..=2 {
                            let sx = if horizontal {
                                (x as i32 + d).clamp(0, width as i32 - 1) as u32
                            } else {
                                x
                            };
                            let sy = if horizontal {
                                y
                            } else {
                                (y as i32 + d).clamp(0, height as i32 - 1) as u32
                            };
                            sum += u32::from(
                                input[(sy as usize * width as usize + sx as usize) * 4 + c],
                            );
                        }
                        pixels[i + c] = (sum / 5) as u8;
                    }
                }
            }
        }
    }
    for pixel in pixels.chunks_exact_mut(4) {
        for channel in &mut pixel[..3] {
            *channel = (u16::from(*channel) * u16::from(MAX_CHANNEL) / 255) as u8;
        }
    }
    let blur = RgbaFrame::new(width, height, pixels.into()).unwrap();
    *cache = Some((source.clone(), blur.clone()));
    blur
}

/// A rounded material region in overlay pixel coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backdrop {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub radius: u32,
}

impl Backdrop {
    pub(crate) fn contains(&self, x: f32, y: f32) -> bool {
        let (x, y) = (x - self.x as f32, y - self.y as f32);
        let (w, h) = (self.width as f32, self.height as f32);
        if x < 0.0 || y < 0.0 || x >= w || y >= h {
            return false;
        }
        let r = (self.radius as f32).min(w / 2.0).min(h / 2.0);
        let dx = x - x.clamp(r, w - r);
        let dy = y - y.clamp(r, h - r);
        dx * dx + dy * dy <= r * r
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{render_preview, AnchoredOverlay, OverlayLayout, PresentationFrame};

    #[test]
    fn materials_filter_detail_preserve_location_and_bound_brightness() {
        let mut pixels = Vec::new();
        for _y in 0..96 {
            for x in 0..384 {
                let level = if x % 2 == 0 { 255 } else { 0 };
                pixels.extend_from_slice(&if x < 192 {
                    [level, 0, 0, 255]
                } else {
                    [0, 0, level, 255]
                });
            }
        }
        let source = RgbaFrame::new(384, 96, pixels.into()).unwrap();
        let blur = blurred(&source);
        assert_eq!(blur.dimensions(), (192, 48));
        let pixel = |x: usize| &blur.pixels()[x * 4..x * 4 + 4];
        assert_eq!(pixel(20), [47, 0, 0, 255]);
        assert_eq!(pixel(170), [0, 0, 47, 255]);
        assert!(pixel(95)[0] > 0 && pixel(95)[2] > 0, "the edge is blurred");
        let white = RgbaFrame::new(1, 1, vec![255; 4].into()).unwrap();
        assert!(blurred(&white)
            .pixels()
            .chunks_exact(4)
            .all(|p| p == [96, 96, 96, 255]));
    }

    #[test]
    fn lock_backdrops_are_clipped_and_sample_the_final_output_position() {
        let mut pixels = Vec::new();
        for _y in 0..32 {
            for x in 0..64 {
                pixels.extend_from_slice(if x < 32 {
                    &[255, 0, 0, 255]
                } else {
                    &[0, 0, 255, 255]
                });
            }
        }
        let background = RgbaFrame::new(64, 32, pixels.into()).unwrap();
        let overlay = RgbaFrame::new(32, 16, vec![0; 32 * 16 * 4].into()).unwrap();
        let frame = PresentationFrame::new(
            Some(background),
            OverlayLayout::new(1).unwrap(),
            None,
            AnchoredOverlay::new(overlay, 0).map(|overlay| {
                overlay.with_backdrop(Backdrop {
                    x: 0,
                    y: 0,
                    width: 32,
                    height: 16,
                    radius: 4,
                })
            }),
        );
        let rendered = render_preview(&frame, 128, 64, 2).unwrap();
        let pixel =
            |x: usize, y: usize| &rendered.pixels()[(y * 128 + x) * 4..(y * 128 + x) * 4 + 4];
        assert_eq!(pixel(40, 48), [96, 0, 0, 255]);
        assert_eq!(pixel(88, 48), [0, 0, 96, 255]);
        assert_eq!(
            pixel(32, 32),
            [255, 0, 0, 255],
            "round corner remains wallpaper"
        );
        assert_eq!(pixel(10, 48), [255, 0, 0, 255], "outside remains undimmed");
    }
}
