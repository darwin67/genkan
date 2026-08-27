use std::env;
use std::sync::Arc;

use greetd_ipc::{codec::TokioCodec, AuthMessageType, ErrorType, Request};
use thiserror::Error;
use tokio::net::UnixStream;
use tokio::sync::Mutex;

#[derive(Debug, Clone)]
pub struct Client {
    stream: Arc<Mutex<UnixStream>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    Success,
    Error {
        authentication: bool,
        description: String,
    },
    Prompt {
        secret: bool,
        message: String,
    },
    Message {
        error: bool,
        message: String,
    },
}

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("GREETD_SOCK is not set; genkan must be launched by greetd")]
    MissingSocket,
    #[error("could not communicate with greetd: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid greetd message: {0}")]
    Protocol(#[from] greetd_ipc::codec::Error),
}

impl Client {
    pub async fn connect() -> Result<Self, AuthError> {
        let socket = env::var_os("GREETD_SOCK").ok_or(AuthError::MissingSocket)?;
        let stream = UnixStream::connect(socket).await?;
        Ok(Self {
            stream: Arc::new(Mutex::new(stream)),
        })
    }

    pub async fn exchange(&self, request: Request) -> Result<Response, AuthError> {
        let mut stream = self.stream.lock().await;
        request.write_to(&mut *stream).await?;
        let response = greetd_ipc::Response::read_from(&mut *stream).await?;
        Ok(response.into())
    }
}

pub async fn begin(username: String) -> Result<(Client, Response), AuthError> {
    let client = Client::connect().await?;
    let response = client.exchange(Request::CreateSession { username }).await?;
    Ok((client, response))
}

impl From<greetd_ipc::Response> for Response {
    fn from(response: greetd_ipc::Response) -> Self {
        match response {
            greetd_ipc::Response::Success => Self::Success,
            greetd_ipc::Response::Error {
                error_type,
                description,
            } => Self::Error {
                authentication: matches!(error_type, ErrorType::AuthError),
                description,
            },
            greetd_ipc::Response::AuthMessage {
                auth_message_type,
                auth_message,
            } => match auth_message_type {
                AuthMessageType::Visible | AuthMessageType::Secret => Self::Prompt {
                    secret: matches!(auth_message_type, AuthMessageType::Secret),
                    message: auth_message,
                },
                AuthMessageType::Info | AuthMessageType::Error => Self::Message {
                    error: matches!(auth_message_type, AuthMessageType::Error),
                    message: auth_message,
                },
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_visible_and_secret_prompts() {
        let visible = greetd_ipc::Response::AuthMessage {
            auth_message_type: AuthMessageType::Visible,
            auth_message: "Username:".into(),
        };
        let secret = greetd_ipc::Response::AuthMessage {
            auth_message_type: AuthMessageType::Secret,
            auth_message: "Password:".into(),
        };

        assert_eq!(
            Response::from(visible),
            Response::Prompt {
                secret: false,
                message: "Username:".into(),
            }
        );
        assert_eq!(
            Response::from(secret),
            Response::Prompt {
                secret: true,
                message: "Password:".into(),
            }
        );
    }

    #[test]
    fn normalizes_information_and_error_messages() {
        let information = greetd_ipc::Response::AuthMessage {
            auth_message_type: AuthMessageType::Info,
            auth_message: "Touch the fingerprint sensor".into(),
        };
        let error = greetd_ipc::Response::AuthMessage {
            auth_message_type: AuthMessageType::Error,
            auth_message: "Fingerprint not recognized".into(),
        };

        assert_eq!(
            Response::from(information),
            Response::Message {
                error: false,
                message: "Touch the fingerprint sensor".into(),
            }
        );
        assert_eq!(
            Response::from(error),
            Response::Message {
                error: true,
                message: "Fingerprint not recognized".into(),
            }
        );
    }

    #[test]
    fn distinguishes_authentication_from_protocol_errors() {
        let authentication = greetd_ipc::Response::Error {
            error_type: ErrorType::AuthError,
            description: "invalid credentials".into(),
        };
        let protocol = greetd_ipc::Response::Error {
            error_type: ErrorType::Error,
            description: "session unavailable".into(),
        };

        assert_eq!(
            Response::from(authentication),
            Response::Error {
                authentication: true,
                description: "invalid credentials".into(),
            }
        );
        assert_eq!(
            Response::from(protocol),
            Response::Error {
                authentication: false,
                description: "session unavailable".into(),
            }
        );
    }
}
