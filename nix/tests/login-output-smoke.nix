{ pkgs, genkan }:

let
  inheritedSocketTest = pkgs.writeText "genkan-inherited-socket-test.py" ''
    import os
    import socket
    import subprocess
    import sys
    import time

    connection = socket.socket(socket.AF_UNIX)
    connection.connect(sys.argv[1])
    inherited = os.dup(connection.fileno())
    env = os.environ.copy()
    env.pop("WAYLAND_DISPLAY", None)
    env["WAYLAND_SOCKET"] = str(inherited)
    process = subprocess.Popen(
        [
            sys.argv[2],
            "login",
            "--preview",
            "selected",
            "--reduce-motion",
            "--authentication-output",
            "HEADLESS-2",
        ],
        env=env,
        pass_fds=(inherited,),
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
    )
    for _ in range(100):
        tree = subprocess.run(
            ["swaymsg", "-s", sys.argv[3], "-t", "get_tree"],
            env=env,
            check=True,
            capture_output=True,
            text=True,
        ).stdout
        if '"name": "Genkan"' in tree:
            break
        assert process.poll() is None, process.stderr.read().decode()
        time.sleep(0.02)
    else:
        raise AssertionError("Genkan did not map with inherited WAYLAND_SOCKET")
    process.terminate()
    _, stderr = process.communicate(timeout=2)
    assert b"dynamic output monitoring is unavailable" in stderr, stderr.decode()
    os.close(inherited)
  '';
in

