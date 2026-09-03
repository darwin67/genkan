use iced::advanced::layout;
use iced::advanced::renderer::{self, Renderer as _};
use iced::advanced::widget::{self, operation, tree, Tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget};
use iced::widget::button;
use iced::{
    mouse, touch, Background, Color, Element, Length, Padding, Point, Rectangle, Size, Task, Vector,
};

use crate::theme;

const DRAG_THRESHOLD: f32 = 6.0;
const REVEAL_MARGIN: f32 = 16.0;

pub(super) fn tile<'a, Message: Clone + 'a>(
    content: impl Into<Element<'a, Message>>,
    on_press: Option<Message>,
    focused: bool,
    width: f32,
    id: widget::Id,
) -> Element<'a, Message> {
    Element::new(AccountTile {
        content: content.into(),
        on_press,
        focused,
        width: Length::Fixed(width),
        padding: Padding::from([12, 10]),
        id,
    })
}

struct AccountTile<'a, Message> {
    content: Element<'a, Message>,
    on_press: Option<Message>,
    focused: bool,
    width: Length,
    padding: Padding,
    id: widget::Id,
}

pub(super) fn id(username: &str) -> widget::Id {
    format!("account-{username}").into()
}

pub(super) fn reveal<Message: Send + 'static>(
    account: widget::Id,
    scrollables: Vec<widget::Id>,
) -> Task<Message> {
    iced::advanced::widget::operate(RevealAccount::find(account, scrollables)).discard()
}

#[derive(Debug, Default)]
struct State {
    mouse_pressed: bool,
    touch_finger: Option<touch::Finger>,
    touch_start: Option<Point>,
    touch_dragged: bool,
}

impl State {
    fn press_mouse(&mut self) {
        self.mouse_pressed = true;
    }

    fn release_mouse(&mut self, over_tile: bool) -> bool {
        let activate = self.mouse_pressed && over_tile;
        self.mouse_pressed = false;
        activate
    }

    fn start_touch(&mut self, finger: touch::Finger, position: Point) {
        self.touch_finger = Some(finger);
        self.touch_start = Some(position);
        self.touch_dragged = false;
    }

    fn move_touch(&mut self, finger: touch::Finger, position: Point) {
        if self.touch_finger != Some(finger) {
            return;
        }
        if let Some(start) = self.touch_start {
            let delta = position - start;
            self.touch_dragged |=
                delta.x * delta.x + delta.y * delta.y > DRAG_THRESHOLD * DRAG_THRESHOLD;
        }
    }

    fn finish_touch(&mut self, finger: touch::Finger) -> bool {
        if self.touch_finger != Some(finger) {
            return false;
        }
        self.touch_finger = None;
        let activate = self.touch_start.take().is_some() && !self.touch_dragged;
        self.touch_dragged = false;
        activate
    }
}

impl<Message> Widget<Message, iced::Theme, iced::Renderer> for AccountTile<'_, Message>
where
    Message: Clone,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn size(&self) -> Size<Length> {
        Size::new(self.width, Length::Shrink)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        layout::padded(limits, self.width, Length::Shrink, self.padding, |limits| {
            self.content
                .as_widget_mut()
                .layout(&mut tree.children[0], renderer, limits)
        })
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn iced::advanced::widget::Operation,
    ) {
        operation.container(Some(&self.id), layout.bounds());
        operation.traverse(&mut |operation| {
            self.content.as_widget_mut().operate(
                &mut tree.children[0],
                layout.children().next().expect("account tile content"),
                renderer,
                operation,
            );
        });
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &iced::Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout.children().next().expect("account tile content"),
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
        if shell.is_event_captured() {
            return;
        }

        let state = tree.state.downcast_mut::<State>();
        let bounds = layout.bounds();
        let capture = match event {
            iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
                if self.on_press.is_some() && cursor.is_over(bounds) =>
            {
                state.press_mouse();
                true
            }
            iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
                if state.mouse_pressed =>
            {
                if state.release_mouse(cursor.is_over(bounds)) {
                    if let Some(message) = self.on_press.clone() {
                        shell.publish(message);
                    }
                }
                true
            }
            iced::Event::Touch(touch::Event::FingerPressed { id, position })
                if self.on_press.is_some() && cursor.is_over(bounds) =>
            {
                state.start_touch(*id, *position);
                false
            }
            iced::Event::Touch(touch::Event::FingerMoved { id, position }) => {
                state.move_touch(*id, *position);
                false
            }
            iced::Event::Touch(touch::Event::FingerLifted { id, .. })
                if state.touch_finger == Some(*id) =>
            {
                let activate = state.finish_touch(*id) && cursor.is_over(bounds);
                if activate {
                    if let Some(message) = self.on_press.clone() {
                        shell.publish(message);
                    }
                }
                // Let an enclosing scrollable observe every touch release so it
                // can always finish its own tap-or-drag gesture bookkeeping.
                false
            }
            iced::Event::Touch(touch::Event::FingerLost { id, .. })
                if state.touch_finger == Some(*id) =>
            {
                state.touch_finger = None;
                state.touch_start = None;
                state.touch_dragged = false;
                state.mouse_pressed = false;
                false
            }
            _ => false,
        };
        if capture {
            shell.capture_event();
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        theme: &iced::Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let state = tree.state.downcast_ref::<State>();
        let status = if self.on_press.is_none() {
            button::Status::Disabled
        } else if cursor.is_over(bounds) {
            if state.mouse_pressed {
                button::Status::Pressed
            } else {
                button::Status::Hovered
            }
        } else {
            button::Status::Active
        };
        let style = theme::account_tile(theme, status, self.focused);
        renderer.fill_quad(
            renderer::Quad {
                bounds,
                border: style.border,
                shadow: style.shadow,
                snap: true,
            },
            style
                .background
                .unwrap_or(Background::Color(Color::TRANSPARENT)),
        );
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            &renderer::Style {
                text_color: style.text_color,
            },
            layout.children().next().expect("account tile content"),
            cursor,
            viewport,
        );
    }

    fn mouse_interaction(
        &self,
        _tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        if self.on_press.is_some() && cursor.is_over(layout.bounds()) {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::default()
        }
    }
}

