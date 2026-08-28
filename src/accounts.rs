use std::fmt;
use std::fs;
use std::io::{Cursor, Read};
use std::path::PathBuf;

use image::io::{Limits, Reader as ImageReader};
use image::ImageFormat;
use thiserror::Error;
use zbus::zvariant::OwnedObjectPath;

const MAX_LABEL_CHARS: usize = 80;
const MAX_ICON_BYTES: u64 = 1024 * 1024;
const MAX_ICON_DIMENSION: u32 = 1024;
const AVATAR_DIMENSION: u32 = 184;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Avatar {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub username: String,
    pub display_name: String,
    pub avatar: Option<Avatar>,
}

impl Account {
    pub fn override_account(username: String, display_name: Option<String>) -> Self {
        Self {
            display_name: presentation_label(display_name.as_deref().unwrap_or(&username)),
            username,
            avatar: None,
        }
    }

    fn from_properties(properties: Properties) -> Option<Self> {
        if properties.system_account || properties.locked || !valid_username(&properties.username) {
            return None;
        }
        let display_name = if properties.real_name.is_empty() {
            properties.username.clone()
        } else {
            presentation_label(&properties.real_name)
        };
        let display_name = if display_name.is_empty() {
            properties.username.clone()
        } else {
            display_name
        };
        let avatar = (!properties.icon_file.is_empty())
            .then(|| PathBuf::from(properties.icon_file))
            .and_then(|path| load_avatar(&path));
        Some(Self {
            username: properties.username,
            display_name,
            avatar,
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

fn valid_username(username: &str) -> bool {
    !username.is_empty()
        && username.chars().count() <= 256
        && !username.chars().any(|character| {
            character.is_control() || character.is_whitespace() || is_directional_control(character)
        })
}

fn presentation_label(value: &str) -> String {
    let label = value
        .split_whitespace()
        .flat_map(|word| [word, " "])
        .flat_map(str::chars)
        .filter(|character| !character.is_control() && !is_directional_control(*character))
        .take(MAX_LABEL_CHARS)
        .collect::<String>();
    label.trim_end().to_owned()
}

fn is_directional_control(character: char) -> bool {
    matches!(
        character,
        '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
    )
}

fn load_avatar(path: &std::path::Path) -> Option<Avatar> {
    let metadata = fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_ICON_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .ok()?
        .take(MAX_ICON_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_ICON_BYTES {
        return None;
    }
    decode_avatar(&bytes)
}

fn decode_avatar(bytes: &[u8]) -> Option<Avatar> {
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    if !matches!(reader.format(), Some(ImageFormat::Png | ImageFormat::Jpeg)) {
        return None;
    }
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_ICON_DIMENSION);
    limits.max_image_height = Some(MAX_ICON_DIMENSION);
    limits.max_alloc = Some(16 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode().ok()?;
    let thumbnail = image
        .thumbnail(AVATAR_DIMENSION, AVATAR_DIMENSION)
        .to_rgba8();
    Some(Avatar {
        width: thumbnail.width(),
        height: thumbnail.height(),
        rgba: thumbnail.into_raw(),
    })
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
        icon_file: user.get_property("IconFile").await?,
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
        assert_eq!(account.avatar, None);
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
    fn rejects_malformed_and_oversized_avatars() {
        assert_eq!(decode_avatar(b"not an image"), None);

        let valid = image::DynamicImage::new_rgb8(2, 3);
        let mut valid_encoded = Cursor::new(Vec::new());
        valid
            .write_to(&mut valid_encoded, ImageFormat::Png)
            .unwrap();
        let avatar = decode_avatar(valid_encoded.get_ref()).unwrap();
        assert!(avatar.width <= AVATAR_DIMENSION);
        assert!(avatar.height <= AVATAR_DIMENSION);
        assert_eq!(
            avatar.rgba.len(),
            (avatar.width * avatar.height * 4) as usize
        );

        let image = image::DynamicImage::new_rgb8(MAX_ICON_DIMENSION + 1, 1);
        let mut encoded = Cursor::new(Vec::new());
        image.write_to(&mut encoded, ImageFormat::Png).unwrap();
        assert_eq!(decode_avatar(encoded.get_ref()), None);

        let path =
            std::env::temp_dir().join(format!("genkan-oversized-avatar-{}", std::process::id()));
        fs::write(&path, vec![0; (MAX_ICON_BYTES + 1) as usize]).unwrap();
        assert_eq!(load_avatar(&path), None);
        fs::remove_file(path).unwrap();
    }
}
