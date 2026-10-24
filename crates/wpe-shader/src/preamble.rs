//! Source prepended to every Wallpaper Engine shader stage.
//!
//! WE shaders are GLSL 1.20-era sources sprinkled with HLSL vocabulary
//! (`mul`, `frac`, `saturate`, `float4`, scalar broadcasting in `pow`/`max`/...).
//! These macros and overloads make them valid GLSL 3.30.

/// Replaced by varyings a neighbouring stage declares but this one lacks.
pub const PLACEHOLDER: &str = "__SHADER_PLACEHOLD__";

pub const COMMON: &str = r#"#version 330
#extension GL_EXT_spec_constant_composites : enable
#extension GL_ARB_separate_shader_objects : enable
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
// wgpu's framebuffer y points down, as Direct3D's does: no sign flip.
#define ddy dFdy
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
#define sampler2DComparison sampler2DShadow
#define texSample2DBackBuffer(s, uv, res) texelFetch((s), clamp(ivec2((uv) * (res).xy), ivec2(0), ivec2((res).xy) - 1), 0)
#define texLoad2D(t, uv, res) texelFetch((t), ivec2((uv) * (res).xy), 0)
#define texSample2DCompare(s, uv, ref) vec4(texture((s), vec3((uv), (ref))))

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
///
/// Without shadows it lights with the legacy `g_Lights*` arrays. With
/// `LIGHTS_SHADOW_MAPPING` it is Wallpaper Engine's generated version: the
/// per-kind `g_L<Kind>_*` arrays sized by the `LIGHTS_<KIND>` combos, the
/// first `LIGHTS_<KIND>_SHADOW` lights of a kind sampling the shadow atlas
/// through the `g_LFeature_*` arrays. Spot lights and directional cascades
/// share `g_LFeature_ShadowProjection*`: spots first, then three cascades
/// per directional light.
///
/// With `LIGHTS_COOKIE` (on its own too) it is the generated version as
/// well: the spot light with a cookie (the first one when it is shadowed,
/// `LIGHTS_SPOT_SHADOW_COOKIE`, else the one after the shadowed ones,
/// `LIGHTS_SPOT_COOKIE`) is coloured by `COOKIE_SAMPLER` projected through
/// its `g_LFeature_ShadowProjection` instead of the cone's falloff, as the
/// `colorCookie` lines of WE's generated code do. Its projection follows the
/// shadowed spots', ahead of the cascades.
pub const LIGHTING_V1: &str = "
vec3 ComputePBRLightShadow(vec3 N, vec3 L, vec3 V, vec3 albedo, vec3 lightColor,
    float radius, float exponent, vec3 specularTint, vec3 baseReflectance,
    float roughness, float metallic, float shadowFactor);

#if LIGHTS_SHADOW_MAPPING || LIGHTS_COOKIE
#ifndef LIGHTS_POINT
#define LIGHTS_POINT 0
#endif
#ifndef LIGHTS_SPOT
#define LIGHTS_SPOT 0
#endif
#ifndef LIGHTS_TUBE
#define LIGHTS_TUBE 0
#endif
#ifndef LIGHTS_DIRECTIONAL
#define LIGHTS_DIRECTIONAL 0
#endif
#ifndef LIGHTS_POINT_SHADOW
#define LIGHTS_POINT_SHADOW 0
#endif
#ifndef LIGHTS_SPOT_SHADOW
#define LIGHTS_SPOT_SHADOW 0
#endif
#ifndef LIGHTS_DIRECTIONAL_SHADOW
#define LIGHTS_DIRECTIONAL_SHADOW 0
#endif
#ifndef LIGHTS_SPOT_COOKIE
#define LIGHTS_SPOT_COOKIE 0
#endif
#ifndef LIGHTS_SPOT_SHADOW_COOKIE
#define LIGHTS_SPOT_SHADOW_COOKIE 0
#endif

vec3 ComputePBRLightShadowInfinite(vec3 N, vec3 L, vec3 V, vec3 albedo, vec3 lightColor,
    vec3 specularTint, vec3 baseReflectance, float roughness, float metallic, float shadowFactor);
