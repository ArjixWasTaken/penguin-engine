mod fixtures;

use fixtures::*;
use glam::Vec3;
use wpe_mdl::{Mdl, MdlError, PlayMode, VertexFormat};

fn tri_model(version: u32, format: u32, vertex: &[f32]) -> Bytes {
    let mut b = Bytes::mdlv(version);
    b.u32(format).u32(1).u32(1).str("materials/m.json").u32(0);
    if version >= 17 {
        b.floats(&[-1., -2., -3., 1., 2., 3.]);
    }
    if version >= 16 {
        b.u32(format);
    }
    b.u32((vertex.len() * 4 * 3) as u32);
    for i in 0..3 {
        let v: Vec<f32> = vertex.iter().map(|x| x + i as f32).collect();
        b.floats(&v);
    }
    b.u32(6).u16(0).u16(1).u16(2);
    if version >= 21 {
        b.u8(0).u8(0);
    }
    if version >= 23 {
        b.u32(0);
    }
    b.u8(0);
    b
}

#[test]
fn model_position_texcoord_v13() {
    let mdl = Mdl::parse(&tri_model(13, 9, &[0., 0., 0., 0.5, 0.25]).0).unwrap();
    assert_eq!(mdl.version, 13);
    assert!(!mdl.is_puppet());
    let m = &mdl.meshes[0];
    assert_eq!(m.material.as_deref(), Some("materials/m.json"));
    assert_eq!(m.positions, [[0., 0., 0.], [1., 1., 1.], [2., 2., 2.]]);
    assert_eq!(m.texcoords[2], [2.5, 2.25]);
    assert!(m.normals.is_empty() && m.blend_indices.is_empty());
    assert_eq!(m.indices, [0, 1, 2]);
    assert!(mdl.warnings.is_empty());
}

#[test]
fn model_normals_v16_reads_format_field() {
    let mdl = Mdl::parse(&tri_model(16, 11, &[1., 2., 3., 0., 0., 1., 0.1, 0.2]).0).unwrap();
    let m = &mdl.meshes[0];
    assert_eq!(
        m.format,
        VertexFormat::POSITION | VertexFormat::NORMAL | VertexFormat::TEXCOORD
    );
    assert_eq!(m.normals[0], [0., 0., 1.]);
    assert_eq!(m.texcoords[1], [1.1, 1.2]);
    assert!(m.bounds.is_none());
}

#[test]
fn model_tangents_v23_with_bounds_and_post_block() {
    let v = [1., 2., 3., 0., 0., 1., 1., 0., 0., -1., 0.5, 0.5];
    let mdl = Mdl::parse(&tri_model(23, 15, &v).0).unwrap();
    let m = &mdl.meshes[0];
    assert_eq!(m.bounds, Some(([-1., -2., -3.], [1., 2., 3.])));
    assert_eq!(m.tangents[0], [1., 0., 0., -1.]);
    assert_eq!(m.texcoords[0], [0.5, 0.5]);
    assert!(mdl.skeleton.is_none());
    assert!(mdl.warnings.is_empty());
}

#[test]
fn model_secondary_texcoord() {
    let v = [0., 0., 0., 0., 0., 1., 1., 0., 0., 1., 0.1, 0.2, 0.3, 0.4];
    assert_eq!(VertexFormat::from_bits_retain(39).stride(), Some(56));
    let mdl = Mdl::parse(&tri_model(19, 39, &v).0).unwrap();
    let m = &mdl.meshes[0];
    assert_eq!(m.texcoords[0], [0.1, 0.2]);
    assert_eq!(m.texcoords1[0], [0.3, 0.4]);
}

#[test]
fn wide_indices_flag() {
    let mut b = Bytes::mdlv(13);
    b.u32(9).u32(1).u32(1).str("").u32(1).u32(3 * 20);
    for i in 0..3 {
        b.floats(&[i as f32, 0., 0., 0., 0.]);
    }
    b.u32(12).u32(0).u32(1).u32(2).u8(0);
    let mdl = Mdl::parse(&b.0).unwrap();
    assert!(mdl.meshes[0].wide_indices);
    assert_eq!(mdl.meshes[0].indices, [0, 1, 2]);
    assert_eq!(mdl.meshes[0].material, None);
}

#[test]
fn large_mesh_implies_wide_indices() {
    let n = 0x1_0001usize;
    let mut b = Bytes::mdlv(13);
    b.u32(9).u32(1).u32(1).str("").u32(0).u32((n * 20) as u32);
    b.0.resize(b.0.len() + n * 20, 0);
    b.u32(12).u32(0).u32(0x1_0000).u32(1).u8(0);
    let mdl = Mdl::parse(&b.0).unwrap();
    assert!(mdl.meshes[0].wide_indices);
    assert_eq!(mdl.meshes[0].indices, [0, 0x1_0000, 1]);
}

