use greetd_ipc::Request;
use iced::{window, Task};

use genkan::auth::{self, Client};

use crate::conversation::{self, Attempt};

use super::{App, Closing, Message};

const CLOSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
const USER_SELECTION_CANCEL_PROGRESS_DELAY: std::time::Duration = std::time::Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Phase {
    Failed,
    DiscoveringUsers,
    CancellingForUserSelection,
    SelectingUser,
    UserSelectionCancellationFailed,
    CreatingSession,
    WaitingForInput,
    Authenticating,
    StartingSession,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AuthTransition {
    phase: Phase,
    effect: AuthEffect,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum AuthEffect {
    Prompt { secret: bool, message: String },
    Acknowledge { error: bool, message: String },
    StartSession(AuthRequest),
    Exit,
    Fail(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum AuthRequest {
    StartSession { cmd: Vec<String>, env: Vec<String> },
}

impl AuthRequest {
    fn into_greetd(self) -> Request {
        match self {
            Self::StartSession { cmd, env } => Request::StartSession { cmd, env },
        }
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
        let transition = transition(self.phase, response, self.selected_session.as_ref());
        match self.apply_auth_transition(transition) {
            AuthEffect::Prompt { secret, message } => {
                self.prompt = message;
                self.secret = secret;
                self.input.clear();
                self.focus_input()
            }
            AuthEffect::Acknowledge { error, message } => {
                self.set_auth_notice(message, error);
                let Some(client) = self.client.clone() else {
                    return self.fail("Lost connection to greetd".into());
                };
                exchange(client, acknowledge_request(), self.attempt)
            }
            AuthEffect::Exit => iced::exit(),
            AuthEffect::StartSession(request) => {
                let Some(client) = self.client.clone() else {
                    return self.fail("Lost connection to greetd".into());
                };
                exchange(client, request.into_greetd(), self.attempt)
            }
            AuthEffect::Fail(message) => self.fail(message),
        }
    }

    pub(super) fn apply_auth_transition(&mut self, transition: AuthTransition) -> AuthEffect {
        self.phase = transition.phase;
        if matches!(transition.effect, AuthEffect::StartSession(_)) {
            self.clear_auth_notice();
        }
        transition.effect
    }

    pub(super) fn fail(&mut self, message: String) -> Task<Message> {
        self.phase = Phase::Failed;
        self.input.clear();
        self.prompt = "Password".into();
        self.secret = true;
        self.message = Some(conversation::bounded_text(&message));
        self.message_is_error = true;
        self.focus_first()
    }

    pub(super) fn set_auth_notice(&mut self, message: String, error: bool) {
        self.message = Some(conversation::bounded_text(&message));
        self.message_is_error = error;
    }

    pub(super) fn clear_auth_notice(&mut self) {
        self.message = None;
        self.message_is_error = false;
    }
}

pub(super) fn transition(
    phase: Phase,
    response: auth::Response,
    session: Option<&crate::sessions::Session>,
) -> AuthTransition {
    if phase == Phase::StartingSession && response == auth::Response::Success {
        return AuthTransition {
            phase,
            effect: AuthEffect::Exit,
        };
    }

    let response = match response {
        auth::Response::Prompt { secret, message } => {
            conversation::Response::Prompt { secret, message }
        }
        auth::Response::Message { error, message } => {
            conversation::Response::Notice { error, message }
        }
        auth::Response::Success => conversation::Response::Success,
        auth::Response::Error {
            authentication: true,
            ..
        } => conversation::Response::Failure("Authentication failed".into()),
        auth::Response::Error { description, .. } => conversation::Response::Failure(description),
    };

    let (phase, effect) = match conversation::transition(response) {
        conversation::Effect::Prompt { secret, message } => (
            Phase::WaitingForInput,
            AuthEffect::Prompt { secret, message },
        ),
        conversation::Effect::Acknowledge { error, message } => {
            (phase, AuthEffect::Acknowledge { error, message })
        }
        conversation::Effect::Authenticated => match session {
            Some(session) => (
                Phase::StartingSession,
                AuthEffect::StartSession(AuthRequest::StartSession {
                    cmd: session.command.clone(),
                    env: session.environment(),
                }),
            ),
            None => (
                Phase::Failed,
                AuthEffect::Fail("No valid Wayland session is selected".into()),
            ),
        },
        conversation::Effect::Failed(message) => (Phase::Failed, AuthEffect::Fail(message)),
    };
    AuthTransition { phase, effect }
}

fn acknowledge_request() -> Request {
    Request::PostAuthMessageResponse { response: None }
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

pub(super) fn cancel_for_user_selection(client: Option<Client>, attempt: Attempt) -> Task<Message> {
    Task::batch([
        Task::perform(
            async move {
                auth::cancel(client)
                    .await
                    .map_err(|error| error.to_string())
            },
            move |result| Message::UserSelectionCancelled { attempt, result },
        ),
        Task::perform(
            async {
                tokio::time::sleep(USER_SELECTION_CANCEL_PROGRESS_DELAY).await;
            },
            move |()| Message::UserSelectionCancellationSlow { attempt },
        ),
    ])
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_every_authentication_response_transition() {
        let session = crate::sessions::Session {
            name: "River".into(),
            command: vec!["/run/current-system/sw/bin/river".into(), "-c".into()],
            session_id: "river-session".into(),
            desktop_names: vec!["River".into(), "wlroots".into()],
        };
        let cases = [
            (
                Phase::Authenticating,
                auth::Response::Prompt {
                    secret: true,
                    message: "Password:".into(),
                },
                AuthTransition {
                    phase: Phase::WaitingForInput,
                    effect: AuthEffect::Prompt {
                        secret: true,
                        message: "Password".into(),
                    },
                },
            ),
            (
                Phase::Authenticating,
                auth::Response::Message {
                    error: false,
                    message: "Touch the security key".into(),
                },
                AuthTransition {
                    phase: Phase::Authenticating,
                    effect: AuthEffect::Acknowledge {
                        error: false,
                        message: "Touch the security key".into(),
                    },
                },
            ),
            (
                Phase::Authenticating,
                auth::Response::Success,
                AuthTransition {
                    phase: Phase::StartingSession,
                    effect: AuthEffect::StartSession(AuthRequest::StartSession {
                        cmd: vec!["/run/current-system/sw/bin/river".into(), "-c".into()],
                        env: vec![
                            "XDG_SESSION_TYPE=wayland".into(),
                            "XDG_SESSION_DESKTOP=river-session".into(),
                            "XDG_CURRENT_DESKTOP=River:wlroots".into(),
                        ],
                    }),
                },
            ),
            (
                Phase::StartingSession,
                auth::Response::Success,
                AuthTransition {
                    phase: Phase::StartingSession,
                    effect: AuthEffect::Exit,
                },
            ),
            (
                Phase::Authenticating,
                auth::Response::Error {
                    authentication: true,
                    description: "daemon detail".into(),
                },
                AuthTransition {
                    phase: Phase::Failed,
                    effect: AuthEffect::Fail("Authentication failed".into()),
                },
            ),
            (
                Phase::StartingSession,
                auth::Response::Error {
                    authentication: false,
                    description: "start rejected".into(),
                },
                AuthTransition {
                    phase: Phase::Failed,
                    effect: AuthEffect::Fail("start rejected".into()),
                },
            ),
        ];

        for (phase, response, expected) in cases {
            assert_eq!(transition(phase, response, Some(&session)), expected);
        }
    }

    #[test]
    fn acknowledges_informational_messages_without_a_response() {
        let transition = transition(
            Phase::Authenticating,
            auth::Response::Message {
                error: false,
                message: "Touch the security key".into(),
            },
            None,
        );

        assert_eq!(transition.phase, Phase::Authenticating);
        assert_eq!(
            transition.effect,
            AuthEffect::Acknowledge {
                error: false,
                message: "Touch the security key".into(),
            }
        );
        assert!(matches!(
            acknowledge_request(),
            Request::PostAuthMessageResponse { response: None }
        ));
    }

    #[test]
    fn successful_authentication_emits_the_selected_session_request() {
        let session = crate::sessions::Session {
            name: "River".into(),
            command: vec!["/run/current-system/sw/bin/river".into(), "-c".into()],
            session_id: "river-session".into(),
            desktop_names: vec!["River".into(), "wlroots".into()],
        };
        let transition = transition(
            Phase::Authenticating,
            auth::Response::Success,
            Some(&session),
        );

        let AuthEffect::StartSession(request) = transition.effect else {
            panic!("successful authentication did not start the selected session");
        };
        let Request::StartSession { cmd, env } = request.into_greetd() else {
            panic!("successful authentication emitted the wrong greetd request");
        };
        assert_eq!(cmd, session.command);
        assert_eq!(
            env,
            [
                "XDG_SESSION_TYPE=wayland",
                "XDG_SESSION_DESKTOP=river-session",
                "XDG_CURRENT_DESKTOP=River:wlroots",
            ]
        );
    }

    #[test]
    fn successful_authentication_requires_a_selected_session() {
        assert_eq!(
            transition(Phase::Authenticating, auth::Response::Success, None),
            AuthTransition {
                phase: Phase::Failed,
                effect: AuthEffect::Fail("No valid Wayland session is selected".into()),
            }
        );
    }
}
