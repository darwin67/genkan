//! The bundled Inter typeface.
//!
//! Both renderers load these bytes directly so login and lock share metrics
//! regardless of which fonts the host's fontconfig provides.

use std::sync::Arc;

use cosmic_text::{fontdb, Attrs, Family, FontSystem};

/// `assets/fonts/InterVariable.ttf`, Inter 4.1. Provenance is recorded in
/// `assets/fonts/README.md`.
pub(crate) const INTER_VARIABLE: &[u8] = include_bytes!("../../assets/fonts/InterVariable.ttf");

/// The family name stored in the bundled font's name table.
pub(crate) const FAMILY: &str = "Inter Variable";

/// The default font for iced text.
pub(crate) fn iced_font() -> iced::Font {
    iced::Font::with_name(FAMILY)
}

/// A cosmic-text font system that includes the bundled font alongside the
/// host's fonts, which remain available for glyph fallback.
pub(crate) fn font_system() -> FontSystem {
    FontSystem::new_with_fonts([fontdb::Source::Binary(Arc::new(INTER_VARIABLE))])
}

/// Text attributes that select the bundled family.
pub(crate) fn attrs() -> Attrs<'static> {
    Attrs::new().family(Family::Name(FAMILY))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmic_text::{Align, Buffer, Metrics, Shaping, Weight, Wrap};

    fn shape(fonts: &mut FontSystem, text: &str, weight: Weight) -> (f32, Vec<fontdb::ID>) {
        let mut buffer = Buffer::new(fonts, Metrics::new(100.0, 120.0));
        buffer.set_size(fonts, None, None);
        buffer.set_wrap(fonts, Wrap::None);
        buffer.set_text(
            fonts,
            text,
            &attrs().weight(weight),
            Shaping::Advanced,
            Some(Align::Left),
        );
        buffer.shape_until_scroll(fonts, true);
        let width = buffer
            .layout_runs()
            .map(|run| run.line_w)
            .fold(0.0, f32::max);
        let faces = buffer
            .layout_runs()
            .flat_map(|run| run.glyphs.iter().map(|glyph| glyph.font_id))
            .collect();
        (width, faces)
    }

    #[test]
    fn text_resolves_to_the_bundled_family() {
        let mut fonts = font_system();
        let (_, faces) = shape(&mut fonts, "Enter Password 10:38", Weight::NORMAL);
        assert!(!faces.is_empty());
        for face in faces {
            let info = fonts.db().face(face).expect("shaped face exists");
            assert!(
                info.families.iter().any(|(name, _)| name == FAMILY),
                "glyph used {:?} instead of {FAMILY}",
                info.families
            );
        }
    }

    #[test]
    fn the_weight_axis_changes_glyph_advances() {
        let mut fonts = font_system();
        let (regular, _) = shape(&mut fonts, "10:38", Weight::NORMAL);
        let (heavy, _) = shape(&mut fonts, "10:38", Weight::EXTRA_BOLD);
        assert!(
            heavy > regular * 1.02,
            "weight 800 ({heavy}) should be wider than weight 400 ({regular})"
        );
    }
}
