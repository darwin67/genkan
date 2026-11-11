//! Asset discovery and rejection through the public CLI, without a display.

#![cfg(feature = "gui")]

use std::path::Path;
use std::process::Command;

fn genkan(directory: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_genkan"));
    command
        .env("GENKAN_WALLPAPER_DIR", directory)
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("GREETD_SOCK")
        .env(
            "DBUS_SYSTEM_BUS_ADDRESS",
            "unix:path=/nonexistent-genkan-assets-test-bus",
        );
    command
}

#[test]
fn list_reports_local_availability_and_preview_rejects_missing_or_unknown_ids() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir(directory.path().join("mov")).unwrap();
    // Availability does not decode the file; listing is cheap and display-free.
    std::fs::write(directory.path().join("mov/tahoe-beach.mov"), []).unwrap();
    let output = genkan(directory.path())
        .args(["assets", "list"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    let listing = String::from_utf8(output.stdout).unwrap();
    assert!(listing.contains("tahoe-beach\tMOV\t3840x2160\t162.3\tinstalled"));
    assert!(listing.contains("iceland\tMOV\t3840x2160\t754.1\tmissing"));
    assert!(listing.contains("palau-jellies\tMOV\t3840x2160\t637.0\tmissing"));
    assert!(listing.contains("tahoe-time-of-day\tHEIC\t6016x3384\t36.3\tmissing"));
    assert!(listing.contains("big-sur-1\tHEIC\t5120x2880\t89.6\tmissing"));

    for id in ["sequoia-night", "tahoe-time-of-day"] {
        let output = genkan(directory.path())
            .args(["assets", "preview", id])
            .output()
            .unwrap();
        assert!(!output.status.success());
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(error.contains("is not installed at"), "{error}");
        assert!(
            error.contains(directory.path().to_str().unwrap()),
            "{error}"
        );
    }
    for id in [
        "missing",
        "../tahoe-beach",
        "/tmp/movie.mov",
        "https://example.com/movie.mov",
    ] {
        let output = genkan(directory.path())
            .args(["assets", "preview", id])
            .output()
            .unwrap();
        assert!(!output.status.success());
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(error.contains("unknown wallpaper"), "{error}");
        assert!(error.contains("genkan assets list"), "{error}");
    }
}
