// The UI layer's composite: one full-screen triangle that reads the layer texture at each pixel
// (it is the target's size) and premultiplies Vello's straight alpha for blending.

@group(0) @binding(0) var layer: texture_2d<f32>;

@vertex
fn vs(@builtin(vertex_index) vertex: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((vertex << 1u) & 2u), f32(vertex & 2u));
    return vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
}

@fragment
fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let size = vec2<i32>(textureDimensions(layer));
    let p = vec2<i32>(floor(pos.xy));
    if p.x >= size.x || p.y >= size.y {
        return vec4<f32>(0.0);
    }
    let c = textureLoad(layer, p, 0);
    return vec4<f32>(c.rgb * c.a, c.a);
}
