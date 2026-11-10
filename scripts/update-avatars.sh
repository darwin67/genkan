#!/usr/bin/env bash
# Re-render the built-in account pictures (RFD 7) after changing an SVG
# source, and record both hashes in assets/avatars/manifest.toml.
#
# The PNGs are committed rather than rendered by the package build, so a
# host that builds Genkan with its own nixpkgs (and so another resvg) still
# installs exactly these pixels. This renders with this flake's pinned resvg.

set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
directory=${AVATAR_DIRECTORY:-$repo_root/assets/avatars}
manifest=$directory/manifest.toml
resvg=$(cd "$repo_root" && nix build --inputs-from . nixpkgs#resvg --no-link --print-out-paths)/bin/resvg
size=$(python3 -c 'import sys, tomllib; print(tomllib.load(open(sys.argv[1], "rb"))["png_size"])' "$manifest")

mapfile -t names < <(
  python3 -c 'import sys, tomllib; [print(a["name"]) for a in tomllib.load(open(sys.argv[1], "rb"))["avatar"]]' "$manifest"
)
for name in "${names[@]}"; do
  "$resvg" --width "$size" --height "$size" "$directory/$name.svg" "$directory/$name.png"
done

python3 - "$manifest" "$directory" <<'EOF'
import hashlib
import re
import sys

manifest, directory = sys.argv[1], sys.argv[2]
text = open(manifest, encoding="utf-8").read()


def digest(path):
    with open(path, "rb") as source:
        return hashlib.sha256(source.read()).hexdigest()


def update(match):
    block, name = match.group(0), match.group(1)
    block = re.sub(r'svg_sha256 = "[0-9a-f]*"', f'svg_sha256 = "{digest(f"{directory}/{name}.svg")}"', block)
    return re.sub(r'png_sha256 = "[0-9a-f]*"', f'png_sha256 = "{digest(f"{directory}/{name}.png")}"', block)


text = re.sub(r'\[\[avatar\]\]\nname = "([a-z]+)"\n(?:.*\n)*?png_sha256 = "[0-9a-f]*"', update, text)
open(manifest, "w", encoding="utf-8").write(text)
EOF

"$repo_root/scripts/check-avatars.py" "$directory"
