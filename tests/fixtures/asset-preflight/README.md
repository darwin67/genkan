# Remote preflight fixture

`hevc.mov` contains four 96×64 HEVC frames at 2 fps, with no audio. Its test
pattern was generated for Genkan; it contains no third-party wallpaper artwork.
The public CLI tests insert an 8 MiB `free` box immediately before the trailing
`moov` box and another 192 KiB `free` box inside it. Existing media chunk offsets
remain unchanged. This requires real seeking and metadata reads across multiple
64 KiB range pages while keeping the committed fixture small.

Generated with FFmpeg 9.0.1 from Nix:

```sh
nix shell --inputs-from . nixpkgs#ffmpeg --command ffmpeg \
  -f lavfi -i 'testsrc2=size=96x64:rate=2' -frames:v 4 \
  -c:v libx265 -preset ultrafast \
  -x265-params 'pools=1:frame-threads=1:log-level=error' \
  -tag:v hvc1 -an hevc.mov
```

The canonical fixture is 9,103 bytes, SHA-256
`97bafd684fc1f48ecaafe71dcbc220b5ba2b33fc24e7b16f0d448434cf6269f6`.
Byte-for-byte regeneration depends on the encoder and muxer versions; tests
assert media behavior rather than a regenerated hash.

`mixed.mov` has the same HEVC track followed by an unrelated H.264 track. A
61,000-byte reservation places the third HEVC packet across the first 64 KiB
range boundary. The server delays the next range, so track selection completes
before enough reordered frames arrive to decode a representative frame.
It is 76,127 bytes, SHA-256
`62ea671674fddcdd17326587c3878d5819584b60f9d397e80e035a5954a44777`.

```sh
nix shell --inputs-from . nixpkgs#ffmpeg --command ffmpeg \
  -i hevc.mov -map 0:v -map 0:v -c:v:0 copy -c:v:1 libx264 \
  -preset ultrafast -tag:v:0 hvc1 -moov_size 61000 -an mixed.mov
```
