use std::fmt;
use std::path::PathBuf;

use thiserror::Error;
use zbus::zvariant::OwnedObjectPath;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub username: String,
    pub display_name: String,
    pub icon_file: Option<PathBuf>,
}

impl Account {
    pub fn override_account(username: String, display_name: Option<String>) -> Self {
        Self {
            display_name: display_name.unwrap_or_else(|| username.clone()),
            username,
            icon_file: None,
        }
    }

    fn from_properties(properties: Properties) -> Option<Self> {
        if properties.system_account || properties.locked || properties.username.is_empty() {
            return None;
        }
        let display_name = if properties.real_name.is_empty() {
            properties.username.clone()
        } else {
            properties.real_name
        };
        let icon_file = (!properties.icon_file.is_empty())
            .then(|| PathBuf::from(properties.icon_file))
            .filter(|path| path.is_file());
        Some(Self {
            username: properties.username,
            display_name,
            icon_file,
        })
    }
}

impl fmt::Display for Account {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.display_name == self.username {
            formatter.write_str(&self.username)
        } else {
            write!(formatter, "{} ({})", self.display_name, self.username)
        }
    }
}

#[derive(Debug)]
struct Properties {
    username: String,
    real_name: String,
    icon_file: String,
    system_account: bool,
    locked: bool,
}

#[derive(Debug, Error)]
pub enum AccountError {
    #[error("AccountsService request failed: {0}")]
    Bus(#[from] zbus::Error),
}

pub async fn discover() -> Result<Vec<Account>, AccountError> {
    let connection = zbus::Connection::system().await?;
    let manager = zbus::Proxy::new(
        &connection,
        "org.freedesktop.Accounts",
        "/org/freedesktop/Accounts",
        "org.freedesktop.Accounts",
    )
    .await?;
    let paths: Vec<OwnedObjectPath> = manager.call("ListCachedUsers", &()).await?;
    let mut accounts = Vec::new();

    for path in paths {
        let user = zbus::Proxy::new(
            &connection,
            "org.freedesktop.Accounts",
            path.as_str(),
            "org.freedesktop.Accounts.User",
        )
        .await?;
        let properties = Properties {
            username: user.get_property("UserName").await?,
            real_name: user.get_property("RealName").await?,
            icon_file: user.get_property("IconFile").await?,
            system_account: user.get_property("SystemAccount").await?,
            locked: user.get_property("Locked").await?,
        };
        if let Some(account) = Account::from_properties(properties) {
            accounts.push(account);
        }
    }

    accounts.sort_by(|left, right| {
        left.display_name
            .cmp(&right.display_name)
            .then_with(|| left.username.cmp(&right.username))
    });
    Ok(accounts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn properties() -> Properties {
        Properties {
            username: "alice".into(),
            real_name: "Alice Example".into(),
            icon_file: String::new(),
            system_account: false,
            locked: false,
        }
    }

    #[test]
    fn keeps_usable_human_accounts() {
        let account = Account::from_properties(properties()).unwrap();
        assert_eq!(account.username, "alice");
        assert_eq!(account.display_name, "Alice Example");
        assert_eq!(account.to_string(), "Alice Example (alice)");
    }

    #[test]
    fn rejects_system_locked_and_unnamed_accounts() {
        let mut system = properties();
        system.system_account = true;
        let mut locked = properties();
        locked.locked = true;
        let mut unnamed = properties();
        unnamed.username.clear();

        assert_eq!(Account::from_properties(system), None);
        assert_eq!(Account::from_properties(locked), None);
        assert_eq!(Account::from_properties(unnamed), None);
    }

    #[test]
    fn administrative_override_defaults_to_username() {
        let account = Account::override_account("operator".into(), None);
        assert_eq!(account.display_name, "operator");
        assert_eq!(account.icon_file, None);
    }
}