#[test]
fn out_of_range_triangles_are_dropped() {
    let mut b = Bytes::mdlv(13);
    b.u32(9).u32(1).u32(1).str("").u32(0).u32(3 * 20);
    b.0.resize(b.0.len() + 60, 0);
    b.u32(12).u16(0).u16(1).u16(2).u16(0).u16(1).u16(3);
    let mdl = Mdl::parse(&b.0).unwrap();
    assert_eq!(mdl.meshes[0].indices, [0, 1, 2]);
    assert_eq!(mdl.warnings.len(), 1);
}

#[test]
fn bad_index_size_fails() {
    let mut b = Bytes::mdlv(13);
    b.u32(9).u32(1).u32(1).str("").u32(0).u32(60);
    b.0.resize(b.0.len() + 60, 0);
    b.u32(4).u16(0).u16(1);
    assert!(matches!(Mdl::parse(&b.0), Err(MdlError::Malformed { .. })));
}

#[test]
fn hostile_and_truncated_inputs_fail_cleanly() {
    assert!(matches!(Mdl::parse(b""), Err(MdlError::BadMagic)));
    assert!(matches!(Mdl::parse(b"MDLV"), Err(MdlError::BadMagic)));
    assert!(matches!(Mdl::parse(&[0xff; 9]), Err(MdlError::BadMagic)));
    let mut b = Bytes::mdlv(13);
    b.u32(9).u32(1).u32(u32::MAX);
    assert!(Mdl::parse(&b.0).is_err());
    let mut b = Bytes::mdlv(13);
    b.u32(9).u32(1).u32(1).str("").u32(0).u32(0xFFFF_FFF0);
    assert!(Mdl::parse(&b.0).is_err());
    let full = tri_model(23, 15, &[0.; 12]).0;
    for cut in 9..full.len() - 1 {
        let _ = Mdl::parse(&full[..cut]);
    }
}

#[test]
fn texcoord_vec4_submesh_with_extra_header_word() {
    let mut b = Bytes::mdlv(23);
    b.u32(STD_PUPPET).u32(1).u32(2);
    b.str("materials/m.json")
        .u32(0)
        .floats(&[0.; 6])
        .u32(9)
        .u32(60);
    b.0.resize(b.0.len() + 60, 0);
    b.u32(6).u16(0).u16(1).u16(2).u8(0).u8(0).u32(0);
    b.str("materials/channelmap.json")
        .u32(2)
        .u32(1)
        .floats(&[0.; 6])
        .u32(0x0080_0021)
        .u32(44);
    b.floats(&[1., 2., 3.])
        .u32(0)
        .u32(0)
        .u32(0)
        .u32(0)
        .floats(&[0.1, 0.2, 0.3, 0.4]);
    b.u32(6).u16(0).u16(0).u16(0).u8(0).u8(0).u32(0);
    b.u8(0);
    let mdl = Mdl::parse(&b.0).unwrap();
    assert!(mdl.warnings.is_empty(), "{:?}", mdl.warnings);
    let m = &mdl.meshes[1];
    assert_eq!(m.material.as_deref(), Some("materials/channelmap.json"));
    assert_eq!(m.positions, [[1., 2., 3.]]);
    assert_eq!(m.texcoords, [[0.1, 0.2]]);
    assert_eq!(m.texcoords1, [[0.3, 0.4]]);
}

#[test]
fn unknown_vertex_format_is_skipped() {
    let mut b = Bytes::mdlv(16);
    b.u32(9).u32(1).u32(2);
    b.str("a").u32(0).u32(0x10 | 1).u32(40);
    b.0.resize(b.0.len() + 40, 0);
    b.u32(6).u16(0).u16(0).u16(0);
    b.str("b").u32(0).u32(9).u32(60);
    b.0.resize(b.0.len() + 60, 0);
    b.u32(6).u16(0).u16(1).u16(2).u8(0);
    let mdl = Mdl::parse(&b.0).unwrap();
    assert_eq!(mdl.meshes.len(), 1);
    assert_eq!(mdl.meshes[0].material.as_deref(), Some("b"));
    assert_eq!(mdl.warnings.len(), 1);
}

