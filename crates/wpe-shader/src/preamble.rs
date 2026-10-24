//! Source prepended to every Wallpaper Engine shader stage.
//!
//! WE shaders are GLSL 1.20-era sources sprinkled with HLSL vocabulary
//! (`mul`, `frac`, `saturate`, `float4`, scalar broadcasting in `pow`/`max`/...).
//! These macros and overloads make them valid GLSL 3.30.

/// Replaced by varyings a neighbouring stage declares but this one lacks.
pub const PLACEHOLDER: &str = "__SHADER_PLACEHOLD__";

pub const COMMON: &str = r#"#version 330
#extension GL_EXT_spec_constant_composites : enable
#define GLSL 1
#define HLSL 0
#define highp

#define CAST2(x) (vec2(x))
#define CAST3(x) (vec3(x))
#define CAST4(x) (vec4(x))
#define CAST3X3(x) (mat3(x))

#define texSample2D texture
#define texSample2DLod textureLod
#define mul(x, y) ((y) * (x))
#define frac fract
#define atan2 atan
#define fmod(x, y) (x-y*trunc(x/y))
#define ddx dFdx
#define ddy(x) dFdy(-(x))
#define saturate(x) (clamp(x, 0.0, 1.0))
#define log10(x) (log(x) / log(10.0))

float _wep(float x, float y) { return pow(abs(x), y); }
vec2 _wep(vec2 x, float y) { return pow(abs(x), vec2(y)); }
vec3 _wep(vec3 x, float y) { return pow(abs(x), vec3(y)); }
vec4 _wep(vec4 x, float y) { return pow(abs(x), vec4(y)); }
vec2 _wep(float x, vec2 y) { return pow(vec2(abs(x)), y); }
vec3 _wep(float x, vec3 y) { return pow(vec3(abs(x)), y); }
vec4 _wep(float x, vec4 y) { return pow(vec4(abs(x)), y); }
vec2 _wep(vec2 x, vec2 y) { return pow(abs(x), y); }
vec3 _wep(vec3 x, vec3 y) { return pow(abs(x), y); }
vec4 _wep(vec4 x, vec4 y) { return pow(abs(x), y); }
#define pow _wep
vec2 _wemx(float x, vec2 y) { return max(vec2(x), y); }
vec3 _wemx(float x, vec3 y) { return max(vec3(x), y); }
vec4 _wemx(float x, vec4 y) { return max(vec4(x), y); }
vec2 _wemx(vec2 x, float y) { return max(x, vec2(y)); }
vec3 _wemx(vec3 x, float y) { return max(x, vec3(y)); }
vec4 _wemx(vec4 x, float y) { return max(x, vec4(y)); }
float _wemx(float x, float y) { return max(x, y); }
vec2 _wemx(vec2 x, vec2 y) { return max(x, y); }
vec3 _wemx(vec3 x, vec3 y) { return max(x, y); }
vec4 _wemx(vec4 x, vec4 y) { return max(x, y); }
#define max _wemx
vec2 _wemn(float x, vec2 y) { return min(vec2(x), y); }
vec3 _wemn(float x, vec3 y) { return min(vec3(x), y); }
vec4 _wemn(float x, vec4 y) { return min(vec4(x), y); }
vec2 _wemn(vec2 x, float y) { return min(x, vec2(y)); }
vec3 _wemn(vec3 x, float y) { return min(x, vec3(y)); }
vec4 _wemn(vec4 x, float y) { return min(x, vec4(y)); }
float _wemn(float x, float y) { return min(x, y); }
vec2 _wemn(vec2 x, vec2 y) { return min(x, y); }
vec3 _wemn(vec3 x, vec3 y) { return min(x, y); }
vec4 _wemn(vec4 x, vec4 y) { return min(x, y); }
#define min _wemn
float _wedot(vec4 x, vec3 y) { return dot(x.xyz, y); }
float _wedot(vec3 x, vec4 y) { return dot(x, y.xyz); }
float _wedot(vec4 x, vec2 y) { return dot(x.xy, y); }
float _wedot(vec2 x, vec4 y) { return dot(x, y.xy); }
float _wedot(vec3 x, vec2 y) { return dot(x.xy, y); }
float _wedot(vec2 x, vec3 y) { return dot(x, y.xy); }
float _wedot(vec2 x, vec2 y) { return dot(x, y); }
float _wedot(vec3 x, vec3 y) { return dot(x, y); }
float _wedot(vec4 x, vec4 y) { return dot(x, y); }
#define dot _wedot
float _westep(float e, float x) { return step(e, x); }
vec2  _westep(float e, vec2  x) { return step(e, x); }
vec3  _westep(float e, vec3  x) { return step(e, x); }
vec4  _westep(float e, vec4  x) { return step(e, x); }
vec2  _westep(vec2  e, vec2  x) { return step(e, x); }
vec3  _westep(vec3  e, vec3  x) { return step(e, x); }
vec4  _westep(vec4  e, vec4  x) { return step(e, x); }
vec2  _westep(vec2  e, float x) { return step(e, vec2(x)); }
vec3  _westep(vec3  e, float x) { return step(e, vec3(x)); }
vec4  _westep(vec4  e, float x) { return step(e, vec4(x)); }
#define step _westep

#define sampler2DBackBuffer sampler2D
#define sampler2DComparison sampler2D
#define texSample2DBackBuffer(s, uv, res) texture((s), (uv))
#define texLoad2D(t, uv, res) texelFetch((t), ivec2((uv) * (res).xy), 0)
#define texSample2DCompare(s, uv, ref) vec4(1.0)

#define float1 float
#define float2 vec2
#define float3 vec3
#define float4 vec4
#define lerp mix

__SHADER_PLACEHOLD__

"#;

pub const VERTEX: &str = "
#define attribute in
#define varying out

";

pub const FRAGMENT: &str = "
#define varying in
#define gl_FragColor glOutColor
out vec4 glOutColor;

";

pub const GEOMETRY: &str = "\n";

/// Injected for `#require LightingV1`; the definition of
/// `ComputePBRLightShadow` comes from `common_pbr_2.h`.
pub const LIGHTING_V1: &str = "
uniform vec4 g_LightsPosition[4];
uniform vec4 g_LightsColorRadius[4];

vec3 ComputePBRLightShadow(vec3 N, vec3 L, vec3 V, vec3 albedo, vec3 lightColor,
    float radius, float exponent, vec3 specularTint, vec3 baseReflectance,
    float roughness, float metallic, float shadowFactor);

vec3 PerformLighting_V1(vec3 worldPos, vec3 albedo, vec3 normal,
    vec3 viewVector, vec3 specularTint, vec3 f0, float roughness, float metallic)
{
    vec3 light = vec3(0.0);
    for (int i = 0; i < 4; ++i) {
        vec3 lightDelta = g_LightsPosition[i].xyz - worldPos;
        float radius = g_LightsColorRadius[i].w;
        if (radius <= 0.0) continue;
        float exp_ = max(g_LightsPosition[i].w, 1e-3);
        light += ComputePBRLightShadow(normal, lightDelta, viewVector,
            albedo, g_LightsColorRadius[i].rgb, radius, exp_,
            specularTint, f0, roughness, metallic, 1.0);
    }
    return light;
}
";
