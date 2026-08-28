use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub name: String,
    pub command: Vec<String>,
    pub session_id: String,
    pub desktop_names: Vec<String>,
}

impl Session {
    pub fn sway(command: Vec<String>) -> Self {
        Self {
            name: "Sway".into(),
            command,
            session_id: "sway".into(),
            desktop_names: vec!["sway".into()],
        }
    }

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

pub fn discover(fallback: Session) -> Vec<Session> {
    let mut directories = vec![
        PathBuf::from("/usr/local/share/wayland-sessions"),
        PathBuf::from("/usr/share/wayland-sessions"),
    ];
    if let Some(data_dirs) = std::env::var_os("XDG_DATA_DIRS") {
        directories
            .extend(std::env::split_paths(&data_dirs).map(|path| path.join("wayland-sessions")));
    }

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
            if let Some(session) = parse_desktop_entry(&path) {
                if seen.insert(session.command.clone()) {
                    sessions.push(session);
                }
            }
        }
    }

    if !sessions
        .iter()
        .any(|session| session.command == fallback.command)
    {
        sessions.push(fallback);
    }
    sessions.sort_by(|left, right| left.name.cmp(&right.name));
    sessions
}

fn parse_desktop_entry(path: &Path) -> Option<Session> {
    let contents = fs::read_to_string(path).ok()?;
    let mut in_entry = false;
    let mut name = None;
    let mut command = None;
    let mut desktop_names = Vec::new();
    let mut hidden = false;

    for line in contents.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key {
            "Name" => name = Some(value.to_owned()),
            "Exec" => command = shell_words::split(value).ok().map(remove_field_codes),
            "DesktopNames" => {
                desktop_names = value
                    .split(';')
                    .filter(|name| !name.is_empty())
                    .map(str::to_owned)
                    .collect();
            }
            "Hidden" => hidden = value.eq_ignore_ascii_case("true"),
            _ => {}
        }
    }

    if hidden {
        return None;
    }
    let command = command?;
    if command.is_empty() {
        return None;
    }
    Some(Session {
        name: name?,
        command,
        session_id: path.file_stem()?.to_str()?.to_owned(),
        desktop_names,
    })
}

fn remove_field_codes(command: Vec<String>) -> Vec<String> {
    command
        .into_iter()
        .filter(|argument| !argument.starts_with('%'))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_wayland_environment() {
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
    fn omits_current_desktop_without_desktop_names() {
        let session = Session {
            name: "Niri".into(),
            command: vec!["niri".into()],
            session_id: "niri".into(),
            desktop_names: Vec::new(),
        };

        assert_eq!(
            session.environment(),
            ["XDG_SESSION_TYPE=wayland", "XDG_SESSION_DESKTOP=niri"]
        );
    }

    #[test]
    fn parses_wayland_session() {
        let directory = std::env::temp_dir().join(format!("genkan-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("sway.desktop");
        fs::write(
            &path,
            "[Desktop Entry]\nName=Sway Desktop\nExec=sway --unsupported-gpu %U\nDesktopNames=sway;wlroots\n",
        )
        .unwrap();

        let parsed = parse_desktop_entry(&path).unwrap();
        assert_eq!(parsed.name, "Sway Desktop");
        assert_eq!(parsed.command, ["sway", "--unsupported-gpu"]);
        assert_eq!(parsed.session_id, "sway");
        assert_eq!(parsed.desktop_names, ["sway", "wlroots"]);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn ignores_hidden_sessions() {
        let directory = std::env::temp_dir().join(format!("genkan-hidden-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("hidden.desktop");
        fs::write(
            &path,
            "[Desktop Entry]\nName=Hidden\nExec=hidden-session\nHidden=true\n",
        )
        .unwrap();

        assert_eq!(parse_desktop_entry(&path), None);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn uses_file_name_as_session_id_without_inventing_a_desktop_name() {
        let directory = std::env::temp_dir().join(format!("genkan-desktop-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("niri.desktop");
        fs::write(&path, "[Desktop Entry]\nName=Niri\nExec=niri --session\n").unwrap();

        let parsed = parse_desktop_entry(&path).unwrap();
        assert_eq!(parsed.session_id, "niri");
        assert!(parsed.desktop_names.is_empty());
        fs::remove_dir_all(directory).unwrap();
    }
}
