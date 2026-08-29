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
  grep -Fq 'vendor 0x1002 was already tested through its shared ICD' "$tmp_dir/vendor.log"
  grep -Fq '1 Vulkan vendor(s)' "$tmp_dir/vendor.log"
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

test_ci_watches_all_scripts() {
  [[ $(grep -Fc '"scripts/**"' "$repo_root/.github/workflows/ci.yml") == 2 ]] || \
    fail "CI push and pull_request filters must watch scripts/**"
}

test_conventional_commit_description
test_vendor_level_hardware_coverage
test_vulkan_discovery_timeout
test_sway_query_failure_is_not_masked
test_external_output_must_be_active
test_ci_watches_all_scripts

echo "Shell regression tests passed"
