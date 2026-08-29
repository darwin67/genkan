#!/usr/bin/env bash

set -euo pipefail

: "${GENKAN_BIN:?GENKAN_BIN must point to the packaged Genkan binary}"

drm_root=${GENKAN_DRM_SYSFS_ROOT:-/sys/class/drm}
dri_root=${GENKAN_DRI_ROOT:-/dev/dri}
icd_root=${GENKAN_VULKAN_ICD_ROOT:-/run/opengl-driver/share/vulkan/icd.d}

if [[ ! -d $drm_root || ! -d $dri_root ]]; then
  echo "No DRM devices are available" >&2
  exit 1
fi

if [[ -z ${WAYLAND_DISPLAY:-} ]]; then
  echo "hardware-smoke must run from a Wayland session" >&2
  exit 1
fi

tmp_dir=$(mktemp -d)
active_group=""

stop_group() {
  if [[ -z $active_group ]]; then
    return
  fi

  kill -TERM -- "-$active_group" 2>/dev/null || true
  for _ in $(seq 1 20); do
    if ! kill -0 -- "-$active_group" 2>/dev/null; then
      break
    fi
    sleep 0.1
  done
  kill -KILL -- "-$active_group" 2>/dev/null || true
  wait "$active_group" 2>/dev/null || true
  active_group=""
}

cleanup() {
  stop_group
  rm -rf "$tmp_dir"
}
trap cleanup EXIT

mkdir -p "$tmp_dir/data/share/wayland-sessions"
cat > "$tmp_dir/data/share/wayland-sessions/smoke.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Hardware smoke test
Exec=true
EOF

connected=0
external=0
echo "Connected DRM outputs:"
for status_file in "$drm_root"/card*-*/status; do
  [[ -e $status_file ]] || continue
  [[ $(<"$status_file") == connected ]] || continue

  output=${status_file#"$drm_root"/}
  output=${output%/status}
  echo "  $output"
  ((connected += 1))
  connector=${output#card*-}
  case $connector in
    eDP-* | LVDS-* | DSI-*) ;;
    *) ((external += 1)) ;;
  esac
done

if ((connected == 0)); then
  echo "No connected DRM outputs were found" >&2
  exit 1
fi
if [[ ${GENKAN_REQUIRE_EXTERNAL_DISPLAY:-0} == 1 ]] && ((external == 0)); then
  echo "No connected external display was found" >&2
  exit 1
fi

compositor_outputs=()
if [[ ${GENKAN_EXERCISE_SWAY_OUTPUTS:-0} == 1 ]]; then
  if [[ -z ${SWAYSOCK:-} ]]; then
    echo "GENKAN_EXERCISE_SWAY_OUTPUTS requires a Sway session" >&2
    exit 1
  fi
  if ! outputs_json=$(swaymsg -t get_outputs -r); then
    echo "Failed to query Sway outputs" >&2
    exit 1
  fi
  if ! outputs_text=$(jq -er '.[] | select(.active) | .name' <<<"$outputs_json"); then
    echo "Sway has no active outputs" >&2
    exit 1
  fi
  mapfile -t compositor_outputs <<<"$outputs_text"

  active_external=0
  for output_name in "${compositor_outputs[@]}"; do
    case $output_name in
      eDP-* | LVDS-* | DSI-*) ;;
      *) ((active_external += 1)) ;;
    esac
  done
  if [[ ${GENKAN_REQUIRE_EXTERNAL_DISPLAY:-0} == 1 ]] && ((active_external == 0)); then
    echo "No active external Sway output was found" >&2
    exit 1
  fi
fi