#[test]
fn v21_morphs_parts_and_v23_clipping_masks_are_skipped() {
    let mut b = Bytes::mdlv(23);
    b.u32(9)
        .u32(1)
        .u32(1)
        .str("")
        .u32(0)
        .floats(&[0.; 6])
        .u32(9)
        .u32(60);
    b.0.resize(b.0.len() + 60, 0);
    b.u32(6).u16(0).u16(1).u16(2);
    b.u8(1).u32(1).u32(36).floats(&[5.; 9]);
    b.u8(1).u32(16).u32(0).u32(0).u32(0).u32(3);
    b.u32(1)
        .u32(915)
        .u32(0)
        .str("masks/clipping_mask_0")
        .u32(0)
        .u32(1)
        .u32(4)
        .u32(2)
        .u32(2)
        .u32(3);
    b.u8(0);
    let mdl = Mdl::parse(&b.0).unwrap();
    assert_eq!(mdl.meshes[0].indices, [0, 1, 2]);
    assert_eq!(mdl.warnings.len(), 1, "{:?}", mdl.warnings);
    assert!(mdl.warnings[0].contains("clipping_mask_0"));
}

/// v14 puppet: standard vertex layout, MDLS v2, MDLA v2 with an event.
fn puppet_v14() -> Bytes {
    let mut b = Bytes::mdlv(14);
    b.u32(STD_PUPPET)
        .u32(1)
        .u32(1)
        .str("materials/p.json")
        .u32(0)
        .u32(3 * 52);
    puppet_vertex(
        &mut b,
        [-1., -1., 0.],
        [0, 0, 0, 0],
        [1., 0., 0., 0.],
        [0., 1.],
        false,
    );
    puppet_vertex(
        &mut b,
        [1., -1., 0.],
        [1, 0, 0, 0],
        [1., 0., 0., 0.],
        [1., 1.],
        false,
    );
    puppet_vertex(
        &mut b,
        [0., 1., 0.],
        [0, 1, 255, 255],
        [0.5, 0.5, 0., 0.],
        [0.5, 0.],
        false,
    );
    b.u32(6).u16(0).u16(1).u16(2);
    let s = b.section("MDLS", 2);
    b.u16(2).u16(0);
    bone(&mut b, -1, [10., 20., 0.], "");
    bone(&mut b, 0, [5., 0., 0.], "{\"a\":null}");
    b.u16(0).u8(0).u32(0).u32(0).u8(1);
    b.0.resize(b.0.len() + 2 * 76, 0);
    b.u8(0);
    b.end_section(s);
    let a = b.section("MDLA", 2);
    b.u32(1)
        .i32(7)
        .i32(0)
        .str("wave")
        .str("loop")
        .f32(10.)
        .i32(2)
        .i32(0)
        .u32(2);
    for (i, t) in [[10., 20., 0.], [5., 0., 0.]].into_iter().enumerate() {
        b.i32(0).u32(3 * 36);
        for f in 0..3 {
            let dx = if i == 1 { f as f32 * 2. } else { 0. };
            frame(&mut b, [t[0] + dx, t[1], 0.], [0.; 3], [1.; 3]);
        }
    }
    b.u32(1).f32(0.1).str(r#"{"frame":1,"name":"step"}"#);
    b.end_section(a);
    b.u8(0);
    b
}

#[test]
fn puppet_v14_skeleton_animation_and_events() {
    let mdl = Mdl::parse(&puppet_v14().0).unwrap();
    assert!(mdl.warnings.is_empty(), "{:?}", mdl.warnings);
    let sk = mdl.skeleton.as_ref().unwrap();
    assert_eq!(sk.version, 2);
    assert_eq!(sk.bones.len(), 2);
    assert_eq!(sk.bones[0].parent, None);
    assert_eq!(sk.bones[1].parent, Some(0));
    assert_eq!(sk.bones[1].bind.w_axis.truncate(), Vec3::new(5., 0., 0.));
    assert_eq!(sk.bones[1].simulation.as_deref(), Some("{\"a\":null}"));
    assert!(sk.bones[0].rest.is_none());
    let m = &mdl.meshes[0];
    assert_eq!(
        m.blend_indices[2],
        [0, 1, 0, 0],
        "out-of-range indices clamp to 0"
    );
    assert_eq!(m.blend_weights[2], [0.5, 0.5, 0., 0.]);
    assert_eq!(m.texcoords[2], [0.5, 0.]);
    assert_eq!(mdl.animation_version, Some(2));
    let a = &mdl.animations[0];
    assert_eq!(
        (a.id, a.name.as_str(), a.mode, a.fps, a.length),
        (7, "wave", PlayMode::Loop, 10., 2)
    );
    assert_eq!(a.tracks[1][2].position, Vec3::new(9., 0., 0.));
    assert_eq!(a.events.len(), 1);
    assert_eq!((a.events[0].frame, a.events[0].name.as_str()), (1, "step"));
}

/// v23 puppet: alternative vertex layout, MDLS v4 with rest poses, MDAT,
/// MDLA v6 with two animations separated by a morph-weight trailer.
fn puppet_v23(rest: bool) -> Bytes {
    let mut b = Bytes::mdlv(23);
    b.u32(STD_PUPPET)
        .u32(1)
        .u32(1)
        .str("materials/p.json")
        .u32(0)
        .floats(&[0.; 6]);
    b.u32(ALT_PUPPET).u32(2 * 80);
    puppet_vertex(
        &mut b,
        [1., 2., 0.],
        [0, 0, 0, 0],
        [1., 0., 0., 0.],
        [0.25, 0.75],
        true,
    );
    puppet_vertex(
        &mut b,
        [3., 4., 0.],
        [1, 0, 0, 0],
        [1., 0., 0., 0.],
        [0.5, 0.5],
        true,
    );
    b.u32(6).u16(0).u16(1).u16(1).u8(0).u8(0).u32(0);
    let s = b.section("MDLS", 4);
    b.u16(3).u16(0);
    bone(&mut b, -1, [0., 0., 0.], "");
    bone(&mut b, 0, [10., 0., 0.], "");
    bone(&mut b, 5, [1., 1., 0.], "");
    b.u16(0).u8(rest as u8);
    if rest {
        b.translated(100., 0., 0.)
            .translated(10., 0., 0.)
            .translated(1., 1., 0.);
    }
    b.u32(0)
        .u32(0)
        .u8(0)
        .u8(1)
        .u32(2)
        .u32(0)
        .u32(1)
        .u8(1)
        .u32(0)
        .u32(100)
        .u32(200);
    b.end_section(s);
    let t = b.section("MDAT", 1);
    b.u16(1).u16(1).str("head").translated(0., 50., 0.);
    b.end_section(t);
    let a = b.section("MDLA", 6);
    b.u32(2);
    for (id, name, mode) in [(183, "Animation 1", "mirror"), (235, "", "single")] {
        b.i32(id)
            .i32(0)
            .str(name)
            .str(mode)
            .f32(30.)
            .i32(1)
            .i32(0)
            .u32(3);
        for _ in 0..3 {
            b.i32(0).u32(2 * 36);
            frame(&mut b, [0.; 3], [0.; 3], [1.; 3]);
            frame(&mut b, [0.; 3], [0.; 3], [1.; 3]);
        }
        b.u32(1).u32(0).u32(8).f32(1.).f32(1.);
        b.0.resize(b.0.len() + 31, 0);
    }
    b.end_section(a);
    let e = b.section("MDLE", 2);
    b.u32(0xdead_beef);
    b.end_section(e);
    b.u8(0);
    b
}

#[test]
fn puppet_v23_sections() {
    let mdl = Mdl::parse(&puppet_v23(true).0).unwrap();
    assert!(mdl.warnings.is_empty(), "{:?}", mdl.warnings);
    let m = &mdl.meshes[0];
    assert_eq!(m.format.bits(), ALT_PUPPET);
    assert_eq!(m.positions[1], [3., 4., 0.]);
    assert_eq!(m.normals[0], [0., 0., 1.]);
    assert_eq!(m.tangents[0], [1., 0., 0., 1.]);
    assert_eq!(m.blend_indices[1], [1, 0, 0, 0]);
    assert_eq!(m.texcoords[0], [0.25, 0.75]);
    let sk = mdl.skeleton.as_ref().unwrap();
    assert_eq!(
        sk.bones[2].parent, None,
        "a parent after the bone becomes a root"
    );
    assert_eq!(sk.bones[0].rest.unwrap().w_axis.x, 100.);
    let head = mdl.attachment("head").unwrap();
    assert_eq!(head.bone, 1);
    assert_eq!(head.transform.w_axis.y, 50.);
    let ids: Vec<i32> = mdl.animations.iter().map(|a| a.id).collect();
    assert_eq!(ids, [183, 235]);
    assert_eq!(mdl.animations[0].mode, PlayMode::Mirror);
    assert_eq!(mdl.animations[1].mode, PlayMode::Single);
    assert_eq!(mdl.animations[1].name, "");
}

#[test]
fn skeleton_without_section_end_offsets() {
    let mut b = Bytes::mdlv(14);
    b.u32(STD_PUPPET).u32(1).u32(1).str("").u32(0).u32(0).u32(0);
    b.tag("MDLS", 2).u32(0).u16(1).u16(0);
    bone(&mut b, -1, [1., 2., 3.], "");
    b.u16(0)
        .u8(1)
        .translated(4., 5., 6.)
        .u32(1)
        .floats(&[0.; 3])
        .u32(0)
        .u8(0)
        .u8(0);
    b.tag("MDAT", 1).u32(0).u16(1).u16(0).str("tip").identity();
    b.u8(0);
    let mdl = Mdl::parse(&b.0).unwrap();
    let bone = &mdl.skeleton.as_ref().unwrap().bones[0];
    assert_eq!(bone.bind.w_axis.truncate(), Vec3::new(1., 2., 3.));
    assert_eq!(bone.rest_local().w_axis.truncate(), Vec3::new(4., 5., 6.));
    assert_eq!(mdl.attachments[0].name, "tip");
    assert!(mdl.animations.is_empty());
}

#[test]
fn bone_matrix_size_must_be_64() {
    let mut b = Bytes::mdlv(14);
    b.u32(STD_PUPPET).u32(1).u32(1).str("").u32(0).u32(0).u32(0);
    let s = b.section("MDLS", 1);
    b.u16(1).u16(0).str("").i32(1).i32(-1).u32(48);
    b.0.resize(b.0.len() + 80, 0);
    b.end_section(s);
    assert!(matches!(Mdl::parse(&b.0), Err(MdlError::Malformed { .. })));
}

#[test]
fn puppet_without_blend_attributes_binds_to_bone_zero() {
    let mut b = Bytes::mdlv(23);
    b.u32(9)
        .u32(1)
        .u32(1)
        .str("")
        .u32(0)
        .floats(&[0.; 6])
        .u32(9)
        .u32(60);
    b.0.resize(b.0.len() + 60, 0);
    b.u32(6).u16(0).u16(1).u16(2).u8(0).u8(0).u32(0);
    let s = b.section("MDLS", 4);
    b.u16(1).u16(0);
    bone(&mut b, -1, [0.; 3], "");
    b.u16(0).u8(0).u32(0).u32(0).u8(0).u8(0).u8(0);
    b.end_section(s);
    b.u8(0);
    let mdl = Mdl::parse(&b.0).unwrap();
    assert!(mdl.is_puppet());
    assert_eq!(mdl.meshes[0].blend_indices, vec![[0; 4]; 3]);
    assert_eq!(mdl.meshes[0].blend_weights, vec![[1., 0., 0., 0.]; 3]);
}

#[test]
fn unusable_animation_timing_is_dropped() {
    let mut b = Bytes::mdlv(14);
    b.u32(STD_PUPPET).u32(1).u32(1).str("").u32(0).u32(0).u32(0);
    let s = b.section("MDLS", 1);
    b.u16(1).u16(0);
    bone(&mut b, -1, [0.; 3], "");
    b.end_section(s);
    let a = b.section("MDLA", 2);
    b.u32(2);
    b.i32(1)
        .i32(0)
        .str("bad")
        .str("loop")
        .f32(0.)
        .i32(10)
        .i32(0)
        .u32(0)
        .u32(0);
    b.i32(2)
        .i32(0)
        .str("good")
        .str("loop")
        .f32(30.)
        .i32(1)
        .i32(0)
        .u32(1)
        .i32(0)
        .u32(36);
    frame(&mut b, [0.; 3], [0.; 3], [1.; 3]);
    b.u32(0);
    b.end_section(a);
    let mdl = Mdl::parse(&b.0).unwrap();
    assert_eq!(mdl.animations.len(), 1);
    assert_eq!(mdl.animations[0].name, "good");
    assert_eq!(mdl.warnings.len(), 1);
}

#[test]
fn bad_frame_size_keeps_earlier_animations() {
    let mut b = Bytes::mdlv(14);
    b.u32(STD_PUPPET).u32(1).u32(1).str("").u32(0).u32(0).u32(0);
    let a = b.section("MDLA", 2);
    b.u32(2);
    b.i32(1)
        .i32(0)
        .str("a")
        .str("loop")
        .f32(30.)
        .i32(1)
        .i32(0)
        .u32(0)
        .u32(0);
    b.i32(2)
        .i32(0)
        .str("b")
        .str("loop")
        .f32(30.)
        .i32(1)
        .i32(0)
        .u32(1)
        .i32(0)
        .u32(35);
    b.0.resize(b.0.len() + 40, 0);
    b.end_section(a);
    let mdl = Mdl::parse(&b.0).unwrap();
    assert_eq!(mdl.animations.len(), 1);
    assert_eq!(mdl.warnings.len(), 1);
}
