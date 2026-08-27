use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub name: String,
    pub command: Vec<String>,
    pub desktop: String,
}

impl Session {
    pub fn sway(command: Vec<String>) -> Self {
        Self {
            name: "Sway".into(),
            command,
            desktop: "sway".into(),
        }
    }

    pub fn environment(&self) -> Vec<String> {
        vec![
            "XDG_SESSION_TYPE=wayland".into(),
            format!("XDG_CURRENT_DESKTOP={}", self.desktop),
            format!("XDG_SESSION_DESKTOP={}", self.desktop),
        ]
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
    let mut desktop = None;
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
            "DesktopNames" => desktop = value.split(';').next().map(str::to_owned),
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
        desktop: desktop.unwrap_or_else(|| {
            path.file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or("wayland")
                .to_owned()
        }),
        command,
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
        assert_eq!(parsed.desktop, "sway");
        fs::remove_dir_all(directory).unwrap();
    }
}
