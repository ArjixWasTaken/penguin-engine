use std::borrow::Cow;
use std::collections::HashMap;

use glam::{Mat4, Vec3};
use serde_json::{Value, json};
use wpe_particles::{ParticleMesh, ParticleSystem, RendererKind, SpawnType, UpdateContext};
use wpe_vfs::{FileSystem, Vfs, VfsError};

struct MemFs(HashMap<String, Vec<u8>>);

impl FileSystem for MemFs {
    fn contains(&self, path: &str) -> bool {
        self.0.contains_key(path)
    }
    fn read(&self, path: &str) -> wpe_vfs::Result<Cow<'_, [u8]>> {
        self.0
            .get(path)
            .map(|b| Cow::Borrowed(b.as_slice()))
            .ok_or_else(|| VfsError::NotFound(path.to_owned()))
    }
    fn files(&self) -> Box<dyn Iterator<Item = &str> + '_> {
        Box::new(self.0.keys().map(String::as_str))
    }
}

fn vfs(files: &[(&str, Value)]) -> Vfs {
    let mut map: HashMap<String, Vec<u8>> = files
        .iter()
        .map(|(p, v)| ((*p).to_owned(), v.to_string().into_bytes()))
        .collect();
    map.insert(
        "materials/p.json".into(),
        json!({"passes": [{"shader": "genericparticle", "blending": "additive",
                            "textures": ["particle/halo"], "combos": {"REFRACT": 0}}]})
        .to_string()
        .into_bytes(),
    );
    let mut v = Vfs::new();
    v.mount("mem", MemFs(map));
    v
}

fn system(def: Value, over: Option<Value>) -> ParticleSystem {
    let v = vfs(&[("particles/a.json", def)]);
    ParticleSystem::load(&v, "particles/a.json", over.as_ref()).unwrap()
}

fn base(rate: f32, life: f32, maxcount: u32) -> Value {
    json!({
        "emitter": [{"name": "sphererandom", "rate": rate, "distancemax": 10, "speedmin": 50, "speedmax": 100}],
        "initializer": [{"name": "lifetimerandom", "min": life, "max": life},
                        {"name": "sizerandom", "min": 10, "max": 20},
                        {"name": "colorrandom", "min": "255 0 0", "max": "0 0 255"}],
        "operator": [{"name": "movement", "gravity": "0 -100 0"}, {"name": "alphafade"}],
        "material": "materials/p.json",
        "maxcount": maxcount,
        "starttime": 0
    })
}

fn run(s: &mut ParticleSystem, seconds: f32, fps: f32) {
    let ctx = UpdateContext::default();
    let frames = (seconds * fps).round() as usize;
    for _ in 0..frames {
        s.update(1.0 / fps, &ctx);
    }
}

#[test]
fn emission_rate_matches_definition() {
    let mut s = system(base(10.0, 100.0, 1000), None);
    run(&mut s, 2.0, 60.0);
    let n = s.particle_count();
    assert!((19..=21).contains(&n), "{n} particles");
}

#[test]
fn maxcount_caps_population() {
    let mut s = system(base(1000.0, 100.0, 37), None);
    run(&mut s, 1.0, 30.0);
    assert_eq!(s.particle_count(), 37);
}

#[test]
fn lifetime_limits_population_and_particles_die() {
    let mut def = base(100.0, 0.5, 1000);
    def["emitter"][0]["duration"] = json!(1.0);
    let mut s = system(def, None);
    run(&mut s, 1.0, 60.0);
    let n = s.particle_count();
    assert!((45..=55).contains(&n), "steady state {n}");
    for p in s.root().particles() {
        assert!(p.lifetime > 0.0 && p.lifetime <= 0.5 + 1e-4);
        assert!(p.alpha >= 0.0 && p.alpha <= 1.0);
    }
    run(&mut s, 1.0, 60.0);
    assert_eq!(s.particle_count(), 0);
}

#[test]
fn instantaneous_emits_once() {
    let mut def = base(0.0, 5.0, 1000);
    def["emitter"][0]["instantaneous"] = json!(25);
    let mut s = system(def, None);
    run(&mut s, 1.0, 60.0);
    assert_eq!(s.particle_count(), 25);
}

#[test]
fn delay_postpones_emission() {
    let mut def = base(10.0, 100.0, 1000);
    def["emitter"][0]["delay"] = json!(1.0);
    let mut s = system(def, None);
    run(&mut s, 0.9, 60.0);
    assert_eq!(s.particle_count(), 0);
    run(&mut s, 1.1, 60.0);
    assert!(s.particle_count() >= 9);
}

#[test]
fn starttime_prewarms() {
    let mut def = base(10.0, 100.0, 1000);
    def["starttime"] = json!(3.0);
    let mut s = system(def, None);
    s.update(1.0 / 60.0, &UpdateContext::default());
    assert!(s.particle_count() >= 29);
}