run_adapter() {
  local vendor=$1
  local card=$2
  local render_node=$3
  local icd=$4
  local driver_library=$5
  local display_connected=$6
  local label=$7
  local run_dir="$tmp_dir/$label"
  local cage_pid genkan_pid expected_render expected_card fd target

  mkdir -p "$run_dir"
  cat > "$run_dir/run-genkan" <<EOF
#!/usr/bin/env bash
echo \$\$ > "$run_dir/genkan.pid"
exec "$GENKAN_BIN" --username smoke
EOF
  chmod +x "$run_dir/run-genkan"

  echo "Testing $label through $dri_root/$render_node with $icd"
  if ! VK_DRIVER_FILES="$icd" timeout --kill-after=2s "${GENKAN_VULKANINFO_TIMEOUT:-10s}" \
    vulkaninfo --summary > "$run_dir/vulkan.log" 2>&1; then
    cat "$run_dir/vulkan.log"
    echo "Vulkan discovery failed or timed out for $label" >&2
    exit 1
  fi
  grep -Eiq "vendorID[[:space:]]*=[[:space:]]*0x0*${vendor}" "$run_dir/vulkan.log"
  if ((display_connected == 0)); then
    echo "Passed Vulkan discovery for $label; no display is connected to this adapter"
    return
  fi

  XDG_DATA_DIRS="$tmp_dir/data/share${XDG_DATA_DIRS:+:$XDG_DATA_DIRS}" \
    ICED_BACKEND=wgpu \
    WLR_RENDERER=vulkan \
    WGPU_BACKEND=vulkan \
    VK_DRIVER_FILES="$icd" \
    setsid cage -d -- "$run_dir/run-genkan" > "$run_dir/cage.log" 2>&1 &
  cage_pid=$!
  active_group=$cage_pid

  for _ in $(seq 1 100); do
    [[ -s $run_dir/genkan.pid ]] && break
    if ! kill -0 "$cage_pid" 2>/dev/null; then
      cat "$run_dir/cage.log"
      echo "Cage exited before starting Genkan on $label" >&2
      exit 1
    fi
    sleep 0.1
  done
  [[ -s $run_dir/genkan.pid ]]
  genkan_pid=$(<"$run_dir/genkan.pid")

  for _ in $(seq 1 30); do
    if ! kill -0 "$genkan_pid" 2>/dev/null; then
      cat "$run_dir/cage.log"
      echo "Genkan exited early on $label" >&2
      exit 1
    fi
    sleep 0.1
  done

  if [[ ${GENKAN_EXERCISE_SWAY_OUTPUTS:-0} == 1 ]]; then
    for output_name in "${compositor_outputs[@]}"; do
      swaymsg -r "[pid=$cage_pid] move container to output $output_name" | jq -e 'all(.success)' >/dev/null
      sleep 0.5
      if ! kill -0 "$genkan_pid" 2>/dev/null; then
        cat "$run_dir/cage.log"
        echo "Genkan exited after moving Cage to $output_name" >&2
        exit 1
      fi
      if ! tree_json=$(swaymsg -t get_tree -r) || ! jq -e \
        --arg output "$output_name" \
        --argjson pid "$cage_pid" \
        '[.. | objects | select(.type? == "output" and .name? == $output) | .. | objects | .pid?] | any(. == $pid)' \
        <<<"$tree_json" >/dev/null; then
        echo "Cage was not found on Sway output $output_name after moving it" >&2
        exit 1
      fi
      echo "Passed $label presentation on $output_name"
    done
  fi

  expected_render="$dri_root/$render_node"
  expected_card="$dri_root/$card"
  found_expected=0
  found_nvidia=0
  for fd in /proc/"$genkan_pid"/fd/*; do
    target=$(readlink "$fd" 2>/dev/null || true)
    [[ $target == "$expected_render" || $target == "$expected_card" ]] && found_expected=1
    [[ $target == /dev/nvidia* ]] && found_nvidia=1
  done
  if ((found_expected == 0)) && ! grep -Fq "$driver_library" /proc/"$genkan_pid"/maps; then
    cat "$run_dir/cage.log"
    echo "Genkan neither opened its DRM node nor retained $driver_library on $label" >&2
    exit 1
  fi
  if [[ $vendor == 1002 ]] && { ((found_nvidia != 0)) || grep -Fiq nvidia /proc/"$genkan_pid"/maps; }; then
    echo "The AMD-only run unexpectedly loaded or opened an NVIDIA driver" >&2
    exit 1
  fi
  if ((found_expected == 0)); then
    echo "$label loaded $driver_library without retaining a DRM device descriptor"
  fi

  stop_group
  ((rendered_adapters += 1))
  echo "Passed $label"
}

tested_vendors=""
tested_adapters=0
rendered_adapters=0
for card_path in "$drm_root"/card[0-9]*; do
  [[ -e $card_path/device/vendor ]] || continue
  vendor=$(<"$card_path/device/vendor")
  vendor=${vendor#0x}
  case $vendor in
    1002) driver=radeon ;;
    10de) driver=nvidia ;;
    *) continue ;;
  esac
  if [[ " $tested_vendors " == *" $vendor "* ]]; then
    echo "Skipping $(basename "$card_path"); vendor 0x$vendor was already tested through its shared ICD"
    continue
  fi

  render_node=""
  for render_path in "$card_path/device/drm"/renderD*; do
    [[ -e $render_path ]] || continue
    render_node=$(basename "$render_path")
    break
  done
  if [[ -z $render_node ]]; then
    echo "No render node found for $(basename "$card_path")" >&2
    exit 1
  fi

  arch=$(uname -m)
  if [[ $driver == radeon ]]; then
    icd=$icd_root/radeon_icd.${arch}.json
    driver_library=libvulkan_radeon
  else
    icd=$icd_root/nvidia_icd.json
    driver_library=libGLX_nvidia
  fi
  if [[ ! -f $icd ]]; then
    echo "Missing Vulkan ICD for $driver: $icd" >&2
    exit 1
  fi

  display_connected=0
  for status_file in "$card_path"-*/status; do
    [[ -e $status_file ]] || continue
    if [[ $(<"$status_file") == connected ]]; then
      display_connected=1
      break
    fi
  done

  run_adapter "$vendor" "$(basename "$card_path")" "$render_node" "$icd" "$driver_library" "$display_connected" "$driver-$(basename "$card_path")"
  tested_vendors="$tested_vendors $vendor"
  ((tested_adapters += 1))
done

if ((tested_adapters == 0)); then
  echo "No supported AMD or NVIDIA adapters were found" >&2
  exit 1
fi

for required in ${GENKAN_REQUIRE_GPU_VENDORS:-}; do
  required=${required#0x}
  if [[ " $tested_vendors " != *" $required "* ]]; then
    echo "Required GPU vendor 0x$required was not tested" >&2
    exit 1
  fi
done

echo "Hardware smoke passed for $tested_adapters Vulkan vendor(s), $rendered_adapters display vendor(s), $connected connected output(s), and $external external output(s)"
