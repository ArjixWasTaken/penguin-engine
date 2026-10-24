use glam::{Mat4, Quat, Vec3};
use serde_json::json;
use wpe_mdl::{
    AnimationLayer, Bone, BoneFrame, Mdl, MdlAnimation, MdlMesh, MorphTarget, PlayMode, Puppet,
    Skeleton,
};

fn bone(parent: Option<usize>, t: Vec3) -> Bone {
    Bone {
        name: String::new(),
        parent,
        bind: Mat4::from_translation(t),
        rest: None,
        simulation: None,
    }
}

fn frame(p: Vec3, angle_z: f32, s: f32) -> BoneFrame {
    BoneFrame {
        position: p,
        angle: Vec3::new(0., 0., angle_z),
        scale: Vec3::splat(s),
    }
}

/// Two bones (root at (10,0), child at +(5,0)); animation 1 moves the root
/// along x by 10 per frame over frames 0..=2 at 1 fps.
fn model(mode: PlayMode) -> Mdl {
    let root = Vec3::new(10., 0., 0.);
    let child = Vec3::new(5., 0., 0.);
    Mdl {
        skeleton: Some(Skeleton {
            version: 1,
            bones: vec![bone(None, root), bone(Some(0), child)],
        }),
        animations: vec![MdlAnimation {
            id: 1,
            name: "move".into(),
            mode,
            fps: 1.0,
            length: 2,
            tracks: vec![
                (0..3)
                    .map(|i| frame(root + Vec3::X * 10. * i as f32, 0., 1.))
                    .collect(),
                vec![frame(child, 0., 1.); 3],
            ],
            events: Vec::new(),
            morph_tracks: Vec::new(),
        }],
        ..Mdl::default()
    }
}

fn layer(extra: serde_json::Value) -> Vec<serde_json::Value> {
    let mut v = json!({"animation": 1, "blend": 1.0, "rate": 1.0, "visible": true});
    v.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    vec![v]
}

fn translation_x(p: &Puppet, t: f64, bone: usize) -> f32 {
    let mut m = Vec::new();
    p.bone_matrices(t, &mut m);
    m[bone].w_axis.x
}

#[test]
fn rest_pose_is_identity() {
    let p = Puppet::new(&model(PlayMode::Loop), &[]);
    assert_eq!(p.bone_count(), 2);
    let mut m = Vec::new();
    p.bone_matrices(3.0, &mut m);
    for b in m {
        assert!(b.abs_diff_eq(Mat4::IDENTITY, 1e-5));
    }
}

#[test]
fn loop_interpolates_and_wraps() {
    let p = Puppet::new(&model(PlayMode::Loop), &layer(json!({})));
    assert_eq!(p.matched_layers(), 1);
    assert!((translation_x(&p, 0.5, 0) - 5.).abs() < 1e-4);
    assert!(
        (translation_x(&p, 0.5, 1) - 5.).abs() < 1e-4,
        "child follows parent"
    );
    assert!(
        (translation_x(&p, 1.5, 0) - 5.).abs() < 1e-4,
        "frame 1 blends back to frame 0"
    );
    assert!((translation_x(&p, 2.25, 0) - 2.5).abs() < 1e-4);
    assert!(
        (translation_x(&p, -0.5, 0) - 5.).abs() < 1e-4,
        "negative time wraps"
    );
}

#[test]
fn rate_scales_time() {
    let p = Puppet::new(&model(PlayMode::Loop), &layer(json!({"rate": 0.5})));
    assert!((translation_x(&p, 1.0, 0) - 5.).abs() < 1e-4);
}

#[test]
fn single_holds_last_frame() {
    let p = Puppet::new(&model(PlayMode::Single), &layer(json!({})));
    assert!((translation_x(&p, 10.0, 0) - 10.).abs() < 1e-4);
}

#[test]
fn mirror_plays_back() {
    let p = Puppet::new(&model(PlayMode::Mirror), &layer(json!({})));
    assert!((translation_x(&p, 1.0, 0) - 10.).abs() < 1e-4);
    assert!((translation_x(&p, 2.0, 0) - 10.).abs() < 1e-4);
    assert!((translation_x(&p, 3.0, 0) - 0.).abs() < 1e-4);
}

#[test]
fn blend_weight_and_visibility() {
    let half = Puppet::new(&model(PlayMode::Loop), &layer(json!({"blend": 0.5})));
    assert!((translation_x(&half, 1.0, 0) - 5.).abs() < 1e-4);
    let hidden = Puppet::new(
        &model(PlayMode::Loop),
        &layer(json!({"visible": {"user": "p", "value": false}})),
    );
    assert_eq!(translation_x(&hidden, 1.0, 0), 0.);
}

#[test]
fn additive_layers_add_their_delta() {
    let mut mdl = model(PlayMode::Loop);
    let mut second = mdl.animations[0].clone();
    second.id = 2;
    mdl.animations.push(second);
    let layers = vec![
        json!({"animation": 1, "additive": false}),
        json!({"animation": 2, "additive": true, "blend": 0.5}),
    ];
    let p = Puppet::new(&mdl, &layers);
    assert!((translation_x(&p, 1.0, 0) - 15.).abs() < 1e-4);
}

