use iced::mouse;
use iced::widget::canvas::{self, Canvas, Frame, Geometry, Path};
#[cfg(test)]
use iced::Color;
use iced::{Element, Length, Point, Rectangle, Renderer, Theme};

use crate::design::style::{FALLBACK_BASE as BASE_COLOR, FALLBACK_BLOBS as BLOB_COLORS};

#[derive(Debug, Clone, Copy)]
pub struct Background {
    elapsed: f32,
}

impl Background {
    pub fn new(elapsed: f32) -> Self {
        Self { elapsed }
    }

    pub fn view<Message: 'static>(self) -> Element<'static, Message> {
        Canvas::new(self)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }
}

impl<Message> canvas::Program<Message> for Background {
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
        frame.fill_rectangle(Point::ORIGIN, bounds.size(), BASE_COLOR);

        let t = self.elapsed / 70.0;
        let blobs = [
            (
                0.24 + 0.05 * t.sin(),
                0.35 + 0.04 * (t * 0.7).cos(),
                0.43,
                BLOB_COLORS[0],
            ),
            (
                0.72 + 0.05 * (t * 0.8).cos(),
                0.28 + 0.06 * t.sin(),
                0.38,
                BLOB_COLORS[1],
            ),
            (
                0.58 + 0.04 * (t * 1.1).sin(),
                0.78 + 0.03 * t.cos(),
                0.48,
                BLOB_COLORS[2],
            ),
        ];

        let scale = bounds.width.max(bounds.height);
        for (x, y, radius, color) in blobs {
            frame.fill(
                &Path::circle(
                    Point::new(x * bounds.width, y * bounds.height),
                    radius * scale,
                ),
                color,
            );
        }

        vec![frame.into_geometry()]
    }
}

#[cfg(test)]
pub(crate) fn contrast_palette() -> (Color, [Color; 3]) {
    (BASE_COLOR, BLOB_COLORS)
}
