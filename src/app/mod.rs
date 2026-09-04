mod account_tile;
mod auth_flow;
mod focus;
mod modal;
mod preview;
mod resettable;
mod view;

use std::time::{Duration, Instant};

use auth_flow::Phase;
use chrono::Local;
use focus::{Navigation as FocusNavigation, Target as FocusTarget};
use greetd_ipc::Request;
use iced::widget::{operation, scrollable, Id};
use iced::{event, keyboard, time, window, Subscription, Task};

use crate::accounts::{self, Account};
use crate::conversation::{self, Attempt, Conversation, Status as ConversationStatus};
use crate::power::{self, Action as PowerAction};
use crate::sessions::{self, Session};
use crate::wallpaper;
use genkan::auth::{self, Client};

pub(crate) use preview::Fixture as PreviewFixture;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Closing {
    WaitingForClient(window::Id),
    WaitingForUserSelectionCancellation(window::Id),
    Cancelling(window::Id),
    Dispatching(window::Id),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PowerState {
    Idle,
    Confirming(PowerAction),
    Executing(PowerAction),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PageNavigation {
    Up,
    Down,
    Start,
    End,
}

impl Closing {
    fn is_cleaning(self, window: window::Id) -> bool {
        matches!(
            self,
            Self::WaitingForClient(id)
                | Self::WaitingForUserSelectionCancellation(id)
                | Self::Cancelling(id)
                if id == window
        )
    }
}

#[derive(Clone)]
pub(crate) struct Config {
    pub(crate) username: Option<String>,
    pub(crate) display_name: Option<String>,
    pub(crate) preview: Option<PreviewFixture>,
    pub(crate) wallpaper: wallpaper::Settings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartupMode {
    ConfiguredIdentity,
    DiscoverAccounts,
    MissingSession,
}

#[derive(Debug)]
pub(crate) struct App {
    username: String,
    display_name: String,
    accounts: Vec<Account>,
    focus_target: Option<FocusTarget>,
    focus_before_modal: Option<FocusTarget>,
    account_scroll_id: Id,
    page_scroll_id: Id,
    input_id: Id,
    conversation: Conversation,
    session_message: Option<String>,
    power_message: Option<String>,
    power_message_is_error: bool,
    preview_message: Option<String>,
    phase: Phase,
    client: Option<Client>,
    sessions: Vec<Session>,
    selected_session: Option<Session>,
    session_menu_open: bool,
    session_selector_key: u64,
    wallpaper: wallpaper::State,
    started_at: Instant,
    now: chrono::DateTime<Local>,
    power_state: PowerState,
    selection_session_cancelled: bool,
    closing: Option<Closing>,
    preview: bool,
}

#[derive(Debug, Clone)]
pub(crate) enum Message {
    Tick,
    WallpaperFrameReady,
    WallpaperAllocated(Result<iced_runtime::image::Allocation, iced_runtime::image::Error>),
    InputChanged(String),
    Submit,
    Retry,
    RetrySession,
    AuthResult {
        attempt: Attempt,
        result: Result<(Option<Client>, auth::Response), String>,
    },
    AccountsResult(Result<Vec<Account>, String>),
    ChangeUser,
    UserSelectionCancelled {
        attempt: Attempt,
        result: Result<(), String>,
    },
    UserSelectionCancellationSlow {
        attempt: Attempt,
    },
    RetryUserSelectionCancellation,
    SelectAccount(Account),
    NavigateFocus(FocusNavigation),
    NavigateModal(FocusNavigation),
    SyncWidgetFocus,
    WidgetFocused(iced::advanced::widget::Id),
    NavigatePage(PageNavigation),
    SelectSession(Session),
    SessionMenuOpened,
    SessionMenuClosed,
    AskPower(PowerAction),
    CancelPower,
    ConfirmPower(PowerAction),
    PowerResult(Result<(), String>),
    Escape,
    CloseRequested(window::Id),
    SessionCancelled(window::Id),
    CloseTimeout(window::Id),
}

impl App {
    pub(crate) fn new(config: Config) -> (Self, Task<Message>) {
        if let Some(fixture) = config.preview {
            return preview::build(
                fixture,
                config.username,
                config.display_name,
                config.wallpaper,
            );
        }
        let sessions = sessions::discover();
        let selected_session = sessions.first().cloned();
        let account = config
            .username
            .map(|username| Account::override_account(username, config.display_name));
        let accounts = account.iter().cloned().collect();
        let startup = startup_mode(selected_session.is_some(), account.is_some());
        let conversation = Conversation::new();
        let attempt = conversation.attempt();

        let mut app = Self {
            username: account
                .as_ref()
                .map(|account| account.username.clone())
                .unwrap_or_default(),
            display_name: account
                .as_ref()
                .map(|account| account.display_name.clone())
                .unwrap_or_else(|| "Select a user".into()),
            accounts,
            focus_target: None,
            focus_before_modal: None,
            account_scroll_id: Id::unique(),
            page_scroll_id: Id::unique(),
            input_id: Id::new("authentication-input"),
            conversation,
            session_message: selected_session
                .is_none()
                .then(|| "No valid Wayland sessions are installed".into()),
            power_message: None,
            power_message_is_error: false,
            preview_message: None,
            phase: match startup {
                StartupMode::ConfiguredIdentity => Phase::CreatingSession,
                StartupMode::DiscoverAccounts => Phase::DiscoveringUsers,
                StartupMode::MissingSession => Phase::Failed,
            },
            client: None,
            sessions,
            selected_session,
            session_menu_open: false,
            session_selector_key: 0,
            wallpaper: wallpaper::State::start(config.wallpaper),
            started_at: Instant::now(),
            now: Local::now(),
            power_state: PowerState::Idle,
            selection_session_cancelled: false,
            closing: None,
            preview: false,
        };
        let task = match startup {
            StartupMode::ConfiguredIdentity => {
                auth_flow::begin(app.username.clone(), attempt, true)
            }
            StartupMode::DiscoverAccounts => discover_accounts(),
            StartupMode::MissingSession => app.focus_first(),
        };
        (app, task)
    }

    pub(crate) fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            time::every(self.tick_interval()).map(|_| Message::Tick),
            self.wallpaper
                .subscription()
                .map(|()| Message::WallpaperFrameReady),
            window::close_requests().map(Message::CloseRequested),
            event::listen_with(focus::keyboard_navigation),
            event::listen_with(focus::pointer_focus_sync),
            event::listen_with(page_navigation),
            event::listen_with(cancel_shortcut),
        ])
    }

    pub(crate) fn update(&mut self, message: Message) -> Task<Message> {
        if let Some(closing) = self.closing {
            let allowed = match closing {
                Closing::WaitingForClient(_) => matches!(
                    &message,
                    Message::AuthResult { .. }
                        | Message::SessionCancelled(_)
                        | Message::CloseTimeout(_)
                ),
                Closing::WaitingForUserSelectionCancellation(_) => matches!(
                    &message,
                    Message::UserSelectionCancelled { .. } | Message::CloseTimeout(_)
                ),
                Closing::Cancelling(_) => {
                    matches!(
                        &message,
                        Message::SessionCancelled(_) | Message::CloseTimeout(_)
                    )
                }
                Closing::Dispatching(_) => false,
            };
            if !allowed {
                return Task::none();
            }
        }

        if self.power_state != PowerState::Idle {
            let allowed = match (self.power_state, &message) {
                (
                    _,
                    Message::Tick
                    | Message::WallpaperFrameReady
                    | Message::WallpaperAllocated(_)
                    | Message::AuthResult { .. }
                    | Message::AccountsResult(_)
                    | Message::CloseRequested(_)
                    | Message::SessionCancelled(_)
                    | Message::CloseTimeout(_),
                ) => true,
                (PowerState::Confirming(_), Message::CancelPower) => true,
                (PowerState::Confirming(_), Message::NavigateModal(_) | Message::Escape) => true,
                (PowerState::Confirming(expected), Message::ConfirmPower(actual)) => {
                    expected == *actual
                }
                (PowerState::Executing(_), Message::PowerResult(_)) => true,
                _ => false,
            };
            if !allowed {
                return Task::none();
            }
        }

        match message {
            Message::WallpaperFrameReady => self.prepare_wallpaper_frame(),
            Message::WallpaperAllocated(result) => {
                self.wallpaper.finish_allocation(result);
                self.prepare_wallpaper_frame()
            }
            Message::Tick if self.preview => Task::none(),
            Message::Tick => {
                self.now = Local::now();
                Task::none()
            }
            Message::InputChanged(value) if self.conversation.update_input(&value) => {
                self.focus_target = Some(FocusTarget::AuthenticationInput);
                Task::none()
            }
            Message::InputChanged(_) => Task::none(),
            Message::AccountsResult(Ok(accounts)) if accounts.is_empty() => {
                self.fail("AccountsService found no unlocked non-system users".into())
            }
            Message::AccountsResult(Ok(accounts)) => {
                self.accounts = accounts;
                if let Some(account) = accounts::preferred_account(&self.accounts).cloned() {
                    self.select_account(account)
                } else {
                    self.phase = Phase::SelectingUser;
                    self.conversation.set_notice("Select a user".into(), false);
                    self.set_focus(FocusTarget::Account(0))
                }
            }
            Message::AccountsResult(Err(error)) => self.fail(error),
            Message::ChangeUser if self.can_change_user() => self.change_user(),
            Message::ChangeUser => Task::none(),
            Message::UserSelectionCancelled { attempt, .. }
                if self.conversation.accepts(attempt)
                    && matches!(
                        self.closing,
                        Some(Closing::WaitingForUserSelectionCancellation(_))
                    ) =>
            {
                let Some(Closing::WaitingForUserSelectionCancellation(window)) = self.closing
                else {
                    unreachable!();
                };
                self.closing = Some(Closing::Dispatching(window));
                window::close(window)
            }
            Message::UserSelectionCancelled {
                attempt,
                result: Ok(()),
            } if self.conversation.accepts(attempt)
                && self.phase == Phase::CancellingForUserSelection =>
            {
                self.phase = Phase::SelectingUser;
                self.selection_session_cancelled = true;
                self.conversation.set_notice("Select a user".into(), false);
                self.set_focus(FocusTarget::Account(0))
            }
            Message::UserSelectionCancelled {
                attempt,
                result: Err(error),
            } if self.conversation.accepts(attempt)
                && self.phase == Phase::CancellingForUserSelection =>
            {
                self.phase = Phase::UserSelectionCancellationFailed;
                self.conversation.set_notice(error, true);
                self.set_focus(FocusTarget::RetryAccountSelection)
            }
            Message::UserSelectionCancellationSlow { attempt }
                if self.conversation.accepts(attempt)
                    && self.phase == Phase::CancellingForUserSelection =>
            {
                self.conversation
                    .set_notice("Still changing user…".into(), false);
                Task::none()
            }
            Message::RetryUserSelectionCancellation
                if self.phase == Phase::UserSelectionCancellationFailed =>
            {
                let attempt = self.conversation.begin_attempt();
                self.phase = Phase::CancellingForUserSelection;
                self.conversation.set_notice("Changing user…".into(), false);
                let focus = self.focus_first();
                if self.preview {
                    return self.finish_preview_user_selection();
                }
                Task::batch([auth_flow::cancel_for_user_selection(None, attempt), focus])
            }
            Message::UserSelectionCancelled { .. }
            | Message::UserSelectionCancellationSlow { .. }
            | Message::RetryUserSelectionCancellation => Task::none(),
            Message::SelectAccount(account)
                if self.can_select_account() && account.username != self.username =>
            {
                self.select_account(account)
            }
            Message::SelectAccount(_) => Task::none(),
            Message::NavigateFocus(navigation) => {
                self.close_session_menu();
                self.navigate_focus(navigation)
            }
            Message::NavigateModal(navigation)
                if matches!(self.power_state, PowerState::Confirming(_)) =>
            {
                self.navigate_focus(navigation)
            }
            Message::NavigateModal(_) => Task::none(),
            Message::SyncWidgetFocus => self.sync_widget_focus(),
            Message::WidgetFocused(focused) => self.apply_widget_focus(focused),
            Message::NavigatePage(navigation) => self.navigate_page(navigation),
            Message::SelectSession(session) if self.can_select_session() => {
                self.close_session_menu();
                self.selected_session = Some(session);
                self.set_focus(FocusTarget::Session)
            }
            Message::SelectSession(_) => Task::none(),
            Message::SessionMenuOpened if self.can_select_session() => {
                self.session_menu_open = true;
                self.set_focus(FocusTarget::Session)
            }
            Message::SessionMenuOpened => Task::none(),
            Message::SessionMenuClosed => {
                self.session_menu_open = false;
                Task::none()
            }
            Message::RetrySession
                if self.phase == Phase::Failed && self.selected_session.is_none() =>
            {
                if self.preview {
                    self.preview_message =
                        Some("Preview mode: session discovery was not retried".into());
                    return Task::none();
                }
                self.sessions = sessions::discover();
                self.selected_session = self.sessions.first().cloned();
                if self.selected_session.is_none() {
                    self.session_message = Some("No valid Wayland sessions are installed".into());
                    return Task::none();
                }
                self.session_message = None;
                if self.username.is_empty() {
                    self.phase = Phase::DiscoveringUsers;
                    self.conversation.clear_notice();
                    discover_accounts()
                } else {
                    self.retry_authentication()
                }
            }
            Message::RetrySession => Task::none(),
            Message::Retry if self.phase == Phase::Failed && self.username.is_empty() => {
                if self.preview {
                    self.conversation
                        .set_notice("Preview: retry was not sent".into(), false);
                    return Task::none();
                }
                self.phase = Phase::DiscoveringUsers;
                self.conversation.clear_notice();
                discover_accounts()
            }
            Message::Retry if self.phase == Phase::Failed && self.preview => {
                self.conversation
                    .set_notice("Preview: retry was not sent".into(), false);
                Task::none()
            }
            Message::Retry if self.phase == Phase::Failed => self.retry_authentication(),
            Message::Retry => Task::none(),
            Message::Submit if self.conversation.status() == ConversationStatus::Waiting => {
                if self.preview {
                    self.conversation.clear_response();
                    self.preview_message = Some("Preview mode: credentials were not sent".into());
                    return self.focus_input();
                }
                let Some(client) = self.client.clone() else {
                    return self.fail("Lost connection to greetd".into());
                };
                let Some((attempt, response)) = self.conversation.submit() else {
                    return Task::none();
                };
                self.phase = Phase::Authenticating;
                auth_flow::exchange(
                    client,
                    Request::PostAuthMessageResponse {
                        response: Some(response),
                    },
                    attempt,
                )
            }
            Message::Submit => Task::none(),
            Message::AuthResult { attempt, result } => self.handle_auth_result(attempt, result),
            Message::AskPower(action) if self.can_request_power() => {
                self.focus_target = Some(FocusTarget::Power(action));
                self.power_state = PowerState::Confirming(action);
                self.power_message = None;
                self.enter_power_dialog()
            }
            Message::AskPower(_) => Task::none(),
            Message::CancelPower if matches!(self.power_state, PowerState::Confirming(_)) => {
                self.power_state = PowerState::Idle;
                self.leave_power_dialog()
            }
            Message::CancelPower => Task::none(),
            Message::ConfirmPower(action) if self.power_state == PowerState::Confirming(action) => {
                if self.preview {
                    self.power_state = PowerState::Idle;
                    self.power_message = Some(format!(
                        "Preview: {} was not requested",
                        action.label().to_lowercase()
                    ));
                    self.power_message_is_error = false;
                    return self.leave_power_dialog();
                }
                self.power_state = PowerState::Executing(action);
                self.focus_target = None;
                Task::perform(power::execute(action), |result| {
                    Message::PowerResult(result.map_err(|error| error.to_string()))
                })
            }
            Message::ConfirmPower(_) => Task::none(),
            Message::PowerResult(Ok(()))
                if self.power_state == PowerState::Executing(PowerAction::Suspend) =>
            {
                self.power_state = PowerState::Idle;
                self.power_message = None;
                self.leave_power_dialog()
            }
            Message::PowerResult(Ok(())) => Task::none(),
            Message::PowerResult(Err(error)) => {
                self.power_state = PowerState::Idle;
                self.power_message = Some(conversation::bounded_text(&error));
                self.power_message_is_error = true;
                self.leave_power_dialog()
            }
            Message::Escape if matches!(self.power_state, PowerState::Confirming(_)) => {
                self.update(Message::CancelPower)
            }
            Message::Escape if self.session_menu_open => {
                self.close_session_menu();
                Task::none()
            }
            Message::Escape if self.phase == Phase::WaitingForInput => self.blur_input(),
            Message::Escape => Task::none(),
            Message::CloseRequested(window) if self.preview => {
                self.conversation.invalidate_attempt();
                window::close(window)
            }
            Message::CloseRequested(window) if self.selection_session_cancelled => {
                self.closing = Some(Closing::Dispatching(window));
                window::close(window)
            }
            Message::CloseRequested(window) if self.phase == Phase::CancellingForUserSelection => {
                self.closing = Some(Closing::WaitingForUserSelectionCancellation(window));
                auth_flow::close_timeout(window)
            }
            Message::CloseRequested(window) if self.client.is_some() => {
                self.conversation.invalidate_attempt();
                self.closing = Some(Closing::Cancelling(window));
                Task::batch([
                    auth_flow::cancel_for_close(self.client.take(), window),
                    auth_flow::close_timeout(window),
                ])
            }
            Message::CloseRequested(window) if self.phase == Phase::CreatingSession => {
                self.closing = Some(Closing::WaitingForClient(window));
                auth_flow::close_timeout(window)
            }
            Message::CloseRequested(window) => {
                self.conversation.invalidate_attempt();
                self.closing = Some(Closing::Cancelling(window));
                Task::batch([
                    auth_flow::cancel_for_close(None, window),
                    auth_flow::close_timeout(window),
                ])
            }
            Message::SessionCancelled(window)
                if self
                    .closing
                    .is_some_and(|closing| closing.is_cleaning(window)) =>
            {
                self.closing = Some(Closing::Dispatching(window));
                window::close(window)
            }
            Message::CloseTimeout(window)
                if self
                    .closing
                    .is_some_and(|closing| closing.is_cleaning(window)) =>
            {
                self.conversation.invalidate_attempt();
                self.closing = Some(Closing::Dispatching(window));
                window::close(window)
            }
            Message::SessionCancelled(_) | Message::CloseTimeout(_) => Task::none(),
        }
    }

    fn tick_interval(&self) -> Duration {
        if self.wallpaper.has_frame() {
            Duration::from_secs(1)
        } else {
            Duration::from_millis(50)
        }
    }

    fn prepare_wallpaper_frame(&mut self) -> Task<Message> {
        self.wallpaper
            .prepare_latest()
            .map_or_else(Task::none, |handle| {
                iced_runtime::image::allocate(handle).map(Message::WallpaperAllocated)
            })
    }

    fn select_account(&mut self, account: Account) -> Task<Message> {
        let replacing_account = !self.username.is_empty();
        self.username = account.username;
        self.display_name = account.display_name;
        self.close_session_menu();
        self.focus_target = None;
        self.phase = Phase::CreatingSession;
        let attempt = self.conversation.begin_attempt();
        let recover = !self.selection_session_cancelled;
        self.selection_session_cancelled = false;
        if self.preview {
            let _ = self.conversation.receive(
                attempt,
                conversation::Response::Prompt {
                    secret: true,
                    message: "Password".into(),
                },
            );
            self.phase = Phase::WaitingForInput;
            self.preview_message =
                Some("Preview mode: credentials and power actions are simulated".into());
            self.focus_input()
        } else if replacing_account {
            auth_flow::restart(self.client.take(), self.username.clone(), attempt)
        } else {
            auth_flow::begin(self.username.clone(), attempt, recover)
        }
    }

    fn retry_authentication(&mut self) -> Task<Message> {
        self.phase = Phase::CreatingSession;
        let client = self.client.take();
        let attempt = self.conversation.begin_attempt();
        auth_flow::restart(client, self.username.clone(), attempt)
    }

    fn change_user(&mut self) -> Task<Message> {
        let client = self.client.take();
        let attempt = self.conversation.begin_attempt();
        self.username.clear();
        self.display_name = "Select a user".into();
        self.selection_session_cancelled = false;
        self.phase = Phase::CancellingForUserSelection;
        self.conversation.set_notice("Changing user…".into(), false);
        let focus = self.focus_first();
        if self.preview {
            self.finish_preview_user_selection()
        } else {
            Task::batch([auth_flow::cancel_for_user_selection(client, attempt), focus])
        }
    }

    fn finish_preview_user_selection(&mut self) -> Task<Message> {
        self.phase = Phase::SelectingUser;
        self.conversation.set_notice("Select a user".into(), false);
        self.set_focus(FocusTarget::Account(0))
    }

    fn close_session_menu(&mut self) {
        if self.session_menu_open {
            self.session_menu_open = false;
            self.session_selector_key = self.session_selector_key.wrapping_add(1);
        }
    }

    fn navigate_page(&self, navigation: PageNavigation) -> Task<Message> {
        let scrollables = [self.page_scroll_id.clone(), self.account_scroll_id.clone()];
        Task::batch(scrollables.map(|id| match navigation {
            PageNavigation::Up => {
                operation::scroll_by(id, scrollable::AbsoluteOffset { x: 0.0, y: -400.0 })
            }
            PageNavigation::Down => {
                operation::scroll_by(id, scrollable::AbsoluteOffset { x: 0.0, y: 400.0 })
            }
            PageNavigation::Start => {
                operation::snap_to(id, scrollable::RelativeOffset { x: 0.0, y: 0.0 })
            }
            PageNavigation::End => {
                operation::snap_to(id, scrollable::RelativeOffset { x: 0.0, y: 1.0 })
            }
        }))
    }

    fn power_dialog_interactive(&self) -> bool {
        self.closing.is_none() && matches!(self.power_state, PowerState::Confirming(_))
    }

    fn can_request_power(&self) -> bool {
        self.closing.is_none()
            && self.power_state == PowerState::Idle
            && matches!(
                self.phase,
                Phase::WaitingForInput
                    | Phase::Failed
                    | Phase::SelectingUser
                    | Phase::UserSelectionCancellationFailed
            )
    }

    fn can_change_user(&self) -> bool {
        self.closing.is_none()
            && self.power_state == PowerState::Idle
            && self.accounts.len() > 1
            && matches!(self.phase, Phase::WaitingForInput | Phase::Failed)
    }

    fn can_select_session(&self) -> bool {
        self.closing.is_none()
            && self.power_state == PowerState::Idle
            && self.selected_session.is_some()
            && !matches!(self.phase, Phase::Authenticating | Phase::StartingSession)
    }

    fn can_select_account(&self) -> bool {
        self.closing.is_none()
            && self.power_state == PowerState::Idle
            && self.phase == Phase::SelectingUser
    }

    fn background_elapsed(&self) -> f32 {
        if self.preview {
            0.0
        } else {
            self.started_at.elapsed().as_secs_f32()
        }
    }
}

