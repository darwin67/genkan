#!/usr/bin/env bash

set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' EXIT

fail() {
  echo "regression test failed: $*" >&2
  exit 1
}

expect_failure() {
  local expected=$1
  shift
  if "$@" > "$tmp_dir/command.log" 2>&1; then
    cat "$tmp_dir/command.log" >&2
    fail "command unexpectedly succeeded: $*"
  fi
  grep -Fq "$expected" "$tmp_dir/command.log" || {
    cat "$tmp_dir/command.log" >&2
    fail "missing expected failure: $expected"
  }
}

expect_failure_status() {
  local expected_status=$1
  local expected=$2
  shift 2
  local status=0
  "$@" > "$tmp_dir/command.log" 2>&1 || status=$?
  [[ $status -eq $expected_status ]] || {
    cat "$tmp_dir/command.log" >&2
    fail "expected status $expected_status, got $status: $*"
  }
  grep -Fq "$expected" "$tmp_dir/command.log" || {
    cat "$tmp_dir/command.log" >&2
    fail "missing expected failure: $expected"
  }
}

run_commit_check() {
  local repository=$1
  local base=$2
  local head=$3
  (cd "$repository" && "$repo_root/scripts/check-commits.sh" "$base" "$head")
}

test_conventional_commit_description() {
  local repository="$tmp_dir/commits"
  git init -q "$repository"
  git -C "$repository" config user.name Test
  git -C "$repository" config user.email test@example.com
  printf 'fixture\n' > "$repository/fixture"
  git -C "$repository" add fixture
  git -C "$repository" commit -qm 'test: add fixture'
  local base tree invalid
  base=$(git -C "$repository" rev-parse HEAD)
  tree=$(git -C "$repository" rev-parse HEAD^{tree})
  invalid=$(printf 'fix:    \n\n' | git -C "$repository" commit-tree "$tree" -p "$base")

  expect_failure "invalid Conventional Commit" \
    run_commit_check "$repository" "$base" "$invalid"
}

make_drm_fixture() {
  local fixture=$1
  mkdir -p \
    "$fixture/drm/card0/device/drm/renderD128" \
    "$fixture/drm/card1/device/drm/renderD129" \
    "$fixture/drm/card9-eDP-1" \
    "$fixture/dri" \
    "$fixture/icd" \
    "$fixture/bin"
  printf '0x1002\n' > "$fixture/drm/card0/device/vendor"
  printf '0x1002\n' > "$fixture/drm/card1/device/vendor"
  printf 'connected\n' > "$fixture/drm/card9-eDP-1/status"
  : > "$fixture/icd/radeon_icd.$(uname -m).json"
}

run_hardware_fixture() {
  local fixture=$1
  shift
  env \
    GENKAN_BIN=/bin/true \
    GENKAN_DRM_SYSFS_ROOT="$fixture/drm" \
    GENKAN_DRI_ROOT="$fixture/dri" \
    GENKAN_VULKAN_ICD_ROOT="$fixture/icd" \
    WAYLAND_DISPLAY=fixture \
    PATH="$fixture/bin:$PATH" \
    "$@" \
    bash "$repo_root/scripts/hardware-smoke.sh"
}

test_vendor_level_hardware_coverage() {
  local fixture="$tmp_dir/vendor"
  make_drm_fixture "$fixture"
  cat > "$fixture/bin/vulkaninfo" <<'EOF'
#!/usr/bin/env bash
printf 'vendorID = 0x1002\n'
EOF
  chmod +x "$fixture/bin/vulkaninfo"

  run_hardware_fixture "$fixture" > "$tmp_dir/vendor.log"
  grep -Fq '1 Vulkan vendor(s)' "$tmp_dir/vendor.log"
  [[ $(grep -Fc 'Testing radeon-' "$tmp_dir/vendor.log") == 1 ]]
}

test_vulkan_discovery_timeout() {
  local fixture="$tmp_dir/timeout"
  make_drm_fixture "$fixture"
  cat > "$fixture/bin/vulkaninfo" <<'EOF'
#!/usr/bin/env bash
sleep 10
EOF
  chmod +x "$fixture/bin/vulkaninfo"

  expect_failure "Vulkan discovery failed or timed out" \
    run_hardware_fixture "$fixture" GENKAN_VULKANINFO_TIMEOUT=0.1s
}

