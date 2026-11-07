//! The lock screen's overlays, drawn on the CPU from the design tokens.
//!
//! The composition is the login screen's without the session and power items
//! (RFD 5): a clock anchored `clock-top` below the top edge of the
//! authentication output, and the identity column (avatar, name, password
//! pill and StatusMessage lines) whose pill sits `identity-bottom` above the
//! bottom edge. Each is one overlay rendered at [`DENSITY`] pixels per logical
//! point; the session-lock runtime anchors and scales them per output.
//!
//! Response text never reaches the text renderer. The pill shows a masked
//! count of characters, even for visible PAM prompts, so the shaping and glyph
//! caches never hold application-owned response text.

use chrono::{DateTime, Local};
use cosmic_text::{
    Align, Attrs, Buffer, Color, FontSystem, LayoutRun, Metrics, Shaping, SwashCache, Weight, Wrap,
};
use genkan_session_lock::{AnchoredOverlay, OverlayLayout, PresentationFrame, RgbaFrame};

use crate::conversation::{Conversation, Status};
use crate::design::font;
use crate::design::glyph::{self, Kind as Glyph};
use crate::design::iced::color as glyph_color;
use crate::design::text::{fit_single_line_by, initials, prompt_placement, PromptPlacement};
use crate::design::tokens::{
    color as palette, component, radius, size, spacing, text_style, Rgba, TextStyle,
};

use super::identity::Identity;

/// Overlay pixels per logical point. Outputs at buffer scale 3 show overlay
/// pixels one to one; other scales filter them.
pub(super) const DENSITY: u32 = 3;
/// The most masked characters the pill shows.
const MAX_MASK: usize = 18;
/// The most lines of a prompt shown above the pill at once, including the
/// page hint when the prompt needs more than one page.
const PROMPT_LINES: usize = 4;
const SECURING: &str = "Securing session…";
const AUTHENTICATING: &str = "Authenticating…";
const RETRY_HINT: &str = "Press Return to try again";

/// The layout the runtime applies to [`frame`]'s overlays: the clock keeps
/// `identity-gap` above the identity column and the status-bar inset from
/// the sides, scaling down as login's clock does.
fn layout() -> OverlayLayout {
    OverlayLayout::new(DENSITY)
        .expect("the lock overlay density is supported")
        .with_top_fit(
            points(spacing::IDENTITY_GAP),
            points(spacing::BAR_INSET_RIGHT),
            (component::CLOCK_MIN_SCALE * 100.0).round() as u32,
        )
}

/// A presentation of the wallpaper, undimmed, with the clock and identity
/// overlays anchored to the authentication output's edges.
pub(super) fn frame(
    background: Option<RgbaFrame>,
    clock: Option<RgbaFrame>,
    identity: Option<RgbaFrame>,
    shake: f32,
) -> PresentationFrame {
    PresentationFrame::new(
        background,
        layout(),
        clock.and_then(|clock| AnchoredOverlay::new(clock, points(spacing::CLOCK_TOP))),
        identity.and_then(|identity| {
            let (width, height) = identity.dimensions();
            let backdrop = genkan_session_lock::material::Backdrop {
                x: ((width as f32 - px(size::FIELD_WIDTH)) / 2.0 + px(shake)).max(0.0) as u32,
                y: height.saturating_sub(px(spacing::IDENTITY_BOTTOM + size::FIELD_HEIGHT) as u32),
                width: px(size::FIELD_WIDTH) as u32,
                height: px(size::FIELD_HEIGHT) as u32,
                radius: px(radius::FIELD) as u32,
            };
            AnchoredOverlay::new(identity, 0).map(|overlay| overlay.with_backdrop(backdrop))
        }),
    )
    .with_dimming(None)
}

fn points(value: f32) -> u32 {
    value.round() as u32
}

/// Pixels for a length in logical points.
fn px(value: f32) -> f32 {
    value * DENSITY as f32
}

/// What the identity column shows for one authentication state.
#[derive(Debug, PartialEq, Eq)]
struct IdentityContent<'a> {
    placeholder: Option<&'a str>,
    above: Option<&'a str>,
    masked: usize,
    progress: Option<&'static str>,
    notice: Option<&'a str>,
    retry_hint: bool,
}

/// The fonts and glyph cache the lock overlays are drawn with.
pub(super) struct Renderer {
    fonts: FontSystem,
    glyphs: SwashCache,
    pub(super) keyboard: genkan_session_lock::keyboard::KeyboardStatus,
    pub(super) shake: f32,
}

impl Renderer {
    pub(super) fn new() -> Self {
        Self {
            fonts: font::font_system(),
            glyphs: SwashCache::new(),
            keyboard: Default::default(),
            shake: 0.0,
        }
    }