#[test]
fn same_seed_same_frames() {
    let mesh_after = |seed: u64| {
        let mut s = system(base(50.0, 1.0, 500), None);
        s.set_seed(seed);
        run(&mut s, 1.5, 30.0);
        let mut m = ParticleMesh::default();
        s.mesh(&mut m);
        m
    };
    let a = mesh_after(7);
    assert_eq!(a, mesh_after(7));
    assert_ne!(a, mesh_after(8));
}

#[test]
fn sprite_mesh_layout() {
    let mut s = system(base(20.0, 10.0, 100), None);
    run(&mut s, 1.0, 60.0);
    let n = s.particle_count() as u32;
    assert!(n > 0);
    let mut m = ParticleMesh::default();
    s.mesh(&mut m);
    assert_eq!(m.vertex_count, n * 4);
    assert_eq!(m.indices.len() as u32, n * 6);
    let names: Vec<(&str, u8)> = m.attributes.iter().map(|a| (a.0.as_str(), a.1)).collect();
    assert_eq!(
        names,
        [
            ("a_Position", 3),
            ("a_TexCoordVec4", 4),
            ("a_Color", 4),
            ("a_TexCoordC2", 2)
        ]
    );
    for a in &m.attributes {
        assert_eq!(a.2.len() as u32, m.vertex_count * a.1 as u32, "{}", a.0);
    }
    let tc = m.attribute("a_TexCoordVec4").unwrap();
    assert_eq!(&tc[0..2], &[0.0, 1.0]);
    assert_eq!(&tc[8..10], &[1.0, 0.0]);
    let combos = s.combos();
    assert_eq!(combos.get("REFRACT").map(String::as_str), Some("0"));
    assert_eq!(combos.get("GS_ENABLED").map(String::as_str), Some("0"));
    assert!(!combos.contains_key("THICKFORMAT"));
    assert_eq!(s.root().shader(), "genericparticle");
    assert_eq!(s.material(), "materials/p.json");
}

#[test]
fn spritetrail_uses_velocity_layout() {
    let mut def = base(20.0, 10.0, 100);
    def["renderer"] =
        json!([{"name": "spritetrail", "length": 0.02, "maxlength": 5, "minlength": 1}]);
    let mut s = system(def, None);
    run(&mut s, 0.5, 60.0);
    let c = s.combos();
    assert_eq!(c.get("TRAILRENDERER").map(String::as_str), Some("1"));
    assert_eq!(c.get("THICKFORMAT").map(String::as_str), Some("1"));
    assert_eq!(s.uniforms()["g_RenderVar0"], vec![0.02, 5.0, 1.0, 0.0]);
    let mut m = ParticleMesh::default();
    s.mesh(&mut m);
    let c1 = m.attribute("a_TexCoordVec4C1").unwrap();
    assert_eq!(c1.len() as u32, m.vertex_count * 4);
    assert!(c1.chunks(4).any(|v| v[0] != 0.0 || v[1] != 0.0));
}

#[test]
fn sprites_keep_a_custom_material_shader() {
    // A project's own particle shader (say the legacy `particle`, tinting
    // from material colours) draws sprites; ribbons stay on the rope shader.
    let material = json!({"passes": [{"shader": "particle", "blending": "additive",
                                      "textures": ["particle/halo"]}]});
    for (renderer, shader) in [("sprite", "particle"), ("rope", "genericropeparticle")] {
        let mut def = base(20.0, 2.0, 100);
        def["material"] = json!("materials/custom.json");
        def["renderer"] = json!([{"name": renderer}]);
        let v = vfs(&[
            ("particles/a.json", def),
            ("materials/custom.json", material.clone()),
        ]);
        let s = ParticleSystem::load(&v, "particles/a.json", None).unwrap();
        assert_eq!(s.root().shader(), shader, "{renderer}");
    }
}

#[test]
fn rope_and_ropetrail_meshes_are_valid() {
    for (name, kind) in [
        ("rope", RendererKind::Rope),
        ("ropetrail", RendererKind::RopeTrail),
    ] {
        let mut def = base(20.0, 2.0, 100);
        def["renderer"] = json!([{"name": name, "length": 0.5, "segments": 5, "subdivision": 2}]);
        let mut s = system(def, None);
        run(&mut s, 1.0, 60.0);
        assert_eq!(s.root().renderer(), Some(kind));
        assert_eq!(s.root().shader(), "genericropeparticle");
        let mut m = ParticleMesh::default();
        s.mesh(&mut m);
        assert!(m.vertex_count > 0, "{name}");
        assert_eq!(m.indices.len() % 3, 0);
        assert!(m.indices.iter().all(|&i| i < m.vertex_count));
        for a in &m.attributes {
            assert_eq!(a.2.len() as u32, m.vertex_count * a.1 as u32);
            assert!(a.2.iter().all(|f| f.is_finite()), "{name} {}", a.0);
        }
    }
}