test_sway_query_failure_is_not_masked() {
  local fixture="$tmp_dir/sway-failure"
  make_drm_fixture "$fixture"
  cat > "$fixture/bin/swaymsg" <<'EOF'
#!/usr/bin/env bash
printf '[{"name":"eDP-1","active":true}]\n'
exit 1
EOF
  chmod +x "$fixture/bin/swaymsg"

  expect_failure "Failed to query Sway outputs" \
    run_hardware_fixture "$fixture" GENKAN_EXERCISE_SWAY_OUTPUTS=1 SWAYSOCK=fixture
}

test_external_output_must_be_active() {
  local fixture="$tmp_dir/external"
  make_drm_fixture "$fixture"
  mkdir -p "$fixture/drm/card9-DP-1"
  printf 'connected\n' > "$fixture/drm/card9-DP-1/status"
  cat > "$fixture/bin/swaymsg" <<'EOF'
#!/usr/bin/env bash
printf '[{"name":"eDP-1","active":true},{"name":"DP-1","active":false}]\n'
EOF
  chmod +x "$fixture/bin/swaymsg"

  expect_failure "No active external Sway output was found" \
    run_hardware_fixture "$fixture" \
    GENKAN_EXERCISE_SWAY_OUTPUTS=1 \
    GENKAN_REQUIRE_EXTERNAL_DISPLAY=1 \
    SWAYSOCK=fixture
}

make_presentation_stubs() {
  local fixture=$1
  rm -rf "$fixture/drm/card0/device/drm/renderD128"
  mkdir -p \
    "$fixture/drm/card2/device/drm/renderD130" \
    "$fixture/drm/card2-DP-1"
  printf '0x1002\n' > "$fixture/drm/card2/device/vendor"
  printf 'connected\n' > "$fixture/drm/card2-DP-1/status"
  : > "$fixture/dri/card2"

  cat > "$fixture/bin/vulkaninfo" <<'EOF'
#!/usr/bin/env bash
case ${VK_DRIVER_FILES:-} in
  *nvidia*) printf 'vendorID = 0x10de\n' ;;
  *) printf 'vendorID = 0x1002\n' ;;
esac
EOF
  cat > "$fixture/bin/cage" <<'EOF'
#!/usr/bin/env bash
for argument in "$@"; do
  command=$argument
done
echo $$ > "$GENKAN_TEST_STATE/leader.pid"
exec 9< "$GENKAN_TEST_DRI/card2"
if [[ ${GENKAN_TEST_CAGE_WRONG_DRI:-0} == 1 ]]; then
  exec 8< "$GENKAN_TEST_DRI/card-wrong"
fi
"$command" &
child=$!
echo "$child" > "$GENKAN_TEST_STATE/child.pid"
trap 'wait "$child" 2>/dev/null || true; exit 0' TERM
wait "$child"
EOF
  cat > "$fixture/bin/genkan" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" > "$GENKAN_TEST_STATE/genkan.args"
exec 9< "$GENKAN_TEST_DRI/card2"
if [[ ${GENKAN_TEST_GENKAN_WRONG_DRI:-0} == 1 ]]; then
  exec 8< "$GENKAN_TEST_DRI/card-wrong"
fi
if [[ -n ${GENKAN_TEST_OPPOSING_DRIVER:-} ]]; then
  exec env LD_PRELOAD="$GENKAN_TEST_OPPOSING_DRIVER" sleep 30
fi
sleep 30
EOF
  cat > "$fixture/bin/swaymsg" <<'EOF'
#!/usr/bin/env bash
case "$*" in
  *get_outputs*)
    printf '[{"name":"DP-1","active":true}]\n'
    ;;
  *' move container to output '*)
    expression=$*
    pid=${expression#*pid=}
    pid=${pid%%]*}
    output=${expression##* output }
    printf '%s\n' "$pid" > "$GENKAN_TEST_STATE/pid"
    printf '%s\n' "$output" > "$GENKAN_TEST_STATE/output"
    printf '[{"success":true}]\n'
    ;;
  *get_tree*)
    pid=$(<"$GENKAN_TEST_STATE/pid")
    output=$(<"$GENKAN_TEST_STATE/output")
    if [[ ${GENKAN_TEST_WRONG_OUTPUT:-0} == 1 ]]; then
      output=DP-wrong
    fi
    printf '{"nodes":[{"type":"output","name":"%s","nodes":[{"pid":%s}]}]}\n' "$output" "$pid"
    ;;
esac
EOF
  chmod +x "$fixture/bin/"*
}