    /// The prompt shown above the pill, if the current prompt is placed
    /// there rather than inside it.
    pub(super) fn prompt_above<'a>(
        &mut self,
        conversation: &'a Conversation,
        confirmed: bool,
    ) -> Option<&'a str> {
        if !confirmed || !matches!(conversation.status(), Status::Waiting | Status::Submitting) {
            return None;
        }
        let limit = px(placeholder_width());
        match prompt_placement(conversation.prompt(), conversation.is_secret(), |prompt| {
            self.measure(prompt, text_style::FIELD) <= limit
        }) {
            PromptPlacement::Above(prompt) => Some(prompt),
            PromptPlacement::Placeholder(_) => None,
        }
    }

    fn content<'a>(
        &mut self,
        conversation: &'a Conversation,
        confirmed: bool,
    ) -> IdentityContent<'a> {
        let status = conversation.status();
        let waiting = confirmed && status == Status::Waiting;
        let above = self.prompt_above(conversation, confirmed);
        let placeholder = (waiting && above.is_none()).then(|| {
            match prompt_placement(conversation.prompt(), conversation.is_secret(), |_| true) {
                PromptPlacement::Placeholder(prompt) | PromptPlacement::Above(prompt) => prompt,
            }
        });
        IdentityContent {
            placeholder,
            above,
            masked: if waiting {
                conversation.input().chars().count()
            } else {
                0
            },
            progress: if !confirmed {
                Some(SECURING)
            } else if status == Status::Submitting {
                Some(AUTHENTICATING)
            } else {
                None
            },
            notice: confirmed.then(|| conversation.notice()).flatten(),
            retry_hint: confirmed && status == Status::Failed,
        }
    }

    /// The short date over the time, centered in an overlay as wide as the
    /// wider of the two.
    pub(super) fn clock(&mut self, now: DateTime<Local>) -> RgbaFrame {
        let date = now.format("%a %b %-d").to_string();
        let time = now.format("%-I:%M").to_string();
        let content_width = self
            .measure(&date, text_style::DATE)
            .max(self.measure(&time, text_style::CLOCK));
        let width = content_width + 2.0 * px(component::CLOCK_OVERHANG);
        let height = px(text_style::DATE.line_height + text_style::CLOCK.line_height);
        let mut canvas = Canvas::new(width, height);
        let line = |y: f32, style: TextStyle| TextBox {
            x: 0.0,
            y,
            width: canvas.width as f32,
            height: px(style.line_height),
        };
        let (date_box, time_box) = (
            line(0.0, text_style::DATE),
            line(px(text_style::DATE.line_height), text_style::CLOCK),
        );
        self.draw_line(
            &mut canvas,
            &date,
            text_style::DATE,
            palette::TEXT_DATE,
            date_box,
            Align::Center,
        );
        self.draw_line(
            &mut canvas,
            &time,
            text_style::CLOCK,
            palette::CLOCK_GLASS,
            time_box,
            Align::Center,
        );
        canvas.into_frame()
    }

    /// The identity column, from the top of the avatar down to the bottom
    /// edge of the output: avatar, name, an optional prompt, the pill, and
    /// the `identity-bottom` band of StatusMessage lines under it.
    pub(super) fn identity(
        &mut self,
        identity: &Identity,
        conversation: &Conversation,
        confirmed: bool,
        page: usize,
    ) -> RgbaFrame {
        let content = self.content(conversation, confirmed);
        let width = px(size::MESSAGE_WIDTH);
        let gap = px(spacing::IDENTITY_GAP);
        let line_height = px(text_style::MESSAGE.line_height);
        let above = content
            .above
            .map(|prompt| self.paginate(&reflow(prompt), PROMPT_LINES, page));
        let avatar_top = 0.0;
        let name_top = avatar_top + px(size::AVATAR) + gap;
        let above_top = name_top + px(text_style::NAME.line_height) + gap;
        let pill_top = above_top
            + above
                .as_ref()
                .map_or(0.0, |above| above.height_lines as f32 * line_height + gap);
        let pill_bottom = pill_top + px(size::FIELD_HEIGHT);
        let mut canvas = Canvas::new(width, pill_bottom + px(spacing::IDENTITY_BOTTOM));

        let avatar = Circle {
            center_x: canvas.width as f32 / 2.0,
            center_y: avatar_top + px(size::AVATAR) / 2.0,
            diameter: px(size::AVATAR),
        };
        let pill = TextBox {
            x: (canvas.width as f32 - px(size::FIELD_WIDTH)) / 2.0 + px(self.shake),
            y: pill_top,
            width: px(size::FIELD_WIDTH),
            height: px(size::FIELD_HEIGHT),
        };
        let submit = (content.masked > 0).then(|| Circle {
            center_x: pill.x + pill.width - px(component::SUBMIT_INSET) - px(size::SUBMIT) / 2.0,
            center_y: pill.y + pill.height / 2.0,
            diameter: px(size::SUBMIT),
        });
        canvas.draw_shapes(avatar, pill, submit);

        self.draw_line(
            &mut canvas,
            &initials(&identity.display_name),
            text_style::INITIALS,
            palette::TEXT_PRIMARY,
            TextBox {
                x: avatar.center_x - avatar.diameter / 2.0,
                y: avatar.center_y - px(text_style::INITIALS.line_height) / 2.0,
                width: avatar.diameter,
                height: px(text_style::INITIALS.line_height),
            },
            Align::Center,
        );
        let name = self.fit(&identity.display_name, text_style::NAME, width);
        self.draw_line(
            &mut canvas,
            &name,
            text_style::NAME,
            palette::TEXT_PRIMARY,
            TextBox {
                x: 0.0,
                y: name_top,
                width,
                height: px(text_style::NAME.line_height),
            },
            Align::Center,
        );
        if let Some(above) = &above {
            self.draw_paged(&mut canvas, above, above_top, page);
        }

        let text = TextBox {
            x: pill.x + px(component::FIELD_PADDING_LEADING),
            y: pill.y,
            width: px(placeholder_width()
                - if self.keyboard.caps_lock {
                    component::GLYPH_ARROW + component::ITEM_CONTENT_GAP
                } else {
                    0.0
                }),
            height: pill.height,
        };
        if content.masked > 0 {
            let mask = self.fit_mask(content.masked, text.width);
            self.draw_line(
                &mut canvas,
                &mask,
                text_style::FIELD,
                palette::TEXT_PRIMARY,
                text,
                Align::Left,
            );
        } else if let Some(placeholder) = content.placeholder {
            let placeholder = self.fit(placeholder, text_style::FIELD, text.width);
            self.draw_line(
                &mut canvas,
                &placeholder,
                text_style::FIELD,
                palette::TEXT_PLACEHOLDER,
                text,
                Align::Left,
            );
        }
        if self.keyboard.caps_lock {
            let side = px(component::GLYPH_ARROW).round() as u32;
            canvas.draw_glyph(
                Glyph::CapsLock,
                palette::TEXT_PRIMARY,
                (pill.x + pill.width - px(component::FIELD_PADDING_TRAILING) - side as f32) as i32,
                (pill.y + (pill.height - side as f32) / 2.0) as i32,
                side,
            );
        }
        if let Some(submit) = submit {
            let side = px(component::GLYPH_ARROW).round() as u32;
            canvas.draw_glyph(
                Glyph::ArrowRight,
                palette::TEXT_PRIMARY,
                (submit.center_x - side as f32 / 2.0).round() as i32,
                (submit.center_y - side as f32 / 2.0).round() as i32,
                side,
            );
        }

        // The band under the pill: progress, then the PAM notice, then how
        // to retry after a failure, each a StatusMessage.
        let band_bottom = canvas.height as f32;
        let mut y = pill_bottom + gap;
        let single = |renderer: &mut Self, canvas: &mut Canvas, message: &str, y: &mut f32| {
            let message = renderer.fit(message, text_style::MESSAGE, width);
            renderer.draw_line(
                canvas,
                &message,
                text_style::MESSAGE,
                palette::TEXT_PRIMARY,
                TextBox {
                    x: 0.0,
                    y: *y,
                    width,
                    height: line_height,
                },
                Align::Center,
            );
            *y += line_height + gap;
        };
        if let Some(progress) = content.progress {
            single(self, &mut canvas, progress, &mut y);
        }
        if let Some(notice) = content.notice {
            let reserved = if content.retry_hint {
                line_height + gap
            } else {
                0.0
            };
            let lines = ((band_bottom - y - reserved) / line_height)
                .floor()
                .max(1.0) as usize;
            let notice = self.paginate(&reflow(notice), lines, page);
            self.draw_paged(&mut canvas, &notice, y, page);
            y += notice.height_lines as f32 * line_height + gap;
        }
        if content.retry_hint {
            single(self, &mut canvas, RETRY_HINT, &mut y);
        }
        canvas.into_frame()
    }

    pub(super) fn status(&mut self, network: Option<&str>) -> Option<RgbaFrame> {
        let text = [self.keyboard.layout.as_deref(), network]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("   ");
        if text.is_empty() {
            return None;
        }
        let text = self.fit(&text, text_style::BAR, px(size::MESSAGE_WIDTH));
        let width = self.measure(&text, text_style::BAR).ceil();
        let height = px(size::BAR_HEIGHT);
        let mut canvas = Canvas::new(width, height);
        self.draw_line(
            &mut canvas,
            &text,
            text_style::BAR,
            palette::TEXT_PRIMARY,
            TextBox {
                x: 0.0,
                y: 0.0,
                width,
                height,
            },
            Align::Right,
        );
        Some(canvas.into_frame())
    }

    /// Shapes `text` in `style`, aligned within `width` pixels when given.
    fn shape(
        &mut self,
        text: &str,
        style: TextStyle,
        width: Option<f32>,
        wrap: Wrap,
        align: Align,
    ) -> Buffer {
        let mut buffer = Buffer::new(
            &mut self.fonts,
            Metrics::new(px(style.size), px(style.line_height)),
        );
        buffer.set_size(&mut self.fonts, width, None);
        buffer.set_wrap(&mut self.fonts, wrap);
        buffer.set_text(
            &mut self.fonts,
            text,
            &attrs(style),
            Shaping::Advanced,
            Some(align),
        );
        buffer.shape_until_scroll(&mut self.fonts, true);
        buffer
    }

    /// The advance width of `text` on one line, in pixels.
    fn measure(&mut self, text: &str, style: TextStyle) -> f32 {
        self.shape(text, style, None, Wrap::None, Align::Left)
            .layout_runs()
            .map(|run| run.line_w)
            .fold(0.0, f32::max)
    }

    fn fit(&mut self, text: &str, style: TextStyle, width: f32) -> String {
        fit_single_line_by(text, width, |candidate| self.measure(candidate, style))
    }

    /// As many mask characters as there are response characters, up to what
    /// fits in `width` pixels.
    fn fit_mask(&mut self, characters: usize, width: f32) -> String {
        let mut mask = masked_response(characters);
        while mask.chars().count() > 1 && self.measure(&mask, text_style::FIELD) > width {
            mask.pop();
        }
        mask
    }

    fn paginate(&mut self, text: &str, max_lines: usize, page: usize) -> Paged {
        let buffer = self.shape(
            text,
            text_style::MESSAGE,
            Some(px(size::MESSAGE_WIDTH)),
            Wrap::WordOrGlyph,
            Align::Center,
        );
        let line_count = buffer.layout_runs().count().max(1);
        let line_height = px(text_style::MESSAGE.line_height);
        let (page_lines, height_lines) = if line_count <= max_lines {
            (line_count, line_count)
        } else {
            let page_lines = max_lines.saturating_sub(1).max(1);
            (page_lines, page_lines + 1)
        };
        let (pages, first_line, shown_lines) = page_layout(
            line_count,
            (page_lines as f32 * line_height).ceil() as u32,
            line_height,
            page,
        );
        Paged {
            buffer,
            pages,
            first_line,
            shown_lines,
            page_lines,
            height_lines,
        }
    }

    fn draw_paged(&mut self, canvas: &mut Canvas, paged: &Paged, top: f32, page: usize) {
        let line_height = px(text_style::MESSAGE.line_height);
        let clip = TextBox {
            x: 0.0,
            y: top,
            width: canvas.width as f32,
            height: paged.page_lines as f32 * line_height,
        };
        let runs = paged
            .buffer
            .layout_runs()
            .skip(paged.first_line)
            .take(paged.shown_lines)
            .collect::<Vec<_>>();
        let page_top = runs.first().map_or(0.0, |run| run.line_top);
        self.draw_runs(
            canvas,
            &runs,
            0.0,
            top - page_top,
            clip,
            palette::TEXT_PRIMARY,
        );
        if paged.pages > 1 {
            let hint = format!(
                "{} / {} · Tab for more",
                page % paged.pages + 1,
                paged.pages
            );
            self.draw_line(
                canvas,
                &hint,
                text_style::MESSAGE,
                palette::TEXT_SECONDARY,
                TextBox {
                    x: 0.0,
                    y: top + clip.height,
                    width: canvas.width as f32,
                    height: line_height,
                },
                Align::Center,
            );
        }
    }

    fn draw_line(
        &mut self,
        canvas: &mut Canvas,
        text: &str,
        style: TextStyle,
        color: Rgba,
        bounds: TextBox,
        align: Align,
    ) {
        // Callers fit the text first; anything wider is clipped, never wrapped.
        let buffer = self.shape(text, style, Some(bounds.width), Wrap::None, align);
        let runs = buffer.layout_runs().collect::<Vec<_>>();
        self.draw_runs(canvas, &runs, bounds.x, bounds.y, bounds, color);
    }

    /// Blends shaped runs offset by (`x`, `y`) pixels, clipped to `clip`.
    fn draw_runs(
        &mut self,
        canvas: &mut Canvas,
        runs: &[LayoutRun<'_>],
        x: f32,
        y: f32,
        clip: TextBox,
        color: Rgba,
    ) {
        let requested = rgba8(color);
        let base = Color::rgba(requested[0], requested[1], requested[2], u8::MAX);
        let (left, top) = (clip.x.floor() as i32, clip.y.floor() as i32);
        let (right, bottom) = (
            (clip.x + clip.width).ceil() as i32,
            (clip.y + clip.height).ceil() as i32,
        );
        for shadow in [true, false] {
            for run in runs {
                for glyph in run.glyphs {
                    let physical = glyph.physical((x, y), 1.0);
                    let baseline = run.line_y.round() as i32;
                    self.glyphs.with_pixels(
                        &mut self.fonts,
                        physical.cache_key,
                        glyph.color_opt.unwrap_or(base),
                        |offset_x, offset_y, glyph_color| {
                            let pixel_x = physical.x + offset_x;
                            let pixel_y = baseline + physical.y + offset_y;
                            if pixel_x < left
                                || pixel_y < top
                                || pixel_x >= right
                                || pixel_y >= bottom
                            {
                                return;
                            }
                            if shadow {
                                let alpha =
                                    glyph_pixel(glyph_color.as_rgba(), rgba8(palette::TEXT_SHADOW))
                                        [3];
                                let offset = DENSITY as i32;
                                for (dx, dy) in
                                    [(-offset, 0), (offset, 0), (0, -offset), (0, offset)]
                                {
                                    canvas.blend(pixel_x + dx, pixel_y + dy, [0, 0, 0, alpha]);
                                }
                            } else {
                                canvas.blend(
                                    pixel_x,
                                    pixel_y,
                                    glyph_pixel(glyph_color.as_rgba(), requested),
                                );
                            }
                        },
                    );
                }
            }
        }
    }
}

