#[cfg(feature = "gui")]
fn main() {
    // codecparsers has no Rust binding. Keep its native HEVC structures behind
    // a small opaque C interface instead of duplicating their ABI or parser.
    let library = pkg_config::Config::new()
        .atleast_version("1.26")
        .probe("gstreamer-codecparsers-1.0")
        .expect("GStreamer HEVC parser development files are required");
    cc::Build::new()
        .file("src/wallpaper/temporal.c")
        .includes(library.include_paths)
        .opt_level(2)
        .warnings(true)
        .compile("wallpaper_temporal");
    println!("cargo:rerun-if-changed=src/wallpaper/temporal.c");
}

#[cfg(not(feature = "gui"))]
fn main() {}
