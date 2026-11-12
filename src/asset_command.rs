//! Discover, explicitly cache, and preview wallpapers without changing a session.

use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};

use crate::{LoginArguments, Placement, PreviewFixture, WallpaperAppearance};

mod catalog;
mod download;
mod preflight;

use catalog::Asset;

const DEFAULT_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Debug, Args)]
pub(crate) struct Arguments {
    /// Use [assets.<key>] entries from this TOML file instead of the curated catalog.
    #[arg(long, global = true, value_name = "PATH")]
    catalog: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// List catalog wallpapers, their resolution, size, and local availability.
    List {
        /// Include full names, source URLs, and usage rights below each row.
        #[arg(long)]
        details: bool,
    },
    /// Download and validate an original into the local wallpaper cache.
    Download {
        id: String,
        /// Supply your own key and direct HTTP(S) source.
        #[arg(long)]
        url: Option<String>,
        /// Optional trusted SHA-256 for a custom direct source.
        #[arg(long, requires = "url")]
        sha256: Option<String>,
        /// Maximum download and expanded media size (default: 2 GiB).
        #[arg(long, default_value_t = DEFAULT_MAX_BYTES, value_parser = clap::value_parser!(u64).range(4096..=16 * 1024 * 1024 * 1024))]
        max_bytes: u64,
    },
    /// Check a remote source using bounded range reads, without installing it.
    Preflight {
        /// Catalog ID, or your own key when supplying --url.
        id: String,
        /// Inspect this direct HTTP(S) source instead of consulting a catalog.
        #[arg(long)]
        url: Option<String>,
        /// Maximum media response bytes consumed; no automatic full download.
        #[arg(long, default_value_t = 8 * 1024 * 1024, value_parser = clap::value_parser!(u64).range(4096..=64 * 1024 * 1024))]
        max_bytes: u64,
    },
    #[command(hide = true)]
    PreflightWorker {
        #[arg(long, value_parser = clap::value_parser!(u64).range(4096..=64 * 1024 * 1024))]
        max_bytes: u64,
        #[arg(long)]
        local_file: Option<PathBuf>,
        #[arg(long)]
        poster_file: Option<PathBuf>,
    },
    #[command(hide = true)]
    DownloadWorker {
        #[arg(long)]
        stage: PathBuf,
        #[arg(long, value_parser = clap::value_parser!(u64).range(4096..=16 * 1024 * 1024 * 1024))]
        max_bytes: u64,
    },
    /// Preview a wallpaper in a safe window; download it first if missing.
    Preview {
        /// Wallpaper ID from `genkan assets list`.
        id: String,
        /// Maximum download and expanded media size (default: 2 GiB).
        #[arg(long, default_value_t = DEFAULT_MAX_BYTES, value_parser = clap::value_parser!(u64).range(4096..=16 * 1024 * 1024 * 1024))]
        max_bytes: u64,
        /// Choose a dynamic HEIC's light or dark appearance.
        #[arg(long, value_enum, default_value = "automatic")]
        appearance: WallpaperAppearance,
        /// Show a MOV poster, or disable HEIC dissolves while keeping scheduling.
        #[arg(long)]
        reduce_motion: bool,
    },
}

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub(crate) fn run(arguments: Arguments) -> Result<()> {
    if let Command::DownloadWorker {
        ref stage,
        max_bytes,
    } = arguments.command
    {
        return download::worker(stage, max_bytes);
    }
    if let Command::PreflightWorker {
        max_bytes,
        ref local_file,
        ref poster_file,
    } = arguments.command
    {
        let code = preflight::worker(max_bytes, local_file.as_deref(), poster_file.as_deref())?;
        std::process::exit(code);
    }
    if let Command::Preflight { id, url, max_bytes } = arguments.command {
        let asset = if let Some(url) = url {
            Asset::from_entry(&id, [("url".into(), url.into())].into_iter().collect())?
        } else {
            find_asset(catalog::load(arguments.catalog.as_deref())?, &id)?
        };
        let code = preflight::run(&asset, max_bytes)?;
        std::process::exit(code);
    }
    let assets = catalog::load(arguments.catalog.as_deref())?;
    let directory = crate::wallpaper::asset_directory()?;
    match arguments.command {
        Command::List { details } => {
            list(&assets, &directory, details, &mut std::io::stdout().lock())?
        }
        Command::Download {
            id,
            url,
            sha256,
            max_bytes,
        } => {
            let asset = if let Some(url) = url {
                if catalog::declared(arguments.catalog.as_deref())?
                    .iter()
                    .any(|asset| asset.id == id)
                {
                    return Err(
                        "this key belongs to the catalog; use a different custom key".into(),
                    );
                }
                let mut metadata: toml::Table = [("url".into(), url.into())].into_iter().collect();
                if let Some(hash) = sha256 {
                    metadata.insert("sha256".into(), hash.into());
                }
                Asset::from_entry(&id, metadata)?
            } else {
                find_asset(assets, &id)?
            };
            download::run(asset, &directory, max_bytes)?;
        }
        Command::Preview {
            id,
            max_bytes,
            appearance,
            reduce_motion,
        } => {
            let asset = find_asset(assets, &id)?;
            if !asset.path(&directory).is_some_and(|path| path.is_file()) {
                download::run(asset, &directory, max_bytes)?;
                // Tokio's download signal handlers outlive its runtime. Exec
                // resets them and reloads the newly published cache snapshot.
                return Err(std::process::Command::new(std::env::current_exe()?)
                    .args(std::env::args_os().skip(1))
                    .exec()
                    .into());
            }
            let arguments = preview_arguments(asset, &directory, appearance, reduce_motion)?;
            eprintln!("Preview only: close the window to exit; no settings are changed.");
            crate::run_login(arguments)?;
        }
        Command::Preflight { .. }
        | Command::PreflightWorker { .. }
        | Command::DownloadWorker { .. } => unreachable!(),
    }
    Ok(())
}

