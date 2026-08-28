use std::error::Error;

use clap::Parser;
use genkan::auth::{self, Client, Response};
use greetd_ipc::Request;

#[derive(Debug, Parser)]
struct Arguments {
    #[arg(long)]
    username: String,
    #[arg(long)]
    wrong_password: String,
    #[arg(long)]
    password: String,
    #[arg(long)]
    session_command: String,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let arguments = Arguments::parse();

    let (client, response) = auth::recover_and_begin(arguments.username.clone()).await?;
    let response = complete_authentication(&client, response, &arguments.wrong_password).await?;
    if !matches!(
        response,
        Response::Error {
            authentication: true,
            ..
        }
    ) {
        return Err(format!("wrong password returned unexpected response: {response:?}").into());
    }

    let (client, response) = auth::restart(Some(client), arguments.username).await?;
    let response = complete_authentication(&client, response, &arguments.password).await?;
    if response != Response::Success {
        return Err(format!("correct password returned unexpected response: {response:?}").into());
    }

    let response = client
        .exchange(Request::StartSession {
            cmd: vec![arguments.session_command],
            env: vec!["GENKAN_E2E=passed".into()],
        })
        .await?;
    if response != Response::Success {
        return Err(format!("StartSession returned unexpected response: {response:?}").into());
    }

    Ok(())
}

async fn complete_authentication(
    client: &Client,
    mut response: Response,
    password: &str,
) -> Result<Response, auth::AuthError> {
    loop {
        response = match response {
            Response::Prompt { .. } => {
                client
                    .exchange(Request::PostAuthMessageResponse {
                        response: Some(password.into()),
                    })
                    .await?
            }
            Response::Message { .. } => {
                client
                    .exchange(Request::PostAuthMessageResponse { response: None })
                    .await?
            }
            Response::Success | Response::Error { .. } => return Ok(response),
        };
    }
}
