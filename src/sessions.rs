use std::collections::BTreeSet;
use std::env;
use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use rclip_desktop_entry::{parse, EntryType, ExecPiece, FieldCode, Locale};
use rustix::fs::{access, Access};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub name: String,
    pub command: Vec<String>,
    pub session_id: String,
    pub desktop_names: Vec<String>,
}

impl Session {
    pub fn environment(&self) -> Vec<String> {
        let mut environment = vec![
            "XDG_SESSION_TYPE=wayland".into(),
            format!("XDG_SESSION_DESKTOP={}", self.session_id),
        ];
        if !self.desktop_names.is_empty() {
            environment.push(format!(
                "XDG_CURRENT_DESKTOP={}",
                self.desktop_names.join(":")
            ));
        }
        environment
    }
}

impl fmt::Display for Session {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.name)
    }
}

pub fn discover() -> Vec<Session> {
    let data_dirs = env::var_os("XDG_DATA_DIRS")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    discover_in(&session_directories(&data_dirs))
}

fn session_directories(data_dirs: &OsStr) -> Vec<PathBuf> {
    env::split_paths(data_dirs)
        .filter(|path| path.is_absolute())
        .map(|path| path.join("wayland-sessions"))
        .collect()
}

fn discover_in(directories: &[PathBuf]) -> Vec<Session> {
    let mut seen = BTreeSet::new();
    let mut sessions = Vec::new();

    for directory in directories {
        let Ok(entries) = fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("desktop") {
                continue;
            }
            let Some(id) = path.file_name().and_then(|value| value.to_str()) else {
                continue;
            };
            if seen.contains(id) {
                continue;
            }
            seen.insert(id.to_owned());
            match parse_desktop_entry(&path) {
                Some(ParsedEntry::Visible(session)) => {
                    sessions.push(session);
                }
                Some(ParsedEntry::Hidden) => {}
                None => {}
            }
        }
    }

    sessions.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.session_id.cmp(&right.session_id))
    });
    sessions
}

enum ParsedEntry {
    Hidden,
    Visible(Session),
}

fn parse_desktop_entry(path: &Path) -> Option<ParsedEntry> {
    let locale_name = env::var("LC_ALL")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            env::var("LC_MESSAGES")
                .ok()
                .filter(|value| !value.is_empty())
        })
        .or_else(|| env::var("LANG").ok().filter(|value| !value.is_empty()));
    let locale = locale_name.as_deref().and_then(Locale::parse);
    parse_desktop_entry_with_locale(path, locale.as_ref())
}

fn parse_desktop_entry_with_locale(
    path: &Path,
    locale: Option<&Locale<'_>>,
) -> Option<ParsedEntry> {
    let contents = fs::read(path).ok()?;
    let file = parse(&contents).ok()?;
    let entry = file.desktop_entry()?;

    let hidden = entry.boolean("Hidden").transpose().ok()?.unwrap_or(false);
    let no_display = entry
        .boolean("NoDisplay")
        .transpose()
        .ok()?
        .unwrap_or(false);
    if hidden || no_display {
        return Some(ParsedEntry::Hidden);
    }
    if file.entry_type() != Some(EntryType::Application) {
        return None;
    }

    let name = file.name(locale)?.to_unescaped().ok()?;

    if let Some(try_exec) = entry.value("TryExec") {
        let try_exec = try_exec.to_unescaped().ok()?;
        if !executable_available(&try_exec) {
            return None;
        }
    }

    let command = expand_exec(&file, path, &name)?;
    if !executable_available(&command[0]) {
        return None;
    }

    let desktop_names = entry
        .list("DesktopNames")
        .into_iter()
        .flatten()
        .map(|value| value.to_unescaped())
        .collect::<Result<Vec<_>, _>>()
        .ok()?;

    Some(ParsedEntry::Visible(Session {
        name,
        command,
        session_id: path.file_stem()?.to_str()?.to_owned(),
        desktop_names,
    }))
}

