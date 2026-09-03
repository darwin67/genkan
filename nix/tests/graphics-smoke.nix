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
      pkgs.imagemagick
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
    stop_smoke() {
      if [ -z "$smoke_pid" ]; then
        return
      fi

      kill -TERM -- "-$smoke_pid" 2>/dev/null || true
      for _ in $(seq 1 20); do
        if ! kill -0 -- "-$smoke_pid" 2>/dev/null; then
          break
        fi
        sleep 0.1
      done
      kill -KILL -- "-$smoke_pid" 2>/dev/null || true
      wait "$smoke_pid" 2>/dev/null || true
      smoke_pid=""
    }
    stop_weston() {
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
        weston_pid=""
      fi
      rm -f "$XDG_RUNTIME_DIR/wayland-genkan"
    }
    start_weston() {
      local log=$1

      weston \
        --backend=headless-backend.so \
        --renderer=pixman \
        --width=1280 \
        --height=720 \
        --idle-time=0 \
        --shell=kiosk \
        --socket=wayland-genkan \
        --debug \
        --log="$log" &
      weston_pid=$!

      for _ in $(seq 1 100); do
        if [ -S "$XDG_RUNTIME_DIR/wayland-genkan" ]; then
          return
        fi
        if ! kill -0 "$weston_pid" 2>/dev/null; then
          cat "$log"
          exit 1
        fi
        sleep 0.1
      done
      echo "Weston did not create its Wayland socket" >&2
      exit 1
    }
    cleanup() {
      stop_smoke
      stop_weston
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
    exec ${genkan}/bin/genkan login --username smoke --reduce-motion
    EOF
    chmod +x "$TMPDIR/run-genkan"

    start_weston "$TMPDIR/weston-cage.log"

    WAYLAND_DISPLAY=wayland-genkan \
      ICED_BACKEND=wgpu \
      WLR_RENDERER=pixman \
      WGPU_BACKEND=vulkan \
      VK_DRIVER_FILES=${pkgs.mesa}/share/vulkan/icd.d/lvp_icd.${pkgs.stdenv.hostPlatform.parsed.cpu.name}.json \
      LIBGL_ALWAYS_SOFTWARE=1 \
      setsid cage -- "$TMPDIR/run-genkan" \
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

    stop_smoke
    stop_weston
    start_weston "$TMPDIR/weston-preview.log"

    cat > "$TMPDIR/run-genkan-static" <<EOF
    #!${pkgs.runtimeShell}
    echo \$\$ > "$TMPDIR/genkan.pid"
    exec ${genkan}/bin/genkan login --windowed --preview selected --width 1280 --height 720
    EOF
    chmod +x "$TMPDIR/run-genkan-static"

    rm -f "$TMPDIR/genkan.pid"
    WAYLAND_DISPLAY=wayland-genkan \
      ICED_BACKEND=wgpu \
      WGPU_BACKEND=vulkan \
      VK_DRIVER_FILES=${pkgs.mesa}/share/vulkan/icd.d/lvp_icd.${pkgs.stdenv.hostPlatform.parsed.cpu.name}.json \
      LIBGL_ALWAYS_SOFTWARE=1 \
      setsid "$TMPDIR/run-genkan-static" \
      > "$TMPDIR/static.log" 2>&1 &
    smoke_pid=$!

    for _ in $(seq 1 100); do
      [ -s "$TMPDIR/genkan.pid" ] && break
      kill -0 "$smoke_pid" 2>/dev/null
      sleep 0.1
    done
    test -s "$TMPDIR/genkan.pid"
    genkan_pid=$(cat "$TMPDIR/genkan.pid")

    mkdir "$TMPDIR/captures"
    previous_frame=""
    poster_frame=""
    for _ in $(seq 1 30); do
      rm -f "$TMPDIR/captures"/wayland-screenshot-*.png
      (
        cd "$TMPDIR/captures"
        WAYLAND_DISPLAY=wayland-genkan timeout 5s weston-screenshooter
      )
      screenshot=$(find "$TMPDIR/captures" -name 'wayland-screenshot-*.png' -print -quit)
      test -n "$screenshot"
      if ! kill -0 "$genkan_pid" 2>/dev/null; then
        cat "$TMPDIR/static.log"
        echo "Genkan exited during static poster capture" >&2
        exit 1
      fi
      colors=$(identify -format '%k' "$screenshot")
      if [ "$colors" -ge 16 ]; then
        frame=$(magick "$screenshot" rgba:- | sha256sum | cut -d' ' -f1)
        if [ -n "$previous_frame" ] && [ "$frame" = "$previous_frame" ]; then
          poster_frame=$frame
          break
        fi
        previous_frame=$frame
      fi
      sleep 0.1
    done
    if [ -z "$poster_frame" ]; then
      cat "$TMPDIR/static.log"
      echo "Static preview never produced a stable non-blank poster frame" >&2
      exit 1
    fi
    stop_smoke

    cat > "$TMPDIR/run-genkan-animated" <<EOF
    #!${pkgs.runtimeShell}
    echo \$\$ > "$TMPDIR/genkan.pid"
    exec ${genkan}/bin/genkan login --windowed --preview selected --animated-preview --width 1280 --height 720
    EOF
    chmod +x "$TMPDIR/run-genkan-animated"

    rm -f "$TMPDIR/genkan.pid"
    WAYLAND_DISPLAY=wayland-genkan \
      ICED_BACKEND=wgpu \
      WGPU_BACKEND=vulkan \
      VK_DRIVER_FILES=${pkgs.mesa}/share/vulkan/icd.d/lvp_icd.${pkgs.stdenv.hostPlatform.parsed.cpu.name}.json \
      LIBGL_ALWAYS_SOFTWARE=1 \
      setsid "$TMPDIR/run-genkan-animated" \
      > "$TMPDIR/animated.log" 2>&1 &
    smoke_pid=$!

    for _ in $(seq 1 100); do
      if [ -s "$TMPDIR/genkan.pid" ]; then
        break
      fi
      if ! kill -0 "$smoke_pid" 2>/dev/null; then
        cat "$TMPDIR/animated.log"
        echo "Genkan exited before the animated graphics smoke started" >&2
        exit 1
      fi
      sleep 0.1
    done
    test -s "$TMPDIR/genkan.pid"
    genkan_pid=$(cat "$TMPDIR/genkan.pid")

    distinct_frames=""
    frame_count=0
    for _ in $(seq 1 30); do
      if ! kill -0 "$genkan_pid" 2>/dev/null; then
        cat "$TMPDIR/animated.log"
        echo "Genkan exited during animated frame capture" >&2
        exit 1
      fi
      rm -f "$TMPDIR/captures"/wayland-screenshot-*.png
      (
        cd "$TMPDIR/captures"
        WAYLAND_DISPLAY=wayland-genkan timeout 5s weston-screenshooter
      )
      screenshot=$(find "$TMPDIR/captures" -name 'wayland-screenshot-*.png' -print -quit)
      test -n "$screenshot"
      if ! kill -0 "$genkan_pid" 2>/dev/null; then
        cat "$TMPDIR/animated.log"
        echo "Genkan exited during animated frame capture" >&2
        exit 1
      fi
      colors=$(identify -format '%k' "$screenshot")
      if [ "$colors" -ge 16 ]; then
        frame=$(magick "$screenshot" rgba:- | sha256sum | cut -d' ' -f1)
        if [ "$frame" != "$poster_frame" ]; then
          case " $distinct_frames " in
            *" $frame "*) ;;
            *)
              distinct_frames="$distinct_frames $frame"
              frame_count=$((frame_count + 1))
              ;;
          esac
        fi
      fi
      if [ "$frame_count" -ge 2 ]; then
        break
      fi
      sleep 0.5
    done
    if [ "$frame_count" -lt 2 ]; then
      cat "$TMPDIR/animated.log"
      echo "Animated wallpaper produced fewer than two distinct frames beyond its poster" >&2
      exit 1
    fi

    initial_rss=$(awk '/^VmRSS:/ { print $2 }' /proc/"$genkan_pid"/status)
    maximum_rss=$initial_rss
    for _ in $(seq 1 10); do
      sleep 0.5
      current_rss=$(awk '/^VmRSS:/ { print $2 }' /proc/"$genkan_pid"/status)
      if [ "$current_rss" -gt "$maximum_rss" ]; then
        maximum_rss=$current_rss
      fi
    done
    if [ $((maximum_rss - initial_rss)) -gt 262144 ]; then
      echo "Wallpaper rendering grew by more than 256 MiB after frame delivery" >&2
      exit 1
    fi

    stop_smoke

    cat "$TMPDIR/weston-cage.log"
    cat "$TMPDIR/weston-preview.log"
    cat "$TMPDIR/cage.log"
    cat "$TMPDIR/static.log"
    cat "$TMPDIR/animated.log"

    touch "$out"
  ''
