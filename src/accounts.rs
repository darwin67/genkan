use std::fmt;

use thiserror::Error;
use zbus::zvariant::OwnedObjectPath;

const MAX_LABEL_CHARS: usize = 80;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub username: String,
    pub display_name: String,
}

impl Account {
    pub fn override_account(username: String, display_name: Option<String>) -> Self {
        Self {
            display_name: presentation_label(display_name.as_deref().unwrap_or(&username))
                .unwrap_or_else(|| username.clone()),
            username,
        }
    }

    fn from_properties(properties: Properties) -> Option<Self> {
        if properties.system_account || properties.locked || !valid_username(&properties.username) {
            return None;
        }
        let display_name = if properties.real_name.is_empty() {
            properties.username.clone()
        } else {
            presentation_label(&properties.real_name).unwrap_or_else(|| properties.username.clone())
        };
        Some(Self {
            username: properties.username,
            display_name,
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
    system_account: bool,
    locked: bool,
}

#[derive(Debug, Error)]
pub enum AccountError {
    #[error("AccountsService request failed: {0}")]
    Bus(#[from] zbus::Error),
}

pub(crate) fn valid_username(username: &str) -> bool {
    !username.is_empty()
        && username.chars().count() <= 256
        && !username.chars().any(|character| {
            character.is_control() || character.is_whitespace() || is_format_control(character)
        })
}

pub(crate) fn presentation_label(value: &str) -> Option<String> {
    let label = value
        .split_whitespace()
        .flat_map(|word| [word, " "])
        .flat_map(str::chars)
        .filter(|character| !character.is_control() && !is_format_control(*character))
        .take(MAX_LABEL_CHARS)
        .collect::<String>();
    let label = label.trim_end();
    (!label.is_empty()).then(|| label.to_owned())
}

fn is_format_control(character: char) -> bool {
    matches!(
        character,
        '\u{00ad}'
            | '\u{061c}'
            | '\u{180e}'
            | '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{206f}'
            | '\u{feff}'
    )
}

async fn load_properties(
    connection: &zbus::Connection,
    path: &OwnedObjectPath,
) -> Result<Properties, zbus::Error> {
    let user = zbus::Proxy::new(
        connection,
        "org.freedesktop.Accounts",
        path.as_str(),
        "org.freedesktop.Accounts.User",
    )
    .await?;
    Ok(Properties {
        username: user.get_property("UserName").await?,
        real_name: user.get_property("RealName").await?,
        system_account: user.get_property("SystemAccount").await?,
        locked: user.get_property("Locked").await?,
    })
}

fn collect_accounts<E>(
    results: impl IntoIterator<Item = Result<Properties, E>>,
) -> Result<Vec<Account>, E> {
    let mut accounts = Vec::new();
    let mut first_error = None;
    for result in results {
        match result {
            Ok(properties) => accounts.extend(Account::from_properties(properties)),
            Err(error) if first_error.is_none() => first_error = Some(error),
            Err(_) => {}
        }
    }
    if accounts.is_empty() {
        if let Some(error) = first_error {
            return Err(error);
        }
    }
    accounts.sort_by(|left, right| {
        left.display_name
            .cmp(&right.display_name)
            .then_with(|| left.username.cmp(&right.username))
    });
    Ok(accounts)
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
    let mut users = Vec::with_capacity(paths.len());
    for path in paths {
        users.push(load_properties(&connection, &path).await);
    }
    collect_accounts(users).map_err(AccountError::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn properties() -> Properties {
        Properties {
            username: "alice".into(),
            real_name: "Alice Example".into(),
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
    }

    #[test]
    fn retains_valid_accounts_when_an_object_fails() {
        let accounts = collect_accounts([Ok(properties()), Err("stale object")]).unwrap();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].username, "alice");

        assert_eq!(
            collect_accounts([Err::<Properties, _>("stale object")]),
            Err("stale object")
        );
    }

    #[test]
    fn bounds_account_labels() {
        let mut value = properties();
        value.real_name = format!("Alice\n\u{202e}{}", "x".repeat(100));
        let account = Account::from_properties(value).unwrap();
        assert!(!account.display_name.contains('\n'));
        assert!(!account.display_name.contains('\u{202e}'));
        assert!(account.display_name.chars().count() <= MAX_LABEL_CHARS);
    }

    #[test]
    fn rejects_labels_containing_only_format_controls() {
        assert_eq!(presentation_label("\u{202e}\u{200b}"), None);
    }
}