pkgs.runCommand "genkan-login-output-smoke"
  {
    nativeBuildInputs = with pkgs; [
      coreutils
      grim
      gnugrep
      imagemagick
      python3
      sway
      wtype
    ];
  }
  ''
    set -eEuo pipefail
    runtime=$(mktemp -d)
    config=$(mktemp)
    sway_log=$(mktemp)
    login_log=$(mktemp)
    selected=$(mktemp --suffix=.png)
    background=$(mktemp --suffix=.png)
    selected_baseline=$(mktemp --suffix=.png)
    background_authentication=$(mktemp --suffix=.png)
    before_input=$(mktemp --suffix=.png)
    after_input=$(mktemp --suffix=.png)
    expected_input=$(mktemp --suffix=.png)
    login_pid=
    report_failure() {
      status=$?
      echo "login output smoke failed at line $1" >&2
      cat "$sway_log" "$login_log" >&2
      exit "$status"
    }
    trap 'report_failure $LINENO' ERR
    cleanup() {
      [[ -z "$login_pid" ]] || kill "$login_pid" 2>/dev/null || true
      [[ -z "''${sway_pid:-}" ]] || kill "$sway_pid" 2>/dev/null || true
      rm -rf "$runtime" "$config" "$sway_log" "$login_log" \
        "$selected" "$background" "$selected_baseline" \
        "$background_authentication" "$before_input" "$after_input" "$expected_input"
    }
    trap cleanup EXIT
    chmod 700 "$runtime"
    printf '%s\n' 'output * mode 1280x800' 'seat * hide_cursor 1000' > "$config"
    XDG_RUNTIME_DIR="$runtime" DBUS_SESSION_BUS_ADDRESS="unix:path=$runtime/no-session-bus" \
      WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS=2 \
      WLR_LIBINPUT_NO_DEVICES=1 sway -c "$config" -d >"$sway_log" 2>&1 &
    sway_pid=$!
    for _ in $(seq 1 100); do
      socket=$(find "$runtime" -maxdepth 1 -type s -name 'wayland-*' -print -quit)
      ipc=$(find "$runtime" -maxdepth 1 -type s -name 'sway-ipc.*.sock' -print -quit)
      [[ -n "''${socket:-}" && -n "''${ipc:-}" ]] && break
      sleep 0.05
    done
    [[ -n "''${socket:-}" && -n "''${ipc:-}" ]]

    output_x() {
      XDG_RUNTIME_DIR="$runtime" swaymsg -s "$ipc" -t get_outputs -r \
        | python3 -c '
import json
import sys

name = sys.argv[1]
output = next(output for output in json.load(sys.stdin) if output["name"] == name)
print(str(output["rect"]["x"]) + ".0")
' "$1"
    }
    start_preview() {
      local fixture=$1
      local output=$2
      local expected_geometry="''${3:-width: 1280.0, height: 800.0, layout_width: 2560.0}"
      local expected_width="''${4:-1280.0}"
      local expected_x
      expected_x=$(output_x "$output")
      : > "$login_log"
      env WAYLAND_DISPLAY=$(basename "$socket") XDG_RUNTIME_DIR="$runtime" \
        ${genkan}/bin/genkan login --preview "$fixture" --reduce-motion \
          --authentication-output "$output" >"$login_log" 2>&1 &
      login_pid=$!
      for _ in $(seq 1 200); do
        XDG_RUNTIME_DIR="$runtime" swaymsg -s "$ipc" -t get_tree \
          | grep -Fq '"name": "Genkan"' && break
        kill -0 "$login_pid"
        sleep 0.02
      done
      XDG_RUNTIME_DIR="$runtime" swaymsg -s "$ipc" \
        '[title="Genkan"] fullscreen enable global' >/dev/null
      for _ in $(seq 1 200); do
        grep -Fq "$expected_geometry" "$login_log" && break
        kill -0 "$login_pid"
        sleep 0.02
      done
      grep -Fq "$expected_geometry" "$login_log"
      grep -Fq "x: $expected_x, y: 0.0, width: $expected_width" "$login_log"
    }
    stop_preview() {
      kill "$login_pid"
      wait "$login_pid" 2>/dev/null || true
      login_pid=
      sleep 0.1
    }

    start_preview session-menu HEADLESS-1
    sleep 0.3
    XDG_RUNTIME_DIR="$runtime" WAYLAND_DISPLAY=$(basename "$socket") \
      grim -o HEADLESS-2 "$selected_baseline"
    XDG_RUNTIME_DIR="$runtime" WAYLAND_DISPLAY=$(basename "$socket") \
      grim -o HEADLESS-1 "$background_authentication"
    stop_preview

    start_preview session-menu HEADLESS-2
    sleep 0.3
    XDG_RUNTIME_DIR="$runtime" WAYLAND_DISPLAY=$(basename "$socket") \
      grim -o HEADLESS-2 "$selected"
    XDG_RUNTIME_DIR="$runtime" WAYLAND_DISPLAY=$(basename "$socket") \
      grim -o HEADLESS-1 "$background"
    [[ $(identify -format '%k' "$selected") -gt 256 ]]
    [[ $(identify -format '%k' "$background") -gt 256 ]]
    # Switching the requested output must change each output's role, not just
    # produce two different halves of the wallpaper.
    changed=$(magick compare -metric AE "$selected" "$selected_baseline" null: 2>&1 \
      | awk '{ print $1 }' || true)
    [[ "$changed" -gt 10000 ]]
    changed=$(magick compare -metric AE "$background" "$background_authentication" null: 2>&1 \
      | awk '{ print $1 }' || true)
    [[ "$changed" -gt 10000 ]]
    topology_updates=$(grep -Fc 'layout_width: 2560.0, layout_height: 800.0' "$login_log")
    XDG_RUNTIME_DIR="$runtime" swaymsg -s "$ipc" output HEADLESS-2 disable >/dev/null
    for _ in $(seq 1 200); do
      grep -Fq 'layout_width: 1280.0, layout_height: 800.0' "$login_log" && break
      sleep 0.02
    done
    grep -Fq 'layout_width: 1280.0, layout_height: 800.0' "$login_log"
    kill -0 "$login_pid"
    XDG_RUNTIME_DIR="$runtime" swaymsg -s "$ipc" output HEADLESS-2 enable >/dev/null
    expected_x=$(output_x HEADLESS-2)
    for _ in $(seq 1 200); do
      [[ $(grep -Fc 'layout_width: 2560.0, layout_height: 800.0' "$login_log") -gt "$topology_updates" ]] && break
      sleep 0.02
    done
    [[ $(grep -Fc 'layout_width: 2560.0, layout_height: 800.0' "$login_log") -gt "$topology_updates" ]]
    grep -F 'selected authentication region' "$login_log" | tail -n 1 \
      | grep -Fq "x: $expected_x, y: 0.0, width: 1280.0"
    stop_preview

    start_preview visible-prompt HEADLESS-2
    XDG_RUNTIME_DIR="$runtime" WAYLAND_DISPLAY=$(basename "$socket") wtype -k tab
    sleep 0.1
    XDG_RUNTIME_DIR="$runtime" WAYLAND_DISPLAY=$(basename "$socket") \
      wtype -M ctrl -k a -m ctrl abcdef
    XDG_RUNTIME_DIR="$runtime" WAYLAND_DISPLAY=$(basename "$socket") \
      wtype -k home -k right -k right -k right
    XDG_RUNTIME_DIR="$runtime" swaymsg -s "$ipc" output HEADLESS-2 disable >/dev/null
    for _ in $(seq 1 200); do
      grep -Fq 'layout_width: 1280.0, layout_height: 800.0' "$login_log" && break
      sleep 0.02
    done
    XDG_RUNTIME_DIR="$runtime" swaymsg -s "$ipc" \
      output HEADLESS-1 mode 1024x480 >/dev/null
    for _ in $(seq 1 200); do
      grep -Fq 'width: 1024.0, height: 480.0, layout_width: 1024.0' "$login_log" && break
      sleep 0.02
    done
    grep -Fq 'width: 1024.0, height: 480.0, layout_width: 1024.0' "$login_log"
    sleep 0.2
    XDG_RUNTIME_DIR="$runtime" WAYLAND_DISPLAY=$(basename "$socket") \
      grim -o HEADLESS-1 "$before_input"
    XDG_RUNTIME_DIR="$runtime" WAYLAND_DISPLAY=$(basename "$socket") wtype X
    sleep 0.2
    XDG_RUNTIME_DIR="$runtime" WAYLAND_DISPLAY=$(basename "$socket") \
      grim -o HEADLESS-1 "$after_input"
    changed=$(magick compare -metric AE "$before_input" "$after_input" null: 2>&1 \
      | awk '{ print $1 }' || true)
    [[ "$changed" -gt 200 ]]
    stop_preview

    start_preview visible-prompt HEADLESS-1 \
      'width: 1024.0, height: 480.0, layout_width: 1024.0' 1024.0
    XDG_RUNTIME_DIR="$runtime" WAYLAND_DISPLAY=$(basename "$socket") wtype -k tab
    sleep 0.1
    XDG_RUNTIME_DIR="$runtime" WAYLAND_DISPLAY=$(basename "$socket") \
      wtype -M ctrl -k a -m ctrl abcXdef
    sleep 0.2
    XDG_RUNTIME_DIR="$runtime" WAYLAND_DISPLAY=$(basename "$socket") \
      grim -o HEADLESS-1 "$expected_input"
    difference=$(magick compare -metric AE "$after_input" "$expected_input" null: 2>&1 \
      | awk '{ print $1 }' || true)
    [[ "$difference" -lt 200 ]]
    stop_preview

    # An inherited socket is reserved for iced/winit. Auxiliary output
    # discovery must not consume it before the GUI connects.
    python3 ${inheritedSocketTest} "$socket" ${genkan}/bin/genkan "$ipc"

    touch "$out"
  ''
