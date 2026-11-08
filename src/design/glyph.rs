//! Stroked line icons, rasterized with tiny-skia for the iced login and the
//! session lock's CPU renderer.
//!
//! Each glyph is described in its SVG view box from the component previews and
//! scaled to the bounds it is given. These are original drawings, not Apple
//! symbols.

use std::collections::HashMap;
use std::f32::consts::PI;
use std::sync::{Mutex, OnceLock};

use iced::widget::image::{FilterMethod, Handle, Image};
use iced::{Color, Element, Length, Size};

use super::tokens::component;

/// Glyphs are rasterized at this multiple of their logical size and scaled
/// down by the image pipeline, which keeps strokes crisp at any output scale.
const SUPERSAMPLE: f32 = 4.0;

/// Segments used to approximate the power symbol's arc.
const ARC_SEGMENTS: u16 = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Kind {
    /// A display on a stand: the session item.
    Monitor,
    /// The power symbol.
    Power,
    /// A menu check mark.
    Check,
    /// A rightward arrow: submit.
    ArrowRight,
    /// A leftward arrow: back to the account list.
    ArrowLeft,
    CapsLock,
}

impl Kind {
    /// The glyph's default size in logical points.
    pub(crate) fn size(self) -> Size {
        match self {
            Self::Monitor => Size::new(
                component::GLYPH_MONITOR_WIDTH,
                component::GLYPH_MONITOR_HEIGHT,
            ),
            Self::Power => Size::new(component::GLYPH_POWER, component::GLYPH_POWER),
            Self::Check => Size::new(component::GLYPH_CHECK_WIDTH, component::GLYPH_CHECK_HEIGHT),
            Self::ArrowRight | Self::ArrowLeft | Self::CapsLock => {
                Size::new(component::GLYPH_ARROW, component::GLYPH_ARROW)
            }
        }
    }

    /// The SVG view box the strokes below are expressed in.
    fn view_box(self) -> Size {
        match self {
            Self::Monitor => Size::new(15.0, 12.0),
            Self::Power => Size::new(20.0, 20.0),
            Self::Check => Size::new(11.0, 10.0),
            Self::ArrowRight | Self::ArrowLeft | Self::CapsLock => Size::new(14.0, 14.0),
        }
    }

    /// Stroke width in view-box units.
    fn stroke_width(self) -> f32 {
        match self {
            Self::Monitor => 1.3,
            Self::Power => 2.0,
            Self::Check => 1.7,
            Self::ArrowRight | Self::ArrowLeft | Self::CapsLock => 2.2,
        }
    }

    /// The glyph's strokes in view-box units.
    fn path(self) -> Option<tiny_skia::Path> {
        let mut builder = tiny_skia::PathBuilder::new();
        match self {
            Self::Monitor => {
                rounded_rectangle(&mut builder, 1.0, 1.0, 13.0, 7.6, 1.5);
                builder.move_to(5.0, 11.0);
                builder.line_to(10.0, 11.0);
                builder.move_to(7.5, 8.6);
                builder.line_to(7.5, 11.0);
            }
            Self::Power => {
                builder.move_to(10.0, 3.0);
                builder.line_to(10.0, 9.4);
                // The SVG arc from (13.8, 5.7) to (6.2, 5.7) with radius 5.6,
                // taking the long way around below its center.
                let (center_y, opening) = power_arc();
                let radius = 5.6;
                for step in 0..=ARC_SEGMENTS {
                    let angle =
                        opening + (PI - 2.0 * opening) * f32::from(step) / f32::from(ARC_SEGMENTS);
                    let (x, y) = (10.0 + radius * angle.cos(), center_y + radius * angle.sin());
                    if step == 0 {
                        builder.move_to(x, y);
                    } else {
                        builder.line_to(x, y);
                    }
                }
            }
            Self::Check => {
                builder.move_to(1.2, 5.2);
                builder.line_to(4.0, 8.0);
                builder.line_to(9.8, 1.4);
            }
            Self::ArrowRight => {
                builder.move_to(2.6, 7.0);
                builder.line_to(11.0, 7.0);
                builder.move_to(7.6, 3.4);
                builder.line_to(11.0, 7.0);
                builder.line_to(7.6, 10.6);
            }
            Self::ArrowLeft => {
                builder.move_to(11.4, 7.0);
                builder.line_to(3.0, 7.0);
                builder.move_to(6.4, 3.4);
                builder.line_to(3.0, 7.0);
                builder.line_to(6.4, 10.6);
            }
            Self::CapsLock => {
                builder.move_to(2.0, 6.0);
                builder.line_to(7.0, 1.0);
                builder.line_to(12.0, 6.0);
                builder.line_to(9.0, 6.0);
                builder.line_to(9.0, 9.0);
                builder.line_to(5.0, 9.0);
                builder.line_to(5.0, 6.0);
                builder.close();
                builder.move_to(5.0, 12.0);
                builder.line_to(9.0, 12.0);
            }
        }
        builder.finish()
    }
}

