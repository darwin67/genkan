//! Account pictures as avatar fills (RFD 7).
//!
//! A decoded [`Picture`] is a square of premultiplied RGBA. The login draws it
//! as an image clipped to the avatar circle, so this masks it once, with an
//! antialiased edge, and converts it to the straight alpha iced expects.

use genkan::avatar::{Picture, SIZE};
use iced::widget::image::Handle;

/// The picture as a tiny-skia pixmap, for renderers that fill the avatar with
/// it directly.
pub(crate) fn pixmap(picture: &Picture) -> Option<tiny_skia::Pixmap> {
    let size = tiny_skia::IntSize::from_wh(SIZE, SIZE)?;
    tiny_skia::Pixmap::from_vec(picture.pixels().to_vec(), size)
}

/// The picture inside an antialiased circle, as an iced image.
pub(crate) fn circle(picture: &Picture) -> Option<Handle> {
    let source = pixmap(picture)?;
    let mut masked = tiny_skia::Pixmap::new(SIZE, SIZE)?;
    let radius = SIZE as f32 / 2.0;
    let circle = tiny_skia::PathBuilder::from_circle(radius, radius, radius)?;
    let paint = tiny_skia::Paint {
        shader: tiny_skia::Pattern::new(
            source.as_ref(),
            tiny_skia::SpreadMode::Pad,
            tiny_skia::FilterQuality::Nearest,
            1.0,
            tiny_skia::Transform::identity(),
        ),
        anti_alias: true,
        ..tiny_skia::Paint::default()
    };
    masked.fill_path(
        &circle,
        &paint,
        tiny_skia::FillRule::Winding,
        tiny_skia::Transform::identity(),
        None,
    );
    let pixels: Vec<u8> = masked
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
    Some(Handle::from_rgba(SIZE, SIZE, pixels))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(rgba: [u8; 4]) -> Picture {
        Picture::from_premultiplied(rgba.repeat((SIZE * SIZE) as usize)).unwrap()
    }

    fn pixels(handle: &Handle) -> Vec<u8> {
        match handle {
            Handle::Rgba { pixels, .. } => pixels.to_vec(),
            _ => panic!("an RGBA handle"),
        }
    }

    #[test]
    fn the_picture_is_masked_to_a_circle() {
        let pixels = pixels(&circle(&solid([30, 120, 200, 255])).unwrap());
        let at = |x: u32, y: u32| {
            let index = ((y * SIZE + x) * 4) as usize;
            <[u8; 4]>::try_from(&pixels[index..index + 4]).unwrap()
        };
        assert_eq!(at(SIZE / 2, SIZE / 2), [30, 120, 200, 255]);
        assert_eq!(at(0, 0)[3], 0, "the corners are transparent");
        assert_eq!(at(SIZE - 1, SIZE - 1)[3], 0);
        let edge = at(SIZE / 2, 0)[3];
        assert!(edge > 0, "the circle reaches the edge");
    }

    #[test]
    fn premultiplied_pixels_become_straight_alpha() {
        let pixels = pixels(&circle(&solid([64, 32, 0, 128])).unwrap());
        let center = (((SIZE / 2) * SIZE + SIZE / 2) * 4) as usize;
        let [red, green, blue, alpha] = <[u8; 4]>::try_from(&pixels[center..center + 4]).unwrap();
        assert_eq!(alpha, 128);
        assert!(red.abs_diff(128) <= 1 && green.abs_diff(64) <= 1 && blue == 0);
    }
}
