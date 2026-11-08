//! Design tokens mirrored from `docs/design-system/tokens.json`.
//!
//! The JSON file is the reference; a unit test fails when the two disagree.
//! Values are logical points measured from the macOS Tahoe login window and
//! lock screen. Both the login greeter and the lock renderer read them;
//! tokens for components neither draws yet are allowed to be unused.
#![cfg_attr(not(test), allow(dead_code))]

/// An sRGB color with straight alpha.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Rgba {
    pub(crate) red: u8,
    pub(crate) green: u8,
    pub(crate) blue: u8,
    pub(crate) alpha: f32,
}

const fn rgb(red: u8, green: u8, blue: u8) -> Rgba {
    rgba(red, green, blue, 1.0)
}

const fn rgba(red: u8, green: u8, blue: u8, alpha: f32) -> Rgba {
    Rgba {
        red,
        green,
        blue,
        alpha,
    }
}

pub(crate) mod color {
    use super::{rgb, rgba, Rgba};

    pub(crate) const TEXT_PRIMARY: Rgba = rgb(255, 255, 255);
    pub(crate) const TEXT_SHADOW: Rgba = rgba(0, 0, 0, 0.08);
    pub(crate) const TEXT_DATE: Rgba = rgba(255, 255, 255, 0.94);
    pub(crate) const TEXT_SECONDARY: Rgba = rgba(255, 255, 255, 0.78);
    pub(crate) const TEXT_PLACEHOLDER: Rgba = rgba(255, 255, 255, 0.72);
    pub(crate) const TEXT_MENU_HEADING: Rgba = rgba(255, 255, 255, 0.55);
    pub(crate) const CLOCK_GLASS: Rgba = rgba(236, 246, 255, 0.86);
    pub(crate) const FIELD_GLASS: Rgba = rgba(78, 84, 92, 0.5);
    pub(crate) const FIELD_BORDER: Rgba = rgba(255, 255, 255, 0.22);
    pub(crate) const AVATAR_TOP: Rgba = rgb(0xa9, 0xaf, 0xbb);
    pub(crate) const AVATAR_BOTTOM: Rgba = rgb(0x85, 0x8a, 0x96);
    pub(crate) const AVATAR_RING: Rgba = rgba(255, 255, 255, 0.55);
    pub(crate) const ITEM_ACTIVE: Rgba = rgba(255, 255, 255, 0.26);
    pub(crate) const MENU_MATERIAL: Rgba = rgba(36, 38, 44, 0.72);
    pub(crate) const MENU_BORDER: Rgba = rgba(255, 255, 255, 0.18);
    pub(crate) const ALERT_MATERIAL: Rgba = rgba(40, 42, 48, 0.8);
    pub(crate) const ACCENT: Rgba = rgb(0x2f, 0x6f, 0xe0);
    pub(crate) const PILL_SECONDARY: Rgba = rgba(255, 255, 255, 0.12);
    pub(crate) const FOCUS_RING: Rgba = rgba(80, 150, 255, 0.85);
    pub(crate) const SCRIM_MODAL: Rgba = rgba(0, 0, 0, 0.3);

    pub(super) const ALL: &[(&str, Rgba)] = &[
        ("text-primary", TEXT_PRIMARY),
        ("text-shadow", TEXT_SHADOW),
        ("text-date", TEXT_DATE),
        ("text-secondary", TEXT_SECONDARY),
        ("text-placeholder", TEXT_PLACEHOLDER),
        ("text-menu-heading", TEXT_MENU_HEADING),
        ("clock-glass", CLOCK_GLASS),
        ("field-glass", FIELD_GLASS),
        ("field-border", FIELD_BORDER),
        ("avatar-top", AVATAR_TOP),
        ("avatar-bottom", AVATAR_BOTTOM),
        ("avatar-ring", AVATAR_RING),
        ("item-active", ITEM_ACTIVE),
        ("menu-material", MENU_MATERIAL),
        ("menu-border", MENU_BORDER),
        ("alert-material", ALERT_MATERIAL),
        ("accent", ACCENT),
        ("pill-secondary", PILL_SECONDARY),
        ("focus-ring", FOCUS_RING),
        ("scrim-modal", SCRIM_MODAL),
    ];
}