run_presentation_fixture() {
  local fixture=$1
  shift
  run_hardware_fixture "$fixture" \
    GENKAN_BIN="$fixture/bin/genkan" \
    GENKAN_EXERCISE_SWAY_OUTPUTS=1 \
    GENKAN_REQUIRE_EXTERNAL_DISPLAY=1 \
    GENKAN_TEST_DRI="$fixture/dri" \
    GENKAN_TEST_STATE="$fixture/state" \
    SWAYSOCK=fixture \
    "$@"
}

assert_fixture_process_stopped() {
  local fixture=$1
  local pid_file pid
  for pid_file in leader.pid child.pid; do
    pid=$(<"$fixture/state/$pid_file")
    if kill -0 "$pid" 2>/dev/null; then
      fail "hardware fixture left $pid_file process $pid running"
    fi
  done
}

test_representative_selection_placement_and_cleanup() {
  local fixture="$tmp_dir/presentation"
  local library
  make_drm_fixture "$fixture"
  mkdir -p "$fixture/state"
  make_presentation_stubs "$fixture"
  library=$(ldd "$(type -P sleep)" | awk '/libc\.so/ { print $3; exit }')
  cp "$library" "$fixture/libnvidia-fixture.so"
  cp "$library" "$fixture/libvulkan_radeon-fixture.so"

  expect_failure "unexpectedly loaded or opened an NVIDIA driver" \
    run_presentation_fixture "$fixture" GENKAN_TEST_OPPOSING_DRIVER="$fixture/libnvidia-fixture.so"
  assert_fixture_process_stopped "$fixture"

  run_presentation_fixture "$fixture" > "$tmp_dir/presentation.log"
  grep -Fq 'Skipping card0; it has no render node' "$tmp_dir/presentation.log"
  grep -Fq 'Testing radeon-card2' "$tmp_dir/presentation.log"
  grep -Fq 'presentation on DP-1' "$tmp_dir/presentation.log"
  grep -Fxq 'login --username smoke --reduce-motion' "$fixture/state/genkan.args"
  assert_fixture_process_stopped "$fixture"

  printf '0x10de\n' > "$fixture/drm/card2/device/vendor"
  : > "$fixture/icd/nvidia_icd.json"
  expect_failure "unexpectedly loaded an AMD driver" \
    run_presentation_fixture "$fixture" GENKAN_TEST_OPPOSING_DRIVER="$fixture/libvulkan_radeon-fixture.so"
  assert_fixture_process_stopped "$fixture"
  run_presentation_fixture "$fixture" > "$tmp_dir/nvidia-presentation.log"
  grep -Fq 'Testing nvidia-card2' "$tmp_dir/nvidia-presentation.log"
  grep -Fxq 'login --username smoke --reduce-motion' "$fixture/state/genkan.args"
  assert_fixture_process_stopped "$fixture"

  : > "$fixture/dri/card-wrong"
  expect_failure "opened a DRM node belonging to another adapter" \
    run_presentation_fixture "$fixture" GENKAN_TEST_CAGE_WRONG_DRI=1
  assert_fixture_process_stopped "$fixture"
  expect_failure "opened a DRM node belonging to another adapter" \
    run_presentation_fixture "$fixture" GENKAN_TEST_GENKAN_WRONG_DRI=1
  assert_fixture_process_stopped "$fixture"

  expect_failure "Cage was not found on Sway output DP-1" \
    run_presentation_fixture "$fixture" GENKAN_TEST_WRONG_OUTPUT=1
  assert_fixture_process_stopped "$fixture"
}

test_ci_watches_all_scripts() {
  [[ $(grep -Fc '"scripts/**"' "$repo_root/.github/workflows/ci.yml") == 2 ]] || \
    fail "CI push and pull_request filters must watch scripts/**"
}

test_dev_preview_does_not_inherit_host_identity() {
  local command
  command=$(env -u PREVIEW make --no-print-directory -n -C "$repo_root" PREVIEW=selected dev)
  [[ $command == *'-- login --windowed --preview "selected"'* ]] ||
    fail "make dev must select the deterministic default fixture"
  [[ $command != *'--username'* ]] ||
    fail "make dev must not inject a host-dependent username"
}