/// A wrapped StatusMessage split into pages of whole lines.
struct Paged {
    buffer: Buffer,
    pages: usize,
    first_line: usize,
    shown_lines: usize,
    /// Text lines per page.
    page_lines: usize,
    /// Lines the block occupies on every page, including the page hint, so
    /// paging never moves what is around it.
    height_lines: usize,
}

#[derive(Debug, Clone, Copy)]
struct TextBox {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

#[derive(Debug, Clone, Copy)]
struct Circle {
    center_x: f32,
    center_y: f32,
    diameter: f32,
}

/// Straight-alpha RGBA pixels for one overlay.
struct Canvas {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl Canvas {
    fn new(width: f32, height: f32) -> Self {
        let width = width.ceil().max(1.0) as u32;
        let height = height.ceil().max(1.0) as u32;
        Self {
            width,
            height,
            pixels: vec![0; width as usize * height as usize * 4],
        }
    }

    fn into_frame(self) -> RgbaFrame {
        RgbaFrame::new(self.width, self.height, self.pixels.into())
            .expect("lock overlay has valid dimensions")
    }

    fn blend(&mut self, x: i32, y: i32, source: [u8; 4]) {
        blend_pixel(&mut self.pixels, self.width, x, y, source);
    }

    /// Draws the avatar, the pill and, when there is input, the submit
    /// circle, anti-aliased. They are drawn before any text, which is blended
    /// over them.
    fn draw_shapes(&mut self, avatar: Circle, pill: TextBox, submit: Option<Circle>) {
        let Some(mut pixmap) = tiny_skia::Pixmap::new(self.width, self.height) else {
            return;
        };
        let identity = tiny_skia::Transform::identity();
        let ring = px(component::AVATAR_RING_WIDTH);
        if let Some(path) = circle(avatar, 0.0) {
            let top = avatar.center_y - avatar.diameter / 2.0;
            let gradient = tiny_skia::LinearGradient::new(
                tiny_skia::Point::from_xy(avatar.center_x, top),
                tiny_skia::Point::from_xy(avatar.center_x, top + avatar.diameter),
                vec![
                    tiny_skia::GradientStop::new(0.0, skia_color(palette::AVATAR_TOP)),
                    tiny_skia::GradientStop::new(1.0, skia_color(palette::AVATAR_BOTTOM)),
                ],
                tiny_skia::SpreadMode::Pad,
                identity,
            );
            if let Some(shader) = gradient {
                let paint = tiny_skia::Paint {
                    shader,
                    anti_alias: true,
                    ..tiny_skia::Paint::default()
                };
                pixmap.fill_path(&path, &paint, tiny_skia::FillRule::Winding, identity, None);
            }
        }
        if let Some(path) = circle(avatar, ring / 2.0) {
            stroke(&mut pixmap, &path, palette::AVATAR_RING, ring);
        }

        let hairline = px(component::HAIRLINE);
        let corner = px(radius::FIELD);
        if let Some(path) = rounded_rect(pill, corner, 0.0) {
            pixmap.fill_path(
                &path,
                &paint(palette::FIELD_GLASS),
                tiny_skia::FillRule::Winding,
                identity,
                None,
            );
        }
        if let Some(path) = rounded_rect(pill, corner, hairline / 2.0) {
            stroke(&mut pixmap, &path, palette::FIELD_BORDER, hairline);
        }

        if let Some(submit) = submit {
            let outline = px(component::CIRCLE_OUTLINE_WIDTH);
            if let Some(path) = circle(submit, outline / 2.0) {
                stroke(&mut pixmap, &path, component::CIRCLE_OUTLINE, outline);
            }
        }

        for (target, source) in self.pixels.chunks_exact_mut(4).zip(pixmap.pixels()) {
            let straight = source.demultiply();
            target.copy_from_slice(&[
                straight.red(),
                straight.green(),
                straight.blue(),
                straight.alpha(),
            ]);
        }
    }

