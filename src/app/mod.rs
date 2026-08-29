mod auth_flow;
mod preview;
mod view;

use std::time::{Duration, Instant};

use auth_flow::{Attempt, Phase};
use chrono::Local;
use greetd_ipc::Request;
use iced::widget::text_input;
use iced::{time, window, Subscription, Task};

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

#[derive(Debug)]
pub(crate) struct App {
    username: String,
    display_name: String,
    accounts: Vec<Account>,
    input: String,
    input_id: text_input::Id,
    prompt: String,
    message: Option<String>,
    message_is_error: bool,
    secret: bool,
    phase: Phase,
    client: Option<Client>,
    sessions: Vec<Session>,
    selected_session: Option<Session>,
    started_at: Instant,
    now: chrono::DateTime<Local>,
    power_state: PowerState,
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
    AuthResult {
        attempt: Attempt,
        result: Result<(Option<Client>, auth::Response), String>,
    },
    AccountsResult(Result<Vec<Account>, String>),
    ChangeUser,
    UserSelectionCancelled(Result<(), String>),
    RetryUserSelectionCancellation,
    SelectAccount(Account),
    SelectSession(Session),
    AskPower(PowerAction),
    CancelPower,
    ConfirmPower(PowerAction),
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
        let configured = selected_session.is_some() && account.is_some();
        let discovering = selected_session.is_some() && account.is_none();
        let attempt = Attempt::initial();

