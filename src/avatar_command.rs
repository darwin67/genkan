//! Logged-in users choose pictures through AccountsService, never greetd.

use std::io::Write;
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use genkan::avatar;

#[derive(Debug, Args)]
pub(crate) struct Arguments {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print the installed built-in picture names.
    List,
    /// Set your account picture to a built-in name or a local PNG/JPEG photo.
    Set {
        #[arg(required_unless_present = "file", conflicts_with = "file")]
        name: Option<String>,
        /// Import a centered square photo, scaled to 512 by 512.
        #[arg(long, value_name = "PATH")]
        file: Option<PathBuf>,
    },
    /// Remove your picture and show initials instead.
    Clear,
}

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub(crate) fn run(arguments: Arguments) -> Result<()> {
    let executable = std::env::current_exe()?;
    let directory = avatar::builtin_directory(&executable)
        .ok_or("cannot locate the installed account pictures")?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    match arguments.command {
        Command::List => {
            for name in builtin_names(&directory)? {
                println!("{name}");
            }
        }
        Command::Set {
            file: Some(path), ..
        } => {
            let png = avatar::photo_png(&executable, &path)?;
            // NamedTempFile uses O_EXCL and mode 0600. Hold it until the
            // synchronous D-Bus reply: AccountsService has copied it by then.
            let temporary = temporary_png(&png)?;
            runtime.block_on(crate::accounts::set_own_picture(
                temporary
                    .path()
                    .to_str()
                    .ok_or("temporary path is not UTF-8")?,
            ))?;
        }
        Command::Set {
            name: Some(name), ..
        } => {
            let path = builtin_path(&directory, &name)?;
            runtime.block_on(crate::accounts::set_own_picture(
                path.to_str().ok_or("installed picture path is not UTF-8")?,
            ))?;
        }
        Command::Set { .. } => unreachable!("clap requires a name or --file"),
        Command::Clear => runtime.block_on(crate::accounts::set_own_picture(""))?,
    }
    Ok(())
}

fn builtin_names(directory: &Path) -> Result<Vec<String>> {
    let manifest: toml::Table =
        std::fs::read_to_string(directory.join("manifest.toml"))?.parse()?;
    let entries = manifest
        .get("avatar")
        .and_then(toml::Value::as_array)
        .ok_or("installed picture manifest has no avatar list")?;
    let mut names = Vec::new();
    for entry in entries {
        let name = entry
            .get("name")
            .and_then(toml::Value::as_str)
            .filter(|name| !name.is_empty() && name.bytes().all(|byte| byte.is_ascii_lowercase()))
            .ok_or("invalid name in installed picture manifest")?;
        if names.iter().any(|existing| existing == name) {
            return Err("duplicate name in installed picture manifest".into());
        }
        names.push(name.to_owned());
    }
    Ok(names)
}

fn builtin_path(directory: &Path, name: &str) -> Result<PathBuf> {
    if !builtin_names(directory)?
        .iter()
        .any(|candidate| candidate == name)
    {
        return Err(format!("unknown built-in picture {name:?}; use 'genkan avatar list'").into());
    }
    Ok(directory.join(format!("{name}.png")))
}

fn temporary_png(png: &[u8]) -> std::io::Result<tempfile::NamedTempFile> {
    let mut file = tempfile::Builder::new()
        .prefix("genkan-avatar-")
        .suffix(".png")
        .tempfile()?;
    file.write_all(png)?;
    file.flush()?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn only_installed_names_can_select_packaged_pngs() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("manifest.toml"),
            include_str!("../assets/avatars/manifest.toml"),
        )
        .unwrap();
        let names = builtin_names(directory.path()).unwrap();
        assert_eq!(names.len(), 12);
        assert_eq!(names[0], "lake");
        assert_eq!(names[11], "citrus");
        assert_eq!(
            builtin_path(directory.path(), "moon").unwrap(),
            directory.path().join("moon.png")
        );
        for name in [
            "", "Moon", "missing", "../moon", "/moon", "moon.png", "moon\n",
        ] {
            assert!(builtin_path(directory.path(), name).is_err(), "{name:?}");
        }
        std::fs::write(
            directory.path().join("manifest.toml"),
            "[[avatar]]\nname = '../escape'\n",
        )
        .unwrap();
        assert!(builtin_names(directory.path()).is_err());
    }

    #[test]
    fn temporary_png_is_private_exclusive_and_removed_on_success_or_error() {
        let file = temporary_png(b"test png bytes").unwrap();
        let path = file.path().to_owned();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"test png bytes");
        assert_eq!(
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::AlreadyExists
        );
        drop(file);
        assert!(!path.exists());

        let mut error_path = PathBuf::new();
        let mut fail = || -> Result<()> {
            let file = temporary_png(b"error path")?;
            error_path = file.path().to_owned();
            Err("AccountsService refused".into())
        };
        assert!(fail().is_err());
        assert!(!error_path.exists());
    }
}
