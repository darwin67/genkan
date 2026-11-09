//! Genkan's design system: the bundled typeface, the design tokens shared by
//! the iced login greeter and the session lock's CPU renderer, and the iced
//! conversions, styles, glyphs and text policy the login components use.
//!
//! Values mirror `docs/design-system/tokens.json`; see RFD 5.

pub(crate) mod font;
pub(crate) mod glyph;
pub(crate) mod iced;
pub(crate) mod material;
pub(crate) mod picture;
pub(crate) mod style;
pub(crate) mod text;
pub(crate) mod tokens;