#[test]
fn instance_override_scales_count_and_replaces_color() {
    let mut s = system(
        base(1000.0, 100.0, 100),
        Some(json!({"count": 0.5, "colorn": "0 1 0", "size": 2, "alpha": 0.5})),
    );
    run(&mut s, 1.0, 30.0);
    assert_eq!(s.particle_count(), 50);
    for p in s.root().particles() {
        assert_eq!(p.init_color, Vec3::new(0.0, 1.0, 0.0));
        assert!(p.init_size >= 20.0 && p.init_size <= 40.0);
        assert!((p.init_alpha - 0.5).abs() < 1e-6);
    }
}

#[test]
fn rate_override_speeds_up_time() {
    let mut s = system(base(10.0, 100.0, 1000), Some(json!({"rate": 2})));
    run(&mut s, 1.0, 60.0);
    let n = s.particle_count();
    assert!((19..=21).contains(&n), "{n}");
}

#[test]
fn event_children_follow_parent_particles() {
    let parent = json!({
        "emitter": [{"name": "sphererandom", "rate": 0, "instantaneous": 3, "distancemax": 0}],
        "initializer": [{"name": "lifetimerandom", "min": 0.5, "max": 0.5},
                        {"name": "velocityrandom", "min": "100 0 0", "max": "100 0 0"}],
        "operator": [{"name": "movement"}],
        "material": "materials/p.json", "maxcount": 3,
        "children": [{"name": "particles/child.json", "type": "eventfollow", "maxcount": 10},
                     {"name": "particles/death.json", "type": "eventdeath"}]
    });
    let child = json!({
        "emitter": [{"name": "sphererandom", "rate": 20, "distancemax": 0}],
        "initializer": [{"name": "lifetimerandom", "min": 5, "max": 5}],
        "material": "materials/p.json", "maxcount": 100
    });
    let death = json!({
        "emitter": [{"name": "sphererandom", "rate": 0, "instantaneous": 4, "distancemax": 0}],
        "initializer": [{"name": "lifetimerandom", "min": 5, "max": 5}],
        "material": "materials/p.json", "maxcount": 100
    });
    let v = vfs(&[
        ("particles/a.json", parent),
        ("particles/child.json", child),
        ("particles/death.json", death),
    ]);
    let mut s = ParticleSystem::load(&v, "particles/a.json", None).unwrap();
    assert_eq!(s.subsystems().len(), 3);
    assert_eq!(s.subsystems()[1].spawn_type(), SpawnType::EventFollow);
    assert_eq!(s.subsystems()[1].parent(), Some(0));
    run(&mut s, 0.25, 60.0);
    let follow = &s.subsystems()[1];
    assert_eq!(follow.instance_count(), 3);
    assert!(follow.particle_count() >= 3 * 4);
    assert!(follow.particles().any(|p| p.position.x > 5.0));
    assert_eq!(s.subsystems()[2].particle_count(), 0);
    run(&mut s, 0.5, 60.0);
    assert_eq!(s.root().particle_count(), 0);
    assert_eq!(s.subsystems()[2].particle_count(), 3 * 4);
    let frozen = s.subsystems()[1].particle_count();
    run(&mut s, 0.5, 60.0);
    assert_eq!(s.subsystems()[1].particle_count(), frozen);
}

#[test]
fn cursor_drives_link_mouse_emitter() {
    let mut def = base(50.0, 10.0, 100);
    def["emitter"][0]["distancemax"] = json!(0);
    def["emitter"][0]["speedmax"] = json!(0);
    def["emitter"][0]["speedmin"] = json!(0);
    def["operator"] = json!([]);
    def["controlpoint"] = json!([{"id": 0, "flags": 1, "offset": "0 0 0"}]);
    let mut s = system(def, None);
    let ctx = UpdateContext {
        transform: Mat4::from_translation(Vec3::new(100.0, 0.0, 0.0)),
        cursor: Some(Vec3::new(150.0, 20.0, 0.0)),
        ..Default::default()
    };
    for _ in 0..30 {
        s.update(1.0 / 60.0, &ctx);
    }
    assert!(s.particle_count() > 0);
    for p in s.root().particles() {
        assert!((p.position - Vec3::new(50.0, 20.0, 0.0)).length() < 1e-3);
    }
}

#[test]
fn unknown_entries_are_reported_not_fatal() {
    let mut def = base(10.0, 1.0, 10);
    def["operator"] = json!([{"name": "frobnicate"}]);
    let s = system(def, None);
    assert_eq!(
        s.unknown_features().collect::<Vec<_>>(),
        ["operator:frobnicate"]
    );
}

#[test]
fn missing_file_is_an_error() {
    let v = vfs(&[]);
    assert!(ParticleSystem::load(&v, "particles/nope.json", None).is_err());
}
