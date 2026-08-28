mod auth_flow;
mod view;

use std::time::{Duration, Instant};

use auth_flow::{Attempt, Phase};
use chrono::Local;
use greetd_ipc::Request;
use iced::{time, window, Subscription, Task};

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
    pub(crate) username: String,
    pub(crate) display_name: String,
    pub(crate) session_command: String,
}

#[derive(Debug)]
pub(crate) struct App {
    username: String,
    display_name: String,
    input: String,
    prompt: String,
    message: Option<String>,
    message_is_error: bool,
    secret: bool,
    phase: Phase,
    client: Option<Client>,
    sessions: Vec<Session>,
    selected_session: Session,
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
        let command = shell_words::split(&config.session_command)
            .ok()
            .filter(|parts| !parts.is_empty())
            .unwrap_or_else(|| vec!["sway".into(), "--unsupported-gpu".into()]);
        let fallback = Session::sway(command);
        let sessions = sessions::discover(fallback.clone());
        let selected_session = sessions
            .iter()
            .find(|session| session.command == fallback.command)
            .cloned()
            .unwrap_or(fallback);
        let attempt = Attempt::initial();

        let app = Self {
            username: config.username,
            display_name: config.display_name,
            input: String::new(),
            prompt: "Password".into(),
            message: None,
            message_is_error: false,
            secret: true,
            phase: Phase::CreatingSession,
            client: None,
            sessions,
            selected_session,
            started_at: Instant::now(),
            now: Local::now(),
            confirmation: None,
            attempt,
            closing: None,
        };
        let task = auth_flow::begin(app.username.clone(), attempt, true);
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
            Message::SelectSession(session)
                if !matches!(self.phase, Phase::Authenticating | Phase::StartingSession) =>
            {
                self.selected_session = session;
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        let mut attempt = Attempt::initial();
        attempt.advance();
        App {
            username: "darwin".into(),
            display_name: "Darwin".into(),
            input: "secret".into(),
            prompt: "Password".into(),
            message: Some("Keep this message".into()),
            message_is_error: false,
            secret: true,
            phase: Phase::WaitingForInput,
            client: None,
            sessions: vec![Session::sway(vec!["sway".into()])],
            selected_session: Session::sway(vec!["sway".into()]),
            started_at: Instant::now(),
            now: Local::now(),
            confirmation: None,
            attempt,
            closing: None,
        }
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
