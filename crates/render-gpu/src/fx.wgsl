// Effects on the GPU (see effects.rs): separable Gaussian blurs and square spreads over a layer
// drawn smaller. Every pass reads `src` with textureLoad (past its edges reads as transparent)
// and writes one pixel of its output (a binding of its own per format) per invocation.

struct Params {
    // The blur's standard deviation, in this texture's pixels.
    sigma: f32,
    // Taps each side (blur), or the spread's radius (morph).
    reach: i32,
    // 1: grow (morph), 0: shrink.
    grow: i32,
    // 1: an inner shadow, cast by what lies outside the coverage (coverage inverted, and past the
    // texture's edges everything is outside).
    outside: i32,
    // The shadow's colour, straight alpha.
    color: vec4<f32>,
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(2) var<uniform> p: Params;

fn at(c: vec2<i32>) -> vec4<f32> {
    let size = vec2<i32>(textureDimensions(src));
    if c.x < 0 || c.y < 0 || c.x >= size.x || c.y >= size.y {
        return vec4<f32>(f32(p.outside));
    }
    return textureLoad(src, c, 0);
}

fn weight(i: i32) -> f32 {
    let x = f32(i);
    return exp(-x * x / (2.0 * p.sigma * p.sigma));
}

fn inside(c: vec2<i32>, size: vec2<u32>) -> bool {
    return c.x < i32(size.x) && c.y < i32(size.y);
}

// Coverage: vello writes straight alpha; its alpha is what casts the shadow.
@group(0) @binding(1) var dst_r: texture_storage_2d<r32float, write>;

@compute @workgroup_size(8, 8)
fn extract(@builtin(global_invocation_id) id: vec3<u32>) {
    let c = vec2<i32>(id.xy);
    if !inside(c, textureDimensions(dst_r)) { return; }
    let a = at(c).a;
    textureStore(dst_r, c, vec4<f32>(select(a, 1.0 - a, p.outside == 1), 0.0, 0.0, 0.0));
}

fn morph(c: vec2<i32>, step: vec2<i32>) -> f32 {
    var v = select(1.0, 0.0, p.grow == 1);
    for (var i = -p.reach; i <= p.reach; i++) {
        let s = at(c + step * i).r;
        v = select(min(v, s), max(v, s), p.grow == 1);
    }
    return v;
}

@compute @workgroup_size(8, 8)
fn morph_h(@builtin(global_invocation_id) id: vec3<u32>) {
    let c = vec2<i32>(id.xy);
    if !inside(c, textureDimensions(dst_r)) { return; }
    textureStore(dst_r, c, vec4<f32>(morph(c, vec2<i32>(1, 0)), 0.0, 0.0, 0.0));
}

@compute @workgroup_size(8, 8)
fn morph_v(@builtin(global_invocation_id) id: vec3<u32>) {
    let c = vec2<i32>(id.xy);
    if !inside(c, textureDimensions(dst_r)) { return; }
    textureStore(dst_r, c, vec4<f32>(morph(c, vec2<i32>(0, 1)), 0.0, 0.0, 0.0));
}

fn gauss_r(c: vec2<i32>, step: vec2<i32>) -> f32 {
    var sum = 0.0;
    var total = 0.0;
    for (var i = -p.reach; i <= p.reach; i++) {
        let w = weight(i);
        sum += at(c + step * i).r * w;
        total += w;
    }
    return sum / max(total, 1e-6);
}

@compute @workgroup_size(8, 8)
fn alpha_h(@builtin(global_invocation_id) id: vec3<u32>) {
    let c = vec2<i32>(id.xy);
    if !inside(c, textureDimensions(dst_r)) { return; }
    textureStore(dst_r, c, vec4<f32>(gauss_r(c, vec2<i32>(1, 0)), 0.0, 0.0, 0.0));
}

// The shadow: blurred coverage, tinted (straight alpha).
@group(0) @binding(3) var dst_shadow: texture_storage_2d<rgba8unorm, write>;

@compute @workgroup_size(8, 8)
fn alpha_v_tint(@builtin(global_invocation_id) id: vec3<u32>) {
    let c = vec2<i32>(id.xy);
    if !inside(c, textureDimensions(dst_shadow)) { return; }
    let a = clamp(gauss_r(c, vec2<i32>(0, 1)), 0.0, 1.0);
    textureStore(dst_shadow, c, vec4<f32>(p.color.rgb, p.color.a * a));
}

// Colour blurs (layer and background blurs), premultiplied between the passes.
@group(0) @binding(4) var dst_wide: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(8, 8)
fn rgba_h(@builtin(global_invocation_id) id: vec3<u32>) {
    let c = vec2<i32>(id.xy);
    if !inside(c, textureDimensions(dst_wide)) { return; }
    var sum = vec4<f32>(0.0);
    var total = 0.0;
    for (var i = -p.reach; i <= p.reach; i++) {
        let w = weight(i);
        let s = at(c + vec2<i32>(i, 0));
        sum += vec4<f32>(s.rgb * s.a, s.a) * w;
        total += w;
    }
    textureStore(dst_wide, c, sum / max(total, 1e-6));
}

@group(0) @binding(5) var dst_rgba: texture_storage_2d<rgba8unorm, write>;

@compute @workgroup_size(8, 8)
fn rgba_v(@builtin(global_invocation_id) id: vec3<u32>) {
    let c = vec2<i32>(id.xy);
    if !inside(c, textureDimensions(dst_rgba)) { return; }
    var sum = vec4<f32>(0.0);
    var total = 0.0;
    for (var i = -p.reach; i <= p.reach; i++) {
        let w = weight(i);
        sum += at(c + vec2<i32>(0, i)) * w;
        total += w;
    }
    // Premultiplied out (the image is drawn as such).
    let v = clamp(sum / max(total, 1e-6), vec4<f32>(0.0), vec4<f32>(1.0));
    textureStore(dst_rgba, c, vec4<f32>(min(v.rgb, vec3<f32>(v.a)), v.a));
}

