@group(0) @binding(0) var frame: texture_2d<f32>;
@group(0) @binding(1) var<storage, read> coordinates: array<u32>;

@vertex
fn vertex(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let positions = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    return vec4(positions[index], 0.0, 1.0);
}

@fragment
fn fragment(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let x = coordinates[1u + u32(position.x)];
    let y = coordinates[1u + coordinates[0] + u32(position.y)];
    return vec4(textureLoad(frame, vec2<i32>(i32(x), i32(y)), 0).rgb, 1.0);
}