test_preview_evidence_rejects_dead_application() {
  # shellcheck source=preview-evidence-lib.sh
  source "$repo_root/scripts/preview-evidence-lib.sh"
  local case_dir="$tmp_dir/dead-preview"
  mkdir -p "$case_dir"
  printf 'renderer failed\n' > "$case_dir/genkan.log"

  if require_running_process 999999 "$case_dir/genkan.log" >/dev/null 2>&1; then
    fail "preview evidence must reject a dead Genkan process"
  fi
}

test_preview_evidence_rejects_blank_frame() {
  # shellcheck source=preview-evidence-lib.sh
  source "$repo_root/scripts/preview-evidence-lib.sh"
  local fixture="$tmp_dir/blank-frame"
  mkdir -p "$fixture/bin"
  cat > "$fixture/bin/identify" <<'EOF'
#!/usr/bin/env bash
printf '480 600 1'
EOF
  chmod +x "$fixture/bin/identify"

  if PATH="$fixture/bin:$PATH" valid_preview_frame ignored.png 480 600; then
    fail "preview evidence must reject compositor-only frames"
  fi
}

test_preview_evidence_requires_consecutive_frames() {
  # shellcheck source=preview-evidence-lib.sh
  source "$repo_root/scripts/preview-evidence-lib.sh"
  local previous=""

  ! advance_frame_stability previous frame-a
  [[ $previous == frame-a ]]
  ! advance_frame_stability previous ""
  [[ -z $previous ]]
  ! advance_frame_stability previous frame-a
  [[ $previous == frame-a ]]
  advance_frame_stability previous frame-a
}

test_preview_evidence_rejects_baseline_differences() {
  # shellcheck source=preview-evidence-lib.sh
  source "$repo_root/scripts/preview-evidence-lib.sh"
  local fixture="$tmp_dir/preview-baseline"
  mkdir -p "$fixture/bin"
  cat > "$fixture/bin/compare" <<'EOF'
#!/usr/bin/env bash
[[ $* == '-metric PDC -channel RGB -fuzz 5% expected.png actual.png null:' ]] || exit 2
printf '40000 (0.039)' >&2
exit 1
EOF
  chmod +x "$fixture/bin/compare"

  PATH="$fixture/bin:$PATH" check_preview_baseline actual.png expected.png >/dev/null 2>&1 ||
    fail "preview evidence must accept differences within its changed-pixel boundary"

  sed -i 's/(0.039)/(0.041)/' "$fixture/bin/compare"
  if PATH="$fixture/bin:$PATH" check_preview_baseline actual.png expected.png >/dev/null 2>&1; then
    fail "preview evidence must reject pixel differences from its baseline"
  fi
}

test_preview_evidence_rejects_unexpected_connections() {
  # shellcheck source=preview-evidence-lib.sh
  source "$repo_root/scripts/preview-evidence-lib.sh"
  local trace="$tmp_dir/connect.trace"
  local wayland="$tmp_dir/runtime/wayland-preview"
  printf 'connect(3, {sa_family=AF_UNIX, sun_path="%s"}, 110) = 0\n' "$wayland" > "$trace"
  check_preview_connections "$trace" "$wayland"

  printf 'connect(4, {sa_family=AF_UNIX, sun_path="/run/dbus/system_bus_socket"}, 110) = -1 ENOENT\n' >> "$trace"
  if check_preview_connections "$trace" "$wayland" >/dev/null 2>&1; then
    fail "preview evidence must reject every non-Wayland connection attempt"
  fi
}

test_reference_image_manifest_rejects_missing_and_invalid_images() {
  "$repo_root/scripts/check-reference-images.sh" > /dev/null

  local fixture="$tmp_dir/reference-images"
  cp -R "$repo_root/rfd/0001/reference-images" "$fixture"
  rm "$fixture/secret-prompt.png"
  expect_failure "missing reference image: secret-prompt.png" \
    env REFERENCE_IMAGE_DIR="$fixture" "$repo_root/scripts/check-reference-images.sh"

  cp "$repo_root/rfd/0001/reference-images/secret-prompt.png" "$fixture/secret-prompt.png"
  printf 'not a PNG\n' > "$fixture/account-selection.png"
  expect_failure "invalid reference PNG header: account-selection.png" \
    env REFERENCE_IMAGE_DIR="$fixture" "$repo_root/scripts/check-reference-images.sh"
}

make_reference_fixture() {
  local fixture=$1
  rm -rf "$fixture"
  cp -R "$repo_root/rfd/0001/reference-images" "$fixture"
}

