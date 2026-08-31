#!/usr/bin/env bash

set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
image_dir=${REFERENCE_IMAGE_DIR:-$repo_root/rfd/0001/reference-images}

# shellcheck source=reference-images-manifest.sh
source "$repo_root/scripts/reference-images-manifest.sh"

declare -A expected=()
for entry in "${REFERENCE_IMAGE_MANIFEST[@]}"; do
  read -r name width height <<< "$entry"
  expected[$name]="$width $height"
done

for name in "${!expected[@]}"; do
  image="$image_dir/$name"
  if [[ ! -e $image && ! -L $image ]]; then
    echo "missing reference image: $name" >&2
    exit 1
  fi
  [[ -f $image && ! -L $image ]] || {
    echo "reference image must be a regular non-symlink: $name" >&2
    exit 1
  }

  header=$(od -An -v -tx1 -N24 "$image" | tr -d '[:space:]')
  [[ ${#header} -eq 48 && ${header:0:32} == 89504e470d0a1a0a0000000d49484452 ]] || {
    echo "invalid reference PNG header: $name" >&2
    exit 1
  }

  width=$((16#${header:32:8}))
  height=$((16#${header:40:8}))
  read -r expected_width expected_height <<< "${expected[$name]}"
  [[ $width -eq $expected_width && $height -eq $expected_height ]] || {
    echo "unexpected reference dimensions for $name: ${width}x${height}" >&2
    exit 1
  }
done

entries=$(mktemp)
trap 'rm -f "$entries"' EXIT
if ! find "$image_dir" -maxdepth 1 -mindepth 1 -print0 > "$entries"; then
  echo "failed to enumerate reference images" >&2
  exit 1
fi
while IFS= read -r -d '' image; do
  name=${image##*/}
  [[ -v expected[$name] ]] || {
    echo "unexpected reference image: $name" >&2
    exit 1
  }
done < "$entries"
rm -f "$entries"
trap - EXIT

echo "Reference image manifest passed"
