mod account_tile;
mod auth_flow;
mod modal;
mod preview;
mod view;

use std::time::{Duration, Instant};

use auth_flow::{Attempt, Phase};
use chrono::Local;
use greetd_ipc::Request;
use iced::widget::{scrollable, text_input};
use iced::{event, keyboard, time, window, Subscription, Task};

use crate::accounts::{self, Account};
use crate::power::{self, Action as PowerAction};
use crate::sessions::{self, Session};
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
pub(crate) enum PowerDialogFocus {
    Cancel,
    Confirm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PowerDialogNavigation {
    Next,
    Previous,
    Activate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AccountNavigation {
    Next,
    Previous,
    Activate,
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

pub(crate) struct Config {
    pub(crate) username: Option<String>,
    pub(crate) display_name: Option<String>,
    pub(crate) preview: Option<PreviewFixture>,
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
    focused_account: Option<usize>,
    account_scroll_id: scrollable::Id,
    page_scroll_id: scrollable::Id,
    input: String,
    input_id: text_input::Id,
    prompt: String,
    message: Option<String>,
    message_is_error: bool,
    session_message: Option<String>,
    power_message: Option<String>,
    power_message_is_error: bool,
    preview_message: Option<String>,
    secret: bool,
    phase: Phase,
    client: Option<Client>,
    sessions: Vec<Session>,
    selected_session: Option<Session>,
    started_at: Instant,
    now: chrono::DateTime<Local>,
    power_state: PowerState,
    power_dialog_focus: PowerDialogFocus,
    attempt: Attempt,
    selection_session_cancelled: bool,
    closing: Option<Closing>,
    preview: bool,
}

#[derive(Debug, Clone)]
pub(crate) enum Message {
    Tick,
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
    NavigateAccount(AccountNavigation),
    NavigatePage(PageNavigation),
    SelectSession(Session),
    AskPower(PowerAction),
    CancelPower,
    ConfirmPower(PowerAction),
    NavigatePowerDialog(PowerDialogNavigation),
    PowerResult(Result<(), String>),
    CloseRequested(window::Id),
    SessionCancelled(window::Id),
    CloseTimeout(window::Id),
}

impl App {
    pub(crate) fn new(config: Config) -> (Self, Task<Message>) {
        if let Some(fixture) = config.preview {
            return preview::build(fixture, config.username, config.display_name);
        }
        let sessions = sessions::discover();
        let selected_session = sessions.first().cloned();
        let account = config
            .username
            .map(|username| Account::override_account(username, config.display_name));
        let accounts = account.iter().cloned().collect();
        let startup = startup_mode(selected_session.is_some(), account.is_some());
        let attempt = Attempt::initial();

        let app = Self {
            username: account
                .as_ref()
                .map(|account| account.username.clone())
                .unwrap_or_default(),
            display_name: account
                .as_ref()
                .map(|account| account.display_name.clone())
                .unwrap_or_else(|| "Select a user".into()),
            accounts,
            focused_account: None,
            account_scroll_id: scrollable::Id::unique(),
            page_scroll_id: scrollable::Id::unique(),
            input: String::new(),
            input_id: text_input::Id::new("authentication-input"),
            prompt: "Password".into(),
            message: None,
            message_is_error: false,
            session_message: selected_session
                .is_none()
                .then(|| "No valid Wayland sessions are installed".into()),
            power_message: None,
            power_message_is_error: false,
            preview_message: None,
            secret: true,
            phase: match startup {
                StartupMode::ConfiguredIdentity => Phase::CreatingSession,
                StartupMode::DiscoverAccounts => Phase::DiscoveringUsers,
                StartupMode::MissingSession => Phase::Failed,
            },
            client: None,
            sessions,
            selected_session,
            started_at: Instant::now(),
            now: Local::now(),
            power_state: PowerState::Idle,
            power_dialog_focus: PowerDialogFocus::Cancel,
            attempt,
            selection_session_cancelled: false,
            closing: None,
            preview: false,
        };
        let task = match startup {
            StartupMode::ConfiguredIdentity => {
                auth_flow::begin(app.username.clone(), attempt, true)
            }
            StartupMode::DiscoverAccounts => discover_accounts(),
            StartupMode::MissingSession => Task::none(),
        };
        (app, task)
    }

    pub(crate) fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            time::every(Duration::from_millis(50)).map(|_| Message::Tick),
            window::close_requests().map(Message::CloseRequested),
            keyboard::on_key_press(account_navigation),
            event::listen_with(page_navigation),
            event::listen_with(cancel_shortcut),
            event::listen_with(power_dialog_navigation),
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
                    | Message::AuthResult { .. }
                    | Message::AccountsResult(_)
                    | Message::CloseRequested(_)
                    | Message::SessionCancelled(_)
                    | Message::CloseTimeout(_),
                ) => true,
                (PowerState::Confirming(_), Message::CancelPower) => true,
                (PowerState::Confirming(_), Message::NavigatePowerDialog(_)) => true,
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
            Message::Tick if self.preview => Task::none(),
            Message::Tick => {
                self.now = Local::now();
                Task::none()
            }
            Message::InputChanged(value) if self.phase == Phase::WaitingForInput => {
                self.input = value;
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
                    self.focused_account = Some(0);
                    self.phase = Phase::SelectingUser;
                    self.message = Some("Select a user".into());
                    self.message_is_error = false;
                    self.scroll_to_focused_account()
                }
            }
            Message::AccountsResult(Err(error)) => self.fail(error),
            Message::ChangeUser if self.can_change_user() => self.change_user(),
            Message::ChangeUser => Task::none(),
            Message::UserSelectionCancelled { attempt, .. }
                if attempt == self.attempt
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
            } if attempt == self.attempt && self.phase == Phase::CancellingForUserSelection => {
                self.phase = Phase::SelectingUser;
                self.selection_session_cancelled = true;
                self.message = Some("Select a user".into());
                self.message_is_error = false;
                self.scroll_to_focused_account()
            }
            Message::UserSelectionCancelled {
                attempt,
                result: Err(error),
            } if attempt == self.attempt && self.phase == Phase::CancellingForUserSelection => {
                self.phase = Phase::UserSelectionCancellationFailed;
                self.message = Some(auth_flow::bounded_auth_text(&error));
                self.message_is_error = true;
                Task::none()
            }
            Message::UserSelectionCancellationSlow { attempt }
                if attempt == self.attempt && self.phase == Phase::CancellingForUserSelection =>
            {
                self.message = Some("Still changing user…".into());
                self.message_is_error = false;
                Task::none()
            }
            Message::RetryUserSelectionCancellation
                if self.phase == Phase::UserSelectionCancellationFailed =>
            {
                self.phase = Phase::CancellingForUserSelection;
                self.message = Some("Changing user…".into());
                self.message_is_error = false;
                if self.preview {
                    return self.finish_preview_user_selection();
                }
                let attempt = self.attempt.advance();
                auth_flow::cancel_for_user_selection(None, attempt)
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
            Message::NavigateAccount(navigation) if self.can_select_account() => {
                self.navigate_account(navigation)
            }
            Message::NavigateAccount(_) => Task::none(),
            Message::NavigatePage(navigation) => self.navigate_page(navigation),
            Message::SelectSession(session) if self.can_select_session() => {
                self.selected_session = Some(session);
                Task::none()
            }
            Message::SelectSession(_) => Task::none(),
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
                    self.message = None;
                    discover_accounts()
                } else {
                    self.retry_authentication()
                }
            }
            Message::RetrySession => Task::none(),
            Message::Retry if self.phase == Phase::Failed && self.username.is_empty() => {
                if self.preview {
                    self.message = Some("Preview: retry was not sent".into());
                    self.message_is_error = false;
                    return Task::none();
                }
                self.phase = Phase::DiscoveringUsers;
                self.message = None;
                discover_accounts()
            }
            Message::Retry if self.phase == Phase::Failed && self.preview => {
                self.message = Some("Preview: retry was not sent".into());
                self.message_is_error = false;
                Task::none()
            }
            Message::Retry if self.phase == Phase::Failed => self.retry_authentication(),
            Message::Retry => Task::none(),
            Message::Submit if self.phase == Phase::WaitingForInput => {
                if self.preview {
                    self.input.clear();
                    self.preview_message = Some("Preview mode: credentials were not sent".into());
                    return self.focus_input();
                }
                let Some(client) = self.client.clone() else {
                    return self.fail("Lost connection to greetd".into());
                };
                let response = std::mem::take(&mut self.input);
                self.phase = Phase::Authenticating;
                auth_flow::exchange(
                    client,
                    Request::PostAuthMessageResponse {
                        response: Some(response),
                    },
                    self.attempt,
                )
            }
            Message::Submit => Task::none(),
            Message::AuthResult { attempt, result } => self.handle_auth_result(attempt, result),
            Message::AskPower(action) if self.can_request_power() => {
                self.power_state = PowerState::Confirming(action);
                self.power_dialog_focus = PowerDialogFocus::Cancel;
                self.power_message = None;
                Task::none()
            }
            Message::AskPower(_) => Task::none(),
            Message::CancelPower if matches!(self.power_state, PowerState::Confirming(_)) => {
                self.power_state = PowerState::Idle;
                if self.phase == Phase::WaitingForInput {
                    self.focus_input()
                } else {
                    Task::none()
                }
            }
            Message::CancelPower => Task::none(),
            Message::NavigatePowerDialog(navigation)
                if matches!(self.power_state, PowerState::Confirming(_)) =>
            {
                match navigation {
                    PowerDialogNavigation::Next | PowerDialogNavigation::Previous => {
                        self.power_dialog_focus = match self.power_dialog_focus {
                            PowerDialogFocus::Cancel => PowerDialogFocus::Confirm,
                            PowerDialogFocus::Confirm => PowerDialogFocus::Cancel,
                        };
                        Task::none()
                    }
                    PowerDialogNavigation::Activate => match self.power_dialog_focus {
                        PowerDialogFocus::Cancel => self.update(Message::CancelPower),
                        PowerDialogFocus::Confirm => {
                            let PowerState::Confirming(action) = self.power_state else {
                                unreachable!();
                            };
                            self.update(Message::ConfirmPower(action))
                        }
                    },
                }
            }
            Message::NavigatePowerDialog(_) => Task::none(),
            Message::ConfirmPower(action) if self.power_state == PowerState::Confirming(action) => {
                if self.preview {
                    self.power_state = PowerState::Idle;
                    self.power_message = Some(format!(
                        "Preview: {} was not requested",
                        action.label().to_lowercase()
                    ));
                    self.power_message_is_error = false;
                    return if self.phase == Phase::WaitingForInput {
                        self.focus_input()
                    } else {
                        Task::none()
                    };
                }
                self.power_state = PowerState::Executing(action);
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
                if self.phase == Phase::WaitingForInput {
                    self.focus_input()
                } else {
                    Task::none()
                }
            }
            Message::PowerResult(Ok(())) => Task::none(),
            Message::PowerResult(Err(error)) => {
                self.power_state = PowerState::Idle;
                self.power_message = Some(auth_flow::bounded_auth_text(&error));
                self.power_message_is_error = true;
                if self.phase == Phase::WaitingForInput {
                    self.focus_input()
                } else {
                    Task::none()
                }
            }
            Message::CloseRequested(window) if self.preview => window::close(window),
            Message::CloseRequested(window) if self.selection_session_cancelled => {
                self.closing = Some(Closing::Dispatching(window));
                window::close(window)
            }
            Message::CloseRequested(window) if self.phase == Phase::CancellingForUserSelection => {
                self.closing = Some(Closing::WaitingForUserSelectionCancellation(window));
                auth_flow::close_timeout(window)
            }
            Message::CloseRequested(window) if self.client.is_some() => {
                self.attempt.advance();
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
                self.attempt.advance();
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
                self.attempt.advance();
                self.closing = Some(Closing::Dispatching(window));
                window::close(window)
            }
            Message::SessionCancelled(_) | Message::CloseTimeout(_) => Task::none(),
        }
    }

    fn select_account(&mut self, account: Account) -> Task<Message> {
        let replacing_account = !self.username.is_empty();
        self.username = account.username;
        self.display_name = account.display_name;
        self.focused_account = None;
        self.input.clear();
        self.message = None;
        self.message_is_error = false;
        self.phase = Phase::CreatingSession;
        let attempt = self.attempt.advance();
        let recover = !self.selection_session_cancelled;
        self.selection_session_cancelled = false;
        if self.preview {
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
        self.message = None;
        self.message_is_error = false;
        self.phase = Phase::CreatingSession;
        let client = self.client.take();
        let attempt = self.attempt.advance();
        auth_flow::restart(client, self.username.clone(), attempt)
    }

    fn change_user(&mut self) -> Task<Message> {
        let client = self.client.take();
        let attempt = self.attempt.advance();
        self.username.clear();
        self.display_name = "Select a user".into();
        self.focused_account = (!self.accounts.is_empty()).then_some(0);
        self.input.clear();
        self.selection_session_cancelled = false;
        self.phase = Phase::CancellingForUserSelection;
        self.message = Some("Changing user…".into());
        self.message_is_error = false;
        if self.preview {
            self.finish_preview_user_selection()
        } else {
            Task::batch([
                auth_flow::cancel_for_user_selection(client, attempt),
                self.scroll_to_focused_account(),
            ])
        }
    }

    fn finish_preview_user_selection(&mut self) -> Task<Message> {
        self.phase = Phase::SelectingUser;
        self.message = Some("Select a user".into());
        self.scroll_to_focused_account()
    }

    fn navigate_account(&mut self, navigation: AccountNavigation) -> Task<Message> {
        let Some(current) = self.focused_account else {
            self.focused_account = (!self.accounts.is_empty()).then_some(0);
            return self.scroll_to_focused_account();
        };
        match navigation {
            AccountNavigation::Next => {
                self.focused_account = Some((current + 1) % self.accounts.len());
                self.scroll_to_focused_account()
            }
            AccountNavigation::Previous => {
                self.focused_account =
                    Some((current + self.accounts.len() - 1) % self.accounts.len());
                self.scroll_to_focused_account()
            }
            AccountNavigation::Activate => {
                let account = self.accounts[current].clone();
                self.select_account(account)
            }
        }
    }

    fn scroll_to_focused_account(&self) -> Task<Message> {
        let Some(index) = self.focused_account else {
            return Task::none();
        };
        account_tile::reveal(
            account_tile::id(&self.accounts[index].username),
            vec![self.account_scroll_id.clone(), self.page_scroll_id.clone()],
        )
    }

    pub(super) fn focus_input(&self) -> Task<Message> {
        Task::batch([
            text_input::focus(self.input_id.clone()),
            account_tile::reveal(
                iced::advanced::widget::Id::new("authentication-input-anchor"),
                vec![self.page_scroll_id.clone()],
            ),
        ])
    }

    fn navigate_page(&self, navigation: PageNavigation) -> Task<Message> {
        let scrollables = [self.page_scroll_id.clone(), self.account_scroll_id.clone()];
        Task::batch(scrollables.map(|id| match navigation {
            PageNavigation::Up => {
                scrollable::scroll_by(id, scrollable::AbsoluteOffset { x: 0.0, y: -400.0 })
            }
            PageNavigation::Down => {
                scrollable::scroll_by(id, scrollable::AbsoluteOffset { x: 0.0, y: 400.0 })
            }
            PageNavigation::Start => {
                scrollable::snap_to(id, scrollable::RelativeOffset { x: 0.0, y: 0.0 })
            }
            PageNavigation::End => {
                scrollable::snap_to(id, scrollable::RelativeOffset { x: 0.0, y: 1.0 })
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

fn account_navigation(key: keyboard::Key, modifiers: keyboard::Modifiers) -> Option<Message> {
    use keyboard::key::Named;

    let message = match key.as_ref() {
        keyboard::Key::Named(Named::Tab) if modifiers.shift() => AccountNavigation::Previous,
        keyboard::Key::Named(Named::Tab | Named::ArrowRight | Named::ArrowDown) => {
            AccountNavigation::Next
        }
        keyboard::Key::Named(Named::ArrowLeft | Named::ArrowUp) => AccountNavigation::Previous,
        keyboard::Key::Named(Named::Enter | Named::Space) => AccountNavigation::Activate,
        _ => return None,
    };
    Some(Message::NavigateAccount(message))
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
    .then_some(Message::CancelPower)
}

fn power_dialog_navigation(
    event: iced::Event,
    _status: event::Status,
    _window: window::Id,
) -> Option<Message> {
    let iced::Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) = event else {
        return None;
    };
    use keyboard::key::Named;
    match key.as_ref() {
        keyboard::Key::Named(Named::Tab) if modifiers.shift() => Some(
            Message::NavigatePowerDialog(PowerDialogNavigation::Previous),
        ),
        keyboard::Key::Named(Named::Tab | Named::ArrowLeft | Named::ArrowRight) => {
            Some(Message::NavigatePowerDialog(PowerDialogNavigation::Next))
        }
        keyboard::Key::Named(Named::Enter | Named::Space) => Some(Message::NavigatePowerDialog(
            PowerDialogNavigation::Activate,
        )),
        _ => None,
    }
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
        let mut attempt = Attempt::initial();
        attempt.advance();
        App {
            username: "darwin".into(),
            display_name: "Darwin".into(),
            accounts: Vec::new(),
            focused_account: None,
            account_scroll_id: scrollable::Id::unique(),
            page_scroll_id: scrollable::Id::unique(),
            input: "secret".into(),
            input_id: text_input::Id::new("test-authentication-input"),
            prompt: "Password".into(),
            message: Some("Keep this message".into()),
            message_is_error: false,
            session_message: None,
            power_message: None,
            power_message_is_error: false,
            preview_message: None,
            secret: true,
            phase: Phase::WaitingForInput,
            client: None,
            sessions: vec![session()],
            selected_session: Some(session()),
            started_at: Instant::now(),
            now: Local::now(),
            power_state: PowerState::Idle,
            power_dialog_focus: PowerDialogFocus::Cancel,
            attempt,
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
        assert_eq!(app.input, "not-a-real-password");

        let _ = app.update(Message::Submit);
        assert_eq!(app.phase, Phase::WaitingForInput);
        assert!(app.input.is_empty());
        assert_eq!(
            app.preview_message.as_deref(),
            Some("Preview mode: credentials were not sent")
        );
        assert_eq!(app.message.as_deref(), Some("Keep this message"));
    }

    #[test]
    fn preview_never_dispatches_power_actions() {
        let mut app = app();
        app.preview = true;

        let _ = app.update(Message::AskPower(PowerAction::Suspend));
        assert_eq!(
            app.power_state,
            PowerState::Confirming(PowerAction::Suspend)
        );

        let _ = app.update(Message::ConfirmPower(PowerAction::Suspend));
        assert_eq!(app.power_state, PowerState::Idle);
        assert_eq!(
            app.power_message.as_deref(),
            Some("Preview: sleep was not requested")
        );
        assert!(!app.power_message_is_error);
        assert_eq!(app.message.as_deref(), Some("Keep this message"));
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
        assert_eq!(app.message.as_deref(), Some("Select a user"));
    }

    #[test]
    fn change_user_waits_for_cancellation_before_selecting_an_account() {
        let mut app = app();
        app.accounts = vec![account("darwin"), account("alice")];
        let attempt = app.attempt;

        let _ = app.update(Message::ChangeUser);

        assert_eq!(app.phase, Phase::CancellingForUserSelection);
        assert!(app.username.is_empty());
        assert!(app.input.is_empty());
        assert_ne!(app.attempt, attempt);
        assert!(!app.can_select_account());

        let _ = app.update(Message::SelectAccount(account("alice")));
        assert!(app.username.is_empty());

        let _ = app.update(Message::UserSelectionCancelled {
            attempt: app.attempt,
            result: Ok(()),
        });
        assert_eq!(app.phase, Phase::SelectingUser);
        assert!(app.can_select_account());
        assert!(app.selection_session_cancelled);

        let _ = app.update(Message::SelectAccount(account("alice")));

        assert_eq!(app.phase, Phase::CreatingSession);
        assert_eq!(app.username, "alice");
        assert_eq!(app.display_name, "ALICE");
        assert!(app.input.is_empty());
        assert!(app.message.is_none());
        assert!(!app.selection_session_cancelled);
    }

    #[test]
    fn repeated_change_user_is_ignored_while_cancellation_is_in_flight() {
        let mut app = app();
        app.accounts = vec![account("darwin"), account("alice")];

        let _ = app.update(Message::ChangeUser);
        let attempt = app.attempt;
        let _ = app.update(Message::ChangeUser);

        assert_eq!(app.phase, Phase::CancellingForUserSelection);
        assert_eq!(app.attempt, attempt);
        assert_eq!(app.message.as_deref(), Some("Changing user…"));
        assert!(app.input.is_empty());
    }

    #[test]
    fn failed_authentication_can_change_account_without_restarting() {
        let mut app = app();
        app.phase = Phase::Failed;
        app.accounts = vec![account("darwin"), account("alice")];
        let attempt = app.attempt;

        let _ = app.update(Message::ChangeUser);

        assert_eq!(app.phase, Phase::CancellingForUserSelection);
        assert_ne!(app.attempt, attempt);
        assert!(app.username.is_empty());
        assert!(app.input.is_empty());
    }

    #[test]
    fn cancellation_failure_keeps_account_selection_safe_and_retryable() {
        let mut app = app();
        app.accounts = vec![account("darwin"), account("alice")];

        let _ = app.update(Message::ChangeUser);
        let _ = app.update(Message::UserSelectionCancelled {
            attempt: app.attempt,
            result: Err("greetd unavailable".into()),
        });

        assert_eq!(app.phase, Phase::UserSelectionCancellationFailed);
        assert_eq!(app.message.as_deref(), Some("greetd unavailable"));
        assert!(app.message_is_error);
        assert!(!app.can_select_account());

        let _ = app.update(Message::SelectAccount(account("alice")));
        assert!(app.username.is_empty());

        let _ = app.update(Message::RetryUserSelectionCancellation);
        assert_eq!(app.phase, Phase::CancellingForUserSelection);
        assert_eq!(app.message.as_deref(), Some("Changing user…"));
    }

    #[test]
    fn slow_cancellation_remains_authoritative_and_blocks_retry() {
        let mut app = app();
        app.accounts = vec![account("darwin"), account("alice")];

        let _ = app.update(Message::ChangeUser);
        let attempt = app.attempt;
        let _ = app.update(Message::UserSelectionCancellationSlow { attempt });

        assert_eq!(app.phase, Phase::CancellingForUserSelection);
        assert_eq!(app.message.as_deref(), Some("Still changing user…"));
        assert!(!app.message_is_error);
        assert!(!app.can_select_account());

        let _ = app.update(Message::RetryUserSelectionCancellation);
        let _ = app.update(Message::SelectAccount(account("alice")));

        assert_eq!(app.phase, Phase::CancellingForUserSelection);
        assert_eq!(app.attempt, attempt);
        assert!(app.username.is_empty());
    }

    #[test]
    fn stale_progress_does_not_mark_a_new_cancellation_as_slow() {
        let mut app = app();
        app.accounts = vec![account("darwin"), account("alice")];

        let _ = app.update(Message::ChangeUser);
        let old_attempt = app.attempt;
        let _ = app.update(Message::UserSelectionCancelled {
            attempt: old_attempt,
            result: Err("greetd unavailable".into()),
        });
        let _ = app.update(Message::RetryUserSelectionCancellation);
        let current_attempt = app.attempt;

        assert_ne!(current_attempt, old_attempt);
        assert_eq!(app.message.as_deref(), Some("Changing user…"));

        let _ = app.update(Message::UserSelectionCancellationSlow {
            attempt: old_attempt,
        });

        assert_eq!(app.attempt, current_attempt);
        assert_eq!(app.message.as_deref(), Some("Changing user…"));
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
        assert_eq!(app.message.as_deref(), Some("Select a user"));
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
        app.focused_account = Some(0);

        let _ = app.update(Message::NavigateAccount(AccountNavigation::Previous));
        assert_eq!(app.focused_account, Some(1));

        let _ = app.update(Message::NavigateAccount(AccountNavigation::Next));
        assert_eq!(app.focused_account, Some(0));

        let _ = app.update(Message::NavigateAccount(AccountNavigation::Activate));
        assert_eq!(app.username, "alice");
        assert_eq!(app.phase, Phase::WaitingForInput);
        assert_eq!(app.focused_account, None);
    }

    #[test]
    fn keyboard_navigation_is_ignored_while_account_tiles_are_disabled() {
        let mut app = app();
        app.phase = Phase::CancellingForUserSelection;
        app.accounts = vec![account("alice"), account("bob")];
        app.focused_account = Some(0);

        let _ = app.update(Message::NavigateAccount(AccountNavigation::Next));
        let _ = app.update(Message::NavigateAccount(AccountNavigation::Activate));

        assert_eq!(app.focused_account, Some(0));
        assert_eq!(app.username, "darwin");
        assert_eq!(app.phase, Phase::CancellingForUserSelection);
    }

    #[test]
    fn maps_account_selection_keys_without_capturing_other_input() {
        use keyboard::key::Named;

        assert!(matches!(
            account_navigation(keyboard::Key::Named(Named::Tab), keyboard::Modifiers::SHIFT),
            Some(Message::NavigateAccount(AccountNavigation::Previous))
        ));
        assert!(matches!(
            account_navigation(
                keyboard::Key::Named(Named::Enter),
                keyboard::Modifiers::empty()
            ),
            Some(Message::NavigateAccount(AccountNavigation::Activate))
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
        assert!(account_navigation(
            keyboard::Key::Character("a".into()),
            keyboard::Modifiers::empty()
        )
        .is_none());
        assert!(matches!(
            cancel_shortcut_key(keyboard::Key::Named(Named::Escape)),
            Some(Message::CancelPower)
        ));
    }

    #[test]
    fn empty_account_discovery_reports_configuration_error() {
        let mut app = app();
        app.phase = Phase::DiscoveringUsers;

        let _ = app.update(Message::AccountsResult(Ok(Vec::new())));

        assert_eq!(app.phase, Phase::Failed);
        assert_eq!(
            app.message.as_deref(),
            Some("AccountsService found no unlocked non-system users")
        );
        assert!(app.message_is_error);
    }

    #[test]
    fn ignores_responses_from_abandoned_attempts() {
        let mut app = app();
        let _ = app.update(Message::AuthResult {
            attempt: Attempt::initial(),
            result: Ok((
                None,
                auth::Response::Error {
                    authentication: false,
                    description: "late failure".into(),
                },
            )),
        });

        assert_eq!(app.phase, Phase::WaitingForInput);
        assert_eq!(app.input, "secret");
        assert_eq!(app.message.as_deref(), Some("Keep this message"));
    }

    #[test]
    fn ignores_input_outside_a_pam_prompt() {
        let mut app = app();
        app.phase = Phase::Authenticating;

        let _ = app.update(Message::InputChanged("replacement".into()));
        let _ = app.update(Message::Submit);

        assert_eq!(app.input, "secret");
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
        let attempt = app.attempt;

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
        assert_eq!(app.attempt, attempt);
        assert_eq!(app.message.as_deref(), Some("Authentication failed"));

        let _ = app.update(Message::Retry);
        assert_eq!(app.phase, Phase::CreatingSession);
        assert_ne!(app.attempt, attempt);
    }

    #[test]
    fn prompt_sequences_replace_and_clear_the_previous_response() {
        let mut app = app();
        app.input = "previous response".into();

        let _ = app.handle_auth_response(auth::Response::Prompt {
            secret: false,
            message: "Login code:".into(),
        });
        assert_eq!(app.phase, Phase::WaitingForInput);
        assert_eq!(app.prompt, "Login code");
        assert!(!app.secret);
        assert!(app.input.is_empty());
        assert_eq!(app.message.as_deref(), Some("Keep this message"));

        app.set_auth_notice("First security-key instruction".into(), false);
        app.set_auth_notice("Security key accepted".into(), false);

        app.input = "123456".into();
        let _ = app.handle_auth_response(auth::Response::Prompt {
            secret: true,
            message: "Password:".into(),
        });
        assert_eq!(app.prompt, "Password");
        assert!(app.secret);
        assert!(app.input.is_empty());
        assert_eq!(app.message.as_deref(), Some("Security key accepted"));
        assert!(!app.message_is_error);
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

        assert_eq!(app.message.as_deref(), Some("Keep this message"));
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
            attempt: app.attempt,
            result: Err("greetd closed the socket".into()),
        });

        assert_eq!(app.phase, Phase::Failed);
        assert_eq!(app.message.as_deref(), Some("greetd closed the socket"));
        assert!(app.message_is_error);
    }

    #[test]
    fn successful_authentication_supersedes_the_previous_pam_notice() {
        let mut app = app();
        app.phase = Phase::Authenticating;
        app.message = Some("Previous PAM error".into());
        app.message_is_error = true;

        let transition = super::auth_flow::transition(
            app.phase,
            auth::Response::Success,
            app.selected_session.as_ref(),
        );
        assert!(matches!(
            app.apply_auth_transition(transition),
            super::auth_flow::AuthEffect::StartSession(_)
        ));

        assert_eq!(app.phase, Phase::StartingSession);
        assert!(app.message.is_none());
        assert!(!app.message_is_error);
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
        assert_eq!(app.input, "secret");
        assert_eq!(app.power_state, PowerState::Idle);
        assert_eq!(app.power_message.as_deref(), Some("not authorized"));
        assert!(app.power_message_is_error);
        assert_eq!(app.message.as_deref(), Some("Keep this message"));
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
        assert_eq!(app.input, "secret");
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
        assert_eq!(app.power_dialog_focus, PowerDialogFocus::Cancel);

        let _ = app.update(Message::NavigatePowerDialog(
            PowerDialogNavigation::Activate,
        ));
        assert_eq!(app.power_state, PowerState::Idle);

        let _ = app.update(Message::AskPower(PowerAction::PowerOff));
        let _ = app.update(Message::NavigatePowerDialog(PowerDialogNavigation::Next));
        assert_eq!(app.power_dialog_focus, PowerDialogFocus::Confirm);

        let _ = app.update(Message::NavigatePowerDialog(
            PowerDialogNavigation::Activate,
        ));
        assert_eq!(app.power_state, PowerState::Idle);
        assert_eq!(
            app.power_message.as_deref(),
            Some("Preview: shut down was not requested")
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
        assert_eq!(app.message.as_deref(), Some("Keep this message"));
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
            attempt: app.attempt,
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
            attempt: app.attempt,
            result: Err("socket closed".into()),
        });

        assert_eq!(app.closing, Some(Closing::Cancelling(window)));
    }

    #[test]
    fn close_timeout_invalidates_a_late_creation_result() {
        let mut app = app();
        let window = window::Id::unique();
        let old_attempt = app.attempt;
        app.phase = Phase::CreatingSession;
        app.closing = Some(Closing::WaitingForClient(window));

        let _ = app.update(Message::CloseTimeout(window));
        assert_eq!(app.closing, Some(Closing::Dispatching(window)));
        assert_ne!(app.attempt, old_attempt);

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
        assert_eq!(app.prompt, "Password");
    }

    #[test]
    fn idle_close_without_a_client_enters_bounded_cleanup() {
        let mut app = app();
        let window = window::Id::unique();
        let old_attempt = app.attempt;
        app.phase = Phase::Failed;

        let _ = app.update(Message::CloseRequested(window));

        assert_eq!(app.closing, Some(Closing::Cancelling(window)));
        assert_ne!(app.attempt, old_attempt);
        let closing_attempt = app.attempt;

        let _ = app.update(Message::Retry);

        assert_eq!(app.phase, Phase::Failed);
        assert_eq!(app.attempt, closing_attempt);
        assert_eq!(app.closing, Some(Closing::Cancelling(window)));
    }

    #[test]
    fn retry_is_ignored_after_cancellation_dispatches_close() {
        let mut app = app();
        let window = window::Id::unique();
        let attempt = app.attempt;
        app.phase = Phase::Failed;
        app.closing = Some(Closing::Cancelling(window));

        let _ = app.update(Message::SessionCancelled(window));
        assert_eq!(app.closing, Some(Closing::Dispatching(window)));

        let _ = app.update(Message::Retry);

        assert_eq!(app.phase, Phase::Failed);
        assert_eq!(app.attempt, attempt);
        assert_eq!(app.closing, Some(Closing::Dispatching(window)));
    }
}
