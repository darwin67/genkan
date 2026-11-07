//! Draw a wallpaper sample behind a control, using its final window bounds.
use iced::advanced::image::{self, Renderer as _};
use iced::advanced::{
    widget::{Tree, Widget},
    Clipboard, Layout, Shell,
};
use iced::{mouse, Element, Length, Rectangle, Size};

pub(crate) fn backdrop<'a, M: 'a>(
    content: impl Into<Element<'a, M>>,
    wallpaper: Option<genkan_session_lock::RgbaFrame>,
    radius: f32,
    window: Size,
    offset_x: f32,
) -> Element<'a, M> {
    let Some(wallpaper) = wallpaper else {
        return content.into();
    };
    let blur = genkan_session_lock::material::blurred(&wallpaper);
    Element::new(Backdrop {
        content: content.into(),
        radius,
        blur,
        aspect: wallpaper.dimensions().0 as f32 / wallpaper.dimensions().1 as f32,
        window,
        offset_x,
    })
}

struct Backdrop<'a, M> {
    content: Element<'a, M>,
    blur: genkan_session_lock::RgbaFrame,
    aspect: f32,
    radius: f32,
    window: Size,
    offset_x: f32,
}

#[derive(Clone, Copy, PartialEq)]
struct Sample {
    bounds: Rectangle,
    window: Size,
    aspect: f32,
    radius: f32,
    offset_x: f32,
}

impl Sample {
    fn image(self, blur: &genkan_session_lock::RgbaFrame) -> image::Handle {
        type Entry = (genkan_session_lock::RgbaFrame, Sample, image::Handle);
        thread_local! {
            static CACHE: std::cell::RefCell<std::collections::VecDeque<Entry>> = const {
                std::cell::RefCell::new(std::collections::VecDeque::new())
            };
        }
        CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            if let Some((_, _, handle)) = cache.iter().find(|(source, sample, _)| {
                source.pixels().as_ptr() == blur.pixels().as_ptr() && *sample == self
            }) {
                return handle.clone();
            }
            let (width, height, pixels) = self.raster(blur);
            let handle = image::Handle::from_rgba(width, height, pixels);
            if cache.len() == 8 {
                cache.pop_front();
            }
            cache.push_back((blur.clone(), self, handle.clone()));
            handle
        })
    }

    /// Rasterize the *control*, including its rounded alpha mask. tiny-skia
    /// does not implement image-local clipping, so never submit a full-window
    /// image and rely on clip_bounds to keep it inside a control.
    fn raster(self, blur: &genkan_session_lock::RgbaFrame) -> (u32, u32, Vec<u8>) {
        let width = (self.bounds.width * 2.0).ceil().max(1.0) as u32;
        let height = (self.bounds.height * 2.0).ceil().max(1.0) as u32;
        let cover_height = self.window.height.max(self.window.width / self.aspect);
        let cover_width = cover_height * self.aspect;
        let origin_x = (self.window.width - cover_width) / 2.0;
        let origin_y = (self.window.height - cover_height) / 2.0;
        let (bw, bh) = blur.dimensions();
        let radius = self
            .radius
            .min(self.bounds.width / 2.0)
            .min(self.bounds.height / 2.0);
        let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
        for y in 0..height {
            let py = (y as f32 + 0.5) * self.bounds.height / height as f32;
            for x in 0..width {
                let px = (x as f32 + 0.5) * self.bounds.width / width as f32;
                let sx = ((self.bounds.x + self.offset_x + px - origin_x) / cover_width
                    * bw as f32
                    - 0.5)
                    .clamp(0.0, (bw - 1) as f32);
                let sy = ((self.bounds.y + py - origin_y) / cover_height * bh as f32 - 0.5)
                    .clamp(0.0, (bh - 1) as f32);
                let (left, top) = (sx as u32, sy as u32);
                for channel in 0..3 {
                    let at = |x: u32, y: u32| {
                        f32::from(blur.pixels()[((y * bw + x) * 4) as usize + channel])
                    };
                    let horizontal = |y| {
                        at(left, y) * (1.0 - sx.fract())
                            + at((left + 1).min(bw - 1), y) * sx.fract()
                    };
                    pixels.push(
                        (horizontal(top) * (1.0 - sy.fract())
                            + horizontal((top + 1).min(bh - 1)) * sy.fract())
                        .round() as u8,
                    );
                }
                let dx = px - px.clamp(radius, self.bounds.width - radius);
                let dy = py - py.clamp(radius, self.bounds.height - radius);
                let alpha = ((radius - dx.hypot(dy)) * 2.0 + 0.5).clamp(0.0, 1.0);
                pixels.push((alpha * 255.0).round() as u8);
            }
        }
        (width, height, pixels)
    }
}

impl<M> Widget<M, iced::Theme, iced::Renderer> for Backdrop<'_, M> {
    fn tag(&self) -> iced::advanced::widget::tree::Tag {
        self.content.as_widget().tag()
    }
    fn state(&self) -> iced::advanced::widget::tree::State {
        self.content.as_widget().state()
    }
    fn children(&self) -> Vec<Tree> {
        self.content.as_widget().children()
    }
    fn diff(&self, tree: &mut Tree) {
        self.content.as_widget().diff(tree);
    }
    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }
    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &iced::advanced::layout::Limits,
    ) -> iced::advanced::layout::Node {
        self.content.as_widget_mut().layout(tree, renderer, limits)
    }
    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn iced::advanced::widget::Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(tree, layout, renderer, operation);
    }
    fn update(
        &mut self,
        tree: &mut Tree,
        event: &iced::Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
        viewport: &Rectangle,
    ) {
        self.content.as_widget_mut().update(
            tree, event, layout, cursor, renderer, clipboard, shell, viewport,
        );
    }
    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        theme: &iced::Theme,
        style: &iced::advanced::renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let sample = Sample {
            bounds,
            window: self.window,
            aspect: self.aspect,
            radius: self.radius,
            offset_x: self.offset_x,
        };
        renderer.draw_image(image::Image::new(sample.image(&self.blur)), bounds, bounds);
        // iced batches quads before images within a layer. Keep child fills,
        // borders, and focus rings above the backdrop, not behind its image.
        iced::advanced::Renderer::with_layer(renderer, *viewport, |renderer| {
            self.content
                .as_widget()
                .draw(tree, renderer, theme, style, layout, cursor, viewport);
        });
    }
    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        self.content
            .as_widget()
            .mouse_interaction(tree, layout, cursor, viewport, renderer)
    }
    fn overlay<'a>(
        &'a mut self,
        tree: &'a mut Tree,
        layout: Layout<'a>,
        renderer: &iced::Renderer,
        viewport: &Rectangle,
        translation: iced::Vector,
    ) -> Option<iced::advanced::overlay::Element<'a, M, iced::Theme, iced::Renderer>> {
        self.content
            .as_widget_mut()
            .overlay(tree, layout, renderer, viewport, translation)
    }
}