fn expand_exec(
    file: &rclip_desktop_entry::DesktopFile<'_>,
    path: &Path,
    name: &str,
) -> Option<Vec<String>> {
    let exec = file.exec()?;
    exec.validate().ok()?;
    let mut command = Vec::new();

    for argument in exec.args() {
        let argument = argument.ok()?;
        let mut expanded = String::new();
        for piece in argument.pieces() {
            match piece.ok()? {
                ExecPiece::Char(character) => expanded.push(character),
                ExecPiece::Field(FieldCode::TranslatedName) => expanded.push_str(name),
                ExecPiece::Field(FieldCode::DesktopFileLocation) => {
                    expanded.push_str(path.to_str()?)
                }
                ExecPiece::Field(FieldCode::Deprecated(_)) => {}
                ExecPiece::Field(
                    FieldCode::SingleFile
                    | FieldCode::FileList
                    | FieldCode::SingleUrl
                    | FieldCode::UrlList
                    | FieldCode::Icon,
                ) => return None,
                _ => return None,
            }
        }
        if !expanded.is_empty() {
            command.push(expanded);
        }
    }

    if command
        .first()
        .is_some_and(|program| !program.contains('='))
    {
        Some(command)
    } else {
        None
    }
}

fn executable_available(program: &str) -> bool {
    if program.contains('/') {
        let path = Path::new(program);
        return path.is_absolute() && is_executable(path);
    }
    env::var_os("PATH")
        .map(|path| {
            env::split_paths(&path).any(|directory| is_executable(&directory.join(program)))
        })
        .unwrap_or(false)
}