// Defined by common_pbr_2.h, which some shaders include after this
// (genericparticle hoists it ahead).
vec3 PointSegmentDelta(vec3 pos, vec3 segmentA, vec3 segmentB);
vec3 CalculateProjectedCoords(vec3 worldPos, mat4 shadowViewProjection);
#if LIGHTS_SHADOW_MAPPING
float PerformShadowMapping(vec3 projectedCoords, vec4 atlasTransform);
float PerformPointShadowMapping(vec4 shadowMapCoords);
vec4 CalculateProjectedCoordsCascades(vec3 worldPos, mat4 shadowViewProjection);
vec4 CalculateProjectedCoordsPoint(vec3 worldPos, vec3 lightOrigin, vec4 projectionInfo, vec4 atlasTransform);
#endif

#if LIGHTS_POINT
uniform vec4 g_LPoint_Color[LIGHTS_POINT];
uniform vec4 g_LPoint_Origin[LIGHTS_POINT];
#endif
#if LIGHTS_SPOT
uniform vec4 g_LSpot_Color[LIGHTS_SPOT];
uniform vec4 g_LSpot_Origin[LIGHTS_SPOT];
uniform vec4 g_LSpot_Direction[LIGHTS_SPOT];
uniform vec4 g_LSpot_Exponent[LIGHTS_SPOT];
#endif
#if LIGHTS_TUBE
uniform vec4 g_LTube_Color[LIGHTS_TUBE];
uniform vec4 g_LTube_OriginA[LIGHTS_TUBE];
uniform vec4 g_LTube_OriginB[LIGHTS_TUBE];
#endif
#if LIGHTS_DIRECTIONAL
uniform vec4 g_LDirectional_Color[LIGHTS_DIRECTIONAL];
uniform vec4 g_LDirectional_Direction[LIGHTS_DIRECTIONAL];
#endif
#if LIGHTS_POINT_SHADOW
uniform vec4 g_LFeature_ShadowPointProjection[LIGHTS_POINT_SHADOW];
uniform vec4 g_LFeature_ShadowPointProjectionTransform[LIGHTS_POINT_SHADOW];
#endif
#if LIGHTS_SPOT_SHADOW || LIGHTS_SPOT_COOKIE || LIGHTS_DIRECTIONAL_SHADOW
uniform mat4 g_LFeature_ShadowProjection[LIGHTS_SPOT_SHADOW + LIGHTS_SPOT_COOKIE + 3 * LIGHTS_DIRECTIONAL_SHADOW];
uniform vec4 g_LFeature_ShadowProjectionTransform[LIGHTS_SPOT_SHADOW + LIGHTS_SPOT_COOKIE + 3 * LIGHTS_DIRECTIONAL_SHADOW];
#endif

