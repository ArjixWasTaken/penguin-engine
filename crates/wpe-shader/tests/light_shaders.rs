//! Wallpaper Engine's shadow-mapped and volumetric light shaders compile,
//! with the bindings the renderer gives them. Needs the stock assets
//! (skipped without them; `WPE_ASSETS` overrides the Steam path).

use std::collections::BTreeMap;

use wpe_shader::reflect::TextureKind;
use wpe_shader::{ShaderRequest, TexSlot, compile};
use wpe_vfs::{PhysicalFs, Vfs};

fn assets() -> Option<Vfs> {
    let dir = std::env::var_os("WPE_ASSETS")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            let home = std::env::var_os("HOME")?;
            Some(
                std::path::Path::new(&home)
                    .join(".local/share/Steam/steamapps/common/wallpaper_engine/assets"),
            )
        })?;
    let fs = PhysicalFs::new(&dir).ok()?;
    let mut vfs = Vfs::new();
    vfs.mount("assets", fs);
    vfs.contains("shaders/genericimage4.frag").then_some(vfs)
}

fn combos(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

fn slots(n: usize) -> Vec<TexSlot> {
    vec![
        TexSlot {
            enabled: true,
            components: [false; 3],
        };
        n
    ]
}

const SHADOWS: &[(&str, &str)] = &[
    ("LIGHTS_SHADOW_MAPPING", "1"),
    ("LIGHTS_SHADOW_MAPPING_QUALITY", "3"),
    ("LIGHTS_POINT", "3"),
    ("LIGHTS_POINT_SHADOW", "2"),
    ("LIGHTS_SPOT", "1"),
    ("LIGHTS_SPOT_SHADOW", "1"),
    ("LIGHTS_TUBE", "1"),
    ("LIGHTS_DIRECTIONAL", "1"),
    ("LIGHTS_DIRECTIONAL_SHADOW", "1"),
];

#[test]
fn lit_shaders_sample_the_shadow_atlas() {
    let Some(vfs) = assets() else {
        eprintln!("no Wallpaper Engine assets; skipped");
        return;
    };
    for (shader, extra) in [
        (
            "genericimage4",
            &[("LIGHTING", "1"), ("REFLECTION", "1")][..],
        ),
        ("generic4", &[("LIGHTING", "1")][..]),
    ] {
        let mut c = combos(SHADOWS);
        c.extend(combos(extra));
        let slots = slots(2);
        let compiled = compile(
            &vfs,
            ShaderRequest {
                name: shader,
                combos: &c,
                slots: &slots,
                formats: &[],
            },
        )
        .unwrap_or_else(|e| panic!("{shader}: {e}"));
        let r = &compiled.reflection;
        assert_eq!(
            r.textures.get("g_Texture6").map(|t| t.kind),
            Some(TextureKind::Comparison),
            "{shader}"
        );
        for name in [
            "g_LFeature_ShadowPointProjection",
            "g_LFeature_ShadowPointProjectionTransform",
            "g_LFeature_ShadowProjection",
            "g_LFeature_ShadowProjectionTransform",
            "g_Texture6Texel",
            "g_LPoint_Origin",
        ] {
            assert!(r.uniforms.contains_key(name), "{shader}: {name}");
        }
        assert_eq!(r.uniforms["g_LFeature_ShadowProjection"].ty.count, 4);
    }
}

#[test]
fn volumetric_passes_compile() {
    let Some(vfs) = assets() else {
        eprintln!("no Wallpaper Engine assets; skipped");
        return;
    };
    for (pointlight, shadow, fullscreen) in [("1", "0", "0"), ("1", "1", "0"), ("0", "0", "1")] {
        let mut c = combos(SHADOWS);
        c.extend(combos(&[
            ("QUALITY", "3"),
            ("POINTLIGHT", pointlight),
            ("SHADOW", shadow),
            ("FULLSCREEN", fullscreen),
            ("FOG", "0"),
        ]));
        let slots = slots(4);
        let compiled = compile(
            &vfs,
            ShaderRequest {
                name: "volumetricsfront",
                combos: &c,
                slots: &slots,
                formats: &[],
            },
        )
        .unwrap_or_else(|e| panic!("volumetricsfront {pointlight}{shadow}{fullscreen}: {e}"));
        let r = &compiled.reflection;
        // Depth buffers are read with texelFetch.
        for t in ["g_Texture1", "g_Texture3"] {
            assert_eq!(
                r.textures.get(t).map(|t| t.kind),
                Some(TextureKind::Unfilterable),
                "{t}"
            );
        }
        if shadow == "1" {
            assert_eq!(
                r.textures.get("g_Texture0").map(|t| t.kind),
                Some(TextureKind::Comparison)
            );
        }
        assert!(r.uniforms.contains_key("g_EffectModelMatrix"));
    }
    let slots = slots(1);
    compile(
        &vfs,
        ShaderRequest {
            name: "volumetricsback",
            combos: &BTreeMap::new(),
            slots: &slots,
            formats: &[],
        },
    )
    .expect("volumetricsback");
}

fn compile_with(vfs: &Vfs, shader: &str, pairs: &[(&str, &str)]) -> wpe_shader::CompiledShader {
    let c = combos(pairs);
    let slots = slots(4);
    compile(
        vfs,
        ShaderRequest {
            name: shader,
            combos: &c,
            slots: &slots,
            formats: &[],
        },
    )
    .unwrap_or_else(|e| panic!("{shader} {pairs:?}: {e}"))
}

#[test]
fn cookie_lights_compile() {
    let Some(vfs) = assets() else {
        eprintln!("no Wallpaper Engine assets; skipped");
        return;
    };
    // A cookie light on its own, and shadowed alongside the other shadows.
    let alone: &[(&str, &str)] = &[
        ("LIGHTS_COOKIE", "1"),
        ("LIGHTS_SHADOW_MAPPING", "0"),
        ("LIGHTS_SPOT", "2"),
        ("LIGHTS_SPOT_COOKIE", "1"),
        ("LIGHTS_SPOT_SHADOW_COOKIE", "0"),
    ];
    let mut shadowed = SHADOWS.to_vec();
    shadowed.extend([
        ("LIGHTS_COOKIE", "1"),
        ("LIGHTS_SPOT_COOKIE", "0"),
        ("LIGHTS_SPOT_SHADOW_COOKIE", "1"),
    ]);
    for (shader, cookie, extra) in [
        ("generic4", "g_Texture7", &[("LIGHTING", "1")][..]),
        ("genericimage4", "g_Texture7", &[("LIGHTING", "1")][..]),
        ("genericparticle", "g_Texture5", &[("LIGHTING", "1")][..]),
    ] {
        for (set, projections) in [(alone, 1), (&shadowed[..], 4)] {
            let mut pairs = set.to_vec();
            pairs.extend_from_slice(extra);
            let r = compile_with(&vfs, shader, &pairs).reflection;
            assert_eq!(
                r.textures.get(cookie).map(|t| t.kind),
                Some(TextureKind::Filterable),
                "{shader}"
            );
            assert_eq!(
                r.uniforms["g_LFeature_ShadowProjection"].ty.count, projections,
                "{shader}"
            );
            assert!(r.uniforms.contains_key("g_LSpot_Color"), "{shader}");
        }
    }
}

#[test]
fn fog_compiles() {
    let Some(vfs) = assets() else {
        eprintln!("no Wallpaper Engine assets; skipped");
        return;
    };
    let fog = [
        ("FOG_DIST", "1"),
        ("FOG_HEIGHT", "1"),
        ("FOG_COMPUTED", "1"),
    ];
    for (shader, extra) in [
        ("generic4", &[("LIGHTING", "1")][..]),
        ("genericimage4", &[][..]),
        ("genericparticle", &[][..]),
        (
            "volumetricsfront",
            &[("QUALITY", "2"), ("POINTLIGHT", "1")][..],
        ),
    ] {
        let mut pairs = fog.to_vec();
        pairs.extend_from_slice(extra);
        let r = compile_with(&vfs, shader, &pairs).reflection;
        for name in [
            "g_FogDistanceColor",
            "g_FogDistanceParams",
            "g_FogHeightColor",
            "g_FogHeightParams",
        ] {
            assert!(r.uniforms.contains_key(name), "{shader}: {name}");
        }
    }
}

#[test]
fn spot_volumetrics_compile() {
    let Some(vfs) = assets() else {
        eprintln!("no Wallpaper Engine assets; skipped");
        return;
    };
    for (shadow, cookie) in [("1", false), ("0", true), ("1", true)] {
        let mut pairs = vec![
            ("QUALITY", "2"),
            ("POINTLIGHT", "0"),
            ("FULLSCREEN", "0"),
            ("SHADOW", shadow),
        ];
        if cookie {
            pairs.push(("COOKIE", "1"));
        }
        let r = compile_with(&vfs, "volumetricsfront", &pairs).reflection;
        assert!(r.uniforms.contains_key("g_AltModelMatrix"));
        assert_eq!(
            r.textures.get("g_Texture0").map(|t| t.kind),
            (shadow == "1").then_some(TextureKind::Comparison)
        );
        assert_eq!(
            r.textures.get("g_Texture2").map(|t| t.kind),
            cookie.then_some(TextureKind::Filterable)
        );
    }
}
