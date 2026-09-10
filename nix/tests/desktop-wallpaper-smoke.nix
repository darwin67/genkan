{
  pkgs,
  genkan,
  lockTestGenkan,
}:

pkgs.runCommand "genkan-desktop-wallpaper-smoke"
  {
    nativeBuildInputs = with pkgs; [
      coreutils
      gcc
      grim
      imagemagick
      libheif
      pkg-config
      sway
      weston
      x265
    ];
  }
  ''
    set -euo pipefail
    runtime=$(mktemp -d)
    config=$(mktemp)
    sway_log=$(mktemp)
    wallpaper_log=$(mktemp)
    lock_log=$(mktemp)
    capture_one=$(mktemp --suffix=.png)
    capture_two=$(mktemp --suffix=.png)
    restored_one=$(mktemp --suffix=.png)
    restored_two=$(mktemp --suffix=.png)
    lock_capture_one=$(mktemp --suffix=.png)
    lock_capture_two=$(mktemp --suffix=.png)
    ready=$(mktemp)
    unsupported_runtime=$(mktemp -d)
    unsupported_log=$(mktemp)
    unsupported_client_log=$(mktemp)
    pattern_heic=$(mktemp --suffix=.heic)
    pattern_expected_one=$(mktemp --suffix=.png)
    pattern_expected_two=$(mktemp --suffix=.png)
    wallpaper_pid=
    lock_pid=
    weston_pid=
    cleanup() {
      [[ -z "$lock_pid" ]] || kill "$lock_pid" 2>/dev/null || true
      [[ -z "$wallpaper_pid" ]] || kill "$wallpaper_pid" 2>/dev/null || true
      [[ -z "$weston_pid" ]] || kill "$weston_pid" 2>/dev/null || true
      [[ -z "''${sway_pid:-}" ]] || kill "$sway_pid" 2>/dev/null || true
      rm -rf "$runtime" "$config" "$sway_log" "$wallpaper_log" \
        "$lock_log" "$capture_one" "$capture_two" "$restored_one" "$restored_two" \
        "$lock_capture_one" "$lock_capture_two" "$ready" "$unsupported_runtime" \
        "$unsupported_log" "$unsupported_client_log" "$pattern_heic" \
        "$pattern_expected_one" "$pattern_expected_two"
    }
    report_failure() {
      status=$?
      echo "desktop wallpaper smoke failed at line $1" >&2
      cat "$unsupported_client_log" "$unsupported_log" "$wallpaper_log" \
        "$lock_log" "$sway_log" >&2
      exit "$status"
    }
    trap 'report_failure $LINENO' ERR
    trap cleanup EXIT
    chmod 700 "$runtime"
    chmod 700 "$unsupported_runtime"
    cc $(pkg-config --cflags libheif) \
      ${../../tests/fixtures/dynamic-heic/generate-geometry.c} \
      $(pkg-config --libs libheif) -o generate-geometry
    ./generate-geometry "$pattern_heic"

    XDG_RUNTIME_DIR="$unsupported_runtime" weston --backend=headless-backend.so \
      --socket=wayland-genkan-test --idle-time=0 --log="$unsupported_log" &
    weston_pid=$!
    for _ in $(seq 1 200); do
      kill -0 "$weston_pid"
      [[ -S "$unsupported_runtime/wayland-genkan-test" ]] && break
      sleep 0.01
    done
    [[ -S "$unsupported_runtime/wayland-genkan-test" ]]
    unsupported_status=0
    XDG_RUNTIME_DIR="$unsupported_runtime" WAYLAND_DISPLAY=wayland-genkan-test \
      WAYLAND_DEBUG=client timeout 5s ${genkan}/bin/genkan wallpaper \
        --file ${../../tests/fixtures/dynamic-heic/synthetic-all-properties.heic} \
        >"$unsupported_client_log" 2>&1 || unsupported_status=$?
    [[ "$unsupported_status" -eq 1 ]]
    grep -Fq 'zwlr_layer_shell_v1' "$unsupported_client_log"
    if grep -Eq 'xdg_(surface|toplevel)' "$unsupported_client_log"; then
      echo "unsupported compositor path created an ordinary window" >&2
      exit 1
    fi
    kill "$weston_pid"
    wait "$weston_pid" || true
    weston_pid=

    printf '%s\n' \
      'output * mode 640x360' \
      'output HEADLESS-2 mode 800x360 pos 640 0' \
      'seat * hide_cursor 1000' > "$config"
    XDG_RUNTIME_DIR="$runtime" \
      DBUS_SESSION_BUS_ADDRESS="unix:path=$runtime/no-session-bus" \
      WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS=2 \
      WLR_LIBINPUT_NO_DEVICES=1 sway -c "$config" -d >"$sway_log" 2>&1 &
    sway_pid=$!
    for _ in $(seq 1 400); do
      kill -0 "$sway_pid"
      socket=$(find "$runtime" -maxdepth 1 -type s -name 'wayland-*' -print -quit)
      ipc=$(find "$runtime" -maxdepth 1 -type s -name 'sway-ipc.*.sock' -print -quit)
      [[ -n "''${socket:-}" && -n "''${ipc:-}" ]] && break
      sleep 0.05
    done
    [[ -n "''${socket:-}" && -n "''${ipc:-}" ]]
    export XDG_RUNTIME_DIR="$runtime"
    export WAYLAND_DISPLAY=$(basename "$socket")
    WAYLAND_DEBUG=client ${genkan}/bin/genkan wallpaper \
      --file ${../../tests/fixtures/dynamic-heic/synthetic-all-properties.heic} \
      --reduce-motion >"$wallpaper_log" 2>&1 &
    wallpaper_pid=$!

    capture_wallpaper() {
      output=$1
      destination=$2
      expected_size=$3
      for _ in $(seq 1 100); do
        kill -0 "$wallpaper_pid"
        rm -f "$destination"
        if ! grim -o "$output" "$destination" 2>/dev/null; then
          sleep 0.05
          continue
        fi
        pixel=$(
          magick "$destination" \
            -format '%[hex:p{0,0}]' \
            info: 2>/dev/null
        )
        size=$(identify -format '%wx%h' "$destination")
        colors=$(identify -format '%k' "$destination")
        if [[ "''${pixel:-}" =~ ^(F[A-F]0[0-5]0[0-5]|0[0-5]F[A-F]0[0-5]|0[0-5]0[0-5]F[A-F]|F[A-F]F[A-F]F[A-F])$ \
          && "$size" == "$expected_size" && "$colors" -eq 1 ]]; then
          return
        fi
        sleep 0.05
      done
      echo "dynamic wallpaper frame was not presented on $output" >&2
      return 1
    }
    capture_wallpaper HEADLESS-1 "$capture_one" 640x360
    capture_wallpaper HEADLESS-2 "$capture_two" 800x360
    grep -Eq 'zwlr_layer_surface_v1#[0-9]+\.set_anchor\(15\)' "$wallpaper_log"
    grep -Eq 'zwlr_layer_surface_v1#[0-9]+\.set_exclusive_zone\(-1\)' "$wallpaper_log"
    grep -Eq 'zwlr_layer_surface_v1#[0-9]+\.set_keyboard_interactivity\(0\)' "$wallpaper_log"
    grep -Eq 'wl_surface#[0-9]+\.set_input_region\(wl_region#[0-9]+\)' "$wallpaper_log"
    mapfile -t wallpaper_surfaces < <(
      grep -Eo 'wl_surface#[0-9]+\.attach' "$wallpaper_log" \
        | sed 's/\.attach$//' | sort -u
    )
    [[ ''${#wallpaper_surfaces[@]} -eq 2 ]]
    for surface in "''${wallpaper_surfaces[@]}"; do
      first_commit=$(grep -nFm1 "$surface.commit()" "$wallpaper_log" | cut -d: -f1)
      first_attach=$(grep -nFm1 "$surface.attach(" "$wallpaper_log" | cut -d: -f1)
      [[ -n "$first_commit" && -n "$first_attach" && "$first_commit" -lt "$first_attach" ]]
    done

    surface_for_size() {
      width=$1
      height=$2
      stride=$((width * 4))
      buffer=$(
        grep -E "\.create_buffer\(new id wl_buffer#[0-9]+, 0, $width, $height, $stride, 0\)" \
          "$wallpaper_log" | grep -Eo 'wl_buffer#[0-9]+' | head -1
      )
      grep -E "wl_surface#[0-9]+\.attach\($buffer," "$wallpaper_log" \
        | grep -Eo 'wl_surface#[0-9]+' | head -1
    }
    wallpaper_surface_one=$(surface_for_size 640 360)
    wallpaper_surface_two=$(surface_for_size 800 360)
    [[ -n "$wallpaper_surface_one" && -n "$wallpaper_surface_two" \
      && "$wallpaper_surface_one" != "$wallpaper_surface_two" ]]

    transaction_matches() {
      expected_surface=$1
      buffer=$2
      scale=$3
      awk -v surface="$expected_surface" -v buffer="$buffer" -v scale="$scale" '
        index($0, surface ".set_buffer_scale(") {
          value = $0
          sub("^.*" surface "\\.set_buffer_scale\\(", "", value)
          sub("\\).*$", "", value)
          current_scale = value + 0
        }
        index($0, surface ".attach(") {
          attached = index($0, surface ".attach(" buffer ",") > 0
        }
        index($0, surface ".commit()") && attached && current_scale == scale {
          found = 1
          exit
        }
        END { exit !found }
      '
    }
    if printf '%s\n' \
      'wl_surface#7.set_buffer_scale(2)' 'wl_surface#7.commit()' \
      'wl_surface#7.attach(wl_buffer#9, 0, 0)' \
      'wl_surface#7.set_buffer_scale(1)' 'wl_surface#7.commit()' \
      | transaction_matches wl_surface#7 wl_buffer#9 2; then
      echo "geometry transaction parser accepted a superseded scale" >&2
      exit 1
    fi
    printf '%s\n' 'wl_surface#7.set_buffer_scale(2)' \
      'wl_surface#7.attach(wl_buffer#9, 0, 0)' 'wl_surface#7.commit()' \
      | transaction_matches wl_surface#7 wl_buffer#9 2

    await_new_surface() {
      first_line=$1
      width=$2
      height=$3
      stride=$((width * 4))
      for _ in $(seq 1 100); do
        segment=$(tail -n +"$first_line" "$wallpaper_log")
        buffer=$(
          printf '%s\n' "$segment" \
            | grep -E "\.create_buffer\(new id wl_buffer#[0-9]+, 0, $width, $height, $stride, 0\)" \
            | grep -Eo 'wl_buffer#[0-9]+' | tail -1 || true
        )
        surface=$(
          [[ -z "$buffer" ]] || printf '%s\n' "$segment" \
            | grep -E "wl_surface#[0-9]+\.attach\($buffer," \
            | grep -Eo 'wl_surface#[0-9]+' | tail -1 || true
        )
        if [[ -n "$surface" ]]; then
          printf '%s\n' "$surface"
          return
        fi
        kill -0 "$wallpaper_pid"
        sleep 0.02
      done
      return 1
    }

    await_geometry_commit() {
      first_line=$1
      expected_surface=$2
      scale=$3
      width=$4
      height=$5
      stride=$((width * 4))
      for _ in $(seq 1 100); do
        segment=$(tail -n +"$first_line" "$wallpaper_log")
        buffer=$(
          printf '%s\n' "$segment" \
            | grep -E "\.create_buffer\(new id wl_buffer#[0-9]+, 0, $width, $height, $stride, 0\)" \
            | grep -Eo 'wl_buffer#[0-9]+' | tail -1 || true
        )
        if [[ -n "$buffer" ]] \
          && printf '%s\n' "$segment" \
            | transaction_matches "$expected_surface" "$buffer" "$scale"; then
          return
        fi
        kill -0 "$wallpaper_pid"
        sleep 0.02
      done
      echo "expected $width x $height scale-$scale wallpaper commit was not observed" >&2
      return 1
    }

    # Exercise live scale/transform changes without recreating the surface.
    for scale in 2 1 2 1; do
      first_line=$(( $(wc -l < "$wallpaper_log") + 1 ))
      swaymsg -s "$ipc" output HEADLESS-1 scale "$scale" transform 90 >/dev/null
      await_geometry_commit "$first_line" "$wallpaper_surface_one" "$scale" 360 640
      capture_wallpaper HEADLESS-1 "$restored_one" 360x640
      kill -0 "$wallpaper_pid"
    done
    first_line=$(( $(wc -l < "$wallpaper_log") + 1 ))
    swaymsg -s "$ipc" output HEADLESS-1 scale 1 transform normal >/dev/null
    await_geometry_commit "$first_line" "$wallpaper_surface_one" 1 640 360
    capture_wallpaper HEADLESS-1 "$restored_one" 640x360

    # A real mode resize must produce a fresh full-output presentation.
    first_line=$(( $(wc -l < "$wallpaper_log") + 1 ))
    swaymsg -s "$ipc" output HEADLESS-1 mode 800x450 >/dev/null
    await_geometry_commit "$first_line" "$wallpaper_surface_one" 1 800 450
    capture_wallpaper HEADLESS-1 "$restored_one" 800x450

    swaymsg -s "$ipc" output HEADLESS-2 disable >/dev/null
    sleep 0.1
    kill -0 "$wallpaper_pid"
    first_line=$(( $(wc -l < "$wallpaper_log") + 1 ))
    swaymsg -s "$ipc" output HEADLESS-2 enable scale 2 transform 90 >/dev/null
    wallpaper_surface_two=$(await_new_surface "$first_line" 360 800)
    await_geometry_commit "$first_line" "$wallpaper_surface_two" 2 360 800
    capture_wallpaper HEADLESS-2 "$restored_two" 360x800
    grep -Eq 'wl_buffer#[0-9]+\.release\(\)' "$wallpaper_log"

    # Compare a nonuniform fixture against an independently scaled cover
    # reference on both landscape and transformed portrait outputs.
    kill "$wallpaper_pid"
    wait "$wallpaper_pid" || true
    magick "$pattern_heic[0]" -filter point -resize '800x450^' \
      -gravity center -extent 800x450 "$pattern_expected_one"
    magick "$pattern_heic[0]" -filter point -resize '360x800^' \
      -gravity center -extent 360x800 "$pattern_expected_two"
    ${genkan}/bin/genkan wallpaper --file "$pattern_heic" --reduce-motion \
      >>"$wallpaper_log" 2>&1 &
    wallpaper_pid=$!

    capture_pattern() {
      output=$1
      destination=$2
      expected=$3
      maximum_difference=$4
      for _ in $(seq 1 100); do
        kill -0 "$wallpaper_pid"
        rm -f "$destination"
        if grim -o "$output" "$destination" 2>/dev/null; then
          comparison=$(
            magick compare -metric AE "$expected" "$destination" null: \
              2>&1 >/dev/null || true
          )
          difference=''${comparison%% *}
          if [[ "$difference" =~ ^([0-9]+)(\.[0-9]+)?$ ]]; then
            difference_whole=''${BASH_REMATCH[1]}
            if [[ "$difference_whole" -le "$maximum_difference" ]]; then
              return
            fi
          fi
        fi
        sleep 0.05
      done
      echo "nonuniform cover rendering did not match on $output (last AE: ''${comparison:-unavailable})" >&2
      echo "expected 12x8 diagnostic:" >&2
      magick "$expected" -filter point -resize 12x8\! txt:- >&2
      echo "captured 12x8 diagnostic:" >&2
      magick "$destination" -filter point -resize 12x8\! txt:- >&2
      return 1
    }
    capture_pattern HEADLESS-1 "$restored_one" "$pattern_expected_one" 7200
    capture_pattern HEADLESS-2 "$restored_two" "$pattern_expected_two" 5760

    ${lockTestGenkan}/bin/genkan lock --test-unlock-after-ready \
      --test-unlock-delay-ms 5000 --ready-fd 3 \
      3>"$ready" >"$lock_log" 2>&1 &
    lock_pid=$!
    for _ in $(seq 1 500); do
      kill -0 "$wallpaper_pid"
      grep -Fxq READY "$ready" && break
      kill -0 "$lock_pid"
      sleep 0.01
    done
    grep -Fxq READY "$ready"
    for _ in $(seq 1 500); do
      [[ $(grep -Fc 'committed first opaque buffer for output' "$lock_log") -ge 2 ]] && break
      kill -0 "$lock_pid"
      sleep 0.01
    done
    [[ $(grep -Fc 'committed first opaque buffer for output' "$lock_log") -ge 2 ]]
    if grep -Fq 'authentication accepted; unlocking' "$lock_log"; then
      echo "test lock unlocked before occlusion capture" >&2
      exit 1
    fi

    capture_difference() {
      output=$1
      before=$2
      locked=$3
      rm -f "$locked"
      grim -o "$output" "$locked"
      [[ $(identify -format '%wx%h' "$locked") == $(identify -format '%wx%h' "$before") ]]
      difference=$(magick "$before" "$locked" -compose difference -composite \
        -threshold 0 -format '%[fx:mean*w*h]' info:)
      [[ "$difference" =~ ^[0-9]+([.][0-9]+)?$ ]]
      pixels=$(identify -format '%[fx:w*h]' "$before")
      [[ "$pixels" =~ ^[0-9]+$ ]]
      awk -v difference="$difference" -v pixels="$pixels" \
        'BEGIN { exit !(difference >= pixels - 16) }'
    }
    capture_difference HEADLESS-1 "$restored_one" "$lock_capture_one"
    capture_difference HEADLESS-2 "$restored_two" "$lock_capture_two"
    kill -0 "$lock_pid"
    if grep -Fq 'authentication accepted; unlocking' "$lock_log"; then
      echo "test lock unlocked before both occlusion captures" >&2
      exit 1
    fi
    wait "$lock_pid"
    lock_pid=
    capture_pattern HEADLESS-1 "$restored_one" "$pattern_expected_one" 7200
    capture_pattern HEADLESS-2 "$restored_two" "$pattern_expected_two" 5760
    kill -0 "$wallpaper_pid"
    mkdir "$out"
    cp "$restored_one" "$out/landscape.png"
    cp "$restored_two" "$out/portrait.png"
  ''
