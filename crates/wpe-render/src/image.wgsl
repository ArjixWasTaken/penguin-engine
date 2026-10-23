// Stand-in for Wallpaper Engine's genericimage2 until real shaders are compiled.

struct Uniforms {
    mvp: mat4x4<f32>,
    color: vec4<f32>,
    // xy: card size in pixels, zw: UV scale onto the picture inside a padded texture.
    size_uv: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var tex: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// Triangle strip: left-top, left-bottom, right-top, right-bottom.
@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    let corner = vec2<f32>(f32(vi / 2u), f32(vi % 2u));
    let local = vec2<f32>(corner.x - 0.5, 0.5 - corner.y) * u.size_uv.xy;
    var out: VsOut;
    out.pos = u.mvp * vec4<f32>(local, 0.0, 1.0);
    out.uv = corner * u.size_uv.zw;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return textureSample(tex, samp, in.uv) * u.color;
}
