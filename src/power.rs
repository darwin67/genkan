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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_actions_to_labels_and_logind_methods() {
        assert_eq!(Action::Suspend.label(), "Sleep");
        assert_eq!(Action::Suspend.method(), "Suspend");
        assert_eq!(Action::Reboot.label(), "Restart");
        assert_eq!(Action::Reboot.method(), "Reboot");
        assert_eq!(Action::PowerOff.label(), "Shut Down");
        assert_eq!(Action::PowerOff.method(), "PowerOff");
    }
}