/// A text style. Sizes are logical points; letter spacing is in em.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TextStyle {
    pub(crate) size: f32,
    pub(crate) line_height: f32,
    pub(crate) weight: u16,
    pub(crate) letter_spacing: f32,
}

const fn text(size: f32, line_height: f32, weight: u16) -> TextStyle {
    TextStyle {
        size,
        line_height,
        weight,
        letter_spacing: 0.0,
    }
}

pub(crate) mod text_style {
    use super::{text, TextStyle};

    pub(crate) const CLOCK: TextStyle = TextStyle {
        letter_spacing: -0.02,
        ..text(248.0, 248.0, 800)
    };
    pub(crate) const DATE: TextStyle = text(38.0, 46.0, 600);
    pub(crate) const INITIALS: TextStyle = text(29.0, 36.0, 500);
    pub(crate) const NAME: TextStyle = text(15.0, 18.0, 700);
    pub(crate) const FIELD: TextStyle = text(13.0, 22.0, 500);
    pub(crate) const BAR: TextStyle = text(13.0, 22.0, 600);
    pub(crate) const MENU: TextStyle = text(13.0, 22.0, 500);
    pub(crate) const ALERT_TITLE: TextStyle = text(13.0, 17.0, 700);
    pub(crate) const MESSAGE: TextStyle = text(12.0, 16.0, 600);
    pub(crate) const MENU_HEADING: TextStyle = text(11.0, 15.0, 600);
    pub(crate) const ALERT_BODY: TextStyle = text(11.0, 15.0, 500);

    pub(super) const ALL: &[(&str, TextStyle)] = &[
        ("type-clock", CLOCK),
        ("type-date", DATE),
        ("type-initials", INITIALS),
        ("type-name", NAME),
        ("type-field", FIELD),
        ("type-bar", BAR),
        ("type-menu", MENU),
        ("type-alert-title", ALERT_TITLE),
        ("type-message", MESSAGE),
        ("type-menu-heading", MENU_HEADING),
        ("type-alert-body", ALERT_BODY),
    ];
}

pub(crate) mod spacing {
    pub(crate) const MENU_PADDING: f32 = 5.0;
    pub(crate) const BAR_INSET_TOP: f32 = 8.0;
    pub(crate) const IDENTITY_GAP: f32 = 12.0;
    pub(crate) const BAR_GAP: f32 = 14.0;
    pub(crate) const ALERT_PADDING: f32 = 16.0;
    pub(crate) const ACCOUNT_GAP: f32 = 36.0;
    pub(crate) const BAR_INSET_RIGHT: f32 = 48.0;
    pub(crate) const IDENTITY_BOTTOM: f32 = 123.0;
    pub(crate) const CLOCK_TOP: f32 = 183.0;

    pub(super) const ALL: &[(&str, f32)] = &[
        ("menu-padding", MENU_PADDING),
        ("bar-inset-top", BAR_INSET_TOP),
        ("identity-gap", IDENTITY_GAP),
        ("bar-gap", BAR_GAP),
        ("alert-padding", ALERT_PADDING),
        ("account-gap", ACCOUNT_GAP),
        ("bar-inset-right", BAR_INSET_RIGHT),
        ("identity-bottom", IDENTITY_BOTTOM),
        ("clock-top", CLOCK_TOP),
    ];
}

pub(crate) mod radius {
    pub(crate) const ITEM: f32 = 6.0;
    pub(crate) const MENU_ITEM: f32 = 6.0;
    pub(crate) const FIELD: f32 = 11.0;
    pub(crate) const MENU: f32 = 12.0;
    pub(crate) const PILL: f32 = 14.0;
    pub(crate) const ALERT: f32 = 22.0;
    /// A fraction of the shorter side: 0.5 makes a circle.
    pub(crate) const ROUND: f32 = 0.5;

    pub(super) const PX: &[(&str, f32)] = &[
        ("radius-item", ITEM),
        ("radius-menu-item", MENU_ITEM),
        ("radius-field", FIELD),
        ("radius-menu", MENU),
        ("radius-pill", PILL),
        ("radius-alert", ALERT),
    ];
}