fn is_executable(path: &Path) -> bool {
    fs::metadata(path)
        .map(|metadata| metadata.is_file() && access(path, Access::EXEC_OK).is_ok())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(1);

    fn test_directory(name: &str) -> PathBuf {
        let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let directory =
            env::temp_dir().join(format!("genkan-{name}-{}-{sequence}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        directory
    }

    fn executable(directory: &Path, name: &str) -> PathBuf {
        let path = directory.join(name);
        fs::write(&path, "#!/bin/sh\n").unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).unwrap();
        path
    }

    fn write_session(directory: &Path, id: &str, body: &str) {
        fs::write(
            directory.join(format!("{id}.desktop")),
            format!("[Desktop Entry]\nType=Application\n{body}"),
        )
        .unwrap();
    }

    #[test]
    fn creates_distinct_wayland_desktop_environment() {
        let session = Session {
            name: "River".into(),
            command: vec!["river".into()],
            session_id: "river-session".into(),
            desktop_names: vec!["River".into(), "wlroots".into()],
        };

        assert_eq!(
            session.environment(),
            [
                "XDG_SESSION_TYPE=wayland",
                "XDG_SESSION_DESKTOP=river-session",
                "XDG_CURRENT_DESKTOP=River:wlroots",
            ]
        );
    }

    #[test]
    fn parses_quoting_escapes_and_safe_field_codes() {
        let directory = test_directory("exec");
        let program = executable(&directory, "session program");
        write_session(
            &directory,
            "river",
            &format!(
                "Name=River Session\nExec=\"{}\" \"two words\" 100%% %c %k\nDesktopNames=River;wlroots;\n",
                program.display()
            ),
        );

        let sessions = discover_in(std::slice::from_ref(&directory));
        assert_eq!(sessions.len(), 1);
        assert_eq!(
            sessions[0].command,
            [
                program.to_string_lossy().as_ref(),
                "two words",
                "100%",
                "River Session",
                directory.join("river.desktop").to_string_lossy().as_ref(),
            ]
        );
        assert_eq!(sessions[0].desktop_names, ["River", "wlroots"]);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn selects_localized_session_name() {
        let directory = test_directory("locale");
        let program = executable(&directory, "session");
        write_session(
            &directory,
            "localized",
            &format!(
                "Name=Default Name\nName[fr]=Nom localisé\nExec={}\n",
                program.display()
            ),
        );
        let locale = Locale::parse("fr_FR.UTF-8").unwrap();

        let Some(ParsedEntry::Visible(session)) =
            parse_desktop_entry_with_locale(&directory.join("localized.desktop"), Some(&locale))
        else {
            panic!("expected a visible session");
        };

        assert_eq!(session.name, "Nom localisé");
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn rejects_unsafe_or_invalid_entries() {
        let directory = test_directory("invalid");
        let program = executable(&directory, "session");
        fs::write(
            directory.join("wrong-type.desktop"),
            format!(
                "[Desktop Entry]\nType=Link\nName=Link\nExec={}\n",
                program.display()
            ),
        )
        .unwrap();
        write_session(
            &directory,
            "document-code",
            &format!("Name=Document\nExec={} %U\n", program.display()),
        );
        write_session(
            &directory,
            "unknown-code",
            &format!("Name=Unknown\nExec={} %x\n", program.display()),
        );
        write_session(
            &directory,
            "missing-executable",
            "Name=Missing\nExec=/does/not/exist\n",
        );
        write_session(
            &directory,
            "failed-try-exec",
            &format!(
                "Name=TryExec\nTryExec=/does/not/exist\nExec={}\n",
                program.display()
            ),
        );

        assert!(discover_in(std::slice::from_ref(&directory)).is_empty());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn honors_directory_precedence_and_hidden_masks() {
        let high = test_directory("high");
        let low = test_directory("low");
        let program = executable(&low, "shared-session");
        write_session(
            &high,
            "masked",
            "Name=Masked\nExec=/does/not/matter\nHidden=true\n",
        );
        write_session(
            &low,
            "masked",
            &format!("Name=Visible lower copy\nExec={}\n", program.display()),
        );
        write_session(
            &high,
            "preferred",
            &format!("Name=Preferred\nExec={}\n", program.display()),
        );
        write_session(
            &low,
            "preferred",
            &format!("Name=Lower copy\nExec={}\n", program.display()),
        );
        write_session(
            &low,
            "same-command",
            &format!("Name=Same command\nExec={}\n", program.display()),
        );

        let sessions = discover_in(&[high.clone(), low.clone()]);
        assert_eq!(
            sessions
                .iter()
                .map(|session| session.name.as_str())
                .collect::<Vec<_>>(),
            ["Preferred", "Same command"]
        );
        fs::remove_dir_all(high).unwrap();
        fs::remove_dir_all(low).unwrap();
    }

    #[test]
    fn invalid_higher_priority_entry_masks_lower_copy() {
        let high = test_directory("invalid-high");
        let low = test_directory("valid-low");
        let program = executable(&low, "session");
        write_session(
            &high,
            "shared",
            "Name=Broken\nTryExec=./relative\nExec=./relative\n",
        );
        write_session(
            &low,
            "shared",
            &format!("Name=Lower copy\nExec={}\n", program.display()),
        );

        assert!(discover_in(&[high.clone(), low.clone()]).is_empty());
        fs::remove_dir_all(high).unwrap();
        fs::remove_dir_all(low).unwrap();
    }

    #[test]
    fn rejects_relative_paths_and_inaccessible_execute_bits() {
        let directory = test_directory("permissions");
        write_session(&directory, "relative", "Name=Relative\nExec=./session\n");
        let inaccessible = directory.join("inaccessible");
        fs::write(&inaccessible, "#!/bin/sh\n").unwrap();
        let mut permissions = fs::metadata(&inaccessible).unwrap().permissions();
        permissions.set_mode(0o010);
        fs::set_permissions(&inaccessible, permissions).unwrap();
        write_session(
            &directory,
            "inaccessible",
            &format!("Name=Inaccessible\nExec={}\n", inaccessible.display()),
        );

        assert!(discover_in(std::slice::from_ref(&directory)).is_empty());
        assert!(session_directories(OsStr::new("relative:/usr/share"))
            .iter()
            .all(|path| path.is_absolute()));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn omits_current_desktop_without_desktop_names() {
        let directory = test_directory("desktop-name");
        let program = executable(&directory, "niri");
        write_session(
            &directory,
            "niri",
            &format!("Name=Niri\nExec={}\n", program.display()),
        );

        let session = discover_in(std::slice::from_ref(&directory)).pop().unwrap();
        assert_eq!(session.session_id, "niri");
        assert!(session.desktop_names.is_empty());
        assert_eq!(
            session.environment(),
            ["XDG_SESSION_TYPE=wayland", "XDG_SESSION_DESKTOP=niri"]
        );
        fs::remove_dir_all(directory).unwrap();
    }
}
