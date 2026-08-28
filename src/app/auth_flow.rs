use greetd_ipc::Request;
use iced::{window, Task};

use crate::auth::{self, Client};

use super::{App, Message};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Phase {
    Idle,
    CreatingSession,
    WaitingForInput,
    Authenticating,
    StartingSession,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Attempt(u64);

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
        if let Some(window) = self.closing.take() {
            let client = match result {
                Ok((client, _)) => client,
                Err(_) => None,
            };
            self.attempt.advance();
            return cancel_and_close(client, window);
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

    fn handle_auth_response(&mut self, response: auth::Response) -> Task<Message> {
        match response {
            auth::Response::Prompt { secret, message } => {
                self.prompt = clean_prompt(&message);
                self.secret = secret;
                self.input.clear();
                self.phase = Phase::WaitingForInput;
                Task::none()
            }
            auth::Response::Message { error, message } => {
                self.message = Some(message);
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
            auth::Response::Success if self.phase == Phase::StartingSession => iced::exit(),
            auth::Response::Success => {
                let Some(client) = self.client.clone() else {
                    return self.fail("Lost connection to greetd".into());
                };
                self.phase = Phase::StartingSession;
                self.message = Some("Starting session…".into());
                let session = self.selected_session.clone();
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
            auth::Response::Error {
                authentication,
                description,
            } => {
                if authentication {
                    self.client = None;
                    self.phase = Phase::CreatingSession;
                    self.input.clear();
                    self.prompt = "Password".into();
                    self.secret = true;
                    self.message = Some("Authentication failed".into());
                    self.message_is_error = true;
                    let attempt = self.attempt.advance();
                    begin(self.username.clone(), attempt, false)
                } else {
                    self.fail(description)
                }
            }
        }
    }

    pub(super) fn fail(&mut self, message: String) -> Task<Message> {
        self.phase = Phase::Idle;
        self.input.clear();
        self.prompt = "Password".into();
        self.secret = true;
        self.message = Some(message);
        self.message_is_error = true;
        Task::none()
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
    Task::perform(
        async move {
            let needs_recovery = match client {
                Some(client) => auth::cancel(Some(client)).await.is_err(),
                None => true,
            };
            if needs_recovery {
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

pub(super) fn cancel_and_close(client: Option<Client>, window: window::Id) -> Task<Message> {
    Task::perform(auth::cancel(client), move |_| {
        Message::SessionCancelled(window)
    })
}

pub(super) fn clean_prompt(prompt: &str) -> String {
    let prompt = prompt.trim().trim_end_matches(':').trim();
    if prompt.is_empty() {
        "Password".into()
    } else {
        prompt.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_pam_prompts() {
        assert_eq!(clean_prompt("Password: "), "Password");
        assert_eq!(clean_prompt(""), "Password");
    }
}