pub(crate) mod size {
    pub(crate) const SUBMIT: f32 = 16.0;
    pub(crate) const FIELD_HEIGHT: f32 = 22.0;
    pub(crate) const BAR_HEIGHT: f32 = 22.0;
    pub(crate) const ITEM_HEIGHT: f32 = 22.0;
    pub(crate) const PILL_HEIGHT: f32 = 28.0;
    pub(crate) const AVATAR: f32 = 75.0;
    pub(crate) const ACCOUNT_WIDTH: f32 = 110.0;
    pub(crate) const FIELD_WIDTH: f32 = 142.0;
    pub(crate) const MENU_WIDTH: f32 = 210.0;
    pub(crate) const ALERT_WIDTH: f32 = 260.0;
    pub(crate) const MESSAGE_WIDTH: f32 = 300.0;

    pub(super) const ALL: &[(&str, f32)] = &[
        ("submit-size", SUBMIT),
        ("field-height", FIELD_HEIGHT),
        ("bar-height", BAR_HEIGHT),
        ("item-height", ITEM_HEIGHT),
        ("pill-height", PILL_HEIGHT),
        ("avatar-size", AVATAR),
        ("account-width", ACCOUNT_WIDTH),
        ("field-width", FIELD_WIDTH),
        ("menu-width", MENU_WIDTH),
        ("alert-width", ALERT_WIDTH),
        ("message-width", MESSAGE_WIDTH),
    ];
}

/// Measurements internal to one component, taken from
/// `docs/design-system/components/bundle.css` and the component previews.
///
/// They are not design tokens: each belongs to a single component's anatomy
/// and is not shared across components, so `tokens.json` does not carry them.
/// They live here so that view code never introduces literal sizes or colors.
pub(crate) mod component {
    use super::{rgba, Rgba};

    /// Width of a hairline border (`.5px` in the CSS).
    pub(crate) const HAIRLINE: f32 = 0.5;
    /// Width of the keyboard focus ring drawn with `color::FOCUS_RING`.
    pub(crate) const FOCUS_RING_WIDTH: f32 = 2.5;

    /// Horizontal inset of a status-bar item with a label.
    pub(crate) const ITEM_PADDING_X: f32 = 6.0;
    /// Horizontal inset of a glyph-only status-bar item.
    pub(crate) const ICON_ITEM_PADDING_X: f32 = 5.0;
    /// Gap between a status-bar item's glyph and its label.
    pub(crate) const ITEM_CONTENT_GAP: f32 = 5.0;
    /// Gap between the status bar and a menu dropped from it.
    pub(crate) const MENU_OFFSET: f32 = 4.0;

    /// Leading inset of a menu item, which holds the check mark.
    pub(crate) const MENU_ITEM_LEADING: f32 = 24.0;
    /// Trailing inset of a menu item.
    pub(crate) const MENU_ITEM_TRAILING: f32 = 10.0;
    /// Inset of the check mark inside the leading area.
    pub(crate) const MENU_CHECK_INSET: f32 = 8.0;
    /// Vertical inset of a menu heading.
    pub(crate) const MENU_HEADING_PADDING_Y: f32 = 3.0;

    /// Height of the text line, and so of the caret, inside the password
    /// pill. It is shorter than the pill so the caret clears its border.
    pub(crate) const FIELD_CARET_HEIGHT: f32 = 16.0;
    /// Leading inset of text in the password pill.
    pub(crate) const FIELD_PADDING_LEADING: f32 = 10.0;
    /// Trailing inset of text in the password pill, clearing the submit arrow.
    pub(crate) const FIELD_PADDING_TRAILING: f32 = 24.0;
    /// Inset of the submit circle from the pill's trailing edge.
    pub(crate) const SUBMIT_INSET: f32 = 3.0;
    /// Outline width of the submit and back circles.
    pub(crate) const CIRCLE_OUTLINE_WIDTH: f32 = 1.2;
    /// Outline color of the submit and back circles.
    pub(crate) const CIRCLE_OUTLINE: Rgba = rgba(255, 255, 255, 0.85);

