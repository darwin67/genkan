//! Stroked line icons drawn with the iced canvas.
//!
//! Each glyph is described in its SVG view box from the component previews and
//! scaled to the bounds it is given. These are original drawings, not Apple
//! symbols.

use std::f32::consts::PI;

use iced::mouse;
use iced::widget::canvas::{
    self, path::Arc, Canvas, Frame, Geometry, LineCap, LineJoin, Path, Stroke,
};
use iced::{Color, Element, Length, Point, Rectangle, Renderer, Size, Theme};

use super::tokens::component;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
            Self::ArrowRight | Self::ArrowLeft => {
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
            Self::ArrowRight | Self::ArrowLeft => Size::new(14.0, 14.0),
        }
    }

    /// Stroke width in view-box units.
    fn stroke_width(self) -> f32 {
        match self {
            Self::Monitor => 1.3,
            Self::Power => 2.0,
            Self::Check => 1.7,
            Self::ArrowRight | Self::ArrowLeft => 2.2,
        }
    }

    fn path(self, scale: f32) -> Path {
        let point = |x: f32, y: f32| Point::new(x * scale, y * scale);
        Path::new(|builder| match self {
            Self::Monitor => {
                builder.rounded_rectangle(
                    point(1.0, 1.0),
                    Size::new(13.0 * scale, 7.6 * scale),
                    (1.5 * scale).into(),
                );
                builder.move_to(point(5.0, 11.0));
                builder.line_to(point(10.0, 11.0));
                builder.move_to(point(7.5, 8.6));
                builder.line_to(point(7.5, 11.0));
            }
            Self::Power => {
                builder.move_to(point(10.0, 3.0));
                builder.line_to(point(10.0, 9.4));
                // The SVG arc from (6.2, 5.7) to (13.8, 5.7) with radius 5.6,
                // taking the long way around below its center.
                let radius = 5.6;
                let half_chord: f32 = 3.8;
                let center_y = 5.7 + (radius * radius - half_chord * half_chord).sqrt();
                let opening = (5.7 - center_y).atan2(half_chord);
                builder.arc(Arc {
                    center: point(10.0, center_y),
                    radius: radius * scale,
                    start_angle: opening.into(),
                    end_angle: (PI - opening).into(),
                });
            }
            Self::Check => {
                builder.move_to(point(1.2, 5.2));
                builder.line_to(point(4.0, 8.0));
                builder.line_to(point(9.8, 1.4));
            }
            Self::ArrowRight => {
                builder.move_to(point(2.6, 7.0));
                builder.line_to(point(11.0, 7.0));
                builder.move_to(point(7.6, 3.4));
                builder.line_to(point(11.0, 7.0));
                builder.line_to(point(7.6, 10.6));
            }
            Self::ArrowLeft => {
                builder.move_to(point(11.4, 7.0));
                builder.line_to(point(3.0, 7.0));
                builder.move_to(point(6.4, 3.4));
                builder.line_to(point(3.0, 7.0));
                builder.line_to(point(6.4, 10.6));
            }
        })
    }
}

#[derive(Debug, Clone, Copy)]
struct Glyph {
    kind: Kind,
    color: Color,
}

impl<Message> canvas::Program<Message> for Glyph {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let view_box = self.kind.view_box();
        let scale = (bounds.width / view_box.width).min(bounds.height / view_box.height);
        frame.stroke(
            &self.kind.path(scale),
            Stroke::default()
                .with_color(self.color)
                .with_width(self.kind.stroke_width() * scale)
                .with_line_cap(LineCap::Round)
                .with_line_join(LineJoin::Round),
        );
        vec![frame.into_geometry()]
    }
}

/// A glyph at its default size.
pub(crate) fn glyph<'a, Message: 'a>(kind: Kind, color: Color) -> Element<'a, Message> {
    sized(kind, color, kind.size())
}

/// A glyph scaled to `size`.
pub(crate) fn sized<'a, Message: 'a>(kind: Kind, color: Color, size: Size) -> Element<'a, Message> {
    Canvas::new(Glyph { kind, color })
        .width(Length::Fixed(size.width))
        .height(Length::Fixed(size.height))
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
        let center_y = 5.7 + (radius * radius - 3.8f32 * 3.8).sqrt();
        let opening = (5.7 - center_y).atan2(3.8);
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
}