reference_fixture_digest() {
  local fixture=$1
  (
    cd "$fixture"
    find . -maxdepth 1 -type f -print0 | sort -z | xargs -0 sha256sum
  )
}

test_reference_image_manifest_rejects_dimensions_and_extra_entries() {
  local fixture="$tmp_dir/reference-manifest"
  make_reference_fixture "$fixture"
  cp "$fixture/visible-prompt.png" "$fixture/unexpected.png"
  expect_failure "unexpected reference image: unexpected.png" \
    env REFERENCE_IMAGE_DIR="$fixture" "$repo_root/scripts/check-reference-images.sh"

  rm "$fixture/unexpected.png"
  printf '\x00\x00\x00\x01' | \
    dd of="$fixture/visible-prompt.png" bs=1 seek=16 conv=notrunc status=none
  expect_failure "unexpected reference dimensions for visible-prompt.png: 1x800" \
    env REFERENCE_IMAGE_DIR="$fixture" "$repo_root/scripts/check-reference-images.sh"

  make_reference_fixture "$fixture"
  printf 'unexpected\n' > "$fixture/notes.txt"
  expect_failure "unexpected reference image: notes.txt" \
    env REFERENCE_IMAGE_DIR="$fixture" "$repo_root/scripts/check-reference-images.sh"
}

test_reference_image_manifest_rejects_symlinks() {
  local fixture="$tmp_dir/reference-symlinks"
  make_reference_fixture "$fixture"
  rm "$fixture/secret-prompt.png"
  ln -s visible-prompt.png "$fixture/secret-prompt.png"
  expect_failure "reference image must be a regular non-symlink: secret-prompt.png" \
    env REFERENCE_IMAGE_DIR="$fixture" "$repo_root/scripts/check-reference-images.sh"

  make_reference_fixture "$fixture"
  ln -s visible-prompt.png "$fixture/unexpected.png"
  expect_failure "unexpected reference image: unexpected.png" \
    env REFERENCE_IMAGE_DIR="$fixture" "$repo_root/scripts/check-reference-images.sh"
}

test_reference_image_manifest_propagates_enumeration_failure() {
  local fixture="$tmp_dir/reference-enumeration"
  local bin_dir="$tmp_dir/reference-find-bin"
  make_reference_fixture "$fixture"
  mkdir -p "$bin_dir"
  cat > "$bin_dir/find" <<'EOF'
#!/usr/bin/env bash
echo "injected enumeration failure" >&2
exit 1
EOF
  chmod +x "$bin_dir/find"

  expect_failure "failed to enumerate reference images" \
    env PATH="$bin_dir:$PATH" REFERENCE_IMAGE_DIR="$fixture" \
      "$repo_root/scripts/check-reference-images.sh"
}

make_reference_nix_stub() {
  local bin_dir=$1
  mkdir -p "$bin_dir"
  cat > "$bin_dir/nix" <<'EOF'
#!/usr/bin/env bash
if [[ $1 == eval ]]; then
  printf 'x86_64-linux'
elif [[ $1 == build ]]; then
  if [[ -n ${NIX_BUILD_ARGS:-} ]]; then
    printf '%s\n' "$*" > "$NIX_BUILD_ARGS"
  fi
  printf '%s\n' "$REFERENCE_SOURCE"
else
  exit 2
fi
EOF
  chmod +x "$bin_dir/nix"
}

test_reference_image_refresh_uses_capture_only_derivation() {
  local source="$tmp_dir/capture-only-source"
  local destination="$tmp_dir/capture-only-destination"
  local bin_dir="$tmp_dir/capture-only-bin"
  local build_args="$tmp_dir/capture-only-build-args"
  make_reference_fixture "$source"
  make_reference_fixture "$destination"
  make_reference_nix_stub "$bin_dir"

  env PATH="$bin_dir:$PATH" REFERENCE_SOURCE="$source" \
    REFERENCE_IMAGE_DIR="$destination" NIX_BUILD_ARGS="$build_args" \
    "$repo_root/scripts/update-reference-images.sh" > /dev/null

  grep -Fxq \
    'build .#packages.x86_64-linux.preview-evidence-capture --no-link --print-out-paths' \
    "$build_args" || fail "reference refresh must build the capture-only derivation"
}

