#!/usr/bin/env bash

set -euo pipefail

: "${GENKAN_BIN:?set GENKAN_BIN to the packaged Genkan executable}"
: "${PREVIEW_OUTPUT_DIR:?set PREVIEW_OUTPUT_DIR to the screenshot destination}"

for command in identify strace weston weston-screenshooter; do
  command -v "$command" >/dev/null || {
    echo "missing preview evidence dependency: $command" >&2
    exit 1
  }
done

work_dir=$(mktemp -d)
weston_pid=""
app_pid=""

cleanup_case() {
  if [[ -n $app_pid ]]; then
    kill -TERM -- "-$app_pid" 2>/dev/null || true
    wait "$app_pid" 2>/dev/null || true
    app_pid=""
  fi
  if [[ -n $weston_pid ]]; then
    kill -TERM "$weston_pid" 2>/dev/null || true
    wait "$weston_pid" 2>/dev/null || true
    weston_pid=""
  fi
}

cleanup() {
  cleanup_case
  rm -rf "$work_dir"
}
trap cleanup EXIT

mkdir -p "$PREVIEW_OUTPUT_DIR"

capture() {
  local name=$1
  local width=$2
  local height=$3
  local fixture=$4
  local case_dir="$work_dir/$name"
  local socket="wayland-$name"
  local screenshot

  mkdir -p "$case_dir/home/.cache/fontconfig" "$case_dir/runtime" "$case_dir/capture"
  chmod 700 "$case_dir/runtime"

  HOME="$case_dir/home" \
    XDG_RUNTIME_DIR="$case_dir/runtime" \
    weston \
      --backend=headless-backend.so \
      --renderer=pixman \
      --width="$width" \
      --height="$height" \
      --idle-time=0 \
      --shell=kiosk \
      --socket="$socket" \
      --debug \
      --log="$case_dir/weston.log" &
  weston_pid=$!

  for _ in $(seq 1 100); do
    [[ -S "$case_dir/runtime/$socket" ]] && break
    if ! kill -0 "$weston_pid" 2>/dev/null; then
      cat "$case_dir/weston.log" >&2
      echo "Weston exited before $name capture" >&2
      exit 1
    fi
    sleep 0.05
  done
  [[ -S "$case_dir/runtime/$socket" ]]

  cat > "$case_dir/run-genkan" <<EOF
#!${SHELL:-/bin/sh}
echo \$\$ > "$case_dir/genkan.pid"
exec strace -f -e trace=connect -o "$case_dir/connect.trace" \
  "$GENKAN_BIN" --windowed --preview "$fixture" --width "$width" --height "$height"
EOF
  chmod +x "$case_dir/run-genkan"

  HOME="$case_dir/home" \
    XDG_RUNTIME_DIR="$case_dir/runtime" \
    WAYLAND_DISPLAY="$socket" \
    GREETD_SOCK="$case_dir/greetd.sock" \
    DBUS_SYSTEM_BUS_ADDRESS="unix:path=$case_dir/system-bus.sock" \
    ICED_BACKEND=wgpu \
    WLR_RENDERER=pixman \
    WGPU_BACKEND=vulkan \
    LIBGL_ALWAYS_SOFTWARE=1 \
    setsid "$case_dir/run-genkan" > "$case_dir/genkan.log" 2>&1 &
  app_pid=$!

  for _ in $(seq 1 100); do
    [[ -s "$case_dir/genkan.pid" ]] && break
    if ! kill -0 "$app_pid" 2>/dev/null; then
      cat "$case_dir/genkan.log" >&2
      echo "Genkan exited before $name capture" >&2
      exit 1
    fi
    sleep 0.05
  done
  [[ -s "$case_dir/genkan.pid" ]]
  sleep 0.5

  (
    cd "$case_dir/capture"
    HOME="$case_dir/home" \
      XDG_RUNTIME_DIR="$case_dir/runtime" \
      WAYLAND_DISPLAY="$socket" \
      timeout 5s weston-screenshooter
  )
  screenshot=$(find "$case_dir/capture" -maxdepth 1 -name 'wayland-screenshot-*.png' -print -quit)
  [[ -n $screenshot ]]
  identify -format '%w %h' "$screenshot" | grep -Fx "$width $height"
  mv "$screenshot" "$PREVIEW_OUTPUT_DIR/$name.png"

  cleanup_case

  if grep -Eq 'greetd\.sock|system-bus\.sock' "$case_dir/connect.trace"; then
    cat "$case_dir/connect.trace" >&2
    echo "preview fixture $fixture attempted to contact a live service" >&2
    exit 1
  fi
}

# Reference states at the default review size.
capture account-selection 1280 800 users
capture secret-prompt 1280 800 secret-prompt
capture visible-prompt 1280 800 visible-prompt
capture authentication-failure 1280 800 authentication-failure
capture power-confirmation 1280 800 power-confirmation

# Responsive evidence at representative laptop, widescreen, ultrawide, and narrow sizes.
capture laptop-large-accounts 1440 900 large-account-set
capture widescreen-users 1920 1080 users
capture ultrawide-selected 2560 1080 selected
capture narrow-selected 480 600 selected
capture narrow-long-authentication 480 600 long-authentication

printf 'Captured %s deterministic preview images\n' "$(find "$PREVIEW_OUTPUT_DIR" -name '*.png' | wc -l)"