vec3 PerformLighting_V1(vec3 worldPos, vec3 color, vec3 normal, vec3 viewVector,
    vec3 specularTint, vec3 ambient, float roughness, float metallic)
{
    vec3 light = CAST3(0.0);
#if LIGHTS_POINT
    for (int i = 0; i < LIGHTS_POINT; ++i) {
        vec3 lightDelta = g_LPoint_Origin[i].xyz - worldPos;
        float shadowFactor = 1.0;
#if LIGHTS_POINT_SHADOW
        if (i < LIGHTS_POINT_SHADOW) {
            vec4 projectedCoords = CalculateProjectedCoordsPoint(worldPos, g_LPoint_Origin[i].xyz,
                g_LFeature_ShadowPointProjection[i], g_LFeature_ShadowPointProjectionTransform[i]);
            shadowFactor = PerformPointShadowMapping(projectedCoords);
        }
#endif
        light += ComputePBRLightShadow(normal, lightDelta, viewVector, color, g_LPoint_Color[i].rgb,
            g_LPoint_Color[i].w, g_LPoint_Origin[i].w, specularTint, ambient, roughness, metallic,
            shadowFactor);
    }
#endif
#if LIGHTS_SPOT
    for (int i = 0; i < LIGHTS_SPOT; ++i) {
        vec3 lightDelta = g_LSpot_Origin[i].xyz - worldPos;
        float spotCookie = -dot(normalize(lightDelta), g_LSpot_Direction[i].xyz);
        spotCookie = smoothstep(g_LSpot_Direction[i].w, g_LSpot_Origin[i].w, spotCookie);
        vec3 spotColor = g_LSpot_Color[i].rgb * spotCookie;
#if LIGHTS_COOKIE && (LIGHTS_SPOT_COOKIE || LIGHTS_SPOT_SHADOW_COOKIE)
        if (i < LIGHTS_SPOT_SHADOW_COOKIE
            || (i >= LIGHTS_SPOT_SHADOW && i < LIGHTS_SPOT_SHADOW + LIGHTS_SPOT_COOKIE)) {
            vec3 cookieCoords = CalculateProjectedCoords(worldPos, g_LFeature_ShadowProjection[i]);
            vec3 colorCookie = texSample2D(COOKIE_SAMPLER, cookieCoords.xy).rgb;
            spotColor = g_LSpot_Color[i].rgb * colorCookie;
        }
#endif
        float shadowFactor = 1.0;
#if LIGHTS_SPOT_SHADOW
        if (i < LIGHTS_SPOT_SHADOW) {
            vec3 projectedCoords = CalculateProjectedCoords(worldPos, g_LFeature_ShadowProjection[i]);
            shadowFactor = PerformShadowMapping(projectedCoords, g_LFeature_ShadowProjectionTransform[i]);
        }
#endif
        light += ComputePBRLightShadow(normal, lightDelta, viewVector, color,
            spotColor, g_LSpot_Color[i].w, g_LSpot_Exponent[i].x,
            specularTint, ambient, roughness, metallic, shadowFactor);
    }
#endif
#if LIGHTS_TUBE
    for (int i = 0; i < LIGHTS_TUBE; ++i) {
        vec3 lightDelta = PointSegmentDelta(worldPos, g_LTube_OriginA[i].xyz, g_LTube_OriginB[i].xyz);
        light += ComputePBRLightShadow(normal, lightDelta, viewVector, color, g_LTube_Color[i].rgb,
            g_LTube_Color[i].w, g_LTube_OriginA[i].w, specularTint, ambient, roughness, metallic, 1.0);
    }
#endif
#if LIGHTS_DIRECTIONAL
    for (int i = 0; i < LIGHTS_DIRECTIONAL; ++i) {
        float shadowFactor = 1.0;
#if LIGHTS_DIRECTIONAL_SHADOW
        if (i < LIGHTS_DIRECTIONAL_SHADOW) {
            int p1 = LIGHTS_SPOT_SHADOW + LIGHTS_SPOT_COOKIE + 3 * i;
            int p2 = p1 + 1;
            int p3 = p1 + 2;
            vec4 projectedCoords1 = CalculateProjectedCoordsCascades(worldPos, g_LFeature_ShadowProjection[p1]);
            vec4 projectedCoords2 = CalculateProjectedCoordsCascades(worldPos, g_LFeature_ShadowProjection[p2]);
            vec4 projectedCoords3 = CalculateProjectedCoordsCascades(worldPos, g_LFeature_ShadowProjection[p3]);
            vec4 uvTransforms = mix(g_LFeature_ShadowProjectionTransform[p1], g_LFeature_ShadowProjectionTransform[p2], projectedCoords1.w);
            uvTransforms = mix(uvTransforms, g_LFeature_ShadowProjectionTransform[p3], projectedCoords2.w);
            projectedCoords1.xyz = mix(projectedCoords1.xyz, projectedCoords2.xyz, projectedCoords1.w);
            projectedCoords1.xyz = mix(projectedCoords1.xyz, projectedCoords3.xyz, projectedCoords2.w);
            shadowFactor = max(projectedCoords3.w, PerformShadowMapping(projectedCoords1.xyz, uvTransforms));
        }
#endif
        light += ComputePBRLightShadowInfinite(normal, g_LDirectional_Direction[i].xyz, viewVector,
            color, g_LDirectional_Color[i].rgb, specularTint, ambient, roughness, metallic, shadowFactor);
    }
#endif
    return light;
}
#else
uniform vec4 g_LightsPosition[4];
uniform vec4 g_LightsColorRadius[4];

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
#endif
";