test_reference_image_refresh_is_failure_safe_and_removes_stale_files() {
  local source="$tmp_dir/reference-source"
  local destination="$tmp_dir/reference-destination"
  local bin_dir="$tmp_dir/reference-bin"
  local failing_bin_dir="$tmp_dir/reference-failing-bin"
  make_reference_fixture "$source"
  make_reference_fixture "$destination"
  make_reference_nix_stub "$bin_dir"

  local original_digest
  original_digest=$(reference_fixture_digest "$destination")
  printf 'not a PNG\n' > "$source/account-selection.png"
  expect_failure "invalid reference PNG header: account-selection.png" \
    env PATH="$bin_dir:$PATH" REFERENCE_SOURCE="$source" \
      REFERENCE_IMAGE_DIR="$destination" "$repo_root/scripts/update-reference-images.sh"
  [[ $(reference_fixture_digest "$destination") == "$original_digest" ]] ||
    fail "failed refresh must preserve the original reference directory"

  make_reference_fixture "$source"
  make_reference_nix_stub "$failing_bin_dir"
  cat > "$failing_bin_dir/install" <<'EOF'
#!/usr/bin/env bash
echo "injected install failure" >&2
exit 1
EOF
  chmod +x "$failing_bin_dir/install"
  expect_failure "injected install failure" \
    env PATH="$failing_bin_dir:$PATH" REFERENCE_SOURCE="$source" \
      REFERENCE_IMAGE_DIR="$destination" "$repo_root/scripts/update-reference-images.sh"
  [[ $(reference_fixture_digest "$destination") == "$original_digest" ]] ||
    fail "failed install must preserve the original reference directory"

  local original_image_digest replacement_digest
  original_image_digest=$(sha256sum "$destination/account-selection.png" | cut -d ' ' -f 1)
  cp "$source/visible-prompt.png" "$source/account-selection.png"
  replacement_digest=$(sha256sum "$source/account-selection.png" | cut -d ' ' -f 1)
  [[ $replacement_digest != "$original_image_digest" ]] ||
    fail "source replacement fixture must differ from the destination"
  cp "$destination/visible-prompt.png" "$destination/stale.png"
  env PATH="$bin_dir:$PATH" REFERENCE_SOURCE="$source" \
    REFERENCE_IMAGE_DIR="$destination" "$repo_root/scripts/update-reference-images.sh" > /dev/null
  env REFERENCE_IMAGE_DIR="$destination" "$repo_root/scripts/check-reference-images.sh" > /dev/null
  [[ ! -e $destination/stale.png ]] ||
    fail "successful refresh must remove stale reference images"
  [[ $(stat -c '%a' "$destination") == 755 ]] ||
    fail "successful refresh must leave the reference directory traversable"
  [[ $(sha256sum "$destination/account-selection.png" | cut -d ' ' -f 1) == "$replacement_digest" ]] ||
    fail "successful refresh must install the corresponding source bytes"
}

test_reference_image_refresh_rolls_back_replacement_failures() {
  local source="$tmp_dir/replacement-source"
  local destination="$tmp_dir/replacement-destination"
  local bin_dir="$tmp_dir/replacement-bin"
  local real_mv
  real_mv=$(command -v mv)
  make_reference_fixture "$source"
  make_reference_fixture "$destination"
  make_reference_nix_stub "$bin_dir"

  cat > "$bin_dir/mv" <<EOF
#!/usr/bin/env bash
count=0
[[ ! -f \$MV_COUNTER ]] || read -r count < "\$MV_COUNTER"
count=\$((count + 1))
printf '%s\n' "\$count" > "\$MV_COUNTER"
if [[ \$count -eq 2 ]]; then
  echo "injected replacement failure" >&2
  exit 1
fi
exec "$real_mv" "\$@"
EOF
  chmod +x "$bin_dir/mv"

  local original_digest
  original_digest=$(reference_fixture_digest "$destination")
  expect_failure "injected replacement failure" \
    env PATH="$bin_dir:$PATH" REFERENCE_SOURCE="$source" \
      REFERENCE_IMAGE_DIR="$destination" MV_COUNTER="$tmp_dir/mv-counter" \
      "$repo_root/scripts/update-reference-images.sh"
  [[ $(reference_fixture_digest "$destination") == "$original_digest" ]] ||
    fail "failed replacement must restore the original reference directory"
}