    /// Blends a design-system glyph `side` pixels square at (`x`, `y`).
    fn draw_glyph(&mut self, kind: Glyph, color: Rgba, x: i32, y: i32, side: u32) {
        let Some(pixels) = glyph::rgba(kind, glyph_color(color), side, side) else {
            return;
        };
        for (index, pixel) in pixels.chunks_exact(4).enumerate() {
            if pixel[3] == 0 {
                continue;
            }
            let (column, row) = (index as u32 % side, index as u32 / side);
            self.blend(
                x + column as i32,
                y + row as i32,
                [pixel[0], pixel[1], pixel[2], pixel[3]],
            );
        }
    }
}

/// A circle `inset` pixels inside `shape`'s edge.
fn circle(shape: Circle, inset: f32) -> Option<tiny_skia::Path> {
    tiny_skia::PathBuilder::from_circle(
        shape.center_x,
        shape.center_y,
        shape.diameter / 2.0 - inset,
    )
}

/// A rounded rectangle `inset` pixels inside `bounds`.
fn rounded_rect(bounds: TextBox, radius: f32, inset: f32) -> Option<tiny_skia::Path> {
    // Cubic Bézier control distance for a quarter circle.
    const KAPPA: f32 = 0.552_284_8;
    let (left, top) = (bounds.x + inset, bounds.y + inset);
    let (right, bottom) = (
        bounds.x + bounds.width - inset,
        bounds.y + bounds.height - inset,
    );
    let radius = (radius - inset)
        .min((right - left) / 2.0)
        .min((bottom - top) / 2.0)
        .max(0.0);
    let control = radius * (1.0 - KAPPA);
    let mut builder = tiny_skia::PathBuilder::new();
    builder.move_to(left + radius, top);
    builder.line_to(right - radius, top);
    builder.cubic_to(
        right - control,
        top,
        right,
        top + control,
        right,
        top + radius,
    );
    builder.line_to(right, bottom - radius);
    builder.cubic_to(
        right,
        bottom - control,
        right - control,
        bottom,
        right - radius,
        bottom,
    );
    builder.line_to(left + radius, bottom);
    builder.cubic_to(
        left + control,
        bottom,
        left,
        bottom - control,
        left,
        bottom - radius,
    );
    builder.line_to(left, top + radius);
    builder.cubic_to(left, top + control, left + control, top, left + radius, top);
    builder.close();
    builder.finish()
}

fn stroke(pixmap: &mut tiny_skia::Pixmap, path: &tiny_skia::Path, color: Rgba, width: f32) {
    pixmap.stroke_path(
        path,
        &paint(color),
        &tiny_skia::Stroke {
            width,
            ..tiny_skia::Stroke::default()
        },
        tiny_skia::Transform::identity(),
        None,
    );
}

fn paint(color: Rgba) -> tiny_skia::Paint<'static> {
    let mut paint = tiny_skia::Paint::default();
    paint.set_color(skia_color(color));
    paint.anti_alias = true;
    paint
}

fn skia_color(color: Rgba) -> tiny_skia::Color {
    let [red, green, blue, alpha] = rgba8(color);
    tiny_skia::Color::from_rgba8(red, green, blue, alpha)
}

fn rgba8(color: Rgba) -> [u8; 4] {
    [
        color.red,
        color.green,
        color.blue,
        (color.alpha.clamp(0.0, 1.0) * 255.0).round() as u8,
    ]
}

fn attrs(style: TextStyle) -> Attrs<'static> {
    let attrs = font::attrs().weight(Weight(style.weight));
    if style.letter_spacing == 0.0 {
        attrs
    } else {
        attrs.letter_spacing(style.letter_spacing)
    }
}