fn startup_mode(has_session: bool, has_configured_identity: bool) -> StartupMode {
    match (has_session, has_configured_identity) {
        (true, true) => StartupMode::ConfiguredIdentity,
        (true, false) => StartupMode::DiscoverAccounts,
        (false, _) => StartupMode::MissingSession,
    }
}

fn page_navigation(
    event: iced::Event,
    status: event::Status,
    _window: window::Id,
) -> Option<Message> {
    let iced::Event::Keyboard(keyboard::Event::KeyPressed { key, .. }) = event else {
        return None;
    };
    page_navigation_key(key, status)
}

fn page_navigation_key(key: keyboard::Key, status: event::Status) -> Option<Message> {
    use keyboard::key::Named;

    match key.as_ref() {
        // Page keys scroll even when a focused text input captures them. Home
        // and End retain their conventional text-editing behavior when captured.
        keyboard::Key::Named(Named::PageUp) => Some(Message::NavigatePage(PageNavigation::Up)),
        keyboard::Key::Named(Named::PageDown) => Some(Message::NavigatePage(PageNavigation::Down)),
        keyboard::Key::Named(Named::Home) if status == event::Status::Ignored => {
            Some(Message::NavigatePage(PageNavigation::Start))
        }
        keyboard::Key::Named(Named::End) if status == event::Status::Ignored => {
            Some(Message::NavigatePage(PageNavigation::End))
        }
        _ => None,
    }
}