test_reference_image_refresh_survives_backup_preparation_failure() {
  local source="$tmp_dir/preparation-source"
  local transaction_parent="$tmp_dir/preparation-transaction"
  local destination="$transaction_parent/reference-images"
  local bin_dir="$tmp_dir/preparation-bin"
  mkdir -p "$transaction_parent"
  make_reference_fixture "$source"
  make_reference_fixture "$destination"
  make_reference_nix_stub "$bin_dir"
  cat > "$bin_dir/mv" <<EOF
#!/usr/bin/env bash
echo "injected backup move failure" >&2
exit 1
EOF
  chmod +x "$bin_dir/mv"

  local original_digest
  original_digest=$(reference_fixture_digest "$destination")
  expect_failure "injected backup move failure" \
    env PATH="$bin_dir:$PATH" REFERENCE_SOURCE="$source" \
      REFERENCE_IMAGE_DIR="$destination" "$repo_root/scripts/update-reference-images.sh"
  [[ $(reference_fixture_digest "$destination") == "$original_digest" ]] ||
    fail "backup preparation failure must preserve the original reference directory"
  [[ -z $(find "$transaction_parent" -maxdepth 1 -name '.reference-images.*' -print -quit) ]] ||
    fail "backup preparation failure must remove transaction artifacts"
}

test_reference_image_refresh_rolls_back_interruption() {
  local source="$tmp_dir/interruption-source"
  local bin_dir="$tmp_dir/interruption-bin"
  local real_mv
  real_mv=$(command -v mv)
  make_reference_fixture "$source"
  make_reference_nix_stub "$bin_dir"
  cat > "$bin_dir/mv" <<EOF
#!/usr/bin/env bash
count=0
[[ ! -f \$MV_COUNTER ]] || read -r count < "\$MV_COUNTER"
count=\$((count + 1))
printf '%s\n' "\$count" > "\$MV_COUNTER"
if [[ \$count -eq 1 ]]; then
  "$real_mv" "\$@"
  echo "injected \$INTERRUPT_SIGNAL transaction interruption" >&2
  kill "-\$INTERRUPT_SIGNAL" "\$PPID"
  exit 0
fi
exec "$real_mv" "\$@"
EOF
  chmod +x "$bin_dir/mv"

  local signal expected_status transaction_parent destination original_digest
  for signal in HUP INT TERM; do
    case $signal in
      HUP) expected_status=129 ;;
      INT) expected_status=130 ;;
      TERM) expected_status=143 ;;
    esac
    transaction_parent="$tmp_dir/interruption-${signal,,}"
    destination="$transaction_parent/reference-images"
    mkdir -p "$transaction_parent"
    make_reference_fixture "$destination"
    original_digest=$(reference_fixture_digest "$destination")
    expect_failure_status "$expected_status" "injected $signal transaction interruption" \
      env PATH="$bin_dir:$PATH" REFERENCE_SOURCE="$source" \
        REFERENCE_IMAGE_DIR="$destination" MV_COUNTER="$tmp_dir/interruption-$signal-counter" \
        INTERRUPT_SIGNAL="$signal" "$repo_root/scripts/update-reference-images.sh"
    [[ $(reference_fixture_digest "$destination") == "$original_digest" ]] ||
      fail "$signal interruption must restore the original reference directory"
    [[ -z $(find "$transaction_parent" -maxdepth 1 -name '.reference-images.*' -print -quit) ]] ||
      fail "$signal interruption must remove transaction artifacts"
  done
}

test_reference_image_refresh_preserves_failed_rollback() {
  local source="$tmp_dir/rollback-source"
  local transaction_parent="$tmp_dir/rollback-transaction"
  local destination="$transaction_parent/reference-images"
  local bin_dir="$tmp_dir/rollback-bin"
  local real_mv backup
  real_mv=$(command -v mv)
  mkdir -p "$transaction_parent"
  make_reference_fixture "$source"
  make_reference_fixture "$destination"
  make_reference_nix_stub "$bin_dir"
  cat > "$bin_dir/mv" <<EOF
#!/usr/bin/env bash
count=0
[[ ! -f \$MV_COUNTER ]] || read -r count < "\$MV_COUNTER"
count=\$((count + 1))
printf '%s\n' "\$count" > "\$MV_COUNTER"
if [[ \$count -eq 2 ]]; then
  echo "injected replacement failure" >&2
  exit 1
fi
if [[ \$count -eq 3 ]]; then
  echo "injected rollback failure" >&2
  exit 1
fi
exec "$real_mv" "\$@"
EOF
  chmod +x "$bin_dir/mv"

  local original_digest
  original_digest=$(reference_fixture_digest "$destination")
  expect_failure "failed to restore reference images" \
    env PATH="$bin_dir:$PATH" REFERENCE_SOURCE="$source" \
      REFERENCE_IMAGE_DIR="$destination" MV_COUNTER="$tmp_dir/rollback-counter" \
      "$repo_root/scripts/update-reference-images.sh"
  backup=$(find "$transaction_parent" -maxdepth 2 -type d -name original -print -quit)
  [[ -n $backup ]] || fail "failed rollback must preserve the original backup"
  [[ $(reference_fixture_digest "$backup") == "$original_digest" ]] ||
    fail "failed rollback must preserve the original backup byte-for-byte"
}

