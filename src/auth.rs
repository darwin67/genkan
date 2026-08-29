use std::env;
use std::path::Path;
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
    #[error("greetd rejected session cancellation: {0}")]
    CancellationRejected(String),
    #[error("greetd returned an unexpected response to session cancellation")]
    UnexpectedCancellationResponse,
}

impl Client {
    pub async fn connect() -> Result<Self, AuthError> {
        let socket = env::var_os("GREETD_SOCK").ok_or(AuthError::MissingSocket)?;
        Self::connect_to(socket).await
    }

    async fn connect_to(socket: impl AsRef<Path>) -> Result<Self, AuthError> {
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
    let socket = env::var_os("GREETD_SOCK").ok_or(AuthError::MissingSocket)?;
    begin_at(Path::new(&socket), username).await
}

async fn begin_at(socket: &Path, username: String) -> Result<(Client, Response), AuthError> {
    let client = Client::connect_to(socket).await?;
    let response = client.exchange(Request::CreateSession { username }).await?;
    Ok((client, response))
}

pub async fn recover_and_begin(username: String) -> Result<(Client, Response), AuthError> {
    let socket = env::var_os("GREETD_SOCK").ok_or(AuthError::MissingSocket)?;
    recover_and_begin_at(Path::new(&socket), username).await
}

async fn recover_and_begin_at(
    socket: &Path,
    username: String,
) -> Result<(Client, Response), AuthError> {
    if let Ok(client) = Client::connect_to(socket).await {
        let _ = client.exchange(Request::CancelSession).await;
    }
    begin_at(socket, username).await
}

pub async fn restart(
    client: Option<Client>,
    username: String,
) -> Result<(Client, Response), AuthError> {
    let socket = env::var_os("GREETD_SOCK").ok_or(AuthError::MissingSocket)?;
    restart_at(client, Path::new(&socket), username).await
}

async fn restart_at(
    client: Option<Client>,
    socket: &Path,
    username: String,
) -> Result<(Client, Response), AuthError> {
    let cancelled = match client {
        Some(client) => cancel(Some(client)).await.is_ok(),
        None => false,
    };
    if cancelled {
        begin_at(socket, username).await
    } else {
        recover_and_begin_at(socket, username).await
    }
}

pub async fn cancel(client: Option<Client>) -> Result<(), AuthError> {
    let client = match client {
        Some(client) => client,
        None => Client::connect().await?,
    };
    match client.exchange(Request::CancelSession).await? {
        Response::Success => Ok(()),
        Response::Error { description, .. } => Err(AuthError::CancellationRejected(description)),
        Response::Prompt { .. } | Response::Message { .. } => {
            Err(AuthError::UnexpectedCancellationResponse)
        }
    }
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
    use greetd_ipc::codec::TokioCodec;
    use tokio::net::UnixListener;

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

    #[tokio::test]
    async fn recovery_cancels_a_stale_session_before_creating_one() {
        let socket = std::env::temp_dir().join(format!("genkan-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&socket);
        let listener = UnixListener::bind(&socket).expect("bind fake greetd socket");
        let server = tokio::spawn(async move {
            let (mut recovery, _) = listener.accept().await.expect("accept recovery client");
            assert!(matches!(
                Request::read_from(&mut recovery).await.unwrap(),
                Request::CancelSession
            ));
            greetd_ipc::Response::Success
                .write_to(&mut recovery)
                .await
                .unwrap();

            let (mut authentication, _) = listener.accept().await.expect("accept auth client");
            assert!(matches!(
                Request::read_from(&mut authentication).await.unwrap(),
                Request::CreateSession { username } if username == "darwin"
            ));
            greetd_ipc::Response::AuthMessage {
                auth_message_type: AuthMessageType::Secret,
                auth_message: "Password:".into(),
            }
            .write_to(&mut authentication)
            .await
            .unwrap();
        });

        let result = recover_and_begin_at(&socket, "darwin".into()).await;
        assert!(matches!(
            result,
            Ok((_, Response::Prompt { secret: true, message })) if message == "Password:"
        ));
        server.await.unwrap();
        std::fs::remove_file(&socket).unwrap();
    }

    #[tokio::test]
    async fn cancellation_requires_a_success_response() {
        let socket =
            std::env::temp_dir().join(format!("genkan-cancel-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&socket);
        let listener = UnixListener::bind(&socket).expect("bind fake greetd socket");
        let client = Client::connect_to(&socket)
            .await
            .expect("connect cancellation client");
        let server = tokio::spawn(async move {
            let (mut active, _) = listener.accept().await.expect("accept cancellation client");
            assert!(matches!(
                Request::read_from(&mut active).await.unwrap(),
                Request::CancelSession
            ));
            greetd_ipc::Response::Error {
                error_type: ErrorType::Error,
                description: "no active session".into(),
            }
            .write_to(&mut active)
            .await
            .unwrap();
        });

        assert!(matches!(
            cancel(Some(client)).await,
            Err(AuthError::CancellationRejected(description)) if description == "no active session"
        ));
        server.await.unwrap();
        std::fs::remove_file(&socket).unwrap();
    }

    #[tokio::test]
    async fn auth_error_is_cancelled_before_creating_another_session() {
        let socket = std::env::temp_dir().join(format!("genkan-retry-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&socket);
        let listener = UnixListener::bind(&socket).expect("bind fake greetd socket");
        let client = Client::connect_to(&socket)
            .await
            .expect("connect active client");
        let server = tokio::spawn(async move {
            let (mut active, _) = listener.accept().await.expect("accept active client");
            assert!(matches!(
                Request::read_from(&mut active).await.unwrap(),
                Request::PostAuthMessageResponse { .. }
            ));
            greetd_ipc::Response::Error {
                error_type: ErrorType::AuthError,
                description: "invalid credentials".into(),
            }
            .write_to(&mut active)
            .await
            .unwrap();
            assert!(matches!(
                Request::read_from(&mut active).await.unwrap(),
                Request::CancelSession
            ));
            greetd_ipc::Response::Success
                .write_to(&mut active)
                .await
                .unwrap();

            let (mut authentication, _) = listener.accept().await.expect("accept retry client");
            assert!(matches!(
                Request::read_from(&mut authentication).await.unwrap(),
                Request::CreateSession { username } if username == "darwin"
            ));
            greetd_ipc::Response::AuthMessage {
                auth_message_type: AuthMessageType::Secret,
                auth_message: "Password:".into(),
            }
            .write_to(&mut authentication)
            .await
            .unwrap();
        });

        let response = client
            .exchange(Request::PostAuthMessageResponse {
                response: Some("wrong".into()),
            })
            .await
            .unwrap();
        assert!(matches!(
            response,
            Response::Error {
                authentication: true,
                ..
            }
        ));
        let result = restart_at(Some(client), &socket, "darwin".into()).await;
        assert!(matches!(
            result,
            Ok((_, Response::Prompt { secret: true, message })) if message == "Password:"
        ));
        server.await.unwrap();
        std::fs::remove_file(&socket).unwrap();
    }

    #[tokio::test]
    async fn restart_recovers_when_active_cancellation_fails() {
        let socket =
            std::env::temp_dir().join(format!("genkan-fallback-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&socket);
        let listener = UnixListener::bind(&socket).expect("bind fake greetd socket");
        let client = Client::connect_to(&socket)
            .await
            .expect("connect active client");
        let server = tokio::spawn(async move {
            let (active, _) = listener.accept().await.expect("accept active client");
            drop(active);

            let (mut recovery, _) = listener.accept().await.expect("accept recovery client");
            assert!(matches!(
                Request::read_from(&mut recovery).await.unwrap(),
                Request::CancelSession
            ));
            greetd_ipc::Response::Success
                .write_to(&mut recovery)
                .await
                .unwrap();

            let (mut authentication, _) = listener.accept().await.expect("accept retry client");
            assert!(matches!(
                Request::read_from(&mut authentication).await.unwrap(),
                Request::CreateSession { username } if username == "darwin"
            ));
            greetd_ipc::Response::AuthMessage {
                auth_message_type: AuthMessageType::Secret,
                auth_message: "Password:".into(),
            }
            .write_to(&mut authentication)
            .await
            .unwrap();
        });

        let result = restart_at(Some(client), &socket, "darwin".into()).await;
        assert!(matches!(
            result,
            Ok((_, Response::Prompt { secret: true, message })) if message == "Password:"
        ));
        server.await.unwrap();
        std::fs::remove_file(&socket).unwrap();
    }
}
