{
  pkgs,
  genkan,
  wallpaperManifest,
}:
let
  closure = pkgs.closureInfo { rootPaths = [ genkan ]; };
in
pkgs.runCommand "genkan-wallpaper-package-check"
  {
    nativeBuildInputs = [
      pkgs.coreutils
      pkgs.findutils
      pkgs.gnugrep
    ];
  }
  ''
    set -eu
    directory=${genkan}/share/genkan/wallpapers
    test "$(find "$directory" -mindepth 1 -printf '%P\n')" = manifest.toml
    test -s "$directory/manifest.toml"
    if grep -F GENKAN_WALLPAPER_DIR ${genkan}/bin/genkan; then
      echo "production wrapper must not override the user's wallpaper cache" >&2
      exit 1
    fi

    # Inspect the real production runtime closure, not a fixture-wrapped
    # package. Fixed-output remote downloads previously appeared as store
    # paths named after install_name. Neither those nor test-cache roots may
    # be retained by the binary or wrapper.
    ${pkgs.lib.concatMapStringsSep "\n" (asset: ''
      if grep -F -- '-${asset.install_name}' ${closure}/store-paths; then
        echo 'production closure contains catalog artwork: ${asset.install_name}' >&2
        exit 1
      fi
    '') (wallpaperManifest.wallpaper ++ wallpaperManifest.dynamic_heic)}
    if grep -E -- '-genkan-(test-)?wallpapers$' ${closure}/store-paths; then
      echo "production closure contains a prepopulated wallpaper cache" >&2
      exit 1
    fi
    cp ${closure}/store-paths "$out"
  ''
