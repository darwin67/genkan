#!/usr/bin/env bash

set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
# shellcheck source=reference-images-manifest.sh
source "$repo_root/scripts/reference-images-manifest.sh"

system=$(nix eval --raw --impure --expr builtins.currentSystem)
output=$(
  cd "$repo_root"
  nix build ".#checks.$system.preview-evidence" --no-link --print-out-paths
)
destination=${REFERENCE_IMAGE_DIR:-$repo_root/rfd/0001/reference-images}

for entry in "${REFERENCE_IMAGE_MANIFEST[@]}"; do
  read -r image _ <<< "$entry"
  [[ -f $output/$image ]] || {
    echo "preview evidence is missing $image" >&2
    exit 1
  }
done

destination_parent=$(dirname "$destination")
mkdir -p "$destination_parent"
stage=$(mktemp -d "$destination_parent/.reference-images.stage.XXXXXX")
backup=""
committed=false

cleanup() {
  local status=$?
  trap - EXIT INT TERM

  if [[ $committed != true && -n $backup && -e $backup ]]; then
    rm -rf "$destination"
    if ! mv "$backup" "$destination"; then
      echo "failed to restore reference images from $backup" >&2
      return 1
    fi
  fi
  [[ -z $stage ]] || rm -rf "$stage"
  [[ $committed != true || -z $backup ]] || rm -rf "$backup"
  return "$status"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

for entry in "${REFERENCE_IMAGE_MANIFEST[@]}"; do
  read -r image _ <<< "$entry"
  install -m 0644 "$output/$image" "$stage/$image"
done

REFERENCE_IMAGE_DIR="$stage" "$repo_root/scripts/check-reference-images.sh"

if [[ -e $destination || -L $destination ]]; then
  backup=$(mktemp -d "$destination_parent/.reference-images.backup.XXXXXX")
  rmdir "$backup"
  mv "$destination" "$backup"
fi

mv "$stage" "$destination"
stage=""
committed=true
[[ -z $backup ]] || rm -rf "$backup"
backup=""

echo "Updated durable RFD 0001 reference images from $output"
