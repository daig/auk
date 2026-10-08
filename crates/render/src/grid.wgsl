// The grid pass (spec §18.1): cell backgrounds from a per-cell color texture, then instanced
// quads for glyphs (from the atlases), decorations and the cursor. Everything is in physical
// pixels; colors are sRGB values blended as they are, like the rest of macOS's text.

struct Uniforms {
    viewport: vec2<f32>,
    cell: vec2<f32>,
    origin: vec2<f32>,
    grid: vec2<u32>,
}

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var backgrounds: texture_2d<f32>;
@group(0) @binding(2) var coverage_atlas: texture_2d<f32>;
@group(0) @binding(3) var color_atlas: texture_2d<f32>;

fn to_clip(px: vec2<f32>) -> vec4<f32> {
    let ndc = px / u.viewport * 2.0 - 1.0;
    return vec4<f32>(ndc.x, -ndc.y, 0.0, 1.0);
}

fn corner(vertex: u32) -> vec2<f32> {
    // A triangle strip: (0,0) (1,0) (0,1) (1,1).
    return vec2<f32>(f32(vertex & 1u), f32(vertex >> 1u));
}

// Backgrounds: one quad over the grid; each pixel reads its cell's color.

@vertex
fn bg_vs(@builtin(vertex_index) vertex: u32) -> @builtin(position) vec4<f32> {
    let size = vec2<f32>(u.grid) * u.cell;
    return to_clip(u.origin + corner(vertex) * size);
}

@fragment
fn bg_fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let cell = vec2<i32>(floor((pos.xy - u.origin) / u.cell));
    let clamped = clamp(cell, vec2<i32>(0), vec2<i32>(u.grid) - 1);
    return textureLoad(backgrounds, clamped, 0);
}

// Foreground: one instance per glyph, decoration or cursor part.

const KIND_COVERAGE: u32 = 0u;
const KIND_COLOR: u32 = 1u;
const KIND_RECT: u32 = 2u;
const KIND_CURLY: u32 = 3u;
const KIND_DOTTED: u32 = 4u;
const KIND_DASHED: u32 = 5u;

struct Instance {
    @location(0) pos: vec2<f32>,
    @location(1) size: vec2<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) color: vec4<f32>,
    @location(4) kind: u32,
    @location(5) param: f32,
}

struct Varyings {
    @builtin(position) clip: vec4<f32>,
    // Pixel position inside the quad.
    @location(0) local: vec2<f32>,
    @location(1) @interpolate(flat) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) @interpolate(flat) kind: u32,
    @location(4) @interpolate(flat) size: vec2<f32>,
    @location(5) @interpolate(flat) param: f32,
}

@vertex
fn fg_vs(@builtin(vertex_index) vertex: u32, inst: Instance) -> Varyings {
    let local = corner(vertex) * inst.size;
    var out: Varyings;
    out.clip = to_clip(u.origin + inst.pos + local);
    out.local = local;
    out.uv = inst.uv;
    out.color = inst.color;
    out.kind = inst.kind;
    out.size = inst.size;
    out.param = inst.param;
    return out;
}

@fragment
fn fg_fs(v: Varyings) -> @location(0) vec4<f32> {
    // Premultiplied output.
    let tint = vec4<f32>(v.color.rgb * v.color.a, v.color.a);
    let texel = vec2<i32>(floor(v.uv + v.local));
    // Patterns follow the grid's x, so they continue across cells and runs.
    let gx = v.clip.x - u.origin.x;
    switch v.kind {
        case KIND_COVERAGE: {
            return tint * textureLoad(coverage_atlas, texel, 0).r;
        }
        case KIND_COLOR: {
            // Already premultiplied; the tint's alpha dims it.
            return textureLoad(color_atlas, texel, 0) * v.color.a;
        }
        case KIND_CURLY: {
            // A sine wave across the band, `param` pixels thick, one period per cell width.
            let thickness = v.param;
            let amplitude = (v.size.y - thickness) * 0.5;
            let phase = gx / u.cell.x * 6.2831853;
            let center = v.size.y * 0.5 - amplitude * sin(phase);
            // Distance to the curve, corrected for its slope.
            let slope = amplitude * 6.2831853 / u.cell.x * cos(phase);
            let d = abs(v.local.y - center) / sqrt(1.0 + slope * slope);
            return tint * clamp(thickness * 0.5 + 0.5 - d, 0.0, 1.0);
        }
        case KIND_DOTTED: {
            let on = fract(gx / (2.0 * v.param)) < 0.5;
            return tint * select(0.0, 1.0, on);
        }
        case KIND_DASHED: {
            let on = fract(gx / (u.cell.x * 0.5)) < 0.6;
            return tint * select(0.0, 1.0, on);
        }
        default: {
            return tint;
        }
    }
}
