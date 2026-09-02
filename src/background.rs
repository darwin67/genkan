use iced::mouse;
use iced::widget::canvas::{self, Canvas, Frame, Geometry, Path};
use iced::widget::{container, Space};
use iced::{Color, Element, Length, Point, Rectangle, Renderer, Theme};

const BASE_COLOR: Color = Color::from_rgb(5.0 / 255.0, 9.0 / 255.0, 24.0 / 255.0);
const BLOB_COLORS: [Color; 3] = [
    Color::from_rgba(51.0 / 255.0, 72.0 / 255.0, 181.0 / 255.0, 0.42),
    Color::from_rgba(142.0 / 255.0, 44.0 / 255.0, 118.0 / 255.0, 0.34),
    Color::from_rgba(23.0 / 255.0, 111.0 / 255.0, 140.0 / 255.0, 0.35),
];
const DIM_COLOR: Color = Color::from_rgba(0.0, 0.0, 0.0, 0.2);

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

pub fn dimming<Message: 'static>() -> Element<'static, Message> {
    container(Space::new(Length::Fill, Length::Fill))
        .style(|_| container::Style {
            background: Some(iced::Background::Color(DIM_COLOR)),
            ..Default::default()
        })
        .into()
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
pub(crate) fn contrast_palette() -> (Color, [Color; 3], Color) {
    (BASE_COLOR, BLOB_COLORS, DIM_COLOR)
}