/// The power arc's center height and its opening angle below the horizontal.
fn power_arc() -> (f32, f32) {
    let radius: f32 = 5.6;
    let half_chord: f32 = 3.8;
    let center_y = 5.7 + (radius * radius - half_chord * half_chord).sqrt();
    (center_y, (5.7 - center_y).atan2(half_chord))
}

fn rounded_rectangle(
    builder: &mut tiny_skia::PathBuilder,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    radius: f32,
) {
    let (right, bottom) = (x + width, y + height);
    builder.move_to(x + radius, y);
    builder.line_to(right - radius, y);
    builder.quad_to(right, y, right, y + radius);
    builder.line_to(right, bottom - radius);
    builder.quad_to(right, bottom, right - radius, bottom);
    builder.line_to(x + radius, bottom);
    builder.quad_to(x, bottom, x, bottom - radius);
    builder.line_to(x, y + radius);
    builder.quad_to(x, y, x + radius, y);
    builder.close();
}

/// Rasterizes a glyph to straight-alpha RGBA at `SUPERSAMPLE` times `size`.
///
/// Glyphs are images rather than canvas geometry because iced 0.14's
/// tiny-skia fallback renderer draws no canvas geometry in this greeter, while
/// both renderers draw images.
fn rasterize(kind: Kind, color: Color, size: Size) -> Option<(u32, u32, Vec<u8>)> {
    let width = (size.width * SUPERSAMPLE).ceil().max(1.0) as u32;
    let height = (size.height * SUPERSAMPLE).ceil().max(1.0) as u32;
    Some((width, height, rgba(kind, color, width, height)?))
}

/// Rasterizes a glyph to straight-alpha RGBA exactly `width`×`height` pixels,
/// fitted and centered in that box. The lock's CPU renderer blends these
/// pixels into its overlay at its own pixel density.
pub(crate) fn rgba(kind: Kind, color: Color, width: u32, height: u32) -> Option<Vec<u8>> {
    let mut pixmap = tiny_skia::Pixmap::new(width, height)?;
    let view_box = kind.view_box();
    let scale = (width as f32 / view_box.width).min(height as f32 / view_box.height);
    let offset_x = (width as f32 - view_box.width * scale) / 2.0;
    let offset_y = (height as f32 - view_box.height * scale) / 2.0;
    let mut paint = tiny_skia::Paint::default();
    paint.set_color(tiny_skia::Color::from_rgba(
        color.r, color.g, color.b, color.a,
    )?);
    paint.anti_alias = true;
    let stroke = tiny_skia::Stroke {
        width: kind.stroke_width(),
        line_cap: tiny_skia::LineCap::Round,
        line_join: tiny_skia::LineJoin::Round,
        ..tiny_skia::Stroke::default()
    };
    pixmap.stroke_path(
        &kind.path()?,
        &paint,
        &stroke,
        tiny_skia::Transform::from_row(scale, 0.0, 0.0, scale, offset_x, offset_y),
        None,
    );
    let pixels = pixmap
        .pixels()
        .iter()
        .flat_map(|pixel| {
            let straight = pixel.demultiply();
            [
                straight.red(),
                straight.green(),
                straight.blue(),
                straight.alpha(),
            ]
        })
        .collect();
    Some(pixels)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Key {
    kind: Kind,
    color: [u8; 4],
    width: u32,
    height: u32,
}

/// Returns a cached image handle so the renderer keeps one texture per glyph.
fn handle(kind: Kind, color: Color, size: Size) -> Handle {
    static CACHE: OnceLock<Mutex<HashMap<Key, Handle>>> = OnceLock::new();
    let key = Key {
        kind,
        color: color.into_rgba8(),
        width: size.width.to_bits(),
        height: size.height.to_bits(),
    };
    let mut cache = CACHE
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    cache
        .entry(key)
        .or_insert_with(|| {
            let (width, height, pixels) =
                rasterize(kind, color, size).unwrap_or((1, 1, vec![0; 4]));
            Handle::from_rgba(width, height, pixels)
        })
        .clone()
}

/// A glyph at its default size.
pub(crate) fn glyph<'a, Message: 'a>(kind: Kind, color: Color) -> Element<'a, Message> {
    sized(kind, color, kind.size())
}

