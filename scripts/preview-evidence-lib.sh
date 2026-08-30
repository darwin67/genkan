#!/usr/bin/env bash

require_running_process() {
  local pid=$1
  local log=$2
  if kill -0 "$pid" 2>/dev/null; then
    return 0
  fi

  [[ ! -f $log ]] || cat "$log" >&2
  echo "Genkan exited before preview evidence was ready" >&2
  return 1
}

valid_preview_frame() {
  local image=$1
  local expected_width=$2
  local expected_height=$3
  local width height colors

  read -r width height colors < <(identify -format '%w %h %k' "$image")
  [[ $width == "$expected_width" && $height == "$expected_height" ]] || return 1
  [[ $colors =~ ^[0-9]+$ && $colors -ge 16 ]]
}

advance_frame_stability() {
  local -n previous_hash_ref=$1
  local current=$2
  local stable=false

  if [[ -n $current && $current == "$previous_hash_ref" ]]; then
    stable=true
  fi
  previous_hash_ref=$current
  [[ $stable == true ]]
}

check_preview_connections() {
  local trace=$1
  local allowed_wayland_socket=$2
  local unexpected

  unexpected=$(grep 'connect(' "$trace" | grep -Fv "sun_path=\"$allowed_wayland_socket\"" || true)
  if [[ -z $unexpected ]]; then
    return 0
  fi

  printf '%s\n' "$unexpected" >&2
  echo "preview attempted a non-Wayland connection" >&2
  return 1
}
