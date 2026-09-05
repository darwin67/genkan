#!/usr/bin/env bash

set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=preview-evidence-lib.sh
source "${PREVIEW_EVIDENCE_LIB:-$script_dir/preview-evidence-lib.sh}"

: "${GENKAN_BIN:?set GENKAN_BIN to the packaged Genkan executable}"
: "${PREVIEW_OUTPUT_DIR:?set PREVIEW_OUTPUT_DIR to the screenshot destination}"

for command in compare identify strace weston weston-screenshooter; do
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
mapfile -t preview_fixtures < <("$GENKAN_BIN" login --list-preview-fixtures)
[[ ${#preview_fixtures[@]} -gt 0 ]]
declare -A covered_fixtures=()

capture() {
  local name=$1
  local width=$2
  local height=$3
  local fixture=$4
  local mode=${5:-login}
  local case_dir="$work_dir/$name"
  local socket="wayland-$name"
  local screenshot
  local previous_hash=""
  local frame_hash

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

  if [[ $mode == login ]]; then
    cat > "$case_dir/run-genkan" <<EOF
#!${SHELL:-/bin/sh}
echo \$\$ > "$case_dir/genkan.pid"
exec strace -f -e trace=connect -o "$case_dir/connect.trace" \
  "$GENKAN_BIN" login --windowed --preview "$fixture" --width "$width" --height "$height"
EOF
  else
    cat > "$case_dir/run-genkan" <<EOF
#!${SHELL:-/bin/sh}
echo \$\$ > "$case_dir/genkan.pid"
exec strace -f -e trace=connect -o "$case_dir/connect.trace" \
  "$GENKAN_BIN" lock --preview "$fixture" --width "$width" --height "$height"
EOF
  fi
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

  # The wrapper PID exists before iced has mapped and painted its window. Give
  # constrained software renderers time to move past a stable compositor-only
  # frame before accepting screenshot stability.
  sleep 2
  require_running_process "$app_pid" "$case_dir/genkan.log"

  screenshot=""
  for _ in $(seq 1 50); do
    require_running_process "$app_pid" "$case_dir/genkan.log"
    rm -f "$case_dir/capture"/wayland-screenshot-*.png
    (
      cd "$case_dir/capture"
      HOME="$case_dir/home" \
        XDG_RUNTIME_DIR="$case_dir/runtime" \
        WAYLAND_DISPLAY="$socket" \
        timeout 5s weston-screenshooter
    )
    require_running_process "$app_pid" "$case_dir/genkan.log"
    screenshot=$(find "$case_dir/capture" -maxdepth 1 -name 'wayland-screenshot-*.png' -print -quit)
    if [[ -n $screenshot ]] && valid_preview_frame "$screenshot" "$width" "$height"; then
      frame_hash=$(sha256sum "$screenshot" | cut -d' ' -f1)
      if advance_frame_stability previous_hash "$frame_hash"; then
        if [[ -z ${PREVIEW_REFERENCE_DIR:-} || ! -f $PREVIEW_REFERENCE_DIR/$name.png ]] ||
          check_preview_baseline \
            "$screenshot" "$PREVIEW_REFERENCE_DIR/$name.png" >/dev/null 2>&1; then
          break
        fi
      fi
    else
      advance_frame_stability previous_hash "" || true
    fi
    screenshot=""
    sleep 0.1
  done
  if [[ -z $screenshot ]]; then
    cat "$case_dir/genkan.log" >&2
    echo "Genkan did not render a stable non-blank $name frame" >&2
    exit 1
  fi
  mv "$screenshot" "$PREVIEW_OUTPUT_DIR/$name.png"
  if [[ $mode == login ]]; then
    covered_fixtures["$fixture"]=1
  fi

  cleanup_case

  check_preview_connections "$case_dir/connect.trace" "$case_dir/runtime/$socket"
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

# Exact software-rendered lock states without acquiring a compositor lock.
capture lock-securing 1280 800 securing lock
capture lock-prompt 1280 800 prompt lock
capture lock-challenge 1280 800 challenge lock
capture lock-submitting 1280 800 submitting lock
capture lock-failure 1280 800 failure lock
capture lock-narrow-prompt 480 600 prompt lock
capture lock-portrait-prompt 1080 1920 prompt lock
capture lock-ultrawide-prompt 2560 720 prompt lock

# Probe every fixture, including states that do not need a named review capture.
for fixture in "${preview_fixtures[@]}"; do
  if [[ ! -v covered_fixtures["$fixture"] ]]; then
    capture "fixture-$fixture" 1280 800 "$fixture"
  fi
done

if [[ -n ${PREVIEW_REFERENCE_DIR:-} ]]; then
  : "${PREVIEW_REFERENCE_MANIFEST:?set PREVIEW_REFERENCE_MANIFEST with PREVIEW_REFERENCE_DIR}"
  # shellcheck source=reference-images-manifest.sh
  source "$PREVIEW_REFERENCE_MANIFEST"
  for entry in "${REFERENCE_IMAGE_MANIFEST[@]}"; do
    read -r name _ <<<"$entry"
    check_preview_baseline "$PREVIEW_OUTPUT_DIR/$name" "$PREVIEW_REFERENCE_DIR/$name"
  done
fi

printf 'Captured %s deterministic preview images\n' "$(find "$PREVIEW_OUTPUT_DIR" -name '*.png' | wc -l)"
