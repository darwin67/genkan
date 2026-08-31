#!/usr/bin/env bash

set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
system=$(nix eval --raw --impure --expr builtins.currentSystem)
output=$(
  cd "$repo_root"
  nix build ".#checks.$system.preview-evidence" --no-link --print-out-paths
)
destination="$repo_root/rfd/0001/reference-images"
images=(
  account-selection.png
  authentication-failure.png
  narrow-selected.png
  power-confirmation.png
  secret-prompt.png
  visible-prompt.png
)

for image in "${images[@]}"; do
  [[ -f $output/$image ]] || {
    echo "preview evidence is missing $image" >&2
    exit 1
  }
done

mkdir -p "$destination"
for image in "${images[@]}"; do
  install -m 0644 "$output/$image" "$destination/$image"
done

"$repo_root/scripts/check-reference-images.sh"
echo "Updated durable RFD 0001 reference images from $output"
