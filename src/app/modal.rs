use iced::advanced::widget::{Tree, Widget};
use iced::advanced::{Clipboard, Layout, Shell};
use iced::{event, mouse, touch, Element, Length, Rectangle, Size};

pub(super) fn barrier<'a, Message, Theme, Renderer>(
    content: impl Into<Element<'a, Message, Theme, Renderer>>,
) -> Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Theme: 'a,
    Renderer: iced::advanced::Renderer + 'a,
{
    Element::new(ModalBarrier {
        content: content.into(),
    })
}

struct ModalBarrier<'a, Message, Theme, Renderer> {
    content: Element<'a, Message, Theme, Renderer>,
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for ModalBarrier<'_, Message, Theme, Renderer>
where
    Renderer: iced::advanced::Renderer,
{
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

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }

    fn layout(
        &self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &iced::advanced::layout::Limits,
    ) -> iced::advanced::layout::Node {
        self.content.as_widget().layout(tree, renderer, limits)
    }

    fn operate(
        &self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn iced::advanced::widget::Operation,
    ) {
        self.content
            .as_widget()
            .operate(tree, layout, renderer, operation);
    }

    fn on_event(
        &mut self,
        tree: &mut Tree,
        event: iced::Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) -> event::Status {
        let content_status = self.content.as_widget_mut().on_event(
            tree,
            event.clone(),
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
        if content_status == event::Status::Captured || blocks_background(&event) {
            event::Status::Captured
        } else {
            event::Status::Ignored
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &iced::advanced::renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.content
            .as_widget()
            .draw(tree, renderer, theme, style, layout, cursor, viewport);
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.content
            .as_widget()
            .mouse_interaction(tree, layout, cursor, viewport, renderer)
    }

    fn overlay<'a>(
        &'a mut self,
        tree: &'a mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        translation: iced::Vector,
    ) -> Option<iced::advanced::overlay::Element<'a, Message, Theme, Renderer>> {
        self.content
            .as_widget_mut()
            .overlay(tree, layout, renderer, translation)
    }
}

fn blocks_background(event: &iced::Event) -> bool {
    matches!(
        event,
        iced::Event::Keyboard(_)
            | iced::Event::Mouse(_)
            | iced::Event::Touch(touch::Event::FingerPressed { .. })
            | iced::Event::Touch(touch::Event::FingerMoved { .. })
            | iced::Event::Touch(touch::Event::FingerLifted { .. })
            | iced::Event::Touch(touch::Event::FingerLost { .. })
    )
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::*;

    struct TestRenderer;

    impl iced::advanced::Renderer for TestRenderer {
        fn start_layer(&mut self, _bounds: Rectangle) {}

        fn end_layer(&mut self) {}

        fn start_transformation(&mut self, _transformation: iced::Transformation) {}

        fn end_transformation(&mut self) {}

        fn fill_quad(
            &mut self,
            _quad: iced::advanced::renderer::Quad,
            _background: impl Into<iced::Background>,
        ) {
        }

        fn clear(&mut self) {}
    }

    struct Spy {
        events: Rc<Cell<usize>>,
        status: event::Status,
    }

    impl Widget<(), (), TestRenderer> for Spy {
        fn size(&self) -> Size<Length> {
            Size::new(Length::Fixed(100.0), Length::Fixed(100.0))
        }

        fn layout(
            &self,
            _tree: &mut Tree,
            _renderer: &TestRenderer,
            _limits: &iced::advanced::layout::Limits,
        ) -> iced::advanced::layout::Node {
            iced::advanced::layout::Node::new(Size::new(100.0, 100.0))
        }

        fn draw(
            &self,
            _tree: &Tree,
            _renderer: &mut TestRenderer,
            _theme: &(),
            _style: &iced::advanced::renderer::Style,
            _layout: Layout<'_>,
            _cursor: mouse::Cursor,
            _viewport: &Rectangle,
        ) {
        }

        fn on_event(
            &mut self,
            _tree: &mut Tree,
            _event: iced::Event,
            _layout: Layout<'_>,
            _cursor: mouse::Cursor,
            _renderer: &TestRenderer,
            _clipboard: &mut dyn Clipboard,
            _shell: &mut Shell<'_, ()>,
            _viewport: &Rectangle,
        ) -> event::Status {
            self.events.set(self.events.get() + 1);
            self.status
        }
    }

    fn dispatch(child_status: event::Status, event: iced::Event) -> (event::Status, usize) {
        let events = Rc::new(Cell::new(0));
        let mut barrier = ModalBarrier {
            content: Element::new(Spy {
                events: events.clone(),
                status: child_status,
            }),
        };
        let mut tree = Tree::new(&barrier as &dyn Widget<(), (), TestRenderer>);
        let node = iced::advanced::layout::Node::new(Size::new(100.0, 100.0));
        let layout = Layout::new(&node);
        let mut clipboard = iced::advanced::clipboard::Null;
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        let status = barrier.on_event(
            &mut tree,
            event,
            layout,
            mouse::Cursor::Available(iced::Point::ORIGIN),
            &TestRenderer,
            &mut clipboard,
            &mut shell,
            &Rectangle::new(iced::Point::ORIGIN, Size::new(100.0, 100.0)),
        );
        (status, events.get())
    }

    #[test]
    fn modal_barrier_blocks_background_pointer_scroll_and_touch_events() {
        let wheel = iced::Event::Mouse(mouse::Event::WheelScrolled {
            delta: mouse::ScrollDelta::Lines { x: 0.0, y: 1.0 },
        });
        assert_eq!(
            dispatch(event::Status::Ignored, wheel),
            (event::Status::Captured, 1)
        );
        assert_eq!(
            dispatch(
                event::Status::Captured,
                iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
            ),
            (event::Status::Captured, 1)
        );
        assert!(blocks_background(&iced::Event::Mouse(
            mouse::Event::ButtonPressed(mouse::Button::Left)
        )));
        assert!(blocks_background(&iced::Event::Touch(
            touch::Event::FingerPressed {
                id: touch::Finger(1),
                position: iced::Point::ORIGIN,
            }
        )));
        assert!(!blocks_background(&iced::Event::Window(
            iced::window::Event::CloseRequested
        )));
    }

    #[test]
    fn modal_barrier_captures_keyboard_events_for_modal_navigation() {
        let tab = iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
            key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Tab),
            modified_key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Tab),
            physical_key: iced::keyboard::key::Physical::Code(iced::keyboard::key::Code::Tab),
            location: iced::keyboard::Location::Standard,
            modifiers: iced::keyboard::Modifiers::empty(),
            text: None,
        });

        assert_eq!(
            dispatch(event::Status::Ignored, tab),
            (event::Status::Captured, 1)
        );
    }
}
