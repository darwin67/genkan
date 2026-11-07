#!/usr/bin/env bash
set -euo pipefail

if [[ $(rustc --version) != *nightly* ]]; then
  echo 'Fuzzing requires nightly Rust; see docs/development.md.' >&2
  exit 1
fi
cargo fuzz --version

# Seed with the existing tiny HEIC fixtures, not catalog/downloaded wallpapers.
mkdir -p fuzz/corpus/{heic_container,heic_metadata,desktop_entry}
cp tests/fixtures/dynamic-heic/*.heic fuzz/corpus/heic_container/
cp fuzz/seeds/desktop_entry/* fuzz/corpus/desktop_entry/
cp fuzz/seeds/heic_metadata/* fuzz/corpus/heic_metadata/
python3 - <<'PY'
import base64
import pathlib
import plistlib

corpus = pathlib.Path('fuzz/corpus/heic_metadata')
# Dictionary shapes used by the production parser's existing unit tests.
properties = {
    'h24': plistlib.loads(pathlib.Path('fuzz/seeds/heic_metadata/time.plist').read_bytes()),
    'solar': {'si': [{'i': 0, 'a': -20.0, 'z': 0.0}, {'i': 1, 'a': 45.0, 'z': 180.0}]},
    'apr': {'l': 0, 'd': 1},
}
for name, value in properties.items():
    for encoding in (plistlib.FMT_XML, plistlib.FMT_BINARY):
        suffix = 'xml' if encoding == plistlib.FMT_XML else 'binary'
        data = plistlib.dumps(value, fmt=encoding)
        encoded = base64.b64encode(data)
        (corpus / f'{name}-{suffix}.plist').write_bytes(data)
        (corpus / f'{name}-{suffix}.base64').write_bytes(encoded)
        (corpus / f'{name}-{suffix}.xmp').write_bytes(
            b'<x:xmpmeta xmlns:x="x" xmlns:a="http://ns.apple.com/namespace/1.0/"><a:'
            + name.encode() + b'>' + encoded + b'</a:' + name.encode() + b'></x:xmpmeta>')
(corpus / 'many-attributes.xmp').write_bytes(
    b'<r xmlns:a="http://ns.apple.com/namespace/1.0/">'
    + b'<x a:h24="" a:solar="" a:apr=""/>' * 1366 + b'</r>')
PY

# A time limit is per target, not a claim of exhaustive coverage. All generated
# artifacts/corpus additions are ignored; preserve and minimize any real crash.
for target in heic_container heic_metadata desktop_entry; do
  cargo fuzz run "$target" -- \
    -max_total_time="${FUZZ_SECONDS:-10}" -max_len=65536 \
    -rss_limit_mb=512 -malloc_limit_mb=128 -timeout=5
done
