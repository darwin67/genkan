use iced::advanced::widget::{self, operation};
use iced::event;
use iced::keyboard;
use iced::widget::text_input;
use iced::{window, Task};

use crate::power::Action as PowerAction;

use super::auth_flow::Phase;
use super::{App, Message, PowerState};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Target {
    Account(usize),
    RetryAccountSelection,
    AuthenticationInput,
    Submit,
    RetryAuthentication,
    ChangeUser,
    Session,
    RetrySession,
    Power(PowerAction),
    DialogCancel,
    DialogConfirm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Navigation {
    Next,
    Previous,
    DirectionNext,
    DirectionPrevious,
    Activate,
}

pub(super) fn keyboard_navigation(
    event: iced::Event,
    status: event::Status,
    _window: window::Id,
) -> Option<Message> {
    let iced::Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) = event else {
        return None;
    };
    let navigation = navigation_key(key, modifiers, event::Status::Ignored)?;
    Some(if status == event::Status::Captured {
        Message::NavigateModal(navigation)
    } else {
        Message::NavigateFocus(navigation)
    })
}

pub(super) fn pointer_focus_sync(
    event: iced::Event,
    _status: event::Status,
    _window: window::Id,
) -> Option<Message> {
    matches!(
        event,
        iced::Event::Mouse(iced::mouse::Event::ButtonPressed(_))
            | iced::Event::Touch(iced::touch::Event::FingerPressed { .. })
    )
    .then_some(Message::SyncWidgetFocus)
}

pub(super) fn navigation_key(
    key: keyboard::Key,
    modifiers: keyboard::Modifiers,
    status: event::Status,
) -> Option<Navigation> {
    if status == event::Status::Captured {
        return None;
    }
    use keyboard::key::Named;

    match key.as_ref() {
        keyboard::Key::Named(Named::Tab) if modifiers.shift() => Navigation::Previous,
        keyboard::Key::Named(Named::Tab) => Navigation::Next,
        keyboard::Key::Named(Named::ArrowRight | Named::ArrowDown) => Navigation::DirectionNext,
        keyboard::Key::Named(Named::ArrowLeft | Named::ArrowUp) => Navigation::DirectionPrevious,
        keyboard::Key::Named(Named::Enter | Named::Space) => Navigation::Activate,
        _ => return None,
    }
    .into()
}

impl App {
    pub(super) fn focus_order(&self) -> Vec<Target> {
        if matches!(self.power_state, PowerState::Confirming(_)) {
            return vec![Target::DialogCancel, Target::DialogConfirm];
        }
        if matches!(self.power_state, PowerState::Executing(_)) || self.closing.is_some() {
            return Vec::new();
        }

        let mut order = Vec::new();
        match self.phase {
            Phase::SelectingUser if self.can_select_account() => {
                order.extend((0..self.accounts.len()).map(Target::Account));
            }
            Phase::UserSelectionCancellationFailed => order.push(Target::RetryAccountSelection),
            Phase::WaitingForInput => {
                order.extend([Target::AuthenticationInput, Target::Submit]);
            }
            Phase::Failed if self.selected_session.is_some() && !self.username.is_empty() => {
                order.push(Target::RetryAuthentication);
            }
            Phase::Failed if self.selected_session.is_some() => {
                order.push(Target::RetryAccountSelection);
            }
            _ => {}
        }

        if self.can_change_user() {
            order.push(Target::ChangeUser);
        }
        if self.can_select_session() && self.selected_session.is_some() {
            order.push(Target::Session);
        }
        if self.phase == Phase::Failed && self.selected_session.is_none() {
            order.push(Target::RetrySession);
        }
        if self.can_request_power() {
            order.extend([
                Target::Power(PowerAction::Suspend),
                Target::Power(PowerAction::Reboot),
                Target::Power(PowerAction::PowerOff),
            ]);
        }
        order
    }

    pub(super) fn navigate_focus(&mut self, navigation: Navigation) -> Task<Message> {
        match navigation {
            Navigation::Next => self.move_focus(1),
            Navigation::Previous => self.move_focus(-1),
            Navigation::DirectionNext => self.move_directional(1),
            Navigation::DirectionPrevious => self.move_directional(-1),
            Navigation::Activate => self.activate_focus(),
        }
    }

    pub(super) fn focus_first(&mut self) -> Task<Message> {
        match self.focus_order().first().copied() {
            Some(target) => self.set_focus(target),
            None => {
                self.close_session_menu();
                self.focus_target = None;
                clear_widget_focus()
            }
        }
    }

    pub(super) fn sync_widget_focus(&mut self) -> Task<Message> {
        if self.focus_target == Some(Target::AuthenticationInput) {
            self.focus_target = None;
        }
        widget::operate(operation::focusable::find_focused()).map(Message::WidgetFocused)
    }

    pub(super) fn apply_widget_focus(&mut self, focused: widget::Id) -> Task<Message> {
        let input: widget::Id = self.input_id.clone().into();
        if focused == input && self.phase == Phase::WaitingForInput {
            self.focus_target = Some(Target::AuthenticationInput);
        }
        Task::none()
    }

