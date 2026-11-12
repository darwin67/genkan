{
  pkgs,
  genkan,
  lockTestGenkan,
  testWallpaperDirectory,
}:
pkgs.runCommand "genkan-niri-smoke"
  {
    nativeBuildInputs = [
      pkgs.python3
      pkgs.grim
    ];
    GENKAN_BIN = "${genkan}/bin/genkan";
    GENKAN_LOCK_TEST_BIN = "${lockTestGenkan}/bin/genkan";
    GENKAN_WALLPAPER_DIR = testWallpaperDirectory;
    NIRI_BIN = "${pkgs.niri}/bin/niri";
    SWAY_BIN = "${pkgs.sway-unwrapped}/bin/sway";
    HEIC_FIXTURE = ../../tests/fixtures/dynamic-heic/synthetic-all-properties.heic;
    LIBGL_DRIVERS_PATH = "${pkgs.mesa.drivers}/lib/dri";
    __EGL_VENDOR_LIBRARY_FILENAMES = "${pkgs.mesa.drivers}/share/glvnd/egl_vendor.d/50_mesa.json";
  }
  ''
    mkdir -p "$out"
    python ${../../scripts/niri-smoke.py}
  ''
