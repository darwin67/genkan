use greetd_ipc::Request;
use iced::widget::text_input;
use iced::{window, Task};

use genkan::auth::{self, Client};

use super::{App, Closing, Message};

const CLOSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
const MAX_AUTH_TEXT_CHARS: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Phase {
    Failed,
    DiscoveringUsers,
    SelectingUser,
    CreatingSession,
    WaitingForInput,
    Authenticating,
    StartingSession,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Attempt(u64);

#[derive(Debug, Clone, PartialEq, Eq)]
enum AuthTransition {
    Prompt { secret: bool, message: String },
    Message { error: bool, message: String },
    StartSession,
    Exit,
    Fail(String),
}

impl Attempt {
    pub(super) fn initial() -> Self {
        Self(1)
    }

    pub(super) fn advance(&mut self) -> Self {
        self.0 = self.0.wrapping_add(1);
        *self
    }
}

impl App {
    pub(super) fn handle_auth_result(
        &mut self,
        attempt: Attempt,
        result: Result<(Option<Client>, auth::Response), String>,
    ) -> Task<Message> {
        if attempt != self.attempt {
            return Task::none();
        }
        if let Some(Closing::WaitingForClient(window)) = self.closing {
            let client = match result {
                Ok((client, _)) => client,
                Err(_) => None,
            };
            self.attempt.advance();
            self.closing = Some(Closing::Cancelling(window));
            return cancel_for_close(client, window);
        }
        match result {
            Ok((client, response)) => {
                if let Some(client) = client {
                    self.client = Some(client);
                }
                self.handle_auth_response(response)
            }
            Err(error) => self.fail(error),
        }
    }

    pub(super) fn handle_auth_response(&mut self, response: auth::Response) -> Task<Message> {
        match transition(self.phase, response) {
            AuthTransition::Prompt { secret, message } => {
                self.prompt = clean_prompt(&message);
                self.secret = secret;
                self.input.clear();
                self.phase = Phase::WaitingForInput;
                text_input::focus(self.input_id.clone())
            }
            AuthTransition::Message { error, message } => {
                self.message = Some(bounded_auth_text(&message));
                self.message_is_error = error;
                let Some(client) = self.client.clone() else {
                    return self.fail("Lost connection to greetd".into());
                };
                exchange(
                    client,
                    Request::PostAuthMessageResponse { response: None },
                    self.attempt,
                )
            }
            AuthTransition::Exit => iced::exit(),
            AuthTransition::StartSession => {
                let Some(client) = self.client.clone() else {
                    return self.fail("Lost connection to greetd".into());
                };
                self.phase = Phase::StartingSession;
                self.message = Some("Starting session…".into());
                let Some(session) = self.selected_session.clone() else {
                    return self.fail("No valid Wayland session is selected".into());
                };
                let attempt = self.attempt;
                Task::perform(
                    async move {
                        let environment = session.environment();
                        client
                            .exchange(Request::StartSession {
                                cmd: session.command,
                                env: environment,
                            })
                            .await
                    },
                    move |result| Message::AuthResult {
                        attempt,
                        result: result
                            .map(|response| (None, response))
                            .map_err(|error| error.to_string()),
                    },
                )
            }
            AuthTransition::Fail(message) => self.fail(message),
        }
    }

    pub(super) fn fail(&mut self, message: String) -> Task<Message> {
        self.phase = Phase::Failed;
        self.input.clear();
        self.prompt = "Password".into();
        self.secret = true;
        self.message = Some(bounded_auth_text(&message));
        self.message_is_error = true;
        Task::none()
    }
}

fn transition(phase: Phase, response: auth::Response) -> AuthTransition {
    match response {
        auth::Response::Prompt { secret, message } => AuthTransition::Prompt { secret, message },
        auth::Response::Message { error, message } => AuthTransition::Message { error, message },
        auth::Response::Success if phase == Phase::StartingSession => AuthTransition::Exit,
        auth::Response::Success => AuthTransition::StartSession,
        auth::Response::Error {
            authentication: true,
            ..
        } => AuthTransition::Fail("Authentication failed".into()),
        auth::Response::Error { description, .. } => AuthTransition::Fail(description),
    }
}

pub(super) fn begin(username: String, attempt: Attempt, recover: bool) -> Task<Message> {
    Task::perform(
        async move {
            if recover {
                auth::recover_and_begin(username).await
            } else {
                auth::begin(username).await
            }
        },
        move |result| Message::AuthResult {
            attempt,
            result: result
                .map(|(client, response)| (Some(client), response))
                .map_err(|error| error.to_string()),
        },
    )
}

pub(super) fn restart(client: Option<Client>, username: String, attempt: Attempt) -> Task<Message> {
    Task::perform(auth::restart(client, username), move |result| {
        Message::AuthResult {
            attempt,
            result: result
                .map(|(client, response)| (Some(client), response))
                .map_err(|error| error.to_string()),
        }
    })
}

pub(super) fn exchange(client: Client, request: Request, attempt: Attempt) -> Task<Message> {
    Task::perform(
        async move { client.exchange(request).await },
        move |result| Message::AuthResult {
            attempt,
            result: result
                .map(|response| (None, response))
                .map_err(|error| error.to_string()),
        },
    )
}

pub(super) fn cancel_for_close(client: Option<Client>, window: window::Id) -> Task<Message> {
    Task::perform(auth::cancel(client), move |_| {
        Message::SessionCancelled(window)
    })
}

pub(super) fn close_timeout(window: window::Id) -> Task<Message> {
    Task::perform(
        async move {
            tokio::time::sleep(CLOSE_TIMEOUT).await;
            window
        },
        Message::CloseTimeout,
    )
}

pub(super) fn clean_prompt(prompt: &str) -> String {
    let prompt = prompt.trim().trim_end_matches(':').trim();
    bounded_auth_text(if prompt.is_empty() {
        "Password"
    } else {
        prompt
    })
}

pub(super) fn bounded_auth_text(value: &str) -> String {
    let mut characters = value.chars();
    let mut bounded = characters
        .by_ref()
        .take(MAX_AUTH_TEXT_CHARS)
        .collect::<String>();
    if characters.next().is_some() {
        bounded.pop();
        bounded.push('…');
    }
    bounded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_pam_prompts() {
        assert_eq!(clean_prompt("Password: "), "Password");
        assert_eq!(clean_prompt(""), "Password");
    }

    #[test]
    fn bounds_pathological_authentication_text() {
        let bounded = bounded_auth_text(&"界".repeat(MAX_AUTH_TEXT_CHARS + 100));
        assert_eq!(bounded.chars().count(), MAX_AUTH_TEXT_CHARS);
        assert!(bounded.ends_with('…'));
        assert_eq!(
            clean_prompt(&format!("{}:", "x".repeat(600))),
            bounded_auth_text(&"x".repeat(600))
        );
    }

    #[test]
    fn maps_every_authentication_response_transition() {
        let cases = [
            (
                Phase::Authenticating,
                auth::Response::Prompt {
                    secret: true,
                    message: "Password:".into(),
                },
                AuthTransition::Prompt {
                    secret: true,
                    message: "Password:".into(),
                },
            ),
            (
                Phase::Authenticating,
                auth::Response::Message {
                    error: false,
                    message: "Touch the security key".into(),
                },
                AuthTransition::Message {
                    error: false,
                    message: "Touch the security key".into(),
                },
            ),
            (
                Phase::Authenticating,
                auth::Response::Success,
                AuthTransition::StartSession,
            ),
            (
                Phase::StartingSession,
                auth::Response::Success,
                AuthTransition::Exit,
            ),
            (
                Phase::Authenticating,
                auth::Response::Error {
                    authentication: true,
                    description: "daemon detail".into(),
                },
                AuthTransition::Fail("Authentication failed".into()),
            ),
            (
                Phase::StartingSession,
                auth::Response::Error {
                    authentication: false,
                    description: "start rejected".into(),
                },
                AuthTransition::Fail("start rejected".into()),
            ),
        ];

        for (phase, response, expected) in cases {
            assert_eq!(transition(phase, response), expected);
        }
    }
}