    fn move_focus(&mut self, delta: isize) -> Task<Message> {
        let order = self.focus_order();
        if order.is_empty() {
            self.close_session_menu();
            self.focus_target = None;
            return clear_widget_focus();
        }
        let index = self
            .focus_target
            .and_then(|target| order.iter().position(|candidate| *candidate == target))
            .map(|index| (index as isize + delta).rem_euclid(order.len() as isize) as usize)
            .unwrap_or_else(|| if delta < 0 { order.len() - 1 } else { 0 });
        self.set_focus(order[index])
    }

    fn move_directional(&mut self, delta: isize) -> Task<Message> {
        match self.focus_target {
            Some(Target::Account(index))
                if self.can_select_account() && !self.accounts.is_empty() =>
            {
                let next =
                    (index as isize + delta).rem_euclid(self.accounts.len() as isize) as usize;
                self.set_focus(Target::Account(next))
            }
            Some(Target::Session) if self.can_select_session() => self.cycle_session(delta),
            Some(Target::DialogCancel | Target::DialogConfirm) => self.move_focus(delta),
            _ => Task::none(),
        }
    }

    fn activate_focus(&mut self) -> Task<Message> {
        match self.focus_target {
            Some(Target::Account(index)) if self.can_select_account() => {
                let Some(account) = self.accounts.get(index).cloned() else {
                    return Task::none();
                };
                self.update(Message::SelectAccount(account))
            }
            Some(Target::Account(_)) => Task::none(),
            Some(Target::RetryAccountSelection)
                if self.phase == Phase::UserSelectionCancellationFailed =>
            {
                self.update(Message::RetryUserSelectionCancellation)
            }
            Some(Target::RetryAccountSelection) => self.update(Message::Retry),
            Some(Target::AuthenticationInput) => Task::none(),
            Some(Target::Submit) => self.update(Message::Submit),
            Some(Target::RetryAuthentication) => self.update(Message::Retry),
            Some(Target::ChangeUser) => self.update(Message::ChangeUser),
            Some(Target::Session) if self.can_select_session() => self.cycle_session(1),
            Some(Target::Session) => Task::none(),
            Some(Target::RetrySession) => self.update(Message::RetrySession),
            Some(Target::Power(action)) => self.update(Message::AskPower(action)),
            Some(Target::DialogCancel) => self.update(Message::CancelPower),
            Some(Target::DialogConfirm) => {
                let PowerState::Confirming(action) = self.power_state else {
                    return Task::none();
                };
                self.update(Message::ConfirmPower(action))
            }
            None => Task::none(),
        }
    }

    fn cycle_session(&mut self, delta: isize) -> Task<Message> {
        if self.sessions.is_empty() {
            return Task::none();
        }
        let current = self
            .selected_session
            .as_ref()
            .and_then(|selected| self.sessions.iter().position(|session| session == selected))
            .unwrap_or(0);
        let next = (current as isize + delta).rem_euclid(self.sessions.len() as isize) as usize;
        self.selected_session = Some(self.sessions[next].clone());
        Task::none()
    }

    pub(super) fn set_focus(&mut self, target: Target) -> Task<Message> {
        if target != Target::Session {
            self.close_session_menu();
        }
        self.focus_target = Some(target);
        match target {
            Target::AuthenticationInput => Task::batch([
                text_input::focus(self.input_id.clone()),
                account_tile_reveal_input(self.page_scroll_id.clone()),
            ]),
            Target::Account(index) => self.reveal_account(index),
            _ => clear_widget_focus(),
        }
    }

    pub(super) fn focus_input(&mut self) -> Task<Message> {
        self.set_focus(Target::AuthenticationInput)
    }

    pub(super) fn blur_input(&mut self) -> Task<Message> {
        self.close_session_menu();
        if self.focus_target == Some(Target::AuthenticationInput) {
            self.focus_target = None;
        }
        clear_widget_focus()
    }

    pub(super) fn enter_power_dialog(&mut self) -> Task<Message> {
        self.focus_before_modal = self.focus_target;
        self.set_focus(Target::DialogCancel)
    }

    pub(super) fn leave_power_dialog(&mut self) -> Task<Message> {
        let order = self.focus_order();
        let target = self
            .focus_before_modal
            .take()
            .filter(|target| order.contains(target))
            .or_else(|| order.first().copied());
        match target {
            Some(target) => self.set_focus(target),
            None => {
                self.close_session_menu();
                self.focus_target = None;
                clear_widget_focus()
            }
        }
    }

    pub(super) fn focused_account(&self) -> Option<usize> {
        match self.focus_target {
            Some(Target::Account(index)) => Some(index),
            _ => None,
        }
    }

    pub(super) fn is_focused(&self, target: Target) -> bool {
        self.focus_target == Some(target)
    }

    fn reveal_account(&self, index: usize) -> Task<Message> {
        let Some(account) = self.accounts.get(index) else {
            return Task::none();
        };
        super::account_tile::reveal(
            super::account_tile::id(&account.username),
            vec![self.account_scroll_id.clone(), self.page_scroll_id.clone()],
        )
    }
}

fn account_tile_reveal_input(page: iced::widget::scrollable::Id) -> Task<Message> {
    super::account_tile::reveal(widget::Id::new("authentication-input-anchor"), vec![page])
}

fn clear_widget_focus() -> Task<Message> {
    widget::operate(operation::focusable::focus::<Message>(widget::Id::unique())).discard()
}