/// A glyph scaled to `size`.
pub(crate) fn sized<'a, Message: 'a>(kind: Kind, color: Color, size: Size) -> Element<'a, Message> {
    Image::new(handle(kind, color, size))
        .width(Length::Fixed(size.width))
        .height(Length::Fixed(size.height))
        .filter_method(FilterMethod::Linear)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyph_sizes_come_from_component_tokens() {
        assert_eq!(Kind::Power.size(), Size::new(14.0, 14.0));
        assert_eq!(Kind::Monitor.size(), Size::new(15.0, 12.0));
        assert_eq!(Kind::ArrowLeft.size(), Kind::ArrowRight.size());
    }

    #[test]
    fn power_arc_meets_the_svg_end_points() {
        let radius: f32 = 5.6;
        let (center_y, opening) = power_arc();
        let start = (
            10.0 + radius * opening.cos(),
            center_y + radius * opening.sin(),
        );
        let end = (
            10.0 + radius * (PI - opening).cos(),
            center_y + radius * (PI - opening).sin(),
        );
        assert!((start.0 - 13.8).abs() < 1e-3 && (start.1 - 5.7).abs() < 1e-3);
        assert!((end.0 - 6.2).abs() < 1e-3 && (end.1 - 5.7).abs() < 1e-3);
        // The arc sweeps through the bottom (angle PI / 2) of the circle.
        assert!(opening < PI / 2.0 && PI - opening > PI / 2.0);
    }

    const KINDS: [Kind; 5] = [
        Kind::Monitor,
        Kind::Power,
        Kind::Check,
        Kind::ArrowRight,
        Kind::ArrowLeft,
    ];

    #[test]
    fn every_glyph_rasterizes_visible_strokes_in_its_color() {
        for kind in KINDS {
            let (width, height, pixels) =
                rasterize(kind, Color::WHITE, kind.size()).expect("glyph rasterizes");
            assert_eq!(pixels.len(), (width * height * 4) as usize);
            let opaque = pixels
                .chunks_exact(4)
                .filter(|pixel| pixel[3] == u8::MAX)
                .count();
            assert!(opaque > 0, "{kind:?} drew no opaque pixels");
            assert!(
                pixels
                    .chunks_exact(4)
                    .filter(|pixel| pixel[3] > 0)
                    .all(|pixel| pixel[..3] == [u8::MAX; 3]),
                "{kind:?} is not pure white after demultiplying"
            );
            assert_eq!(&pixels[..4], &[0, 0, 0, 0], "{kind:?} fills its corner");
        }
    }

    #[test]
    fn pixel_sized_glyphs_fill_the_requested_box() {
        let pixels = rgba(Kind::ArrowRight, Color::WHITE, 27, 27).expect("rasterizes");
        assert_eq!(pixels.len(), 27 * 27 * 4);
        assert!(pixels.chunks_exact(4).any(|pixel| pixel[3] == u8::MAX));
        assert!(rgba(Kind::ArrowRight, Color::WHITE, 0, 27).is_none());
    }

    #[test]
    fn glyphs_are_supersampled_relative_to_their_logical_size() {
        let (width, height, _) =
            rasterize(Kind::Monitor, Color::WHITE, Kind::Monitor.size()).expect("rasterizes");
        assert_eq!((width, height), (60, 48));
    }

    #[test]
    fn handles_are_cached_per_kind_color_and_size() {
        let first = handle(Kind::Power, Color::WHITE, Kind::Power.size());
        let again = handle(Kind::Power, Color::WHITE, Kind::Power.size());
        let other = handle(Kind::Check, Color::WHITE, Kind::Check.size());
        assert_eq!(first.id(), again.id());
        assert_ne!(first.id(), other.id());
    }
}