test_reference_image_refresh_retries_committed_cleanup() {
  local source="$tmp_dir/cleanup-source"
  local transaction_parent="$tmp_dir/cleanup-transaction"
  local destination="$transaction_parent/reference-images"
  local bin_dir="$tmp_dir/cleanup-bin"
  local real_rm
  real_rm=$(command -v rm)
  mkdir -p "$transaction_parent"
  make_reference_fixture "$source"
  make_reference_fixture "$destination"
  make_reference_nix_stub "$bin_dir"
  cat > "$bin_dir/rm" <<EOF
#!/usr/bin/env bash
for argument in "\$@"; do
  if [[ \$argument == *'.reference-images.backup.'* && ! -f \$RM_FAILED ]]; then
    : > "\$RM_FAILED"
    echo "injected committed cleanup failure" >&2
    exit 1
  fi
done
exec "$real_rm" "\$@"
EOF
  chmod +x "$bin_dir/rm"

  expect_failure "injected committed cleanup failure" \
    env PATH="$bin_dir:$PATH" REFERENCE_SOURCE="$source" \
      REFERENCE_IMAGE_DIR="$destination" RM_FAILED="$tmp_dir/rm-failed" \
      "$repo_root/scripts/update-reference-images.sh"
  env REFERENCE_IMAGE_DIR="$destination" "$repo_root/scripts/check-reference-images.sh" > /dev/null
  [[ -z $(find "$transaction_parent" -maxdepth 1 -name '.reference-images.*' -print -quit) ]] ||
    fail "committed cleanup retry must remove transaction artifacts"
}

test_reference_image_refresh_rejects_concurrent_update() {
  local source="$tmp_dir/concurrent-source"
  local destination="$tmp_dir/concurrent-destination"
  local bin_dir="$tmp_dir/concurrent-bin"
  local lock_runner="$tmp_dir/hold-reference-lock"
  make_reference_fixture "$source"
  make_reference_fixture "$destination"
  make_reference_nix_stub "$bin_dir"
  cat > "$lock_runner" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
parent=$1
shift
exec 8< "$parent"
flock -n 8
exec "$@"
EOF
  chmod +x "$lock_runner"

  expect_failure "another reference image refresh is in progress" \
    "$lock_runner" "$tmp_dir" env PATH="$bin_dir:$PATH" REFERENCE_SOURCE="$source" \
      REFERENCE_IMAGE_DIR="$destination" "$repo_root/scripts/update-reference-images.sh"
}

test_conventional_commit_description
test_vendor_level_hardware_coverage
test_vulkan_discovery_timeout
test_sway_query_failure_is_not_masked
test_external_output_must_be_active
test_representative_selection_placement_and_cleanup
test_ci_watches_all_scripts
test_dev_preview_does_not_inherit_host_identity
test_preview_evidence_rejects_dead_application
test_preview_evidence_rejects_blank_frame
test_preview_evidence_requires_consecutive_frames
test_preview_evidence_rejects_baseline_differences
test_preview_evidence_rejects_unexpected_connections
test_reference_image_manifest_rejects_missing_and_invalid_images
test_reference_image_manifest_rejects_dimensions_and_extra_entries
test_reference_image_manifest_rejects_symlinks
test_reference_image_manifest_propagates_enumeration_failure
test_reference_image_refresh_uses_capture_only_derivation
test_reference_image_refresh_is_failure_safe_and_removes_stale_files
test_reference_image_refresh_rolls_back_replacement_failures
test_reference_image_refresh_survives_backup_preparation_failure
test_reference_image_refresh_rolls_back_interruption
test_reference_image_refresh_preserves_failed_rollback
test_reference_image_refresh_retries_committed_cleanup
test_reference_image_refresh_rejects_concurrent_update

echo "Shell regression tests passed"