struct RevealAccount {
    account: widget::Id,
    scrollables: Vec<widget::Id>,
    bounds: Option<Rectangle>,
}

impl RevealAccount {
    fn find(account: widget::Id, scrollables: Vec<widget::Id>) -> Self {
        Self {
            account,
            scrollables,
            bounds: None,
        }
    }
}

impl operation::Operation for RevealAccount {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn operation::Operation)) {
        if self.bounds.is_none() {
            operate(self);
        }
    }

    fn container(&mut self, id: Option<&widget::Id>, bounds: Rectangle) {
        if id == Some(&self.account) {
            self.bounds = Some(bounds);
        }
    }

    fn finish(&self) -> operation::Outcome<()> {
        self.bounds.map_or(operation::Outcome::None, |target| {
            operation::Outcome::Chain(Box::new(RevealInScrollables {
                scrollables: self.scrollables.clone(),
                target,
            }))
        })
    }
}

struct RevealInScrollables {
    scrollables: Vec<widget::Id>,
    target: Rectangle,
}

impl operation::Operation for RevealInScrollables {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn operation::Operation)) {
        operate(self);
    }

    fn scrollable(
        &mut self,
        id: Option<&widget::Id>,
        bounds: Rectangle,
        content_bounds: Rectangle,
        translation: Vector,
        state: &mut dyn operation::Scrollable,
    ) {
        if id.is_some_and(|id| self.scrollables.contains(id)) {
            if let Some(offset) = reveal_offset(bounds, content_bounds, translation, self.target) {
                state.scroll_to(offset.into());
            }
        }
    }
}

fn reveal_offset(
    viewport: Rectangle,
    content: Rectangle,
    translation: Vector,
    target: Rectangle,
) -> Option<operation::scrollable::AbsoluteOffset> {
    let visible_top = viewport.y + translation.y;
    let visible_bottom = visible_top + viewport.height;
    let padded_top = target.y - REVEAL_MARGIN;
    let padded_bottom = target.y + target.height + REVEAL_MARGIN;
    let y = if target.height + 2.0 * REVEAL_MARGIN > viewport.height {
        target.y - content.y
    } else if padded_top < visible_top {
        padded_top - content.y
    } else if padded_bottom > visible_bottom {
        padded_bottom - content.y - viewport.height
    } else {
        return None;
    };
    Some(operation::scrollable::AbsoluteOffset {
        x: translation.x,
        y: y.max(0.0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tap_activates_but_drag_does_not() {
        let mut state = State::default();
        let finger = touch::Finger(1);
        state.start_touch(finger, Point::new(10.0, 10.0));
        state.move_touch(finger, Point::new(13.0, 13.0));
        assert!(state.finish_touch(finger));

        state.start_touch(finger, Point::new(10.0, 10.0));
        state.move_touch(finger, Point::new(10.0, 30.0));
        assert!(!state.finish_touch(finger));
    }

    #[test]
    fn pointer_click_activates_only_when_released_over_the_tile() {
        let mut state = State::default();
        state.press_mouse();
        assert!(state.release_mouse(true));
        assert!(!state.mouse_pressed);

        state.press_mouse();
        assert!(!state.release_mouse(false));
        assert!(!state.mouse_pressed);
        assert!(!state.release_mouse(true));
    }

    #[test]
    fn reveal_offset_uses_target_geometry() {
        let viewport = Rectangle::new(Point::ORIGIN, Size::new(400.0, 200.0));
        let content = Rectangle::new(Point::ORIGIN, Size::new(400.0, 800.0));

        assert_eq!(
            reveal_offset(
                viewport,
                content,
                Vector::new(0.0, 100.0),
                Rectangle::new(Point::new(0.0, 350.0), Size::new(100.0, 80.0)),
            ),
            Some(operation::scrollable::AbsoluteOffset { x: 0.0, y: 246.0 })
        );
        assert_eq!(
            reveal_offset(
                viewport,
                content,
                Vector::new(0.0, 300.0),
                Rectangle::new(Point::new(0.0, 120.0), Size::new(100.0, 80.0)),
            ),
            Some(operation::scrollable::AbsoluteOffset { x: 0.0, y: 104.0 })
        );
        assert_eq!(
            reveal_offset(
                viewport,
                content,
                Vector::new(0.0, 100.0),
                Rectangle::new(Point::new(0.0, 350.0), Size::new(100.0, 300.0)),
            ),
            Some(operation::scrollable::AbsoluteOffset { x: 0.0, y: 350.0 })
        );
    }
}