    /// Offsets, in points, of the nine taps of the soft shadow under
    /// wallpaper text: a three-point box one point below the glyphs. Each tap
    /// draws the text in `color::TEXT_SHADOW`.
    pub(crate) const TEXT_SHADOW_TAPS: [(f32, f32); 9] = [
        (-1.0, 0.0),
        (0.0, 0.0),
        (1.0, 0.0),
        (-1.0, 1.0),
        (0.0, 1.0),
        (1.0, 1.0),
        (-1.0, 2.0),
        (0.0, 2.0),
        (1.0, 2.0),
    ];

    /// Width of the avatar's ring.
    pub(crate) const AVATAR_RING_WIDTH: f32 = 1.5;

    /// Top inset of an alert; its sides and bottom use `ALERT_PADDING`.
    pub(crate) const ALERT_PADDING_TOP: f32 = 20.0;
    /// Gap between an alert's icon, title, body and actions.
    pub(crate) const ALERT_GAP: f32 = 10.0;
    /// Extra space above an alert's buttons.
    pub(crate) const ALERT_ACTIONS_MARGIN: f32 = 6.0;
    /// Gap between an alert's two buttons.
    pub(crate) const ALERT_ACTIONS_GAP: f32 = 8.0;
    /// Diameter of the disc behind an alert's icon.
    pub(crate) const ALERT_ICON: f32 = 48.0;
    /// Size of the glyph inside the alert's icon disc.
    pub(crate) const ALERT_GLYPH: f32 = 24.0;

    /// Horizontal inset of small pill buttons and the preview badge.
    pub(crate) const PILL_PADDING_X: f32 = 14.0;

    /// Glyph sizes, from the SVGs in the component previews.
    pub(crate) const GLYPH_MONITOR_WIDTH: f32 = 15.0;
    pub(crate) const GLYPH_MONITOR_HEIGHT: f32 = 12.0;
    pub(crate) const GLYPH_POWER: f32 = 14.0;
    pub(crate) const GLYPH_CHECK_WIDTH: f32 = 10.0;
    pub(crate) const GLYPH_CHECK_HEIGHT: f32 = 9.0;
    pub(crate) const GLYPH_ARROW: f32 = 9.0;

    /// The smallest the clock may scale to make room for the identity column.
    pub(crate) const CLOCK_MIN_SCALE: f32 = 0.2;
    /// Room the lock leaves on each side of the clock's advance width for
    /// glyph ink that extends past it.
    pub(crate) const CLOCK_OVERHANG: f32 = 8.0;
}

/// Motion. The rejection shake follows macOS: a short damped oscillation.
pub(crate) mod motion {
    use std::time::Duration;

    pub(crate) const SHAKE_DURATION: Duration = Duration::from_millis(400);
    /// Peak horizontal displacement in logical points.
    pub(crate) const SHAKE_AMPLITUDE: f32 = 10.0;
    /// Full oscillations over the shake's duration.
    pub(crate) const SHAKE_CYCLES: f32 = 3.0;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::collections::BTreeSet;

    const REFERENCE: &str = include_str!("../../docs/design-system/tokens.json");

    fn reference() -> Value {
        serde_json::from_str(REFERENCE).expect("tokens.json is valid JSON")
    }