fn find_asset(assets: Vec<Asset>, id: &str) -> Result<Asset> {
    assets
        .into_iter()
        .find(|asset| asset.id == id)
        .ok_or_else(|| format!("unknown wallpaper {id:?}; use 'genkan assets list'").into())
}

fn list(assets: &[Asset], directory: &Path, details: bool, output: &mut impl Write) -> Result<()> {
    let id_width = assets
        .iter()
        .map(|asset| asset.id.len())
        .max()
        .unwrap_or(2)
        .max(2);
    writeln!(
        output,
        "{:<id_width$}  {:<6}  {:<11}  {:>7}  STATUS",
        "ID", "FORMAT", "RESOLUTION", "MiB"
    )?;
    for asset in assets {
        writeln!(
            output,
            "{:<id_width$}  {:<6}  {:<11}  {:>7}  {}",
            asset.id,
            asset.format.as_deref().unwrap_or("?").to_ascii_uppercase(),
            format!(
                "{}x{}",
                asset.width.map_or_else(|| "?".into(), |n| n.to_string()),
                asset.height.map_or_else(|| "?".into(), |n| n.to_string())
            ),
            asset
                .bytes
                .map_or_else(|| "?".into(), |n| format!("{:.1}", n as f64 / 1_048_576.0)),
            if asset.path(directory).is_some_and(|path| path.is_file()) {
                "installed"
            } else {
                "missing"
            },
        )?;
        if details {
            writeln!(output, "  Name:   {}", asset.name)?;
            writeln!(
                output,
                "  Source: {}",
                asset.url.as_deref().unwrap_or("local-only")
            )?;
            writeln!(
                output,
                "  Rights: {}\n",
                asset
                    .metadata
                    .get("rights")
                    .and_then(toml::Value::as_str)
                    .unwrap_or("not supplied")
            )?;
        }
    }
    Ok(())
}

fn preview_arguments(
    asset: Asset,
    directory: &Path,
    appearance: WallpaperAppearance,
    reduce_motion: bool,
) -> Result<LoginArguments> {
    let path = asset
        .path(directory)
        .ok_or("local preview requires a declared format (mov or heic)")?;
    Ok(LoginArguments {
        list_preview_fixtures: false,
        username: None,
        display_name: None,
        windowed: true,
        placement: Placement::Compositor,
        preview: Some(PreviewFixture::Selected),
        width: None,
        height: None,
        wallpaper: asset.movie(),
        wallpaper_file: Some(path.canonicalize()?),
        reduce_motion,
        animated_preview: true,
        appearance,
        authentication_output: None,
    })
}
