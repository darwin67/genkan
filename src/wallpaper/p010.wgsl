// GStreamer 1.28's default P010 limited BT.709/sRGB -> RGBA conversion.
// Integer expansion, filter rounding and independently rounded matrix offsets
// are intentional. This path truncates to 8 bits before the dither stage, so
// despite videoconvert's Bayer default no dither is applied to these pixels.
struct Layout {
    width: u32,
    height: u32,
    y_stride: u32,
    uv_stride: u32,
}
@group(0) @binding(0) var<storage, read> luma: array<u32>;
@group(0) @binding(1) var<storage, read> chroma: array<u32>;
@group(0) @binding(2) var<storage, read_write> rgba: array<u32>;
@group(0) @binding(3) var<uniform> image: Layout;

fn expand(word: u32) -> u32 {
    return word | (word >> 10u);
}

fn uv(x: u32, y: u32) -> vec2<u32> {
    let word = chroma[y * image.uv_stride + x];
    return vec2<u32>(expand(word & 65535u), expand(word >> 16u));
}

fn horizontal(x: u32, y: u32) -> vec2<u32> {
    let current = uv(x / 2u, y);
    if ((x & 1u) == 0u) { return current; }
    return (current + uv(min(x / 2u + 1u, image.width / 2u - 1u), y) + vec2<u32>(1u)) >> vec2<u32>(1u);
}

@compute @workgroup_size(16, 16)
fn convert(@builtin(global_invocation_id) id: vec3<u32>) {
    let x = id.x;
    let y = id.y;
    if (x >= image.width || y >= image.height) { return; }
    let k = y / 2u;
    let center = horizontal(x, k);
    var neighbor: vec2<u32>;
    if ((y & 1u) == 0u) {
        neighbor = horizontal(x, u32(max(i32(k) - 1, 0)));
    } else {
        neighbor = horizontal(x, min(k + 1u, image.height / 2u - 1u));
    }
    let c = vec2<i32>((3u * center + neighbor + vec2<u32>(2u)) >> vec2<u32>(2u));
    let word = (luma[y * image.y_stride + x / 2u] >> (16u * (x & 1u))) & 65535u;
    let Y = i32(expand(word));
    let r = u32(clamp((298 * Y + 459 * c.y - 16259547) >> 16u, 0, 255));
    let g = u32(clamp((298 * Y - 55 * c.x - 136 * c.y + 5038282) >> 16u, 0, 255));
    let b = u32(clamp((298 * Y + 541 * c.x - 18941056) >> 16u, 0, 255));
    rgba[y * image.width + x] = r | (g << 8u) | (b << 16u) | 0xff000000u;
}
