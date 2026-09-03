{ pkgs, genkan }:

pkgs.runCommand "genkan-graphics-smoke"
  {
    FONTCONFIG_FILE = pkgs.makeFontsConf {
      fontDirectories = [ pkgs.dejavu_fonts ];
    };
    nativeBuildInputs = [
      pkgs.cage
      pkgs.coreutils
      pkgs.gnugrep
      pkgs.patchelf
      pkgs.util-linux
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
    smoke_pid=""
    cleanup() {
      if [ -n "$smoke_pid" ]; then
        kill -TERM -- "-$smoke_pid" 2>/dev/null || true
        wait "$smoke_pid" 2>/dev/null || true
      fi
      if [ -n "$weston_pid" ]; then
        kill -TERM "$weston_pid" 2>/dev/null || true
        for _ in $(seq 1 20); do
          if ! kill -0 "$weston_pid" 2>/dev/null; then
            break
          fi
          sleep 0.1
        done
        if kill -0 "$weston_pid" 2>/dev/null; then
          kill -KILL "$weston_pid" 2>/dev/null || true
        fi
        wait "$weston_pid" 2>/dev/null || true
      fi
    }
    trap cleanup EXIT

    patchelf --print-rpath ${genkan}/bin/.genkan-wrapped \
      | tr : '\n' \
      | grep -Fx ${pkgs.addDriverRunpath.driverLink}/lib
    grep -F 'VK_ADD_DRIVER_FILES' ${genkan}/bin/genkan
    grep -F ${pkgs.addDriverRunpath.driverLink}/share/vulkan/icd.d ${genkan}/bin/genkan

    cat > "$TMPDIR/run-genkan" <<EOF
    #!${pkgs.runtimeShell}
    echo \$\$ > "$TMPDIR/genkan.pid"
    exec ${genkan}/bin/genkan login --username smoke
    EOF
    chmod +x "$TMPDIR/run-genkan"

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

    WAYLAND_DISPLAY=wayland-genkan \
      ICED_BACKEND=wgpu \
      WLR_RENDERER=pixman \
      WGPU_BACKEND=vulkan \
      VK_DRIVER_FILES=${pkgs.mesa}/share/vulkan/icd.d/lvp_icd.${pkgs.stdenv.hostPlatform.parsed.cpu.name}.json \
      LIBGL_ALWAYS_SOFTWARE=1 \
      setsid timeout --kill-after=2s --signal=TERM 8s \
      cage -- "$TMPDIR/run-genkan" \
      > "$TMPDIR/cage.log" 2>&1 &
    smoke_pid=$!

    for _ in $(seq 1 100); do
      if [ -s "$TMPDIR/genkan.pid" ]; then
        break
      fi
      if ! kill -0 "$smoke_pid" 2>/dev/null; then
        cat "$TMPDIR/cage.log"
        echo "Cage exited before starting Genkan" >&2
        exit 1
      fi
      sleep 0.1
    done
    test -s "$TMPDIR/genkan.pid"
    genkan_pid=$(cat "$TMPDIR/genkan.pid")

    for _ in $(seq 1 20); do
      if ! kill -0 "$genkan_pid" 2>/dev/null; then
        cat "$TMPDIR/cage.log"
        echo "Genkan exited before the graphics smoke settled" >&2
        exit 1
      fi
      sleep 0.1
    done
    grep -F libvulkan_lvp /proc/"$genkan_pid"/maps

    set +e
    wait "$smoke_pid"
    status=$?
    set -e
    smoke_pid=""

    cat "$TMPDIR/weston.log"
    cat "$TMPDIR/cage.log"
    if [ "$status" -ne 124 ]; then
      echo "Genkan exited before the graphics smoke timeout (status $status)" >&2
      exit 1
    fi

    touch "$out"
  ''
