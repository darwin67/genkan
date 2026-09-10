{
  pkgs,
  genkan,
  lockTestGenkan,
}:

pkgs.runCommand "genkan-desktop-wallpaper-smoke"
  {
    nativeBuildInputs = with pkgs; [
      coreutils
      grim
      imagemagick
      sway
    ];
  }
  ''
    set -eEuo pipefail
    runtime=$(mktemp -d)
    config=$(mktemp)
    sway_log=$(mktemp)
    wallpaper_log=$(mktemp)
    lock_log=$(mktemp)
    capture_one=$(mktemp --suffix=.png)
    capture_two=$(mktemp --suffix=.png)
    restored=$(mktemp --suffix=.png)
    lock_capture=$(mktemp --suffix=.png)
    wallpaper_pid=
    lock_pid=
    cleanup() {
      [[ -z "$lock_pid" ]] || kill "$lock_pid" 2>/dev/null || true
      [[ -z "$wallpaper_pid" ]] || kill "$wallpaper_pid" 2>/dev/null || true
      [[ -z "''${sway_pid:-}" ]] || kill "$sway_pid" 2>/dev/null || true
      rm -rf "$runtime" "$config" "$sway_log" "$wallpaper_log" \
        "$lock_log" "$capture_one" "$capture_two" "$restored" "$lock_capture"
    }
    report_failure() {
      status=$?
      echo "desktop wallpaper smoke failed at line $1" >&2
      cat "$wallpaper_log" "$lock_log" "$sway_log" >&2
      exit "$status"
    }
    trap 'report_failure $LINENO' ERR
    trap cleanup EXIT
    chmod 700 "$runtime"
    printf '%s\n' \
      'output * mode 640x360' \
      'output HEADLESS-2 pos 640 0' \
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
      for _ in $(seq 1 100); do
        kill -0 "$wallpaper_pid"
        grim -o "$output" "$destination" 2>/dev/null || true
        pixel=$(
          magick "$destination" \
            -format '%[hex:p{0,0}]' \
            info: 2>/dev/null || true
        )
        if [[ "''${pixel:-}" =~ ^(F[A-F]0[0-5]0[0-5]|0[0-5]F[A-F]0[0-5]|0[0-5]0[0-5]F[A-F]|F[A-F]F[A-F]F[A-F])$ ]]; then
          return
        fi
        sleep 0.05
      done
      echo "dynamic wallpaper frame was not presented on $output" >&2
      return 1
    }
    capture_wallpaper HEADLESS-1 "$capture_one"
    capture_wallpaper HEADLESS-2 "$capture_two"
    [[ $(identify -format '%wx%h' "$capture_one") == 640x360 ]]
    [[ $(identify -format '%wx%h' "$capture_two") == 640x360 ]]
    [[ $(identify -format '%k' "$capture_one") -eq 1 ]]
    [[ $(identify -format '%k' "$capture_two") -eq 1 ]]
    grep -Eq 'zwlr_layer_surface_v1#[0-9]+\.set_anchor\(15\)' "$wallpaper_log"
    grep -Eq 'zwlr_layer_surface_v1#[0-9]+\.set_exclusive_zone\(-1\)' "$wallpaper_log"
    grep -Eq 'zwlr_layer_surface_v1#[0-9]+\.set_keyboard_interactivity\(0\)' "$wallpaper_log"
    grep -Eq 'wl_surface#[0-9]+\.set_input_region\(wl_region#[0-9]+\)' "$wallpaper_log"
    first_commit=$(grep -nEm1 'wl_surface#[0-9]+\.commit\(\)' "$wallpaper_log" | cut -d: -f1)
    first_attach=$(grep -nEm1 'wl_surface#[0-9]+\.attach\(' "$wallpaper_log" | cut -d: -f1)
    [[ "$first_commit" -lt "$first_attach" ]]

    swaymsg -s "$ipc" output HEADLESS-2 disable >/dev/null
    sleep 0.1
    kill -0 "$wallpaper_pid"
    swaymsg -s "$ipc" output HEADLESS-2 enable scale 2 transform 90 >/dev/null
    capture_wallpaper HEADLESS-2 "$restored"
    [[ $(identify -format '%k' "$restored") -eq 1 ]]
    grep -Eq 'wl_buffer#[0-9]+\.release\(\)' "$wallpaper_log"

    ${lockTestGenkan}/bin/genkan lock --test-unlock-after-ready \
      --test-unlock-delay-ms 500 >"$lock_log" 2>&1 &
    lock_pid=$!
    for _ in $(seq 1 100); do
      kill -0 "$wallpaper_pid"
      grim -o HEADLESS-1 "$lock_capture" 2>/dev/null || true
      difference=$(magick compare -metric AE "$capture_one" "$lock_capture" null: 2>&1 || true)
      awk -v difference="''${difference:-0}" 'BEGIN { exit !(difference > 1000) }' && break
      sleep 0.02
    done
    awk -v difference="''${difference:-0}" 'BEGIN { exit !(difference > 1000) }'
    wait "$lock_pid"
    lock_pid=
    capture_wallpaper HEADLESS-1 "$restored"
    kill -0 "$wallpaper_pid"
    touch $out
  ''