/// The width of the pill's text, in points: inside the leading inset and
/// clear of the submit circle.
fn placeholder_width() -> f32 {
    size::FIELD_WIDTH - component::FIELD_PADDING_LEADING - component::FIELD_PADDING_TRAILING
}

pub(super) fn masked_response(characters: usize) -> String {
    "•".repeat(characters.min(MAX_MASK))
}

pub(super) fn reflow(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(super) fn page_layout(
    line_count: usize,
    text_height: u32,
    line_height: f32,
    page: usize,
) -> (usize, usize, usize) {
    let lines_per_page = ((text_height as f32 / line_height).floor() as usize).max(1);
    let page_count = line_count.max(1).div_ceil(lines_per_page);
    let page = page % page_count;
    let first_line = page * lines_per_page;
    (
        page_count,
        first_line,
        line_count.saturating_sub(first_line).min(lines_per_page),
    )
}

/// A glyph pixel's color with its coverage scaled by the requested opacity.
fn glyph_pixel(glyph: [u8; 4], requested: [u8; 4]) -> [u8; 4] {
    [
        glyph[0],
        glyph[1],
        glyph[2],
        ((u16::from(glyph[3]) * u16::from(requested[3])) / 255) as u8,
    ]
}

fn blend_pixel(pixels: &mut [u8], width: u32, x: i32, y: i32, source: [u8; 4]) {
    if x < 0 || y < 0 || x >= width as i32 {
        return;
    }
    let Some(index) = (y as usize)
        .checked_mul(width as usize)
        .and_then(|row| row.checked_add(x as usize))
        .and_then(|pixel| pixel.checked_mul(4))
    else {
        return;
    };
    let Some(target) = pixels.get_mut(index..index + 4) else {
        return;
    };
    let source_alpha = u32::from(source[3]);
    let target_alpha = u32::from(target[3]);
    let output_alpha = source_alpha * 255 + target_alpha * (255 - source_alpha);
    if output_alpha == 0 {
        return;
    }
    for channel in 0..3 {
        let premultiplied = u32::from(source[channel]) * source_alpha * 255
            + u32::from(target[channel]) * target_alpha * (255 - source_alpha);
        target[channel] = (premultiplied / output_alpha) as u8;
    }
    target[3] = (output_alpha / 255) as u8;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> Identity {
        Identity {
            uid: 1000,
            username: "preview".into(),
            display_name: "Preview User".into(),
        }
    }

    fn waiting(input: &str, prompt: &str, secret: bool, notice: Option<&str>) -> Conversation {
        Conversation::for_preview(
            input.into(),
            prompt.into(),
            notice.map(Into::into),
            false,
            secret,
            Status::Waiting,
        )
    }

    fn pixel(frame: &RgbaFrame, x: f32, y: f32) -> [u8; 4] {
        let (x, y) = (px(x) as usize, px(y) as usize);
        let offset = (y * frame.dimensions().0 as usize + x) * 4;
        frame.pixels()[offset..offset + 4].try_into().unwrap()
    }

    /// The base column: avatar, name and pill, then the band under the pill.
    fn base_height() -> f32 {
        size::AVATAR
            + spacing::IDENTITY_GAP
            + text_style::NAME.line_height
            + spacing::IDENTITY_GAP
            + size::FIELD_HEIGHT
            + spacing::IDENTITY_BOTTOM
    }

    /// The pill's top edge in points, for a column without a prompt above.
    fn pill_top() -> f32 {
        base_height() - spacing::IDENTITY_BOTTOM - size::FIELD_HEIGHT
    }

    fn pill_left() -> f32 {
        (size::MESSAGE_WIDTH - size::FIELD_WIDTH) / 2.0
    }

    #[test]
    fn the_identity_column_is_message_width_and_ends_identity_bottom_under_the_pill() {
        let mut renderer = Renderer::new();
        let frame = renderer.identity(&identity(), &waiting("", "Password", true, None), true, 0);
        assert_eq!(
            frame.dimensions(),
            (
                px(size::MESSAGE_WIDTH) as u32,
                px(base_height()).ceil() as u32
            )
        );

        let challenge = waiting("", &"Step ".repeat(200), false, None);
        let taller = renderer.identity(&identity(), &challenge, true, 0);
        let prompt_block = PROMPT_LINES as f32 * text_style::MESSAGE.line_height;
        assert_eq!(
            taller.dimensions().1,
            px(base_height() + prompt_block + spacing::IDENTITY_GAP).ceil() as u32,
            "a long prompt is paged rather than growing without bound"
        );
    }

    #[test]
    fn the_pill_is_field_glass_with_a_hairline_and_round_ends() {
        let mut renderer = Renderer::new();
        let frame = renderer.identity(&identity(), &waiting("", "Password", true, None), true, 0);
        let middle = pill_top() + size::FIELD_HEIGHT / 2.0;
        // Right of the placeholder, inside the trailing inset.
        let fill = pixel(&frame, pill_left() + size::FIELD_WIDTH - 12.0, middle);
        assert_eq!(fill, rgba8(palette::FIELD_GLASS));
        let edge = pixel(
            &frame,
            pill_left() + size::FIELD_WIDTH / 2.0 + 20.0,
            pill_top(),
        );
        assert!(edge[3] > 0 && edge[..3] != fill[..3], "{edge:?}");
        assert_eq!(pixel(&frame, pill_left(), pill_top())[3], 0, "corner");
        assert_eq!(pixel(&frame, 1.0, middle)[3], 0, "outside the pill");
    }

    #[test]
    fn the_avatar_runs_from_its_top_color_to_its_bottom_color_inside_a_ring() {
        let mut renderer = Renderer::new();
        let frame = renderer.identity(&identity(), &waiting("", "Password", true, None), true, 0);
        let center = size::MESSAGE_WIDTH / 2.0;
        let inset = component::AVATAR_RING_WIDTH + 4.0;
        let top = pixel(&frame, center, inset);
        let bottom = pixel(&frame, center, size::AVATAR - inset);
        assert_eq!((top[3], bottom[3]), (u8::MAX, u8::MAX));
        assert!(
            top[..3] > bottom[..3],
            "{top:?} should be lighter than {bottom:?}"
        );
        let ring = pixel(&frame, center, component::AVATAR_RING_WIDTH / 2.0);
        assert!(ring[0] > top[0], "the ring is lighter than the avatar fill");
    }

    #[test]
    fn response_text_never_reaches_the_glyph_pipeline() {
        let mut renderer = Renderer::new();
        // Visible PAM prompts are masked too: only the character count matters.
        for (secret, prompt) in [(true, "Password"), (false, "Verification code")] {
            let typed = waiting("hunter", prompt, secret, None);
            let other = waiting("qqqqqq", prompt, secret, None);
            assert_eq!(
                renderer.identity(&identity(), &typed, true, 0),
                renderer.identity(&identity(), &other, true, 0),
                "{prompt}"
            );
            let longer = waiting("hunter2", prompt, secret, None);
            assert_ne!(
                renderer.identity(&identity(), &typed, true, 0),
                renderer.identity(&identity(), &longer, true, 0)
            );
        }
        assert_eq!(masked_response(512).chars().count(), MAX_MASK);
        let fitted = renderer.fit_mask(512, px(placeholder_width()));
        assert!(renderer.measure(&fitted, text_style::FIELD) <= px(placeholder_width()));
    }

    #[test]
    fn the_submit_circle_appears_only_with_input() {
        let mut renderer = Renderer::new();
        let x = pill_left() + size::FIELD_WIDTH - component::SUBMIT_INSET - size::SUBMIT / 2.0;
        let y = pill_top() + (size::FIELD_HEIGHT - size::SUBMIT) / 2.0 + 0.2;
        let empty = renderer.identity(&identity(), &waiting("", "Password", true, None), true, 0);
        let typed = renderer.identity(&identity(), &waiting("a", "Password", true, None), true, 0);
        assert_eq!(pixel(&empty, x, y), rgba8(palette::FIELD_GLASS));
        assert!(pixel(&typed, x, y)[3] > rgba8(palette::FIELD_GLASS)[3]);
    }

    #[test]
    fn progress_and_notices_use_the_band_under_the_pill() {
        let mut renderer = Renderer::new();
        let band_is_empty = |frame: &RgbaFrame| {
            let top = px(pill_top() + size::FIELD_HEIGHT).ceil() as usize;
            let stride = frame.dimensions().0 as usize * 4;
            frame.pixels()[top * stride..]
                .iter()
                .skip(3)
                .step_by(4)
                .all(|alpha| *alpha == 0)
        };
        let prompt = waiting("", "Password", true, None);
        assert!(band_is_empty(&renderer.identity(
            &identity(),
            &prompt,
            true,
            0
        )));
        assert!(!band_is_empty(&renderer.identity(
            &identity(),
            &prompt,
            false,
            0
        )));
        let notice = waiting("", "Password", true, Some("Authentication failed"));
        assert!(!band_is_empty(&renderer.identity(
            &identity(),
            &notice,
            true,
            0
        )));

        let securing = renderer.content(&prompt, false);
        assert_eq!(securing.progress, Some(SECURING));
        assert_eq!((securing.placeholder, securing.masked), (None, 0));
        let failed = Conversation::for_preview(
            String::new(),
            "Password".into(),
            Some("Authentication failed".into()),
            true,
            true,
            Status::Failed,
        );
        let failed = renderer.content(&failed, true);
        assert!(failed.retry_hint);
        assert_eq!(failed.placeholder, None, "a failed attempt clears the pill");
        let submitting = Conversation::for_preview(
            String::new(),
            "Password".into(),
            None,
            false,
            true,
            Status::Submitting,
        );
        assert_eq!(
            renderer.content(&submitting, true).progress,
            Some(AUTHENTICATING)
        );
        assert_eq!(
            renderer.content(&prompt, true).placeholder,
            Some("Password")
        );
        assert_eq!(
            renderer
                .content(&waiting("", "", true, None), true)
                .placeholder,
            Some("Password"),
            "an empty PAM prompt is cleaned to the default prompt"
        );
    }

    #[test]
    fn long_text_is_paged_without_moving_the_pill() {
        let mut renderer = Renderer::new();
        let challenge = format!(
            "{}\nEnter the following challenge response: 793146",
            "Step\n".repeat(80)
        );
        let conversation = waiting("", &challenge, false, None);
        let first = renderer.identity(&identity(), &conversation, true, 0);
        let second = renderer.identity(&identity(), &conversation, true, 1);
        assert_eq!(first.dimensions(), second.dimensions());
        assert_ne!(first, second);

        let displayed = reflow(&challenge);
        assert!(!displayed.contains('\n'));
        assert!(displayed.ends_with("Enter the following challenge response: 793146"));
        assert_eq!(page_layout(17, 165, 10.4, 0), (2, 0, 15));
        assert_eq!(page_layout(17, 165, 10.4, 1), (2, 15, 2));
        assert_eq!(page_layout(10, 80, 9.1, 1), (2, 8, 2));
    }

    #[test]
    fn the_clock_draws_the_date_over_the_time_in_clock_glass() {
        let mut renderer = Renderer::new();
        let now = crate::app::preview::preview_now();
        let frame = renderer.clock(now);
        let (width, height) = frame.dimensions();
        assert_eq!(
            height,
            px(text_style::DATE.line_height + text_style::CLOCK.line_height).ceil() as u32
        );
        let time = renderer.measure("9:41", text_style::CLOCK);
        assert!(
            width as f32 >= time
                && (width as f32) < time + px(2.0 * component::CLOCK_OVERHANG) + 1.0
        );
        let glass = rgba8(palette::CLOCK_GLASS);
        let time_top = px(text_style::DATE.line_height) as usize * width as usize * 4;
        assert!(
            frame.pixels()[time_top..]
                .chunks_exact(4)
                .any(|pixel| pixel[0] > 0
                    && pixel[0] < glass[0]
                    && pixel[0] <= pixel[1]
                    && pixel[1] <= pixel[2]
                    && pixel[3] > glass[3]),
            "the translucent clock glass is composited over its dark outline"
        );
        let date_rows = &frame.pixels()[..time_top];
        assert!(
            date_rows.chunks_exact(4).any(|pixel| pixel[3] > 0),
            "the date is drawn"
        );
    }

    #[test]
    fn the_frame_is_undimmed_and_anchored_to_the_output_edges() {
        let identity = RgbaFrame::new(1, 1, vec![0; 4].into()).unwrap();
        let clock = RgbaFrame::new(1, 1, vec![0; 4].into()).unwrap();
        let frame = frame(None, Some(clock.clone()), Some(identity.clone()), 0.0);
        let expected = PresentationFrame::new(
            None,
            OverlayLayout::new(DENSITY)
                .unwrap()
                .with_top_fit(12, 48, 20),
            AnchoredOverlay::new(clock, 183),
            AnchoredOverlay::new(identity, 0).map(|overlay| {
                overlay.with_backdrop(genkan_session_lock::material::Backdrop {
                    x: 0,
                    y: 0,
                    width: px(size::FIELD_WIDTH) as u32,
                    height: px(size::FIELD_HEIGHT) as u32,
                    radius: px(radius::FIELD) as u32,
                })
            }),
        );
        assert_eq!(frame, expected);
    }

    #[test]
    fn alpha_blending_preserves_transparency_for_layered_rendering() {
        let mut transparent = [0, 0, 0, 0];

        blend_pixel(&mut transparent, 1, 0, 0, [7, 10, 24, 210]);
        assert_eq!(transparent, [7, 10, 24, 210]);

        blend_pixel(&mut transparent, 1, 0, 0, [255, 255, 255, 255]);
        assert_eq!(transparent, [255, 255, 255, 255]);

        let mut translucent_white = [255, 255, 255, 128];
        blend_pixel(&mut translucent_white, 1, 0, 0, [255, 255, 255, 128]);
        assert_eq!(translucent_white, [255, 255, 255, 191]);
    }

    #[test]
    fn glyph_rendering_applies_the_requested_opacity() {
        assert_eq!(
            glyph_pixel([240, 240, 240, 128], [240, 240, 240, 190]),
            [240, 240, 240, 95]
        );
        assert_eq!(
            glyph_pixel([12, 140, 220, 255], [255, 255, 255, 128]),
            [12, 140, 220, 128]
        );
    }
}