#[test]
fn rotation_frames_rotate_about_the_bone() {
    let mut mdl = model(PlayMode::Loop);
    let quarter = std::f32::consts::FRAC_PI_2;
    mdl.animations[0].tracks[0] = vec![frame(Vec3::new(10., 0., 0.), quarter, 1.); 3];
    let p = Puppet::new(&mdl, &layer(json!({})));
    let mut m = Vec::new();
    p.bone_matrices(0.0, &mut m);
    let v = m[0].transform_point3(Vec3::new(11., 0., 0.));
    assert!(v.abs_diff_eq(Vec3::new(10., 1., 0.), 1e-4), "{v}");
    let expected =
        Mat4::from_rotation_translation(Quat::from_rotation_z(quarter), Vec3::new(10., 0., 0.))
            * Mat4::from_translation(Vec3::new(-10., 0., 0.));
    assert!(m[0].abs_diff_eq(expected, 1e-4));
}

#[test]
fn rest_pose_differing_from_bind_moves_vertices() {
    let mut mdl = model(PlayMode::Loop);
    mdl.skeleton.as_mut().unwrap().bones[0].rest =
        Some(Mat4::from_translation(Vec3::new(30., 0., 0.)));
    let p = Puppet::new(&mdl, &[]);
    assert!((translation_x(&p, 0.0, 0) - 20.).abs() < 1e-4);
    assert_eq!(p.rest_world(1).unwrap().w_axis.x, 35.);
}

#[test]
fn g_bones_layout_is_column_major_16_floats() {
    let p = Puppet::new(&model(PlayMode::Loop), &layer(json!({})));
    let mut f = Vec::new();
    p.bones(0.5, &mut f);
    assert_eq!(f.len(), 32);
    assert_eq!(&f[0..4], &[1., 0., 0., 0.]);
    assert!((f[12] - 5.).abs() < 1e-4);
    assert_eq!(f[15], 1.);
}

#[test]
fn layer_json_parsing() {
    let l = AnimationLayer::from_json(&json!({
        "animation": 88, "rate": {"user": "r", "value": 2.0}, "blend": "0.5",
        "visible": {"user": {"condition": "1", "name": "x"}, "value": true}, "additive": true
    }))
    .unwrap();
    assert_eq!(
        l,
        AnimationLayer {
            animation: 88,
            rate: 2.0,
            blend: 0.5,
            visible: true,
            additive: true
        }
    );
    assert!(AnimationLayer::from_json(&json!({"rate": 1})).is_none());
}

#[test]
fn unmatched_layers_and_static_models() {
    let p = Puppet::new(&model(PlayMode::Loop), &[json!({"animation": 99})]);
    assert_eq!(p.matched_layers(), 0);
    assert_eq!(p.layers().count(), 1);
    let p = Puppet::new(&Mdl::default(), &[]);
    let mut f = vec![1.0];
    p.bones(1.0, &mut f);
    assert!(f.is_empty());
}

/// [`model`] with two blend shapes on the warp mesh; animation 1 keyframes
/// the first one 0 → 1 → 0.5 and has no track for the second.
fn morph_model(mode: PlayMode) -> Mdl {
    let mut mdl = model(mode);
    let shape = |name: &str| MorphTarget {
        name: name.into(),
        positions: vec![[0.; 3]],
        deltas: vec![[1., 0., 0.]],
    };
    mdl.meshes.push(MdlMesh {
        positions: vec![[0.; 3]],
        morphs: vec![shape("blink"), shape("smile")],
        ..MdlMesh::default()
    });
    mdl.animations[0].morph_tracks = vec![vec![0., 1., 0.5]];
    mdl
}

fn weights(p: &Puppet, t: f64, base: &[f32]) -> Vec<f32> {
    let mut out = Vec::new();
    p.morph_weights(t, base, &mut out);
    out
}

#[test]
fn blend_shape_lookup() {
    let p = Puppet::new(&morph_model(PlayMode::Loop), &[]);
    assert_eq!(p.blend_shape_count(), 2);
    assert_eq!(p.blend_shape_names(), ["blink", "smile"]);
    assert_eq!(p.blend_shape_index("smile"), Some(1));
    assert_eq!(p.blend_shape_index("frown"), None);
    assert_eq!(weights(&p, 1.0, &[0.25]), [0.25, 0.0]);
    let p = Puppet::new(&model(PlayMode::Loop), &[]);
    assert_eq!(p.blend_shape_count(), 0);
}

#[test]
fn animated_blend_shape_weights() {
    let mdl = morph_model(PlayMode::Loop);
    let p = Puppet::new(&mdl, &layer(json!({})));
    assert_eq!(weights(&p, 0.0, &[]), [0.0, 0.0]);
    assert_eq!(weights(&p, 0.5, &[]), [0.5, 0.0]);
    assert_eq!(weights(&p, 1.0, &[0.0, 0.7]), [1.0, 0.7]);
    let p = Puppet::new(&mdl, &layer(json!({"blend": 0.5})));
    assert_eq!(weights(&p, 1.0, &[0.2]), [0.6, 0.0]);
    let p = Puppet::new(&mdl, &layer(json!({"additive": true})));
    assert_eq!(weights(&p, 1.0, &[0.2]), [1.2, 0.0]);
    let p = Puppet::new(&mdl, &layer(json!({"visible": false})));
    assert_eq!(weights(&p, 1.0, &[0.2]), [0.2, 0.0]);
    let p = Puppet::new(&morph_model(PlayMode::Single), &layer(json!({})));
    assert_eq!(
        weights(&p, 5.0, &[]),
        [1.0, 0.0],
        "single holds frame length − 1"
    );
}
