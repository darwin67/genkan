mod auth_flow;
mod view;

use std::time::{Duration, Instant};

use auth_flow::{Attempt, Phase};
use chrono::Local;
use greetd_ipc::Request;
use iced::{time, window, Subscription, Task};

use crate::auth::{self, Client};
use crate::power::{self, Action as PowerAction};
use crate::sessions::{self, Session};

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
    closing: Option<window::Id>,
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
                auth_flow::cancel_and_close(self.client.take(), window)
            }
            Message::CloseRequested(window) if self.phase == Phase::CreatingSession => {
                self.closing = Some(window);
                Task::none()
            }
            Message::CloseRequested(window) => window::close(window),
            Message::SessionCancelled(window) => window::close(window),
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
}
