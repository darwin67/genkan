//! Catalog metadata is supplied by the curator, never inferred from a URL suffix.

use std::path::{Path, PathBuf};

use clap::ValueEnum;

use crate::wallpaper;

use super::Result;

#[derive(Clone, Debug)]
pub(super) struct Asset {
    pub id: String,
    pub name: String,
    pub format: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub bytes: Option<u64>,
    pub url: Option<String>,
    pub metadata: toml::Table,
    install_name: Option<String>,
}

impl Asset {
    pub fn path(&self, directory: &Path) -> Option<PathBuf> {
        let parent = if let Some(generation) = wallpaper::cache_generation(&self.metadata) {
            directory.join("objects").join(generation)
        } else {
            directory.join(self.format.as_ref()?)
        };
        Some(parent.join(self.install_name.as_ref()?))
    }

    pub fn movie(&self) -> wallpaper::Catalog {
        wallpaper::Catalog::from_str(&self.id, false).unwrap_or_default()
    }

    pub fn from_entry(id: &str, entry: toml::Table) -> Result<Self> {
        if id.is_empty()
            || id.len() > 80
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err("asset keys must contain 1–80 ASCII letters, digits, '-' or '_'".into());
        }
        let text = |key: &str| -> Result<Option<String>> {
            entry
                .get(key)
                .map(|value| {
                    value
                        .as_str()
                        .filter(|s| !s.is_empty() && !s.chars().any(char::is_control))
                        .map(str::to_owned)
                        .ok_or_else(|| {
                            format!("{id}: {key} must be nonempty text without control characters")
                                .into()
                        })
                })
                .transpose()
        };
        let number = |key: &str| -> Result<Option<u64>> {
            entry
                .get(key)
                .map(|value| {
                    value
                        .as_integer()
                        .filter(|n| *n > 0)
                        .map(|n| n as u64)
                        .ok_or_else(|| format!("{id}: {key} must be a positive integer").into())
                })
                .transpose()
        };
        let format = text("format")?.map(|s| s.to_ascii_lowercase());
        if entry.contains_key("cache_generation") && wallpaper::cache_generation(&entry).is_none() {
            return Err(format!("{id}: invalid cached generation").into());
        }
        if format
            .as_deref()
            .is_some_and(|s| !matches!(s, "mov" | "heic"))
        {
            return Err(format!("{id}: format must be mov or heic").into());
        }
        for key in ["sha256", "archive_sha256", "poster_sha256"] {
            if let Some(hash) = text(key)? {
                if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err(format!("{id}: {key} must contain 64 hexadecimal digits").into());
                }
            }
        }
        if text("archive_member")?.is_some() {
            for key in ["sha256", "archive_sha256", "byte_size", "archive_byte_size"] {
                if !entry.contains_key(key) {
                    return Err(format!("{id}: archive_member requires {key}").into());
                }
            }
        }
        number("archive_byte_size")?;
        for key in ["rights", "attribution", "source_page_url", "preview_url"] {
            text(key)?;
        }
        if let Some(duration) = entry.get("duration_seconds") {
            let value = duration
                .as_float()
                .or_else(|| duration.as_integer().map(|n| n as f64));
            if !value.is_some_and(|n| n.is_finite() && n > 0.0) {
                return Err(format!("{id}: duration_seconds must be positive and finite").into());
            }
        }
        let dimension = |key| -> Result<Option<u32>> {
            number(key)?
                .map(|n| u32::try_from(n).map_err(Into::into))
                .transpose()
        };
        Ok(Self {
            id: id.into(),
            name: text("name")?.unwrap_or_else(|| id.into()),
            install_name: format.as_ref().map(|format| format!("{id}.{format}")),
            format,
            width: dimension("width")?,
            height: dimension("height")?,
            bytes: number("byte_size")?,
            url: text("url")?,
            metadata: entry,
        })
    }
}

