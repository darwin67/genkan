//! Discover packaged wallpapers and review them without changing a session.

use std::io::Write;
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand, ValueEnum};

use crate::{wallpaper, LoginArguments, Placement, PreviewFixture, WallpaperAppearance};

#[derive(Debug, Args)]
pub(crate) struct Arguments {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// List catalog wallpapers, their resolution, size, and local availability.
    List,
    /// Play a local wallpaper in a safe windowed login preview; change nothing.
    Preview {
        /// Wallpaper ID from `genkan assets list`.
        id: String,
        /// Choose a dynamic HEIC's light or dark appearance.
        #[arg(long, value_enum, default_value = "automatic")]
        appearance: WallpaperAppearance,
        /// Show a MOV poster, or disable HEIC dissolves while keeping scheduling.
        #[arg(long)]
        reduce_motion: bool,
    },
}

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

struct Asset {
    id: String,
    name: String,
    format: &'static str,
    width: i64,
    height: i64,
    bytes: i64,
    install_name: String,
    movie: Option<wallpaper::Catalog>,
}

impl Asset {
    fn path(&self, directory: &Path) -> PathBuf {
        directory.join(self.format).join(&self.install_name)
    }
}

pub(crate) fn run(arguments: Arguments) -> Result<()> {
    let directory = std::env::var_os("GENKAN_WALLPAPER_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            let executable = std::env::current_exe().ok()?;
            Some(
                executable
                    .parent()?
                    .parent()?
                    .join("share/genkan/wallpapers"),
            )
        })
        .ok_or("cannot locate the installed wallpapers")?;
    match arguments.command {
        Command::List => list(&directory, &mut std::io::stdout().lock())?,
        Command::Preview {
            id,
            appearance,
            reduce_motion,
        } => {
            let arguments = preview_arguments(&directory, &id, appearance, reduce_motion)?;
            eprintln!("Preview only: close the window to exit; no settings are changed.");
            crate::run_login(arguments)?;
        }
    }
    Ok(())
}

fn catalog() -> Result<Vec<Asset>> {
    // Compile the same integrity record that Nix packages. Listing works even
    // when media is absent, and never discovers or downloads remote content.
    let manifest: toml::Table = include_str!("../assets/wallpapers/manifest.toml").parse()?;
    let mut assets = Vec::new();
    for (key, format, geometry) in [
        ("wallpaper", "mov", "media"),
        ("dynamic_heic", "heic", "structure"),
    ] {
        let entries = manifest
            .get(key)
            .and_then(toml::Value::as_array)
            .ok_or("wallpaper manifest has no catalog")?;
        for entry in entries {
            let text = |key| {
                entry
                    .get(key)
                    .and_then(toml::Value::as_str)
                    .map(str::to_owned)
                    .ok_or("wallpaper manifest is missing a text field")
            };
            let id = text("id")?;
            let dimensions = entry.get(geometry).ok_or("missing wallpaper dimensions")?;
            let dimension = |key| {
                dimensions
                    .get(key)
                    .and_then(toml::Value::as_integer)
                    .ok_or("missing wallpaper dimension")
            };
            assets.push(Asset {
                movie: if format == "mov" {
                    Some(wallpaper::Catalog::from_str(&id, false)?)
                } else {
                    None
                },
                id,
                name: text("name")?,
                format,
                width: dimension("width")?,
                height: dimension("height")?,
                bytes: entry
                    .get("byte_size")
                    .and_then(toml::Value::as_integer)
                    .ok_or("missing wallpaper byte size")?,
                install_name: text("install_name")?,
            });
        }
    }
    Ok(assets)
}

fn list(directory: &Path, output: &mut impl Write) -> Result<()> {
    writeln!(output, "ID\tFORMAT\tRESOLUTION\tMiB\tSTATUS\tNAME")?;
    for asset in catalog()? {
        writeln!(
            output,
            "{}\t{}\t{}x{}\t{:.1}\t{}\t{}",
            asset.id,
            asset.format.to_ascii_uppercase(),
            asset.width,
            asset.height,
            asset.bytes as f64 / 1_048_576.0,
            if asset.path(directory).is_file() {
                "installed"
            } else {
                "missing"
            },
            asset.name,
        )?;
    }
    Ok(())
}

fn preview_arguments(
    directory: &Path,
    id: &str,
    appearance: WallpaperAppearance,
    reduce_motion: bool,
) -> Result<LoginArguments> {
    let asset = catalog()?
        .into_iter()
        .find(|asset| asset.id == id)
        .ok_or_else(|| format!("unknown wallpaper {id:?}; use 'genkan assets list'"))?;
    let path = asset.path(directory);
    if !path.is_file() {
        return Err(format!(
            "wallpaper {id:?} is not installed at {}; install the Genkan asset package or enter 'nix develop'",
            path.display()
        ).into());
    }
    Ok(LoginArguments {
        list_preview_fixtures: false,
        username: None,
        display_name: None,
        windowed: true,
        placement: Placement::Compositor,
        preview: Some(PreviewFixture::Selected),
        width: None,
        height: None,
        wallpaper: asset.movie.unwrap_or_default(),
        wallpaper_file: Some(path.canonicalize()?),
        reduce_motion,
        animated_preview: true,
        appearance,
        authentication_output: None,
    })
}
