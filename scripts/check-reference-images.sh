#!/usr/bin/env bash

set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
image_dir=${REFERENCE_IMAGE_DIR:-$repo_root/rfd/0001/reference-images}

declare -A expected=(
  [account-selection.png]='1280 800'
  [authentication-failure.png]='1280 800'
  [narrow-selected.png]='480 600'
  [power-confirmation.png]='1280 800'
  [secret-prompt.png]='1280 800'
  [visible-prompt.png]='1280 800'
)

for name in "${!expected[@]}"; do
  image="$image_dir/$name"
  [[ -f $image ]] || {
    echo "missing reference image: $name" >&2
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

while IFS= read -r image; do
  name=${image##*/}
  [[ -v expected[$name] ]] || {
    echo "unexpected reference image: $name" >&2
    exit 1
  }
done < <(find "$image_dir" -maxdepth 1 -type f -name '*.png' -print)

echo "Reference image manifest passed"