pub(super) fn load(path: Option<&Path>) -> Result<Vec<Asset>> {
    let mut assets = declared(path)?;
    if let Ok(directory) = wallpaper::asset_directory() {
        let entries = directory.join("entries");
        if entries.is_dir() {
            for entry in std::fs::read_dir(entries)? {
                let entry = entry?.path();
                if entry.extension().is_none_or(|ext| ext != "toml") {
                    continue;
                }
                let id = entry
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .ok_or("invalid cached key")?;
                let table = read_table(&entry)?;
                let cached = Asset::from_entry(id, table)?;
                if let Some(asset) = assets.iter_mut().find(|asset| asset.id == id) {
                    // A receipt may fill missing observations, never override a
                    // curator's URL, content pin, or declared geometry.
                    if asset.url == cached.url {
                        let mut metadata = cached.metadata;
                        metadata.extend(asset.metadata.clone());
                        *asset = Asset::from_entry(id, metadata)?;
                    }
                } else if path.is_none() {
                    assets.push(cached);
                }
            }
        }
    }
    Ok(assets)
}

pub(super) fn read_table(path: &Path) -> Result<toml::Table> {
    use std::io::Read;
    let mut input = String::new();
    crate::stable_file::open_regular(path)
        .map_err(|_| format!("{} is not a readable regular file", path.display()))?
        .take(1024 * 1024 + 1)
        .read_to_string(&mut input)?;
    if input.len() > 1024 * 1024 {
        return Err("asset catalog exceeds 1 MiB".into());
    }
    Ok(input.parse()?)
}

pub(super) fn declared(path: Option<&Path>) -> Result<Vec<Asset>> {
    if let Some(path) = path {
        let table = read_table(path)?;
        let entries = table
            .get("assets")
            .and_then(toml::Value::as_table)
            .ok_or("catalog must contain [assets.<key>] tables")?;
        return entries
            .iter()
            .map(|(id, entry)| {
                let table = entry
                    .as_table()
                    .ok_or("each asset must be a table")?
                    .clone();
                let asset = Asset::from_entry(id, table)?;
                if asset.url.is_none() {
                    return Err(format!("{id}: url is required").into());
                }
                Ok(asset)
            })
            .collect();
    }

    let manifest: toml::Table = include_str!("../../assets/wallpapers/manifest.toml").parse()?;
    let mut assets = Vec::new();
    for (key, format, geometry) in [
        ("wallpaper", "mov", "media"),
        ("dynamic_heic", "heic", "structure"),
    ] {
        for entry in manifest
            .get(key)
            .and_then(toml::Value::as_array)
            .ok_or("missing packaged catalog")?
        {
            if entry.get("delivery").and_then(toml::Value::as_str) == Some("repository") {
                continue;
            }
            let id = entry
                .get("id")
                .and_then(toml::Value::as_str)
                .ok_or("missing asset id")?;
            let mut metadata = entry.as_table().ok_or("invalid packaged asset")?.clone();
            metadata.insert("format".into(), format.into());
            if let Some(url) = entry.get("source_file_url") {
                metadata.insert("url".into(), url.clone());
            }
            for field in ["width", "height"] {
                if let Some(value) = entry.get(geometry).and_then(|g| g.get(field)) {
                    metadata.insert(field.into(), value.clone());
                }
            }
            assets.push(Asset::from_entry(id, metadata)?);
        }
    }
    Ok(assets)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_selected_asset_keeps_its_metadata_and_path_when_the_receipt_is_replaced() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join("entries")).unwrap();
        let old = Asset::from_entry("mine", "url = 'https://example.com/a'\nformat = 'mov'\ncache_generation = 'download-mine-first'".parse().unwrap()).unwrap();
        std::fs::write(
            directory.path().join("entries/mine.toml"),
            "format = 'heic'\ncache_generation = 'download-mine-second'",
        )
        .unwrap();
        assert_eq!(
            old.path(directory.path()).unwrap(),
            directory
                .path()
                .join("objects/download-mine-first/mine.mov")
        );
    }
}
