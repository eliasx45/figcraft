// Noise (`figcraft_render::grain`): a grain per cell of the layer's own coordinates, its value
// hashed from the cell and the layer as the CPU does (64-bit arithmetic in pairs of 32-bit words).

struct Grain {
    // Texture pixels → grain cells: x' = a x + c y + e, y' = b x + d y + f.
    m0: vec4<f32>,
    m1: vec4<f32>,
    // The two colours (Mono, Duo), straight alpha.
    c1: vec4<f32>,
    c2: vec4<f32>,
    seed: u32,
    // 0 Mono, 1 Duo, 2 Multi.
    mode: u32,
    density: f32,
    multi_alpha: f32,
}

@group(0) @binding(0) var<uniform> g: Grain;
@group(0) @binding(1) var dst_grain: texture_storage_2d<rgba8unorm, write>;

// The 64-bit product of two 32-bit words, as (low, high).
fn mul32(a: u32, b: u32) -> vec2<u32> {
    let a0 = a & 0xFFFFu;
    let a1 = a >> 16u;
    let b0 = b & 0xFFFFu;
    let b1 = b >> 16u;
    let ll = a0 * b0;
    let lh = a0 * b1;
    let hl = a1 * b0;
    let hh = a1 * b1;
    let mid = (ll >> 16u) + (lh & 0xFFFFu) + (hl & 0xFFFFu);
    let lo = (ll & 0xFFFFu) | (mid << 16u);
    let hi = hh + (lh >> 16u) + (hl >> 16u) + (mid >> 16u);
    return vec2<u32>(lo, hi);
}

// a × b, wrapping (words: low, high).
fn mul64(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> {
    let p = mul32(a.x, b.x);
    return vec2<u32>(p.x, p.y + a.x * b.y + a.y * b.x);
}

// a >> 33.
fn shr33(a: vec2<u32>) -> vec2<u32> {
    return vec2<u32>(a.y >> 1u, 0u);
}

// An i32 widened to 64 bits.
fn wide(v: i32) -> vec2<u32> {
    return vec2<u32>(bitcast<u32>(v), select(0u, 0xFFFFFFFFu, v < 0));
}

fn hash(ix: i32, iy: i32, seed: u32) -> vec2<u32> {
    var h = mul64(wide(ix), vec2<u32>(0x7F4A7C15u, 0x9E3779B9u))
        ^ mul64(wide(iy), vec2<u32>(0x27D4EB4Fu, 0xC2B2AE3Du))
        ^ mul64(vec2<u32>(seed, 0u), vec2<u32>(0x9E3779F9u, 0x165667B1u));
    h = h ^ shr33(h);
    h = mul64(h, vec2<u32>(0xED558CCDu, 0xFF51AFD7u));
    h = h ^ shr33(h);
    h = mul64(h, vec2<u32>(0x1A85EC53u, 0xC4CEB9FEu));
    return h ^ shr33(h);
}

// 16 bits of `h` from bit `at` (0, 16, 32 or 48), as 0..1.
fn unit(h: vec2<u32>, at: u32) -> f32 {
    let w = select(h.x, h.y, at >= 32u);
    return f32((w >> (at % 32u)) & 0xFFFFu) / 65536.0;
}

@compute @workgroup_size(8, 8)
fn noise(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(dst_grain);
    if id.x >= size.x || id.y >= size.y { return; }
    let px = vec2<f32>(id.xy) + vec2<f32>(0.5);
    let cx = i32(floor(g.m0.x * px.x + g.m0.z * px.y + g.m1.x));
    let cy = i32(floor(g.m0.y * px.x + g.m0.w * px.y + g.m1.y));
    let h = hash(cx, cy, g.seed);
    var out = vec4<f32>(0.0);
    if unit(h, 0u) < g.density {
        var c = g.c1;
        if g.mode == 1u && (h.y >> 31u) == 1u {
            c = g.c2;
        } else if g.mode == 2u {
            c = vec4<f32>(unit(h, 16u), unit(h, 32u), unit(h, 48u), g.multi_alpha);
        }
        // Premultiplied out.
        out = vec4<f32>(c.rgb * c.a, c.a);
    }
    textureStore(dst_grain, vec2<i32>(id.xy), out);
}
