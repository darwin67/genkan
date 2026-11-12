{ pkgs, genkan }:

# Deliberately use the unmodified production wrapper with no test artwork.
# Fixture-backed screenshot checks cannot substitute for this case.
pkgs.runCommand "genkan-wallpaper-empty-cache"
  {
    nativeBuildInputs = [
      pkgs.coreutils
      pkgs.findutils
      pkgs.gnugrep
      pkgs.imagemagick
      pkgs.strace
      pkgs.util-linux
      pkgs.weston
    ];
  }
  ''
    set -euo pipefail
    export HOME="$TMPDIR/home"
    export XDG_RUNTIME_DIR="$TMPDIR/runtime"
    export WAYLAND_DISPLAY=wayland-empty-cache
    export ICED_BACKEND=wgpu
    export WGPU_BACKEND=vulkan
    export LIBGL_ALWAYS_SOFTWARE=1
    export VK_DRIVER_FILES=${pkgs.mesa}/share/vulkan/icd.d/lvp_icd.${pkgs.stdenv.hostPlatform.parsed.cpu.name}.json
    unset GENKAN_WALLPAPER_DIR XDG_CACHE_HOME
    mkdir -p "$HOME" "$XDG_RUNTIME_DIR" "$TMPDIR/xdg-cache" "$out"
    chmod 700 "$XDG_RUNTIME_DIR"
    source ${../../scripts/preview-evidence-lib.sh}

    app_pid=""
    weston_pid=""
    cleanup() {
      if [[ -n "$app_pid" ]]; then
        kill -TERM -- "-$app_pid" 2>/dev/null || true
        wait "$app_pid" 2>/dev/null || true
      fi
      if [[ -n "$weston_pid" ]]; then
        kill "$weston_pid" 2>/dev/null || true
        wait "$weston_pid" 2>/dev/null || true
      fi
    }
    trap cleanup EXIT
    weston --backend=headless-backend.so --renderer=pixman \
      --width=640 --height=480 --idle-time=0 --shell=kiosk \
      --socket="$WAYLAND_DISPLAY" --debug --log="$out/weston.log" &
    weston_pid=$!
    for _ in $(seq 1 100); do
      [[ -S "$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY" ]] && break
      kill -0 "$weston_pid"
      sleep 0.05
    done
    test -S "$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY"

    for mode in home xdg override; do
      unset GENKAN_WALLPAPER_DIR XDG_CACHE_HOME
      case "$mode" in
        home) arguments=(login --windowed --preview selected --reduce-motion) ;;
        xdg)
          export XDG_CACHE_HOME="$TMPDIR/xdg-cache"
          arguments=(login --windowed --preview selected --animated-preview)
          ;;
        override)
          export GENKAN_WALLPAPER_DIR="$TMPDIR/empty-wallpapers"
          mkdir -p "$GENKAN_WALLPAPER_DIR"
          arguments=(lock --preview prompt)
          ;;
      esac
      # Capture both network and asset access. A valid-looking UI must not
      # pass by secretly opening source-tree posters or running a downloader.
      setsid strace -f -e trace=connect,openat,openat2,execve \
        -o "$out/$mode.trace" ${genkan}/bin/genkan "''${arguments[@]}" \
        --width 640 --height 480 >"$out/$mode.log" 2>&1 &
      app_pid=$!
      sleep 2
      mkdir -p "$TMPDIR/capture-$mode"
      screenshot=""
      for _ in $(seq 1 50); do
        require_running_process "$app_pid" "$out/$mode.log"
        rm -f "$TMPDIR/capture-$mode"/wayland-screenshot-*.png
        (cd "$TMPDIR/capture-$mode"; timeout 5s weston-screenshooter)
        candidate=$(find "$TMPDIR/capture-$mode" -name 'wayland-screenshot-*.png' -print -quit)
        if [[ -n "$candidate" ]] && valid_preview_frame "$candidate" 640 480; then
          screenshot=$candidate
          break
        fi
        sleep 0.1
      done
      test -n "$screenshot"
      cp "$screenshot" "$out/$mode.png"
      kill -TERM -- "-$app_pid" 2>/dev/null || true
      wait "$app_pid" 2>/dev/null || true
      app_pid=""
      check_preview_connections "$out/$mode.trace" "$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY"
      if grep -E 'openat(2)?\(.*\.(jpg|jpeg|heic|mov)".*= [0-9]+' "$out/$mode.trace"; then
        echo "empty-cache production opened wallpaper artwork" >&2
        exit 1
      fi
      if grep -E 'execve\("[^"]*/curl"' "$out/$mode.trace"; then
        echo "empty-cache production attempted an automatic download" >&2
        exit 1
      fi
    done
    # Fontconfig may populate its own cache; artwork must never be created.
    test -z "$(find "$HOME" "$TMPDIR/xdg-cache" "$TMPDIR/empty-wallpapers" \
      -type f \( -iname '*.mov' -o -iname '*.heic' -o -iname '*.jpg' \) -print)"
  ''