    fn tokens<'a>(json: &'a Value, family: &str) -> &'a [Value] {
        json[family]["tokens"]
            .as_array()
            .unwrap_or_else(|| panic!("{family} has a token list"))
    }

    fn name(token: &Value) -> &str {
        token["name"].as_str().expect("token has a name")
    }

    fn px(value: &str) -> f32 {
        value
            .strip_suffix("px")
            .unwrap_or_else(|| panic!("{value} is in px"))
            .parse()
            .expect("px value is a number")
    }

    fn parse_color(value: &str) -> Rgba {
        if let Some(hex) = value.strip_prefix('#') {
            assert_eq!(hex.len(), 6, "{value} is #rrggbb");
            let channel =
                |at: usize| u8::from_str_radix(&hex[at..at + 2], 16).expect("hex channel");
            return rgb(channel(0), channel(2), channel(4));
        }
        let inner = value
            .strip_prefix("rgba(")
            .and_then(|rest| rest.strip_suffix(')'))
            .unwrap_or_else(|| panic!("{value} is #rrggbb or rgba()"));
        let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
        assert_eq!(parts.len(), 4, "{value} has four channels");
        rgba(
            parts[0].parse().expect("red"),
            parts[1].parse().expect("green"),
            parts[2].parse().expect("blue"),
            parts[3].parse().expect("alpha"),
        )
    }

    fn assert_same_names<'a>(
        family: &str,
        json: impl Iterator<Item = &'a str>,
        rust: impl Iterator<Item = &'a str>,
    ) {
        let json: BTreeSet<_> = json.collect();
        let rust: BTreeSet<_> = rust.collect();
        assert_eq!(json, rust, "{family} tokens differ between JSON and Rust");
    }

    #[test]
    fn colors_match_the_reference() {
        let json = reference();
        let colors = json["color"]["tokens"].as_array().expect("color tokens");
        assert_same_names(
            "color",
            colors.iter().map(name),
            color::ALL.iter().map(|(name, _)| *name),
        );
        for token in colors {
            let expected = parse_color(token["value"].as_str().expect("color value"));
            let (_, actual) = color::ALL
                .iter()
                .find(|(candidate, _)| *candidate == name(token))
                .expect("color present");
            assert_eq!(*actual, expected, "{}", name(token));
        }
    }

    #[test]
    fn text_styles_match_the_reference() {
        let json = reference();
        let groups = json["type"]["groups"].as_array().expect("type groups");
        let styles: Vec<&Value> = groups
            .iter()
            .flat_map(|group| group["styles"].as_array().expect("styles"))
            .collect();
        assert_same_names(
            "type",
            styles.iter().map(|style| name(style)),
            text_style::ALL.iter().map(|(name, _)| *name),
        );
        for style in styles {
            let (_, actual) = text_style::ALL
                .iter()
                .find(|(candidate, _)| *candidate == name(style))
                .expect("text style present");
            let letter_spacing = style["letterSpacing"]
                .as_str()
                .map(|value| {
                    value
                        .strip_suffix("em")
                        .expect("letter spacing in em")
                        .parse()
                        .expect("letter spacing number")
                })
                .unwrap_or(0.0);
            let expected = TextStyle {
                size: px(style["fontSize"].as_str().expect("font size")),
                line_height: px(style["lineHeight"].as_str().expect("line height")),
                weight: style["fontWeight"].as_u64().expect("font weight") as u16,
                letter_spacing,
            };
            assert_eq!(*actual, expected, "{}", name(style));
        }
    }

    #[test]
    fn lengths_match_the_reference() {
        let json = reference();
        for (family, rust) in [("spacing", spacing::ALL), ("size", size::ALL)] {
            let list = tokens(&json, family);
            assert_same_names(
                family,
                list.iter().map(name),
                rust.iter().map(|(name, _)| *name),
            );
            for token in list {
                let (_, actual) = rust
                    .iter()
                    .find(|(candidate, _)| *candidate == name(token))
                    .expect("length present");
                assert_eq!(
                    *actual,
                    px(token["value"].as_str().expect("value")),
                    "{}",
                    name(token)
                );
            }
        }
    }

    #[test]
    fn radii_match_the_reference() {
        let json = reference();
        let list = tokens(&json, "radius");
        assert_same_names(
            "radius",
            list.iter().map(name),
            radius::PX
                .iter()
                .map(|(name, _)| *name)
                .chain(std::iter::once("radius-round")),
        );
        for token in list {
            let value = token["value"].as_str().expect("value");
            if name(token) == "radius-round" {
                assert_eq!(value, "50%");
                assert_eq!(radius::ROUND, 0.5);
                continue;
            }
            let (_, actual) = radius::PX
                .iter()
                .find(|(candidate, _)| *candidate == name(token))
                .expect("radius present");
            assert_eq!(*actual, px(value), "{}", name(token));
        }
    }

    #[test]
    fn the_reference_family_is_the_bundled_font() {
        let json = reference();
        let stack = json["type"]["families"]["sans"]
            .as_str()
            .expect("sans stack");
        assert!(stack.starts_with("Inter"), "{stack}");
        assert_eq!(super::super::font::FAMILY, "Inter Variable");
    }
}
