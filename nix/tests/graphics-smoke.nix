{ pkgs, genkan }:

pkgs.runCommand "genkan-graphics-smoke"
  {
    FONTCONFIG_FILE = pkgs.makeFontsConf {
      fontDirectories = [ pkgs.dejavu_fonts ];
    };
    nativeBuildInputs = [
      pkgs.cage
      pkgs.coreutils
      pkgs.weston
    ];
  }
  ''
    set -eu

    export HOME="$TMPDIR/home"
    export XDG_RUNTIME_DIR="$TMPDIR/runtime"
    export XDG_DATA_DIRS="$TMPDIR/data/share"
    mkdir -p "$HOME/.cache/fontconfig" "$XDG_RUNTIME_DIR" "$XDG_DATA_DIRS/wayland-sessions"
    chmod 700 "$XDG_RUNTIME_DIR"

    cat > "$XDG_DATA_DIRS/wayland-sessions/smoke.desktop" <<EOF
    [Desktop Entry]
    Type=Application
    Name=Graphics smoke test
    Exec=${pkgs.coreutils}/bin/true
    EOF

    weston_pid=""
    cleanup() {
      if [ -n "$weston_pid" ]; then
        kill "$weston_pid" 2>/dev/null || true
        wait "$weston_pid" 2>/dev/null || true
      fi
    }
    trap cleanup EXIT

    weston \
      --backend=headless-backend.so \
      --idle-time=0 \
      --socket=wayland-genkan \
      --log="$TMPDIR/weston.log" &
    weston_pid=$!

    for _ in $(seq 1 100); do
      if [ -S "$XDG_RUNTIME_DIR/wayland-genkan" ]; then
        break
      fi
      if ! kill -0 "$weston_pid" 2>/dev/null; then
        cat "$TMPDIR/weston.log"
        exit 1
      fi
      sleep 0.1
    done
    test -S "$XDG_RUNTIME_DIR/wayland-genkan"

    set +e
    WAYLAND_DISPLAY=wayland-genkan \
      WLR_RENDERER=pixman \
      WGPU_BACKEND=vulkan \
      VK_ICD_FILENAMES=${pkgs.mesa}/share/vulkan/icd.d/lvp_icd.${pkgs.stdenv.hostPlatform.parsed.cpu.name}.json \
      LIBGL_ALWAYS_SOFTWARE=1 \
      timeout --signal=TERM 8 \
      cage -- ${genkan}/bin/genkan --username smoke \
      > "$TMPDIR/cage.log" 2>&1
    status=$?
    set -e

    cat "$TMPDIR/weston.log"
    cat "$TMPDIR/cage.log"
    if [ "$status" -ne 124 ]; then
      echo "Genkan exited before the graphics smoke timeout (status $status)" >&2
      exit 1
    fi

    touch "$out"
  ''