fn cancel_shortcut(
    event: iced::Event,
    _status: event::Status,
    _window: window::Id,
) -> Option<Message> {
    let iced::Event::Keyboard(keyboard::Event::KeyPressed { key, .. }) = event else {
        return None;
    };
    cancel_shortcut_key(key)
}

fn cancel_shortcut_key(key: keyboard::Key) -> Option<Message> {
    matches!(
        key.as_ref(),
        keyboard::Key::Named(keyboard::key::Named::Escape)
    )
    .then_some(Message::Escape)
}

fn discover_accounts() -> Task<Message> {
    Task::perform(accounts::discover(), |result| {
        Message::AccountsResult(result.map_err(|error| error.to_string()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Session {
        Session {
            name: "Sway".into(),
            command: vec!["sway".into()],
            session_id: "sway".into(),
            desktop_names: vec!["sway".into()],
        }
    }

    fn app() -> App {
        let mut conversation = Conversation::new();
        let attempt = conversation.begin_attempt();
        let _ = conversation.receive(
            attempt,
            conversation::Response::Prompt {
                secret: true,
                message: "Password".into(),
            },
        );
        conversation.update_input("secret");
        conversation.set_notice("Keep this message".into(), false);
        App {
            username: "darwin".into(),
            display_name: "Darwin".into(),
            accounts: Vec::new(),
            focus_target: Some(FocusTarget::AuthenticationInput),
            focus_before_modal: None,
            account_scroll_id: Id::unique(),
            page_scroll_id: Id::unique(),
            input_id: Id::new("test-authentication-input"),
            conversation,
            session_message: None,
            power_message: None,
            power_message_is_error: false,
            preview_message: None,
            phase: Phase::WaitingForInput,
            client: None,
            sessions: vec![session()],
            selected_session: Some(session()),
            session_menu_open: false,
            session_selector_key: 0,
            wallpaper: wallpaper::State::disabled(),
            started_at: Instant::now(),
            now: Local::now(),
            power_state: PowerState::Idle,
            selection_session_cancelled: false,
            closing: None,
            preview: false,
        }
    }

    fn account(username: &str) -> Account {
        Account::override_account(username.into(), Some(username.to_uppercase()))
    }

    #[test]
    fn preview_accepts_input_without_sending_credentials() {
        let mut app = app();
        app.preview = true;

        let _ = app.update(Message::InputChanged("not-a-real-password".into()));
        assert_eq!(app.conversation.input(), "not-a-real-password");

        let _ = app.update(Message::Submit);
        assert_eq!(app.phase, Phase::WaitingForInput);
        assert!(app.conversation.input().is_empty());
        assert_eq!(
            app.preview_message.as_deref(),
            Some("Preview mode: credentials were not sent")
        );
        assert_eq!(app.conversation.notice(), Some("Keep this message"));
    }

    #[test]
    fn preview_accepts_empty_response_without_sending_credentials() {
        let mut app = app();
        app.preview = true;
        app.conversation.clear_response();

        let _ = app.update(Message::Submit);

        assert_eq!(app.phase, Phase::WaitingForInput);
        assert!(app.conversation.input().is_empty());
        assert_eq!(
            app.preview_message.as_deref(),
            Some("Preview mode: credentials were not sent")
        );
    }

    #[test]
    fn keyboard_activation_accepts_empty_response() {
        let mut app = app();
        app.preview = true;
        app.conversation.clear_response();
        app.focus_target = Some(FocusTarget::Submit);

        let _ = app.update(Message::NavigateFocus(FocusNavigation::Activate));

        assert_eq!(app.phase, Phase::WaitingForInput);
        assert!(app.conversation.input().is_empty());
        assert_eq!(
            app.preview_message.as_deref(),
            Some("Preview mode: credentials were not sent")
        );
    }

    #[test]
    fn preview_never_dispatches_power_actions() {
        for action in [
            PowerAction::Suspend,
            PowerAction::Reboot,
            PowerAction::PowerOff,
        ] {
            let mut app = app();
            app.preview = true;

            let _ = app.update(Message::AskPower(action));
            assert_eq!(app.power_state, PowerState::Confirming(action));

            let _ = app.update(Message::ConfirmPower(action));
            assert_eq!(app.power_state, PowerState::Idle);
            assert_eq!(
                app.power_message.as_deref(),
                Some(
                    format!(
                        "Preview: {} was not requested",
                        action.label().to_lowercase()
                    )
                    .as_str()
                )
            );
            assert!(!app.power_message_is_error);
            assert_eq!(app.conversation.notice(), Some("Keep this message"));
        }
    }

    #[test]
    fn sole_discovered_account_starts_authentication() {
        let mut app = app();
        app.phase = Phase::DiscoveringUsers;

        let _ = app.update(Message::AccountsResult(Ok(vec![account("alice")])));

        assert_eq!(app.phase, Phase::CreatingSession);
        assert_eq!(app.username, "alice");
        assert_eq!(app.display_name, "ALICE");
        assert_eq!(app.accounts.len(), 1);
    }

    #[test]
    fn configured_identity_precedes_account_discovery() {
        assert_eq!(startup_mode(true, true), StartupMode::ConfiguredIdentity);
        assert_eq!(startup_mode(true, false), StartupMode::DiscoverAccounts);
        assert_eq!(startup_mode(false, true), StartupMode::MissingSession);
    }

    #[test]
    fn multiple_discovered_accounts_require_selection() {
        let mut app = app();
        app.phase = Phase::DiscoveringUsers;

        let _ = app.update(Message::AccountsResult(Ok(vec![
            account("alice"),
            account("bob"),
        ])));

        assert_eq!(app.phase, Phase::SelectingUser);
        assert_eq!(app.accounts.len(), 2);
        assert_eq!(app.conversation.notice(), Some("Select a user"));
    }

    #[test]
    fn change_user_waits_for_cancellation_before_selecting_an_account() {
        let mut app = app();
        app.accounts = vec![account("darwin"), account("alice")];
        let attempt = app.conversation.attempt();

        let _ = app.update(Message::ChangeUser);

        assert_eq!(app.phase, Phase::CancellingForUserSelection);
        assert!(app.username.is_empty());
        assert!(app.conversation.input().is_empty());
        assert_ne!(app.conversation.attempt(), attempt);
        assert!(!app.can_select_account());
        assert_eq!(app.focus_target, Some(FocusTarget::Session));

        let _ = app.update(Message::SelectAccount(account("alice")));
        assert!(app.username.is_empty());

        let _ = app.update(Message::UserSelectionCancelled {
            attempt: app.conversation.attempt(),
            result: Ok(()),
        });
        assert_eq!(app.phase, Phase::SelectingUser);
        assert!(app.can_select_account());
        assert!(app.selection_session_cancelled);
        assert_eq!(app.focus_target, Some(FocusTarget::Account(0)));

        let _ = app.update(Message::SelectAccount(account("alice")));

        assert_eq!(app.phase, Phase::CreatingSession);
        assert_eq!(app.username, "alice");
        assert_eq!(app.display_name, "ALICE");
        assert!(app.conversation.input().is_empty());
        assert!(app.conversation.notice().is_none());
        assert!(!app.selection_session_cancelled);
    }

    #[test]
    fn repeated_change_user_is_ignored_while_cancellation_is_in_flight() {
        let mut app = app();
        app.accounts = vec![account("darwin"), account("alice")];

        let _ = app.update(Message::ChangeUser);
        let attempt = app.conversation.attempt();
        let _ = app.update(Message::ChangeUser);

        assert_eq!(app.phase, Phase::CancellingForUserSelection);
        assert_eq!(app.conversation.attempt(), attempt);
        assert_eq!(app.conversation.notice(), Some("Changing user…"));
        assert!(app.conversation.input().is_empty());
    }

    #[test]
    fn failed_authentication_can_change_account_without_restarting() {
        let mut app = app();
        app.phase = Phase::Failed;
        app.accounts = vec![account("darwin"), account("alice")];
        let attempt = app.conversation.attempt();

        let _ = app.update(Message::ChangeUser);

        assert_eq!(app.phase, Phase::CancellingForUserSelection);
        assert_ne!(app.conversation.attempt(), attempt);
        assert!(app.username.is_empty());
        assert!(app.conversation.input().is_empty());
    }

    #[test]
    fn cancellation_failure_keeps_account_selection_safe_and_retryable() {
        let mut app = app();
        app.accounts = vec![account("darwin"), account("alice")];

        let _ = app.update(Message::ChangeUser);
        let _ = app.update(Message::UserSelectionCancelled {
            attempt: app.conversation.attempt(),
            result: Err("greetd unavailable".into()),
        });

        assert_eq!(app.phase, Phase::UserSelectionCancellationFailed);
        assert_eq!(app.conversation.notice(), Some("greetd unavailable"));
        assert!(app.conversation.notice_is_error());
        assert!(!app.can_select_account());
        assert_eq!(app.focus_target, Some(FocusTarget::RetryAccountSelection));

        let _ = app.update(Message::SelectAccount(account("alice")));
        assert!(app.username.is_empty());

        let _ = app.update(Message::RetryUserSelectionCancellation);
        assert_eq!(app.phase, Phase::CancellingForUserSelection);
        assert_eq!(app.conversation.notice(), Some("Changing user…"));
        assert_eq!(app.focus_target, Some(FocusTarget::Session));
    }

    #[test]
    fn slow_cancellation_remains_authoritative_and_blocks_retry() {
        let mut app = app();
        app.accounts = vec![account("darwin"), account("alice")];

        let _ = app.update(Message::ChangeUser);
        let attempt = app.conversation.attempt();
        let _ = app.update(Message::UserSelectionCancellationSlow { attempt });

        assert_eq!(app.phase, Phase::CancellingForUserSelection);
        assert_eq!(app.conversation.notice(), Some("Still changing user…"));
        assert!(!app.conversation.notice_is_error());
        assert!(!app.can_select_account());

        let _ = app.update(Message::RetryUserSelectionCancellation);
        let _ = app.update(Message::SelectAccount(account("alice")));

        assert_eq!(app.phase, Phase::CancellingForUserSelection);
        assert_eq!(app.conversation.attempt(), attempt);
        assert!(app.username.is_empty());
    }

    #[test]
    fn stale_progress_does_not_mark_a_new_cancellation_as_slow() {
        let mut app = app();
        app.accounts = vec![account("darwin"), account("alice")];

        let _ = app.update(Message::ChangeUser);
        let old_attempt = app.conversation.attempt();
        let _ = app.update(Message::UserSelectionCancelled {
            attempt: old_attempt,
            result: Err("greetd unavailable".into()),
        });
        let _ = app.update(Message::RetryUserSelectionCancellation);
        let current_attempt = app.conversation.attempt();

        assert_ne!(current_attempt, old_attempt);
        assert_eq!(app.conversation.notice(), Some("Changing user…"));

        let _ = app.update(Message::UserSelectionCancellationSlow {
            attempt: old_attempt,
        });

        assert_eq!(app.conversation.attempt(), current_attempt);
        assert_eq!(app.conversation.notice(), Some("Changing user…"));
        assert_eq!(app.phase, Phase::CancellingForUserSelection);
    }

    #[test]
    fn preview_change_user_is_immediate_and_does_not_dispatch_cancellation() {
        let mut app = app();
        app.preview = true;
        app.accounts = vec![account("darwin"), account("alice")];

        let _ = app.update(Message::ChangeUser);

        assert_eq!(app.phase, Phase::SelectingUser);
        assert!(app.username.is_empty());
        assert_eq!(app.conversation.notice(), Some("Select a user"));
    }

    #[test]
    fn account_selection_is_disabled_while_authentication_is_in_flight() {
        for phase in [
            Phase::CreatingSession,
            Phase::Authenticating,
            Phase::StartingSession,
        ] {
            let mut app = app();
            app.phase = phase;
            app.accounts = vec![account("darwin"), account("alice")];

            let _ = app.update(Message::SelectAccount(account("alice")));

            assert!(!app.can_select_account(), "phase {phase:?}");
            assert_eq!(app.username, "darwin");
        }
    }

    #[test]
    fn keyboard_navigation_wraps_and_activates_the_focused_account() {
        let mut app = app();
        app.preview = true;
        app.phase = Phase::SelectingUser;
        app.username.clear();
        app.accounts = vec![account("alice"), account("bob")];
        app.focus_target = Some(FocusTarget::Account(0));

        let _ = app.update(Message::NavigateFocus(FocusNavigation::DirectionPrevious));
        assert_eq!(app.focused_account(), Some(1));

        let _ = app.update(Message::NavigateFocus(FocusNavigation::DirectionNext));
        assert_eq!(app.focused_account(), Some(0));

        let _ = app.update(Message::NavigateFocus(FocusNavigation::Activate));
        assert_eq!(app.username, "alice");
        assert_eq!(app.phase, Phase::WaitingForInput);
        assert_eq!(app.focused_account(), None);
    }

    #[test]
    fn keyboard_navigation_is_ignored_while_account_tiles_are_disabled() {
        let mut app = app();
        app.phase = Phase::CancellingForUserSelection;
        app.accounts = vec![account("alice"), account("bob")];
        app.focus_target = Some(FocusTarget::Account(0));

        let _ = app.update(Message::NavigateFocus(FocusNavigation::DirectionNext));
        let _ = app.update(Message::NavigateFocus(FocusNavigation::Activate));

        assert_eq!(app.focused_account(), Some(0));
        assert_eq!(app.username, "darwin");
        assert_eq!(app.phase, Phase::CancellingForUserSelection);
    }

    #[test]
    fn authentication_focus_order_follows_visual_reading_order() {
        let mut app = app();
        app.accounts = vec![account("darwin"), account("alice")];

        assert_eq!(
            app.focus_order(),
            vec![
                FocusTarget::AuthenticationInput,
                FocusTarget::Submit,
                FocusTarget::ChangeUser,
                FocusTarget::Session,
                FocusTarget::Power(PowerAction::Suspend),
                FocusTarget::Power(PowerAction::Reboot),
                FocusTarget::Power(PowerAction::PowerOff),
            ]
        );

        let _ = app.update(Message::NavigateFocus(FocusNavigation::Next));
        assert_eq!(app.focus_target, Some(FocusTarget::Submit));
        let _ = app.update(Message::NavigateFocus(FocusNavigation::Previous));
        assert_eq!(app.focus_target, Some(FocusTarget::AuthenticationInput));
    }

    #[test]
    fn pointer_focused_input_synchronizes_logical_focus() {
        let mut app = app();
        app.focus_target = Some(FocusTarget::Submit);
        let input = app.input_id.clone();

        let _ = app.update(Message::WidgetFocused(input));

        assert_eq!(app.focus_target, Some(FocusTarget::AuthenticationInput));
    }

    #[test]
    fn account_focus_order_reaches_session_and_power_controls() {
        let mut app = app();
        app.preview = true;
        app.phase = Phase::SelectingUser;
        app.username.clear();
        app.accounts = vec![account("alice"), account("bob")];
        app.focus_target = Some(FocusTarget::Account(0));

        assert_eq!(
            app.focus_order(),
            vec![
                FocusTarget::Account(0),
                FocusTarget::Account(1),
                FocusTarget::Session,
                FocusTarget::Power(PowerAction::Suspend),
                FocusTarget::Power(PowerAction::Reboot),
                FocusTarget::Power(PowerAction::PowerOff),
            ]
        );

        let _ = app.update(Message::NavigateFocus(FocusNavigation::Previous));
        assert_eq!(
            app.focus_target,
            Some(FocusTarget::Power(PowerAction::PowerOff))
        );
    }

    #[test]
    fn session_focus_supports_keyboard_selection() {
        let mut app = app();
        let mut river = session();
        river.name = "River".into();
        river.session_id = "river".into();
        app.sessions.push(river.clone());
        app.focus_target = Some(FocusTarget::Session);

        let _ = app.update(Message::NavigateFocus(FocusNavigation::Activate));

        assert_eq!(app.selected_session, Some(river));
        assert_eq!(app.focus_target, Some(FocusTarget::Session));
    }

    #[test]
    fn keyboard_navigation_and_escape_close_an_open_session_menu() {
        let mut app = app();
        app.focus_target = Some(FocusTarget::Session);

        let _ = app.update(Message::SessionMenuOpened);
        assert!(app.session_menu_open);
        let key = app.session_selector_key;

        let _ = app.update(Message::NavigateFocus(FocusNavigation::Next));
        assert!(!app.session_menu_open);
        assert_ne!(app.session_selector_key, key);
        assert_eq!(
            app.focus_target,
            Some(FocusTarget::Power(PowerAction::Suspend))
        );

        let _ = app.update(Message::SessionMenuOpened);
        let key = app.session_selector_key;
        let _ = app.update(Message::Escape);
        assert!(!app.session_menu_open);
        assert_ne!(app.session_selector_key, key);
        assert_eq!(app.focus_target, Some(FocusTarget::Session));
    }

    #[test]
    fn asynchronous_prompt_closes_an_open_session_menu() {
        let mut app = app();
        app.phase = Phase::CreatingSession;
        let _ = app.update(Message::SessionMenuOpened);
        let key = app.session_selector_key;

        let _ = app.update(Message::AuthResult {
            attempt: app.conversation.attempt(),
            result: Ok((
                None,
                auth::Response::Prompt {
                    secret: true,
                    message: "Password:".into(),
                },
            )),
        });

        assert!(!app.session_menu_open);
        assert_ne!(app.session_selector_key, key);
        assert_eq!(app.focus_target, Some(FocusTarget::AuthenticationInput));
    }

    #[test]
    fn missing_session_cannot_open_or_focus_an_empty_selector() {
        let mut app = app();
        app.phase = Phase::Failed;
        app.sessions.clear();
        app.selected_session = None;
        app.focus_target = Some(FocusTarget::RetrySession);

        let _ = app.update(Message::SessionMenuOpened);

        assert!(!app.can_select_session());
        assert!(!app.session_menu_open);
        assert_eq!(app.focus_target, Some(FocusTarget::RetrySession));
    }

    #[test]
    fn stale_session_focus_cannot_change_selection_during_authentication() {
        for phase in [Phase::Authenticating, Phase::StartingSession] {
            let mut app = app();
            let mut river = session();
            river.name = "River".into();
            river.session_id = "river".into();
            app.sessions.push(river);
            app.phase = phase;
            app.focus_target = Some(FocusTarget::Session);
            let selected = app.selected_session.clone();

            let _ = app.update(Message::NavigateFocus(FocusNavigation::Activate));

            assert_eq!(app.selected_session, selected, "phase {phase:?}");
        }
    }

    #[test]
    fn maps_focus_keys_without_capturing_other_input() {
        use keyboard::key::Named;

        assert!(matches!(
            focus::navigation_key(
                keyboard::Key::Named(Named::Tab),
                keyboard::Modifiers::SHIFT,
                event::Status::Ignored,
            ),
            Some(FocusNavigation::Previous)
        ));
        assert!(matches!(
            focus::navigation_key(
                keyboard::Key::Named(Named::Enter),
                keyboard::Modifiers::empty(),
                event::Status::Ignored,
            ),
            Some(FocusNavigation::Activate)
        ));
        assert!(matches!(
            page_navigation_key(
                keyboard::Key::Named(Named::PageDown),
                event::Status::Captured,
            ),
            Some(Message::NavigatePage(PageNavigation::Down))
        ));
        assert!(
            page_navigation_key(keyboard::Key::Named(Named::Home), event::Status::Captured,)
                .is_none()
        );
        assert!(focus::navigation_key(
            keyboard::Key::Character("a".into()),
            keyboard::Modifiers::empty(),
            event::Status::Ignored,
        )
        .is_none());
        assert!(focus::navigation_key(
            keyboard::Key::Named(Named::Space),
            keyboard::Modifiers::empty(),
            event::Status::Captured,
        )
        .is_none());
        assert_eq!(
            focus::navigation_key(
                keyboard::Key::Named(Named::Space),
                keyboard::Modifiers::empty(),
                event::Status::Ignored,
            ),
            Some(FocusNavigation::Activate)
        );
        assert!(matches!(
            cancel_shortcut_key(keyboard::Key::Named(Named::Escape)),
            Some(Message::Escape)
        ));
    }

    #[test]
    fn captured_navigation_is_routed_only_to_an_active_modal() {
        use keyboard::key::{Code, Named, Physical};

        let event = iced::Event::Keyboard(keyboard::Event::KeyPressed {
            key: keyboard::Key::Named(Named::Tab),
            modified_key: keyboard::Key::Named(Named::Tab),
            physical_key: Physical::Code(Code::Tab),
            location: keyboard::Location::Standard,
            modifiers: keyboard::Modifiers::empty(),
            text: None,
            repeat: false,
        });
        assert!(matches!(
            focus::keyboard_navigation(event, event::Status::Captured, window::Id::unique()),
            Some(Message::NavigateModal(FocusNavigation::Next))
        ));

        let mut app = app();
        app.focus_target = Some(FocusTarget::AuthenticationInput);
        let _ = app.update(Message::NavigateModal(FocusNavigation::Activate));
        assert_eq!(app.phase, Phase::WaitingForInput);

        app.preview = true;
        let _ = app.update(Message::AskPower(PowerAction::PowerOff));
        let _ = app.update(Message::NavigateModal(FocusNavigation::Next));
        assert_eq!(app.focus_target, Some(FocusTarget::DialogConfirm));
        let _ = app.update(Message::NavigateModal(FocusNavigation::Activate));
        assert_eq!(app.power_state, PowerState::Idle);
    }

    #[test]
    fn native_input_blur_clears_stale_logical_focus() {
        let mut app = app();

        let _ = app.update(Message::SyncWidgetFocus);
        assert_eq!(app.focus_target, None);

        app.focus_target = Some(FocusTarget::AuthenticationInput);
        let _ = app.update(Message::Escape);
        assert_eq!(app.focus_target, None);

        let _ = app.update(Message::NavigateFocus(FocusNavigation::Next));
        assert_eq!(app.focus_target, Some(FocusTarget::AuthenticationInput));
    }

    #[test]
    fn escape_does_not_resurrect_an_invalidated_account_attempt() {
        let mut app = app();
        app.preview = true;
        app.accounts = vec![account("darwin"), account("alice")];

        let _ = app.update(Message::ChangeUser);
        let _ = app.update(Message::Escape);

        assert_eq!(app.phase, Phase::SelectingUser);
        assert!(app.username.is_empty());
        assert_eq!(app.focus_target, Some(FocusTarget::Account(0)));
    }

    #[test]
    fn empty_account_discovery_reports_configuration_error() {
        let mut app = app();
        app.phase = Phase::DiscoveringUsers;

        let _ = app.update(Message::AccountsResult(Ok(Vec::new())));

        assert_eq!(app.phase, Phase::Failed);
        assert_eq!(
            app.conversation.notice(),
            Some("AccountsService found no unlocked non-system users")
        );
        assert!(app.conversation.notice_is_error());
    }

    #[test]
    fn ignores_responses_from_abandoned_attempts() {
        let mut app = app();
        let _ = app.update(Message::AuthResult {
            attempt: Conversation::new().attempt(),
            result: Ok((
                None,
                auth::Response::Error {
                    authentication: false,
                    description: "late failure".into(),
                },
            )),
        });

        assert_eq!(app.phase, Phase::WaitingForInput);
        assert_eq!(app.conversation.input(), "secret");
        assert_eq!(app.conversation.notice(), Some("Keep this message"));
    }

    #[test]
    fn ignores_input_outside_a_pam_prompt() {
        let mut app = app();
        let _ = app.conversation.submit();
        app.phase = Phase::Authenticating;

        let _ = app.update(Message::InputChanged("replacement".into()));
        let _ = app.update(Message::Submit);

        assert!(app.conversation.input().is_empty());
        assert_eq!(app.phase, Phase::Authenticating);
    }

    #[test]
    fn session_selection_is_disabled_while_authentication_is_in_flight() {
        for phase in [Phase::Authenticating, Phase::StartingSession] {
            let mut app = app();
            app.phase = phase;

            let _ = app.update(Message::SelectSession(Session {
                name: "Other".into(),
                command: vec!["other".into()],
                session_id: "other".into(),
                desktop_names: Vec::new(),
            }));

            assert!(!app.can_select_session(), "phase {phase:?}");
            assert_eq!(app.selected_session.as_ref().unwrap().name, "Sway");
        }
    }

    #[test]
    fn authentication_error_waits_for_explicit_retry() {
        let mut app = app();
        let attempt = app.conversation.attempt();

        let _ = app.update(Message::AuthResult {
            attempt,
            result: Ok((
                None,
                auth::Response::Error {
                    authentication: true,
                    description: "authentication failed".into(),
                },
            )),
        });

        assert_eq!(app.phase, Phase::Failed);
        assert_eq!(app.conversation.attempt(), attempt);
        assert_eq!(app.conversation.notice(), Some("Authentication failed"));
        assert_eq!(app.focus_target, Some(FocusTarget::RetryAuthentication));

        let _ = app.update(Message::Retry);
        assert_eq!(app.phase, Phase::CreatingSession);
        assert_ne!(app.conversation.attempt(), attempt);
    }

    #[test]
    fn prompt_sequences_replace_and_clear_the_previous_response() {
        let mut app = app();
        app.conversation.update_input("previous response");

        let _ = app.handle_auth_response(auth::Response::Prompt {
            secret: false,
            message: "Login code:".into(),
        });
        assert_eq!(app.phase, Phase::WaitingForInput);
        assert_eq!(app.conversation.prompt(), "Login code");
        assert!(!app.conversation.is_secret());
        assert!(app.conversation.input().is_empty());
        assert_eq!(app.conversation.notice(), Some("Keep this message"));

        app.set_auth_notice("First security-key instruction".into(), false);
        app.set_auth_notice("Security key accepted".into(), false);

        app.conversation.update_input("123456");
        let _ = app.handle_auth_response(auth::Response::Prompt {
            secret: true,
            message: "Password:".into(),
        });
        assert_eq!(app.conversation.prompt(), "Password");
        assert!(app.conversation.is_secret());
        assert!(app.conversation.input().is_empty());
        assert_eq!(app.conversation.notice(), Some("Security key accepted"));
        assert!(!app.conversation.notice_is_error());
    }

    #[test]
    fn session_recovery_notice_does_not_replace_authentication_status() {
        let mut app = app();
        app.phase = Phase::Failed;
        app.selected_session = None;
        app.sessions.clear();
        app.session_message = Some("No valid Wayland sessions are installed".into());
        app.preview = true;

        let _ = app.update(Message::RetrySession);

        assert_eq!(app.conversation.notice(), Some("Keep this message"));
        assert_eq!(
            app.session_message.as_deref(),
            Some("No valid Wayland sessions are installed")
        );
        assert_eq!(
            app.preview_message.as_deref(),
            Some("Preview mode: session discovery was not retried")
        );
    }

    #[test]
    fn session_start_transport_failure_enters_failed_state() {
        let mut app = app();
        app.phase = Phase::StartingSession;

        let _ = app.update(Message::AuthResult {
            attempt: app.conversation.attempt(),
            result: Err("greetd closed the socket".into()),
        });

        assert_eq!(app.phase, Phase::Failed);
        assert_eq!(app.conversation.notice(), Some("greetd closed the socket"));
        assert!(app.conversation.notice_is_error());
    }

    #[test]
    fn successful_authentication_supersedes_the_previous_pam_notice() {
        let mut app = app();
        app.phase = Phase::Authenticating;
        app.conversation
            .set_notice("Previous PAM error".into(), true);

        let session = app.selected_session.clone();
        let attempt = app.conversation.attempt();
        let transition = super::auth_flow::transition(
            &mut app.conversation,
            app.phase,
            attempt,
            auth::Response::Success,
            session.as_ref(),
        )
        .unwrap();
        assert!(matches!(
            app.apply_auth_transition(transition),
            super::auth_flow::AuthEffect::StartSession(_)
        ));

        assert_eq!(app.phase, Phase::StartingSession);
        assert!(app.conversation.notice().is_none());
        assert!(!app.conversation.notice_is_error());
        assert_eq!(
            super::view::authentication_controls(Phase::StartingSession, true),
            super::view::AuthenticationControls::Progress("Starting session…")
        );
    }

    #[test]
    fn power_failures_preserve_authentication_state() {
        let mut app = app();
        app.power_state = PowerState::Executing(PowerAction::Reboot);
        let _ = app.update(Message::PowerResult(Err("not authorized".into())));

        assert_eq!(app.phase, Phase::WaitingForInput);
        assert_eq!(app.conversation.input(), "secret");
        assert_eq!(app.power_state, PowerState::Idle);
        assert_eq!(app.power_message.as_deref(), Some("not authorized"));
        assert!(app.power_message_is_error);
        assert_eq!(app.conversation.notice(), Some("Keep this message"));
    }

    #[test]
    fn power_confirmation_blocks_underlying_and_duplicate_input() {
        let mut app = app();

        let _ = app.update(Message::AskPower(PowerAction::PowerOff));
        let _ = app.update(Message::InputChanged("replacement".into()));
        let _ = app.update(Message::Submit);
        let _ = app.update(Message::SelectSession(Session {
            name: "Other".into(),
            command: vec!["other".into()],
            session_id: "other".into(),
            desktop_names: Vec::new(),
        }));
        let _ = app.update(Message::AskPower(PowerAction::Reboot));

        assert_eq!(
            app.power_state,
            PowerState::Confirming(PowerAction::PowerOff)
        );
        assert_eq!(app.phase, Phase::WaitingForInput);
        assert_eq!(app.conversation.input(), "secret");
        assert_eq!(
            app.selected_session
                .as_ref()
                .map(|session| session.name.as_str()),
            Some("Sway")
        );
    }

    #[test]
    fn power_requests_are_blocked_during_authentication_operations() {
        for phase in [
            Phase::DiscoveringUsers,
            Phase::CreatingSession,
            Phase::Authenticating,
            Phase::StartingSession,
        ] {
            let mut app = app();
            app.phase = phase;

            let _ = app.update(Message::AskPower(PowerAction::PowerOff));

            assert_eq!(app.power_state, PowerState::Idle, "phase {phase:?}");
            assert!(!app.can_request_power(), "phase {phase:?}");
        }
    }

    #[test]
    fn power_confirmation_only_executes_the_confirmed_action() {
        let mut app = app();
        let _ = app.update(Message::AskPower(PowerAction::PowerOff));

        assert!(app.power_dialog_interactive());

        let _ = app.update(Message::ConfirmPower(PowerAction::Reboot));
        assert_eq!(
            app.power_state,
            PowerState::Confirming(PowerAction::PowerOff)
        );

        let _ = app.update(Message::ConfirmPower(PowerAction::PowerOff));
        assert_eq!(
            app.power_state,
            PowerState::Executing(PowerAction::PowerOff)
        );
        assert!(!app.power_dialog_interactive());

        let _ = app.update(Message::CancelPower);
        let _ = app.update(Message::AskPower(PowerAction::Reboot));
        assert_eq!(
            app.power_state,
            PowerState::Executing(PowerAction::PowerOff)
        );
    }

    #[test]
    fn power_dialog_starts_on_cancel_and_traps_keyboard_activation() {
        let mut app = app();
        app.preview = true;

        let _ = app.update(Message::AskPower(PowerAction::PowerOff));
        assert_eq!(app.focus_target, Some(FocusTarget::DialogCancel));

        let _ = app.update(Message::NavigateModal(FocusNavigation::Activate));
        assert_eq!(app.power_state, PowerState::Idle);
        assert_eq!(
            app.focus_target,
            Some(FocusTarget::Power(PowerAction::PowerOff))
        );

        let _ = app.update(Message::AskPower(PowerAction::PowerOff));
        let _ = app.update(Message::NavigateModal(FocusNavigation::Next));
        assert_eq!(app.focus_target, Some(FocusTarget::DialogConfirm));

        let _ = app.update(Message::NavigateModal(FocusNavigation::Activate));
        assert_eq!(app.power_state, PowerState::Idle);
        assert_eq!(
            app.power_message.as_deref(),
            Some("Preview: shut down was not requested")
        );
        assert_eq!(
            app.focus_target,
            Some(FocusTarget::Power(PowerAction::PowerOff))
        );
    }

    #[test]
    fn successful_suspend_restores_the_greeter() {
        let mut app = app();
        app.power_state = PowerState::Executing(PowerAction::Suspend);
        app.power_message = Some("Requesting sleep…".into());

        let _ = app.update(Message::PowerResult(Ok(())));

        assert_eq!(app.power_state, PowerState::Idle);
        assert_eq!(app.power_message, None);
        assert_eq!(app.conversation.notice(), Some("Keep this message"));
        assert_eq!(app.phase, Phase::WaitingForInput);
    }

    #[test]
    fn repeated_close_is_ignored_while_waiting_for_cancellation() {
        let mut app = app();
        app.phase = Phase::CreatingSession;
        let first = window::Id::unique();
        let second = window::Id::unique();

        let _ = app.update(Message::CloseRequested(first));
        assert_eq!(app.closing, Some(Closing::WaitingForClient(first)));

        let _ = app.update(Message::CloseRequested(second));
        assert_eq!(app.closing, Some(Closing::WaitingForClient(first)));
    }

    #[test]
    fn repeated_close_is_ignored_while_cancelling() {
        let mut app = app();
        let first = window::Id::unique();
        let second = window::Id::unique();
        app.closing = Some(Closing::Cancelling(first));

        let _ = app.update(Message::CloseRequested(second));

        assert_eq!(app.closing, Some(Closing::Cancelling(first)));
    }

    #[test]
    fn close_waits_for_in_flight_user_selection_cancellation() {
        let mut app = app();
        let window = window::Id::unique();
        app.phase = Phase::CancellingForUserSelection;

        let _ = app.update(Message::CloseRequested(window));

        assert_eq!(
            app.closing,
            Some(Closing::WaitingForUserSelectionCancellation(window))
        );
        assert_eq!(app.phase, Phase::CancellingForUserSelection);

        let _ = app.update(Message::UserSelectionCancelled {
            attempt: app.conversation.attempt(),
            result: Err("greetd unavailable".into()),
        });

        assert_eq!(app.closing, Some(Closing::Dispatching(window)));
        assert_eq!(app.phase, Phase::CancellingForUserSelection);
    }

    #[test]
    fn close_does_not_cancel_an_already_cancelled_selection_session() {
        let mut app = app();
        let window = window::Id::unique();
        app.phase = Phase::SelectingUser;
        app.selection_session_cancelled = true;

        let _ = app.update(Message::CloseRequested(window));

        assert_eq!(app.closing, Some(Closing::Dispatching(window)));
        assert!(app.selection_session_cancelled);
    }

    #[test]
    fn creation_result_starts_deferred_close_cancellation() {
        let mut app = app();
        let window = window::Id::unique();
        app.phase = Phase::CreatingSession;
        app.closing = Some(Closing::WaitingForClient(window));

        let _ = app.update(Message::AuthResult {
            attempt: app.conversation.attempt(),
            result: Err("socket closed".into()),
        });

        assert_eq!(app.closing, Some(Closing::Cancelling(window)));
    }

    #[test]
    fn close_timeout_invalidates_a_late_creation_result() {
        let mut app = app();
        let window = window::Id::unique();
        let old_attempt = app.conversation.attempt();
        app.phase = Phase::CreatingSession;
        app.closing = Some(Closing::WaitingForClient(window));

        let _ = app.update(Message::CloseTimeout(window));
        assert_eq!(app.closing, Some(Closing::Dispatching(window)));
        assert_ne!(app.conversation.attempt(), old_attempt);

        let _ = app.update(Message::AuthResult {
            attempt: old_attempt,
            result: Ok((
                None,
                auth::Response::Prompt {
                    secret: true,
                    message: "Late prompt".into(),
                },
            )),
        });

        assert_eq!(app.phase, Phase::CreatingSession);
        assert_eq!(app.conversation.prompt(), "Password");
    }

    #[test]
    fn idle_close_without_a_client_enters_bounded_cleanup() {
        let mut app = app();
        let window = window::Id::unique();
        let old_attempt = app.conversation.attempt();
        app.phase = Phase::Failed;

        let _ = app.update(Message::CloseRequested(window));

        assert_eq!(app.closing, Some(Closing::Cancelling(window)));
        assert_ne!(app.conversation.attempt(), old_attempt);
        let closing_attempt = app.conversation.attempt();

        let _ = app.update(Message::Retry);

        assert_eq!(app.phase, Phase::Failed);
        assert_eq!(app.conversation.attempt(), closing_attempt);
        assert_eq!(app.closing, Some(Closing::Cancelling(window)));
    }

    #[test]
    fn retry_is_ignored_after_cancellation_dispatches_close() {
        let mut app = app();
        let window = window::Id::unique();
        let attempt = app.conversation.attempt();
        app.phase = Phase::Failed;
        app.closing = Some(Closing::Cancelling(window));

        let _ = app.update(Message::SessionCancelled(window));
        assert_eq!(app.closing, Some(Closing::Dispatching(window)));

        let _ = app.update(Message::Retry);

        assert_eq!(app.phase, Phase::Failed);
        assert_eq!(app.conversation.attempt(), attempt);
        assert_eq!(app.closing, Some(Closing::Dispatching(window)));
    }
}
