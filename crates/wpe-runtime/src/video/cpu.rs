//! Software output: mpv's `sw` render API writes RGBA frames into memory,
//! which the renderer uploads.

use std::ffi::c_void;
use std::sync::{Arc, Mutex};

use libmpv2::Mpv;
use libmpv2_sys as sys;

use super::{RenderContext, VideoError, Wake, params};

/// The most recent decoded frame, taken by the renderer.
#[derive(Clone, Default)]
pub struct Frames(Arc<Mutex<Option<Vec<u8>>>>);

impl Frames {
    pub fn take(&self) -> Option<Vec<u8>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

/// 64-byte aligned pixel storage, as mpv's SIMD paths prefer.
#[repr(C, align(64))]
#[derive(Clone, Copy)]
struct Block([u8; 64]);

pub struct Output {
    render: RenderContext,
    size: [i32; 2],
    stride: usize,
    buf: Vec<Block>,
    frames: Frames,
}

impl Output {
    pub fn new(
        mpv: &Mpv,
        wake: &Arc<Wake>,
        width: u32,
        height: u32,
        frames: Frames,
    ) -> Result<Self, VideoError> {
        let render = RenderContext::new(
            mpv,
            wake,
            "software",
            params([(
                sys::mpv_render_param_type_MPV_RENDER_PARAM_API_TYPE,
                sys::MPV_RENDER_API_TYPE_SW.as_ptr() as *mut c_void,
            )]),
        )?;
        let stride = (width as usize * 4).next_multiple_of(64);
        Ok(Self {
            render,
            size: [width as i32, height as i32],
            stride,
            buf: vec![Block([0; 64]); stride * height as usize / 64],
            frames,
        })
    }
}

impl super::Output for Output {
    fn render(&mut self) -> bool {
        if !self.render.frame_due() {
            return false;
        }
        let format = c"rgb0";
        let rendered = self.render.render(params([
            (
                sys::mpv_render_param_type_MPV_RENDER_PARAM_SW_SIZE,
                self.size.as_mut_ptr() as *mut c_void,
            ),
            (
                sys::mpv_render_param_type_MPV_RENDER_PARAM_SW_FORMAT,
                format.as_ptr() as *mut c_void,
            ),
            (
                sys::mpv_render_param_type_MPV_RENDER_PARAM_SW_STRIDE,
                &mut self.stride as *mut usize as *mut c_void,
            ),
            (
                sys::mpv_render_param_type_MPV_RENDER_PARAM_SW_POINTER,
                self.buf.as_mut_ptr() as *mut c_void,
            ),
        ]));
        if !rendered {
            return false;
        }
        let bytes: &[u8] = unsafe {
            std::slice::from_raw_parts(self.buf.as_ptr() as *const u8, self.buf.len() * 64)
        };
        let row = self.size[0] as usize * 4;
        let mut frame = Vec::with_capacity(row * self.size[1] as usize);
        for line in bytes.chunks_exact(self.stride) {
            frame.extend_from_slice(&line[..row]);
        }
        // "rgb0" leaves the fourth byte undefined; video is opaque.
        for alpha in frame.iter_mut().skip(3).step_by(4) {
            *alpha = 255;
        }
        *self.frames.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(frame);
        true
    }
}
