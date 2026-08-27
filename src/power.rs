use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Suspend,
    Reboot,
    PowerOff,
}

impl Action {
    pub fn label(self) -> &'static str {
        match self {
            Self::Suspend => "Sleep",
            Self::Reboot => "Restart",
            Self::PowerOff => "Shut Down",
        }
    }

    fn method(self) -> &'static str {
        match self {
            Self::Suspend => "Suspend",
            Self::Reboot => "Reboot",
            Self::PowerOff => "PowerOff",
        }
    }
}

#[derive(Debug, Error)]
pub enum PowerError {
    #[error("logind request failed: {0}")]
    Bus(#[from] zbus::Error),
}

pub async fn execute(action: Action) -> Result<(), PowerError> {
    let connection = zbus::Connection::system().await?;
    let proxy = zbus::Proxy::new(
        &connection,
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
    )
    .await?;
    proxy.call_method(action.method(), &(false)).await?;
    Ok(())
}
