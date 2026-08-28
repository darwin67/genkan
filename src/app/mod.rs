mod auth_flow;
mod view;

use std::time::{Duration, Instant};

use auth_flow::{Attempt, Phase};
use chrono::Local;
use greetd_ipc::Request;
use iced::{time, window, Subscription, Task};

use crate::accounts::{self, Account};
use crate::power::{self, Action as PowerAction};
use crate::sessions::{self, Session};
use genkan::auth::{self, Client};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Closing {
    WaitingForClient(window::Id),
    Cancelling(window::Id),
    Dispatching(window::Id),
}

impl Closing {
    fn is_cleaning(self, window: window::Id) -> bool {
        matches!(self, Self::WaitingForClient(id) | Self::Cancelling(id) if id == window)
    }
}

pub(crate) struct Config {
    pub(crate) username: Option<String>,
    pub(crate) display_name: Option<String>,
}

#[derive(Debug)]
pub(crate) struct App {
    username: String,
    display_name: String,
    icon_file: Option<std::path::PathBuf>,
    accounts: Vec<Account>,
    input: String,
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
    confirmation: Option<PowerAction>,
    attempt: Attempt,
    closing: Option<Closing>,
}

#[derive(Debug, Clone)]
pub(crate) enum Message {
    Tick,
    InputChanged(String),
    Submit,
    AuthResult {
        attempt: Attempt,
        result: Result<(Option<Client>, auth::Response), String>,
    },
    AccountsResult(Result<Vec<Account>, String>),
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
        let sessions = sessions::discover();
        let selected_session = sessions.first().cloned();
        let account = config
            .username
            .map(|username| Account::override_account(username, config.display_name));
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
            icon_file: account
                .as_ref()
                .and_then(|account| account.icon_file.clone()),
            accounts: Vec::new(),
            input: String::new(),
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
                Phase::Idle
            },
            client: None,
            sessions,
            selected_session,
            started_at: Instant::now(),
            now: Local::now(),
            confirmation: None,
            attempt,
            closing: None,
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

        match message {
            Message::Tick => {
                self.now = Local::now();
                Task::none()
            }
            Message::InputChanged(value) => {
                self.input = value;
                Task::none()
            }
            Message::AccountsResult(Ok(accounts)) if accounts.len() == 1 => {
                self.accounts = accounts;
                self.select_account(self.accounts[0].clone())
            }
            Message::AccountsResult(Ok(accounts)) if accounts.is_empty() => {
                self.fail("AccountsService found no unlocked non-system users".into())
            }
            Message::AccountsResult(Ok(accounts)) => {
                self.accounts = accounts;
                self.phase = Phase::SelectingUser;
                self.message = Some("Select an account".into());
                self.message_is_error = false;
                Task::none()
            }
            Message::AccountsResult(Err(error)) => self.fail(error),
            Message::SelectAccount(account) if self.phase == Phase::SelectingUser => {
                self.select_account(account)
            }
            Message::SelectAccount(_) => Task::none(),
            Message::SelectSession(session)
                if !matches!(self.phase, Phase::Authenticating | Phase::StartingSession) =>
            {
                self.selected_session = Some(session);
                Task::none()
            }
            Message::SelectSession(_) => Task::none(),
            Message::Submit if self.phase == Phase::Idle => {
                self.message = None;
                self.phase = Phase::CreatingSession;
                let client = self.client.take();
                let attempt = self.attempt.advance();
                auth_flow::restart(client, self.username.clone(), attempt)
            }
            Message::Submit if self.phase == Phase::WaitingForInput => {
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
            Message::AskPower(action) => {
                self.confirmation = Some(action);
                Task::none()
            }
            Message::CancelPower => {
                self.confirmation = None;
                Task::none()
            }
            Message::ConfirmPower(action) => {
                self.confirmation = None;
                self.message = Some(format!("Requesting {}…", action.label().to_lowercase()));
                self.message_is_error = false;
                Task::perform(power::execute(action), |result| {
                    Message::PowerResult(result.map_err(|error| error.to_string()))
                })
            }
            Message::PowerResult(Ok(())) => Task::none(),
            Message::PowerResult(Err(error)) => {
                self.message = Some(error);
                self.message_is_error = true;
                Task::none()
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
        self.username = account.username;
        self.display_name = account.display_name;
        self.icon_file = account.icon_file;
        self.message = None;
        self.message_is_error = false;
        self.phase = Phase::CreatingSession;
        let attempt = self.attempt.advance();
        auth_flow::begin(self.username.clone(), attempt, true)
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
            icon_file: None,
            accounts: Vec::new(),
            input: "secret".into(),
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
            confirmation: None,
            attempt,
            closing: None,
        }
    }

    fn account(username: &str) -> Account {
        Account::override_account(username.into(), Some(username.to_uppercase()))
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
    fn empty_account_discovery_reports_configuration_error() {
        let mut app = app();
        app.phase = Phase::DiscoveringUsers;

        let _ = app.update(Message::AccountsResult(Ok(Vec::new())));

        assert_eq!(app.phase, Phase::Idle);
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
    fn power_failures_preserve_authentication_state() {
        let mut app = app();
        let _ = app.update(Message::PowerResult(Err("not authorized".into())));

        assert_eq!(app.phase, Phase::WaitingForInput);
        assert_eq!(app.input, "secret");
        assert_eq!(app.message.as_deref(), Some("not authorized"));
        assert!(app.message_is_error);
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
        app.phase = Phase::Idle;

        let _ = app.update(Message::CloseRequested(window));

        assert_eq!(app.closing, Some(Closing::Cancelling(window)));
        assert_ne!(app.attempt, old_attempt);
        let closing_attempt = app.attempt;

        let _ = app.update(Message::Submit);

        assert_eq!(app.phase, Phase::Idle);
        assert_eq!(app.attempt, closing_attempt);
        assert_eq!(app.closing, Some(Closing::Cancelling(window)));
    }

    #[test]
    fn retry_is_ignored_after_cancellation_dispatches_close() {
        let mut app = app();
        let window = window::Id::unique();
        let attempt = app.attempt;
        app.phase = Phase::Idle;
        app.closing = Some(Closing::Cancelling(window));

        let _ = app.update(Message::SessionCancelled(window));
        assert_eq!(app.closing, Some(Closing::Dispatching(window)));

        let _ = app.update(Message::Submit);

        assert_eq!(app.phase, Phase::Idle);
        assert_eq!(app.attempt, attempt);
        assert_eq!(app.closing, Some(Closing::Dispatching(window)));
    }
}