        let app = Self {
            username: account
                .as_ref()
                .map(|account| account.username.clone())
                .unwrap_or_default(),
            display_name: account
                .as_ref()
                .map(|account| account.display_name.clone())
                .unwrap_or_else(|| "Select account".into()),
            accounts,
            input: String::new(),
            input_id: text_input::Id::new("authentication-input"),
            prompt: "Password".into(),
            message: selected_session
                .is_none()
                .then(|| "No valid Wayland sessions are installed".into()),
            message_is_error: selected_session.is_none(),
            secret: true,
            phase: if configured {
                Phase::CreatingSession
            } else if discovering {
                Phase::DiscoveringUsers
            } else {
                Phase::Failed
            },
            client: None,
            sessions,
            selected_session,
            started_at: Instant::now(),
            now: Local::now(),
            power_state: PowerState::Idle,
            attempt,
            selection_session_cancelled: false,
            closing: None,
            preview: false,
        };
        let task = if configured {
            auth_flow::begin(app.username.clone(), attempt, true)
        } else if discovering {
            discover_accounts()
        } else {
            Task::none()
        };
        (app, task)
    }

    pub(crate) fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            time::every(Duration::from_millis(50)).map(|_| Message::Tick),
            window::close_requests().map(Message::CloseRequested),
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
                    Message::UserSelectionCancelled(_) | Message::CloseTimeout(_)
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
                    self.phase = Phase::SelectingUser;
                    self.message = Some("Select an account".into());
                    self.message_is_error = false;
                    Task::none()
                }
            }
            Message::AccountsResult(Err(error)) => self.fail(error),
            Message::ChangeUser if self.can_change_user() => self.change_user(),
            Message::ChangeUser => Task::none(),
            Message::UserSelectionCancelled(_)
                if matches!(
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
            Message::UserSelectionCancelled(Ok(()))
                if self.phase == Phase::CancellingForUserSelection =>
            {
                self.phase = Phase::SelectingUser;
                self.selection_session_cancelled = true;
                self.message = Some("Select an account".into());
                self.message_is_error = false;
                Task::none()
            }
            Message::UserSelectionCancelled(Err(error))
                if self.phase == Phase::CancellingForUserSelection =>
            {
                self.phase = Phase::UserSelectionCancellationFailed;
                self.message = Some(auth_flow::bounded_auth_text(&error));
                self.message_is_error = true;
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
                auth_flow::cancel_for_user_selection(None)
            }
            Message::UserSelectionCancelled(_) | Message::RetryUserSelectionCancellation => {
                Task::none()
            }
            Message::SelectAccount(account)
                if self.can_select_account() && account.username != self.username =>
            {
                self.select_account(account)
            }
            Message::SelectAccount(_) => Task::none(),
            Message::SelectSession(session) if self.can_select_session() => {
                self.selected_session = Some(session);
                Task::none()
            }
            Message::SelectSession(_) => Task::none(),
            Message::Retry if self.phase == Phase::Failed && self.selected_session.is_none() => {
                if self.preview {
                    self.message = Some("Preview: retry was not sent".into());
                    self.message_is_error = false;
                    return Task::none();
                }
                self.sessions = sessions::discover();
                self.selected_session = self.sessions.first().cloned();
                if self.selected_session.is_none() {
                    self.message = Some("No valid Wayland sessions are installed".into());
                    self.message_is_error = true;
                    return Task::none();
                }
                if self.username.is_empty() {
                    self.phase = Phase::DiscoveringUsers;
                    self.message = None;
                    discover_accounts()
                } else {
                    self.retry_authentication()
                }
            }
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
                    self.message = Some("Preview: credentials were not sent".into());
                    self.message_is_error = false;
                    return text_input::focus(self.input_id.clone());
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
                Task::none()
            }
            Message::AskPower(_) => Task::none(),
            Message::CancelPower if matches!(self.power_state, PowerState::Confirming(_)) => {
                self.power_state = PowerState::Idle;
                if self.phase == Phase::WaitingForInput {
                    text_input::focus(self.input_id.clone())
                } else {
                    Task::none()
                }
            }
            Message::CancelPower => Task::none(),
            Message::ConfirmPower(action) if self.power_state == PowerState::Confirming(action) => {
                if self.preview {
                    self.power_state = PowerState::Idle;
                    self.message = Some(format!(
                        "Preview: {} was not requested",
                        action.label().to_lowercase()
                    ));
                    self.message_is_error = false;
                    return if self.phase == Phase::WaitingForInput {
                        text_input::focus(self.input_id.clone())
                    } else {
                        Task::none()
                    };
                }
                self.power_state = PowerState::Executing(action);
                self.message = Some(format!("Requesting {}…", action.label().to_lowercase()));
                self.message_is_error = false;
                Task::perform(power::execute(action), |result| {
                    Message::PowerResult(result.map_err(|error| error.to_string()))
                })
            }
            Message::ConfirmPower(_) => Task::none(),
            Message::PowerResult(Ok(()))
                if self.power_state == PowerState::Executing(PowerAction::Suspend) =>
            {
                self.power_state = PowerState::Idle;
                self.message = None;
                if self.phase == Phase::WaitingForInput {
                    text_input::focus(self.input_id.clone())
                } else {
                    Task::none()
                }
            }
            Message::PowerResult(Ok(())) => Task::none(),
            Message::PowerResult(Err(error)) => {
                self.power_state = PowerState::Idle;
                self.message = Some(auth_flow::bounded_auth_text(&error));
                self.message_is_error = true;
                if self.phase == Phase::WaitingForInput {
                    text_input::focus(self.input_id.clone())
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
        self.input.clear();
        self.message = None;
        self.message_is_error = false;
        self.phase = Phase::CreatingSession;
        let attempt = self.attempt.advance();
        let recover = !self.selection_session_cancelled;
        self.selection_session_cancelled = false;
        if self.preview {
            self.phase = Phase::WaitingForInput;
            self.message = Some("Preview mode: credentials and power actions are simulated".into());
            text_input::focus(self.input_id.clone())
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
        self.attempt.advance();
        self.username.clear();
        self.display_name = "Select account".into();
        self.input.clear();
        self.selection_session_cancelled = false;
        self.phase = Phase::CancellingForUserSelection;
        self.message = Some("Changing user…".into());
        self.message_is_error = false;
        if self.preview {
            self.finish_preview_user_selection()
        } else {
            auth_flow::cancel_for_user_selection(client)
        }
    }

    fn finish_preview_user_selection(&mut self) -> Task<Message> {
        self.phase = Phase::SelectingUser;
        self.message = Some("Select an account".into());
        Task::none()
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
            input: "secret".into(),
            input_id: text_input::Id::new("test-authentication-input"),
            prompt: "Password".into(),
            message: Some("Keep this message".into()),
            message_is_error: false,
            secret: true,
            phase: Phase::WaitingForInput,
            client: None,
            sessions: vec![session()],
            selected_session: Some(session()),
            started_at: Instant::now(),
            now: Local::now(),
            power_state: PowerState::Idle,
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
            app.message.as_deref(),
            Some("Preview: credentials were not sent")
        );
        assert!(!app.message_is_error);
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
            app.message.as_deref(),
            Some("Preview: sleep was not requested")
        );
        assert!(!app.message_is_error);
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
    fn multiple_discovered_accounts_require_selection() {
        let mut app = app();
        app.phase = Phase::DiscoveringUsers;

        let _ = app.update(Message::AccountsResult(Ok(vec![
            account("alice"),
            account("bob"),
        ])));

        assert_eq!(app.phase, Phase::SelectingUser);
        assert_eq!(app.accounts.len(), 2);
        assert_eq!(app.message.as_deref(), Some("Select an account"));
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

        let _ = app.update(Message::UserSelectionCancelled(Ok(())));
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
    fn cancellation_failure_keeps_account_selection_safe_and_retryable() {
        let mut app = app();
        app.accounts = vec![account("darwin"), account("alice")];

        let _ = app.update(Message::ChangeUser);
        let _ = app.update(Message::UserSelectionCancelled(Err(
            "greetd unavailable".into()
        )));

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
    fn preview_change_user_is_immediate_and_does_not_dispatch_cancellation() {
        let mut app = app();
        app.preview = true;
        app.accounts = vec![account("darwin"), account("alice")];

        let _ = app.update(Message::ChangeUser);

        assert_eq!(app.phase, Phase::SelectingUser);
        assert!(app.username.is_empty());
        assert_eq!(app.message.as_deref(), Some("Select an account"));
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

        app.input = "123456".into();
        let _ = app.handle_auth_response(auth::Response::Prompt {
            secret: true,
            message: "Password:".into(),
        });
        assert_eq!(app.prompt, "Password");
        assert!(app.secret);
        assert!(app.input.is_empty());
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
    fn power_failures_preserve_authentication_state() {
        let mut app = app();
        app.power_state = PowerState::Executing(PowerAction::Reboot);
        let _ = app.update(Message::PowerResult(Err("not authorized".into())));

        assert_eq!(app.phase, Phase::WaitingForInput);
        assert_eq!(app.input, "secret");
        assert_eq!(app.power_state, PowerState::Idle);
        assert_eq!(app.message.as_deref(), Some("not authorized"));
        assert!(app.message_is_error);
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
    fn successful_suspend_restores_the_greeter() {
        let mut app = app();
        app.power_state = PowerState::Executing(PowerAction::Suspend);
        app.message = Some("Requesting sleep…".into());

        let _ = app.update(Message::PowerResult(Ok(())));

        assert_eq!(app.power_state, PowerState::Idle);
        assert_eq!(app.message, None);
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

        let _ = app.update(Message::UserSelectionCancelled(Err(
            "greetd unavailable".into()
        )));

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
