use iced::mouse;
use iced::widget::canvas::{self, Canvas, Frame, Geometry, Path};
use iced::{Color, Element, Length, Point, Rectangle, Renderer, Theme};

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
        frame.fill_rectangle(Point::ORIGIN, bounds.size(), Color::from_rgb8(5, 9, 24));

        let t = self.elapsed / 70.0;
        let blobs = [
            (
                0.24 + 0.05 * t.sin(),
                0.35 + 0.04 * (t * 0.7).cos(),
                0.43,
                Color::from_rgba8(51, 72, 181, 0.42),
            ),
            (
                0.72 + 0.05 * (t * 0.8).cos(),
                0.28 + 0.06 * t.sin(),
                0.38,
                Color::from_rgba8(142, 44, 118, 0.34),
            ),
            (
                0.58 + 0.04 * (t * 1.1).sin(),
                0.78 + 0.03 * t.cos(),
                0.48,
                Color::from_rgba8(23, 111, 140, 0.35),
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

        frame.fill_rectangle(
            Point::ORIGIN,
            bounds.size(),
            Color::from_rgba8(0, 0, 0, 0.2),
        );
        vec![frame.into_geometry()]
    }
}
