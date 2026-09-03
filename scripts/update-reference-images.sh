#!/usr/bin/env bash

set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
# shellcheck source=reference-images-manifest.sh
source "$repo_root/scripts/reference-images-manifest.sh"

destination=${REFERENCE_IMAGE_DIR:-$repo_root/rfd/0001/reference-images}
destination_parent=$(dirname "$destination")
mkdir -p "$destination_parent"
exec {lock_fd}< "$destination_parent"
if ! flock -n "$lock_fd"; then
  echo "another reference image refresh is in progress" >&2
  exit 1
fi

stage=""
backup_root=""
backup=""
committed=false

cleanup() {
  local status=$?
  trap - EXIT HUP INT TERM

  if [[ $committed != true && -n $backup && ( -e $backup || -L $backup ) ]]; then
    if ! rm -rf -- "$destination"; then
      echo "failed to remove incomplete reference images; original remains at $backup" >&2
      status=1
    elif ! mv -T -- "$backup" "$destination"; then
      echo "failed to restore reference images from $backup" >&2
      status=1
    else
      backup=""
    fi
  fi
  if [[ -n $stage ]] && ! rm -rf -- "$stage"; then
    status=1
  fi
  if [[ -n $backup_root && ( $committed == true || ( ! -e $backup && ! -L $backup ) ) ]] &&
    ! rm -rf -- "$backup_root"; then
    status=1
  fi
  return "$status"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

system=$(nix eval --raw --impure --expr builtins.currentSystem)
output=$(
  cd "$repo_root"
  nix build ".#packages.$system.preview-evidence-capture" --no-link --print-out-paths
)

for entry in "${REFERENCE_IMAGE_MANIFEST[@]}"; do
  read -r image _ <<< "$entry"
  [[ -f $output/$image ]] || {
    echo "preview evidence is missing $image" >&2
    exit 1
  }
done

stage=$(mktemp -d "$destination_parent/.reference-images.stage.XXXXXX")
for entry in "${REFERENCE_IMAGE_MANIFEST[@]}"; do
  read -r image _ <<< "$entry"
  install -m 0644 "$output/$image" "$stage/$image"
done

REFERENCE_IMAGE_DIR="$stage" "$repo_root/scripts/check-reference-images.sh"
chmod 0755 "$stage"

if [[ -e $destination || -L $destination ]]; then
  backup_root=$(mktemp -d "$destination_parent/.reference-images.backup.XXXXXX")
  backup="$backup_root/original"
  mv -T -- "$destination" "$backup"
fi

mv -T -- "$stage" "$destination"
stage=""
committed=true
[[ -z $backup_root ]] || rm -rf -- "$backup_root"
backup_root=""
backup=""

echo "Updated durable RFD 0001 reference images from $output"
